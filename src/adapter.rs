//! Extensible ecosystem adapter API and registry.
//!
//! Each supported package ecosystem implements [`Adapter`] to advertise its
//! lockfile conventions and construct an [`Ecosystem`] plan from two lockfile
//! snapshots. Third-party library callers can register custom adapters into an
//! [`AdapterRegistry`] alongside or in place of the built-in ecosystems.

use crate::cargo::CargoPlan;
use crate::model::Ecosystem;
use crate::npm::NpmAdapter;
use crate::pip::PipPlan;
use crate::pnpm::PnpmAdapter;
use crate::uv::UvAdapter;
use std::path::{Component, Path};

/// Factory and discovery contract for a dependency lockfile ecosystem.
pub trait Adapter: Send + Sync {
    /// Canonical lowercase identifier (e.g. `"cargo"`, `"uv"`, `"npm"`, `"pnpm"`, `"pip"`).
    fn id(&self) -> &'static str;

    /// Accepted alternative `--ecosystem` names.
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Human-readable description shown by `whichdep adapters`.
    fn description(&self) -> &'static str {
        self.id()
    }

    /// Default lockfile names searched in the project directory when
    /// `--lockfile` is omitted.
    fn lockfiles(&self) -> &'static [&'static str];

    /// Whether this adapter recognizes the given relative lockfile path.
    fn matches_lockfile(&self, lockfile: &str) -> bool {
        let name = Path::new(lockfile)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.lockfiles().iter().any(|candidate| *candidate == name)
    }

    /// Parse `good_text` and `bad_text` into a runnable [`Ecosystem`] plan.
    fn plan(
        &self,
        lockfile: &str,
        good_text: &str,
        bad_text: &str,
    ) -> Result<Box<dyn Ecosystem>, String>;
}

/// Collection of adapters used for lockfile detection and plan construction.
pub struct AdapterRegistry {
    adapters: Vec<Box<dyn Adapter>>,
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl AdapterRegistry {
    /// Create an empty registry with no built-in adapters.
    pub fn empty() -> Self {
        Self {
            adapters: Vec::new(),
        }
    }

    /// Create a registry populated with all built-in ecosystem adapters.
    pub fn builtin() -> Self {
        let mut reg = Self::empty();
        reg.register(Box::new(CargoAdapter));
        reg.register(Box::new(UvAdapter));
        reg.register(Box::new(PipAdapter));
        reg.register(Box::new(NpmAdapter));
        reg.register(Box::new(PnpmAdapter));
        reg
    }

    /// Register a new adapter, replacing any existing adapter with the same `id()`.
    pub fn register(&mut self, adapter: Box<dyn Adapter>) -> &mut Self {
        let id = adapter.id();
        if let Some(pos) = self.adapters.iter().position(|a| a.id() == id) {
            self.adapters[pos] = adapter;
        } else {
            self.adapters.push(adapter);
        }
        self
    }

    /// Look up an adapter by canonical id or alias (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&dyn Adapter> {
        let needle = name.to_ascii_lowercase();
        self.adapters
            .iter()
            .find(|a| {
                a.id().eq_ignore_ascii_case(&needle)
                    || a.aliases()
                        .iter()
                        .any(|al| al.eq_ignore_ascii_case(&needle))
            })
            .map(Box::as_ref)
    }

    /// Iterate over all registered adapters in registration order.
    pub fn all(&self) -> impl Iterator<Item = &dyn Adapter> {
        self.adapters.iter().map(Box::as_ref)
    }

    /// Choose a lockfile inside `cwd` and match it to a registered adapter.
    pub fn resolve(
        &self,
        cwd: &Path,
        asked_lockfile: Option<&str>,
        forced_ecosystem: Option<&str>,
    ) -> Result<(String, &dyn Adapter), String> {
        let lockfile = self.choose_lockfile(cwd, asked_lockfile)?;
        let adapter = self.choose_adapter(&lockfile, forced_ecosystem)?;
        Ok((lockfile, adapter))
    }

    fn choose_lockfile(&self, cwd: &Path, asked: Option<&str>) -> Result<String, String> {
        if let Some(p) = asked {
            let p = p.strip_prefix("./").unwrap_or(p);
            let path = Path::new(p);
            if path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err(format!(
                    "the lockfile has to be inside the current directory, got `{p}`: run whichdep in the directory that contains it"
                ));
            }
            return Ok(p.to_string());
        }
        let mut candidates: Vec<&str> = Vec::new();
        for adapter in &self.adapters {
            for &lf in adapter.lockfiles() {
                if !candidates.contains(&lf) {
                    candidates.push(lf);
                }
            }
        }
        let found: Vec<&str> = candidates
            .into_iter()
            .filter(|f| cwd.join(f).is_file())
            .collect();
        match found.as_slice() {
            [one] => Ok((*one).to_string()),
            [] => {
                let supported = self.supported_lockfiles().join(", ");
                Err(format!(
                    "no supported lockfile ({supported}) in the current directory: change into the project or pass --lockfile"
                ))
            }
            many => Err(format!(
                "found {} here: pick one with --lockfile",
                many.join(" and ")
            )),
        }
    }

    fn choose_adapter(&self, lockfile: &str, forced: Option<&str>) -> Result<&dyn Adapter, String> {
        if let Some(name) = forced {
            return self.get(name).ok_or_else(|| {
                let ids: Vec<&str> = self.adapters.iter().map(|a| a.id()).collect();
                format!("unknown ecosystem `{name}`: use {}", ids.join(", "))
            });
        }
        self.adapters
            .iter()
            .find(|a| a.matches_lockfile(lockfile))
            .map(Box::as_ref)
            .ok_or_else(|| {
                let ids: Vec<&str> = self.adapters.iter().map(|a| a.id()).collect();
                format!(
                    "cannot tell which ecosystem `{lockfile}` belongs to: pass --ecosystem with one of: {}",
                    ids.join(", ")
                )
            })
    }

    fn supported_lockfiles(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for adapter in &self.adapters {
            for &lf in adapter.lockfiles() {
                if !out.contains(&lf) {
                    out.push(lf);
                }
            }
        }
        out
    }
}

