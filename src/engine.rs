//! CLI-independent orchestration of a regression experiment.
//!
//! The engine owns search policy, budgets, cache reuse and diagnosis output.
//! It does not know about command-line flags, Git, lockfile formats or process
//! execution. Those are supplied by an adapter and an Oracle implementation.

use crate::dd::{Closure, Outcome, Runner};
use crate::diagnosis::{Diagnosis, TrialEvidence};
use crate::model::Experiment;
use crate::oracle::Oracle;
use crate::store::ExperimentStore;
use crate::strategy::{SearchStrategy, StrategyKind};
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct Engine {
    store: Option<ExperimentStore>,
    resume: bool,
    strategy: StrategyKind,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// Construct an in-memory engine with no filesystem side effects.
    pub fn new() -> Self {
        Self {
            store: None,
            resume: false,
            strategy: StrategyKind::Ddmin,
        }
    }

    /// Enable persistent experiment history and trial caching.
    pub fn with_store(mut self, store: ExperimentStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Reuse matching conclusive trial results from an earlier run.
    /// Persistence can remain enabled while resume is disabled.
    pub fn with_resume(mut self, enabled: bool) -> Self {
        self.resume = enabled;
        self
    }

    /// Select which built-in search strategy the engine runs.
    pub fn with_strategy(mut self, strategy: StrategyKind) -> Self {
        self.strategy = strategy;
        self
    }

    /// Run an experiment against any compatible dependency closure and Oracle
    /// using the configured built-in strategy.
    ///
    /// `Ok(Diagnosis)` also represents valid unsuccessful searches such as a
    /// failing baseline or an exhausted budget. `Err` is reserved for storage
    /// or fatal oracle failures; search outcomes are preserved in
    /// `Diagnosis::search_error`.
    pub fn run<O, C>(
        &self,
        experiment: &Experiment,
        closure: &C,
        oracle: &mut O,
    ) -> Result<Diagnosis, EngineError>
    where
        O: Oracle,
        C: Closure,
    {
        self.run_with_strategy(experiment, closure, oracle, &self.strategy)
    }

    /// Run an experiment using any custom [`SearchStrategy`] implementation.
    pub fn run_with_strategy<O, C, S>(
        &self,
        experiment: &Experiment,
        closure: &C,
        oracle: &mut O,
        strategy: &S,
    ) -> Result<Diagnosis, EngineError>
    where
        O: Oracle,
        C: Closure,
        S: SearchStrategy,
    {
        let started = Instant::now();
        // An arbitrary user-defined Closure may contain hidden rules or state.
        // Unless it supplies a stable fingerprint, keep its history but do not
        // reuse persisted outcomes across runs.
        let closure_identity = closure.cache_fingerprint();
        let namespace = closure_identity.as_ref().map(|closure_key| {
            format!(
                "{}\nclosure:{}:{}",
                experiment.cache_namespace(),
                closure_key.len(),
                closure_key
            )
        });
        if let Some(store) = &self.store {
            store.save_experiment_definition(experiment, closure_identity.as_deref())?;
        }
        let initial_cache: HashMap<Vec<usize>, Outcome> =
            match (&self.store, self.resume, &namespace) {
                (Some(store), true, Some(key)) => store.load_cache(&experiment.id, key)?,
                _ => HashMap::new(),
            };

        let (search, stats, cache, persistence_error, fatal_error) = {
            let mut runner = Runner::new(oracle)
                .with_budget(experiment.max_trials)
                .with_cache(initial_cache);
            if let (Some(store), Some(key)) = (self.store.clone(), namespace.clone()) {
                let id = experiment.id.clone();
                runner = runner.with_trial_callback(move |config, outcome| {
                    store.save_trial(&id, &key, config, outcome)
                });
            }
            let search = strategy.search(
                experiment.units.len(),
                closure,
                &mut runner,
                experiment.max_causes,
            );
            (
                search,
                runner.stats.clone(),
                runner.cache().clone(),
                runner.take_callback_error(),
                runner.fatal_error().map(str::to_string),
            )
        };
        if let Some(msg) = fatal_error {
            return Err(EngineError::OracleFatal(msg));
        }
        if let Some(error) = persistence_error {
            return Err(EngineError::Storage(error));
        }

        let trials = cache
            .iter()
            .map(|(config, &outcome)| TrialEvidence {
                config: config.clone(),
                outcome,
            })
            .collect::<Vec<_>>();
        let mut trials = trials;
        trials.sort_by(|a, b| a.config.cmp(&b.config));

        let diagnosis = Diagnosis::from_search(
            experiment.id.clone(),
            search,
            trials,
            stats.executed,
            stats.cache_hits,
            started.elapsed().as_secs_f64(),
        )
        .with_strategy(strategy.name());

        if let Some(store) = &self.store {
            // Trial outcomes have already been committed one-by-one by the
            // Runner callback. This final run snapshot records the diagnosis.
            store.save_run(experiment, &diagnosis, closure_identity.as_deref())?;
        }
        Ok(diagnosis)
    }
}

#[derive(Debug)]
pub enum EngineError {
    Storage(io::Error),
    OracleFatal(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(e) => write!(f, "experiment storage failed: {e}"),
            Self::OracleFatal(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(e) => Some(e),
            Self::OracleFatal(_) => None,
        }
    }
}

impl From<io::Error> for EngineError {
    fn from(value: io::Error) -> Self {
        Self::Storage(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dd::{DdError, NoClosure, Set};
    use crate::model::{Change, Experiment, Snapshot};
    use crate::store::ExperimentStore;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn experiment(id: &str) -> Experiment {
        Experiment::new(
            id,
            "unit test",
            "test",
            Snapshot::new("good", "good-tree"),
            Snapshot::new("bad", "bad-tree"),
            (0..8)
                .map(|i| Change {
                    name: format!("p{i}"),
                    from: vec!["1".into()],
                    to: vec!["2".into()],
                })
                .collect(),
            "oracle-v1",
        )
    }

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_root() -> PathBuf {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("whichdep-engine-test-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn engine_is_usable_without_a_cli_or_persistent_store() {
        let exp = experiment("memory-run");
        let mut oracle = |set: &Set| {
            if set.contains(&3) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let diagnosis = Engine::new().run(&exp, &NoClosure, &mut oracle).unwrap();
        assert_eq!(diagnosis.findings.len(), 1);
        assert_eq!(diagnosis.findings[0].units, vec![3]);
        assert_eq!(
            diagnosis.verification.status,
            crate::diagnosis::VerificationStatus::Verified
        );
    }

    struct UnfingerprintedClosure;

    impl crate::dd::Closure for UnfingerprintedClosure {
        fn up(&self, set: &Set) -> Set {
            set.clone()
        }
        fn down(&self, set: &Set) -> Set {
            set.clone()
        }
    }

    #[test]
    fn custom_closure_without_identity_does_not_reuse_persistent_trials() {
        let root = temp_root();
        let store = ExperimentStore::open(&root).unwrap();
        let exp = experiment("custom-closure");
        let calls = AtomicUsize::new(0);
        for _ in 0..2 {
            let mut oracle = |set: &Set| {
                calls.fetch_add(1, Ordering::SeqCst);
                if set.contains(&3) {
                    Outcome::Fail
                } else {
                    Outcome::Pass
                }
            };
            let result = Engine::new()
                .with_store(store.clone())
                .with_resume(true)
                .run(&exp, &UnfingerprintedClosure, &mut oracle)
                .unwrap();
            assert!(result.executed_trials > 0);
        }
        assert!(calls.load(Ordering::SeqCst) > 1);
        assert_eq!(store.history("custom-closure").unwrap().len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persistent_resume_reuses_results_and_records_history() {
        let root = temp_root();
        let store = ExperimentStore::open(&root).unwrap();
        let limited = experiment("persistent-run").with_budget(Some(1), 5);
        let calls = AtomicUsize::new(0);
        let mut first = |set: &Set| {
            calls.fetch_add(1, Ordering::SeqCst);
            if set.contains(&3) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let first_result = Engine::new()
            .with_store(store.clone())
            .with_resume(true)
            .run(&limited, &NoClosure, &mut first)
            .unwrap();
        assert_eq!(first_result.search_error, Some(DdError::Exhausted));
        assert_eq!(first_result.executed_trials, 1);

        // A larger budget resumes from the endpoint already evaluated above.
        let complete = experiment("persistent-run").with_budget(Some(30), 5);
        let mut second = |set: &Set| {
            calls.fetch_add(1, Ordering::SeqCst);
            if set.contains(&3) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let second_result = Engine::new()
            .with_store(store.clone())
            .with_resume(true)
            .run(&complete, &NoClosure, &mut second)
            .unwrap();
        assert!(second_result.cache_hits > 0);
        assert!(second_result.executed_trials > 0);
        assert_eq!(second_result.findings[0].units, vec![3]);
        let total_calls = calls.load(Ordering::SeqCst);

        let mut must_not_run = |_: &Set| -> Outcome { panic!("cached oracle should not execute") };
        let third_result = Engine::new()
            .with_store(store.clone())
            .with_resume(true)
            .run(&complete, &NoClosure, &mut must_not_run)
            .unwrap();
        assert_eq!(third_result.executed_trials, 0);
        assert!(third_result.cache_hits > 0);
        assert_eq!(calls.load(Ordering::SeqCst), total_calls);
        assert_eq!(store.history("persistent-run").unwrap().len(), 3);
        let _ = fs::remove_dir_all(root);
    }
}
