//! The little bit of git that whichdep needs: reading a file at a revision and
//! listing the files of a work tree.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(cwd: &Path, args: &[&str]) -> Option<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()
}

pub fn is_inside_work_tree(cwd: &Path) -> bool {
    git(cwd, &["rev-parse", "--is-inside-work-tree"])
        .is_some_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
}

/// The contents of `rel_path` (relative to `cwd`) at `rev`.
pub fn show(cwd: &Path, rev: &str, rel_path: &str) -> Result<String, String> {
    let rel = rel_path.strip_prefix("./").unwrap_or(rel_path);
    let spec = format!("{rev}:./{rel}");
    let out = Command::new("git")
        .args(["show", &spec])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim().trim_start_matches("fatal: ");
        return Err(format!("cannot read {rel} at {rev}: {err}"));
    }
    String::from_utf8(out.stdout).map_err(|_| format!("{rel} at {rev} is not valid UTF-8"))
}

/// `a1b2c3d` for a revision, if git knows it.
pub fn short(cwd: &Path, rev: &str) -> Option<String> {
    let spec = format!("{rev}^{{commit}}");
    let out = git(cwd, &["rev-parse", "--short", &spec])?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Tracked and untracked files that are not ignored, relative to `cwd`.
pub fn tracked_and_untracked(cwd: &Path) -> Option<Vec<PathBuf>> {
    if !is_inside_work_tree(cwd) {
        return None;
    }
    let out = git(
        cwd,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    if !out.status.success() {
        return None;
    }
    let mut files: Vec<PathBuf> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(String::from_utf8_lossy(s).into_owned()))
        .collect();
    files.sort();
    files.dedup();
    Some(files)
}
