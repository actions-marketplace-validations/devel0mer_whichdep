//! Public integration tests for the library API (core engine, Adapter API,
//! pluggable search strategies, and persistent store inspection).

use std::path::Path;
use whichdep::dd::{Culprit, DdError, Findings, Runner, Verdict};
use whichdep::{
    Adapter, AdapterRegistry, Change, Closure, DepClosure, Diagnosis, Ecosystem, Engine,
    Experiment, ExperimentStore, NoClosure, Oracle, Outcome, SearchStrategy, Set, Snapshot,
    StrategyKind, VerificationStatus,
};

#[test]
fn public_library_api_runs_a_custom_oracle() {
    let experiment = Experiment::new(
        "public-api-test",
        "public API regression test",
        "custom",
        Snapshot::new("good", "good-fingerprint"),
        Snapshot::new("bad", "bad-fingerprint"),
        vec![
            Change {
                name: "alpha".into(),
                from: vec!["1".into()],
                to: vec!["2".into()],
            },
            Change {
                name: "beta".into(),
                from: vec!["1".into()],
                to: vec!["2".into()],
            },
        ],
        "oracle-v1",
    );

    let mut oracle = |changes: &Set| {
        if changes.contains(&1) {
            Outcome::Fail
        } else {
            Outcome::Pass
        }
    };
    let diagnosis = Engine::new()
        .with_strategy(StrategyKind::Adaptive)
        .run(&experiment, &NoClosure, &mut oracle)
        .expect("the in-memory engine runs");

    assert_eq!(diagnosis.strategy, "adaptive");
    assert_eq!(diagnosis.findings.len(), 1);
    assert_eq!(diagnosis.findings[0].units, vec![1]);
    assert_eq!(diagnosis.verification.status, VerificationStatus::Verified);

    let round_trip = Diagnosis::from_json(&diagnosis.to_json()).unwrap();
    assert_eq!(round_trip.experiment_id, "public-api-test");
    assert_eq!(round_trip.strategy, "adaptive");
    assert_eq!(round_trip.findings, diagnosis.findings);
    assert_eq!(round_trip.verification.status, VerificationStatus::Verified);
}

#[test]
fn public_library_api_supports_custom_strategy_and_store_inspection() {
    struct AlwaysFirstStrategy;
    impl SearchStrategy for AlwaysFirstStrategy {
        fn name(&self) -> &'static str {
            "always-first"
        }
        fn search<O: Oracle, C: Closure>(
            &self,
            _n: usize,
            _closure: &C,
            runner: &mut Runner<'_, O>,
            _max_culprits: usize,
        ) -> Result<Findings, DdError> {
            let _ = runner.run(&[0].into_iter().collect());
            Ok(Findings {
                culprits: vec![Culprit {
                    units: vec![0],
                    exhausted: false,
                }],
                verdict: Verdict::Verified,
            })
        }
    }

    let root = std::env::temp_dir().join(format!("whichdep-lib-api-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = ExperimentStore::open(&root).unwrap();
    let exp = Experiment::new(
        "custom-strat-exp",
        "custom strategy test",
        "custom",
        Snapshot::new("g", "gf"),
        Snapshot::new("b", "bf"),
        vec![Change {
            name: "a".into(),
            from: vec!["1".into()],
            to: vec!["2".into()],
        }],
        "oracle-custom",
    );
    let mut oracle = |s: &Set| {
        if s.contains(&0) {
            Outcome::Fail
        } else {
            Outcome::Pass
        }
    };
    let diag = Engine::new()
        .with_store(store.clone())
        .with_resume(true)
        .run_with_strategy(&exp, &NoClosure, &mut oracle, &AlwaysFirstStrategy)
        .unwrap();
    assert_eq!(diag.strategy, "always-first");

    let list = store.list_experiments().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "custom-strat-exp");
    assert_eq!(list[0].latest_strategy.as_deref(), Some("always-first"));
    assert_eq!(list[0].runs, 1);

    let reclaimed = store.clean_all().unwrap();
    assert!(reclaimed > 0);
    assert!(store.list_experiments().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn adapter_registry_exposes_all_builtin_ecosystems_and_accepts_custom_adapters() {
    struct CustomEco {
        changes: Vec<Change>,
        closure: DepClosure,
    }
    impl Ecosystem for CustomEco {
        fn kind(&self) -> &'static str {
            "custom-eco"
        }
        fn lockfile(&self) -> &str {
            "custom.lock"
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
    struct CustomAdapter;
    impl Adapter for CustomAdapter {
        fn id(&self) -> &'static str {
            "custom-eco"
        }
        fn lockfiles(&self) -> &'static [&'static str] {
            &["custom.lock"]
        }
        fn plan(&self, _lf: &str, _g: &str, _b: &str) -> Result<Box<dyn Ecosystem>, String> {
            Ok(Box::new(CustomEco {
                changes: Vec::new(),
                closure: DepClosure::new(0, [], []),
            }))
        }
    }

    let mut reg = AdapterRegistry::builtin();
    for builtin in ["cargo", "uv", "pip", "npm", "pnpm"] {
        assert!(
            reg.get(builtin).is_some(),
            "missing builtin adapter {builtin}"
        );
    }
    reg.register(Box::new(CustomAdapter));
    let eco = reg
        .get("custom-eco")
        .unwrap()
        .plan("custom.lock", "", "")
        .unwrap();
    assert_eq!(eco.kind(), "custom-eco");
}
