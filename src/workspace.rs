//! Isolated project copies used to evaluate trials. whichdep itself writes to
//! the copied workspaces rather than the user's project; commands run as the
//! current user, so this is not an OS-level security sandbox.

use crate::fingerprint::StableHasher;
use crate::git;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SCRATCH_ID: AtomicU64 = AtomicU64::new(0);

pub struct Scratch {
    root: PathBuf,
    keep: bool,
}

impl Scratch {
    pub fn new(keep: bool) -> io::Result<Scratch> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut last_error = None;
        for _ in 0..32 {
            let id = SCRATCH_ID.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("whichdep-{}-{nanos}-{id}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
                    }
                    return Ok(Scratch { root, keep });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last_error = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a unique scratch directory",
            )
        }))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self, name: &str) -> io::Result<PathBuf> {
        let p = self.root.join(name);
        fs::create_dir_all(&p)?;
        Ok(p)
    }

    pub fn kept(&self) -> bool {
        self.keep
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// Directories that are never copied: version control, build output and
/// virtual environments.
const SKIP: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    ".venv",
    "venv",
    ".whichdep-venv",
    "__pycache__",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];

fn skipped(rel: &Path) -> bool {
    rel.components()
        .any(|c| SKIP.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

/// Copies the project into `dst` and returns the number of files. Inside a
/// git work tree that is what git would consider part of the project
/// (tracked and untracked but not ignored), elsewhere everything except the
/// directories in [`SKIP`]. Symlinks that escape the source tree are rejected.
pub fn copy_project(src: &Path, dst: &Path) -> io::Result<usize> {
    fs::create_dir_all(dst)?;
    match git::tracked_and_untracked(src) {
        Some(files) => {
            let mut n = 0;
            for rel in files.iter().filter(|r| !skipped(r)) {
                n += copy_entry(src, &src.join(rel), &dst.join(rel))?;
            }
            Ok(n)
        }
        None => walk(src, src, dst),
    }
}

/// Copy an already prepared project snapshot without consulting Git. This is
/// used for per-trial copies of the immutable template and is safe even when
/// the temporary directory happens to live beneath another Git work tree.
pub fn copy_snapshot(src: &Path, dst: &Path) -> io::Result<usize> {
    fs::create_dir_all(dst)?;
    walk(src, src, dst)
}

fn walk(root: &Path, src: &Path, dst: &Path) -> io::Result<usize> {
    fs::create_dir_all(dst)?;
    let mut entries = fs::read_dir(src)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    let mut n = 0;
    for entry in entries {
        let name = entry.file_name();
        if SKIP.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        n += copy_entry(root, &entry.path(), &dst.join(&name))?;
    }
    Ok(n)
}

fn symlink_stays_inside(root: &Path, link: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let Ok(relative_link) = link.strip_prefix(root) else {
        return false;
    };
    let mut depth: isize = relative_link
        .parent()
        .map_or(0, |p| p.components().count() as isize);
    for component in target.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return false,
        }
    }
    true
}

fn copy_entry(root: &Path, from: &Path, to: &Path) -> io::Result<usize> {
    let meta = match fs::symlink_metadata(from) {
        Ok(m) => m,
        // tracked but deleted in the work tree
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() {
        let target = fs::read_link(from)?;
        if !symlink_stays_inside(root, from, &target) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to copy symlink outside the project: {} -> {}",
                    from.display(),
                    target.display()
                ),
            ));
        }
        #[cfg(unix)]
        {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            let _ = fs::remove_file(to);
            std::os::unix::fs::symlink(target, to)?;
            return Ok(1);
        }
        #[cfg(not(unix))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symbolic links are not supported by this workspace copier on this platform",
        ));
    }
    if meta.is_dir() {
        return walk(root, from, to);
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to)?;
    Ok(1)
}

/// Fingerprint every file copied into a trial workspace. Paths and symlink
/// targets are included so moving or retargeting a file invalidates old cache.
pub fn fingerprint_tree(root: &Path) -> io::Result<String> {
    fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
        let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            if SKIP.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            let meta = fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() || meta.is_file() {
                files.push(path);
            } else if meta.is_dir() {
                files.push(path.clone());
                collect(&path, files)?;
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    collect(root, &mut files)?;
    files.sort();
    let mut hash = StableHasher::new();
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
        hash.update(&(relative.len() as u64).to_le_bytes());
        hash.update(relative.as_bytes());
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            hash.update(b"symlink:");
            let target = fs::read_link(&path)?.to_string_lossy().into_owned();
            hash.update(&(target.len() as u64).to_le_bytes());
            hash.update(target.as_bytes());
        } else if meta.is_dir() {
            hash.update(b"directory");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                hash.update(&meta.permissions().mode().to_le_bytes());
            }
        } else {
            hash.update(b"file:");
            hash.update(&meta.len().to_le_bytes());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                hash.update(&meta.permissions().mode().to_le_bytes());
            }
            #[cfg(not(unix))]
            hash.update(&[u8::from(meta.permissions().readonly())]);
            let mut file = fs::File::open(&path)?;
            let mut buffer = [0u8; 16 * 1024];
            loop {
                use std::io::Read;
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
        }
    }
    Ok(hash.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("whichdep-ws-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn copies_sources_but_not_build_output_or_environments() {
        let src = tmp("src");
        fs::create_dir_all(src.join("src")).unwrap();
        fs::create_dir_all(src.join("target/debug")).unwrap();
        fs::create_dir_all(src.join(".venv/bin")).unwrap();
        fs::write(src.join("Cargo.toml"), "x").unwrap();
        fs::write(src.join("src/lib.rs"), "y").unwrap();
        fs::write(src.join("target/debug/big"), "z").unwrap();
        fs::write(src.join(".venv/bin/python"), "z").unwrap();
        let dst = tmp("dst");
        let n = copy_project(&src, &dst.join("work")).unwrap();
        assert_eq!(n, 2);
        assert!(dst.join("work/src/lib.rs").exists());
        assert!(!dst.join("work/target").exists());
        assert!(!dst.join("work/.venv").exists());
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }

    #[test]
    fn scratch_is_removed_unless_kept() {
        let s = Scratch::new(false).unwrap();
        let root = s.root().to_path_buf();
        assert!(root.exists());
        drop(s);
        assert!(!root.exists());
        let s = Scratch::new(true).unwrap();
        let root = s.root().to_path_buf();
        drop(s);
        assert!(root.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_that_escape_the_project() {
        use std::os::unix::fs::symlink;
        let src = tmp("symlink-src");
        let outside = tmp("symlink-outside");
        fs::write(outside.join("secret"), "do not copy").unwrap();
        symlink(outside.join("secret"), src.join("escape")).unwrap();
        let dst = tmp("symlink-dst");
        let error = copy_project(&src, &dst.join("work")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let _ = fs::remove_dir_all(src);
        let _ = fs::remove_dir_all(outside);
        let _ = fs::remove_dir_all(dst);
    }

    #[test]
    fn tree_fingerprint_changes_when_source_changes() {
        let src = tmp("fingerprint");
        fs::write(src.join("a.txt"), "one").unwrap();
        let first = fingerprint_tree(&src).unwrap();
        fs::write(src.join("a.txt"), "two").unwrap();
        let second = fingerprint_tree(&src).unwrap();
        assert_ne!(first, second);
        let _ = fs::remove_dir_all(src);
    }
}