/// Built-in adapter for Rust `Cargo.lock` (versions 3 and 4).
pub struct CargoAdapter;

impl Adapter for CargoAdapter {
    fn id(&self) -> &'static str {
        "cargo"
    }

    fn description(&self) -> &'static str {
        "Rust Cargo.lock (v3, v4)"
    }

    fn lockfiles(&self) -> &'static [&'static str] {
        &["Cargo.lock"]
    }

    fn plan(
        &self,
        lockfile: &str,
        good_text: &str,
        bad_text: &str,
    ) -> Result<Box<dyn Ecosystem>, String> {
        Ok(Box::new(CargoPlan::new(lockfile, good_text, bad_text)?))
    }
}

/// Built-in adapter for pinned Python `requirements.txt` files.
pub struct PipAdapter;

impl Adapter for PipAdapter {
    fn id(&self) -> &'static str {
        "pip"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["requirements"]
    }

    fn description(&self) -> &'static str {
        "Python pinned requirements.txt"
    }

    fn lockfiles(&self) -> &'static [&'static str] {
        &["requirements.txt"]
    }

    fn matches_lockfile(&self, lockfile: &str) -> bool {
        Path::new(lockfile)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.ends_with(".txt"))
    }

    fn plan(
        &self,
        lockfile: &str,
        good_text: &str,
        bad_text: &str,
    ) -> Result<Box<dyn Ecosystem>, String> {
        Ok(Box::new(PipPlan::new(lockfile, good_text, bad_text)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dd::{DepClosure, Set};
    use crate::model::Change;

    struct DummyEco {
        changes: Vec<Change>,
        closure: DepClosure,
    }

    impl Ecosystem for DummyEco {
        fn kind(&self) -> &'static str {
            "dummy"
        }
        fn lockfile(&self) -> &str {
            "dummy.lock"
        }
        fn changes(&self) -> &[Change] {
            &self.changes
        }
        fn closure(&self) -> &DepClosure {
            &self.closure
        }
        fn render(&self, _applied: &Set) -> String {
            String::new()
        }
        fn verify(&self, _applied: &Set, _lock_after_prepare: &str) -> Result<(), String> {
            Ok(())
        }
        fn default_prepare(&self) -> &'static str {
            "true"
        }
        fn env(&self, _trial_dir: &Path, _shared_dir: &Path) -> Vec<(String, String)> {
            Vec::new()
        }
        fn warnings(&self) -> &[String] {
            &[]
        }
    }

    struct DummyAdapter;

    impl Adapter for DummyAdapter {
        fn id(&self) -> &'static str {
            "dummy"
        }
        fn aliases(&self) -> &'static [&'static str] {
            &["custom-dummy"]
        }
        fn lockfiles(&self) -> &'static [&'static str] {
            &["dummy.lock"]
        }
        fn plan(
            &self,
            _lockfile: &str,
            _good_text: &str,
            _bad_text: &str,
        ) -> Result<Box<dyn Ecosystem>, String> {
            Ok(Box::new(DummyEco {
                changes: Vec::new(),
                closure: DepClosure::new(0, [], []),
            }))
        }
    }

    #[test]
    fn registry_registers_and_resolves_custom_adapters() {
        let mut reg = AdapterRegistry::builtin();
        reg.register(Box::new(DummyAdapter));
        assert!(reg.get("dummy").is_some());
        assert!(reg.get("CUSTOM-DUMMY").is_some());
        let dir =
            std::env::temp_dir().join(format!("whichdep-adapter-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dummy.lock"), "").unwrap();

        let (lf, adapter) = reg.resolve(&dir, None, None).unwrap();
        assert_eq!(lf, "dummy.lock");
        assert_eq!(adapter.id(), "dummy");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_parent_escape_lockfile_paths() {
        let reg = AdapterRegistry::builtin();
        let err = reg
            .resolve(Path::new("."), Some("../Cargo.lock"), None)
            .err()
            .unwrap();
        assert!(err.contains("inside the current directory"), "{err}");
    }
}
