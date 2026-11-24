//! What an ecosystem adapter has to provide.

use crate::dd::{DepClosure, Set};
use std::path::Path;

/// One atomic difference between the good and the bad lockfile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    /// Version(s) in the good lockfile; empty when the package is new.
    pub from: Vec<String>,
    /// Version(s) in the bad lockfile; empty when the package was removed.
    pub to: Vec<String>,
}

impl Change {
    pub fn describe(&self) -> String {
        match (self.from.is_empty(), self.to.is_empty()) {
            (false, false) => format!(
                "{} {} -> {}",
                self.name,
                self.from.join(", "),
                self.to.join(", ")
            ),
            (true, false) => format!("{} {} (new)", self.name, self.to.join(", ")),
            (false, true) => format!("{} {} (removed)", self.name, self.from.join(", ")),
            (true, true) => self.name.clone(),
        }
    }
}

/// A named project state used as one endpoint of an experiment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub label: String,
    /// Stable fingerprint of the relevant source/lockfile state.
    pub fingerprint: String,
}

impl Snapshot {
    pub fn new(label: impl Into<String>, fingerprint: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            fingerprint: fingerprint.into(),
        }
    }
}

/// A complete, serialisable description of one regression investigation.
///
/// The core engine does not interpret revisions, paths or ecosystem lockfiles;
/// adapters and callers prepare those inputs and provide the Oracle.
#[derive(Clone, Debug)]
pub struct Experiment {
    pub id: String,
    pub description: String,
    pub ecosystem: String,
    pub baseline: Snapshot,
    pub target: Snapshot,
    pub units: Vec<Change>,
    /// Fingerprint of the oracle command/configuration and relevant runtime.
    pub oracle_fingerprint: String,
    pub max_trials: Option<usize>,
    pub max_causes: usize,
}

impl Experiment {
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        ecosystem: impl Into<String>,
        baseline: Snapshot,
        target: Snapshot,
        units: Vec<Change>,
        oracle_fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            ecosystem: ecosystem.into(),
            baseline,
            target,
            units,
            oracle_fingerprint: oracle_fingerprint.into(),
            max_trials: None,
            max_causes: 5,
        }
    }

    pub fn with_budget(mut self, max_trials: Option<usize>, max_causes: usize) -> Self {
        self.max_trials = max_trials;
        self.max_causes = max_causes.max(1);
        self
    }

    /// Full cache namespace. Every field and list boundary is length/count
    /// prefixed so different unit layouts cannot produce the same identity.
    pub fn cache_namespace(&self) -> String {
        fn put(out: &mut String, value: &str) {
            out.push_str(&format!("{}:", value.len()));
            out.push_str(value);
            out.push('\n');
        }
        fn put_list(out: &mut String, values: &[String]) {
            out.push_str(&format!("{}\n", values.len()));
            for value in values {
                put(out, value);
            }
        }

        let mut out = String::from("whichdep-experiment-cache-v2\n");
        for field in [
            self.ecosystem.as_str(),
            self.baseline.fingerprint.as_str(),
            self.target.fingerprint.as_str(),
            self.oracle_fingerprint.as_str(),
        ] {
            put(&mut out, field);
        }
        out.push_str(&format!("{}\n", self.units.len()));
        for unit in &self.units {
            put(&mut out, &unit.name);
            put_list(&mut out, &unit.from);
            put_list(&mut out, &unit.to);
        }
        out
    }
}

/// A lockfile format plus the knowledge of how to materialise a subset of
/// the changes between two of its revisions.
pub trait Ecosystem {
    /// Short identifier used in reports ("cargo", "pip").
    fn kind(&self) -> &'static str;
    /// The lockfile path, relative to the project root.
    fn lockfile(&self) -> &str;
    /// The units delta debugging works on; index `i` is unit `i`.
    fn changes(&self) -> &[Change];
    /// Which changes need each other.
    fn closure(&self) -> &DepClosure;
    /// Lockfile contents for the configuration "good + these changes".
    fn render(&self, applied: &Set) -> String;
    /// Compares what the resolver left on disk with what was intended.
    /// An `Err` marks the configuration as unresolved.
    fn verify(&self, applied: &Set, lock_after_prepare: &str) -> Result<(), String>;
    /// Shell script that turns the written lockfile into a usable environment.
    fn default_prepare(&self) -> &'static str;
    /// Extra environment for the prepare and test commands.
    fn env(&self, trial_dir: &Path, shared_dir: &Path) -> Vec<(String, String)>;
    /// Things the user should know about the inputs.
    fn warnings(&self) -> &[String];
    /// A command or requirement that puts change `unit` back to its good
    /// version, for the report.
    fn pin_hint(&self, _unit: usize) -> Option<String> {
        None
    }
    /// Optionally learn new closure edges from an unresolved prepare failure log
    /// (e.g. `pip check` reporting missing or incompatible dependencies).
    fn learn_from_prepare_failure(&self, _applied: &Set, _log_text: &str) -> usize {
        0
    }
}
