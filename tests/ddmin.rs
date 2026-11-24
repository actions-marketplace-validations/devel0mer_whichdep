use whichdep::dd::{ddmin, DdError, DepClosure, NoClosure, Outcome, Runner, Set};

fn set(v: &[usize]) -> Set {
    v.iter().copied().collect()
}

/// Fails exactly when every unit in `need` is applied.
fn fails_if_all(need: Vec<usize>) -> impl FnMut(&Set) -> Outcome {
    move |s: &Set| {
        if need.iter().all(|r| s.contains(r)) {
            Outcome::Fail
        } else {
            Outcome::Pass
        }
    }
}

#[test]
fn single_culprit_is_found_in_logarithmic_trials() {
    for k in [0usize, 1, 42, 63, 99] {
        let mut o = fails_if_all(vec![k]);
        let mut r = Runner::new(&mut o);
        let res = ddmin(100, &NoClosure, &mut r).unwrap();
        assert_eq!(res.culprits, vec![k]);
        assert!(!res.exhausted);
        assert!(r.stats.executed <= 20, "k={k}: {} trials", r.stats.executed);
    }
}

#[test]
fn interacting_pair_is_reported_as_a_pair() {
    let mut o = fails_if_all(vec![3, 77]);
    let mut r = Runner::new(&mut o);
    let res = ddmin(100, &NoClosure, &mut r).unwrap();
    assert_eq!(res.culprits, vec![3, 77]);
    assert!(r.stats.executed <= 60, "{} trials", r.stats.executed);
}

#[test]
fn interacting_triple_is_reported_as_a_triple() {
    let mut o = fails_if_all(vec![0, 50, 99]);
    let mut r = Runner::new(&mut o);
    let res = ddmin(100, &NoClosure, &mut r).unwrap();
    assert_eq!(res.culprits, vec![0, 50, 99]);
}

#[test]
fn two_independent_culprits_yield_one_of_them() {
    let mut o = |s: &Set| {
        if s.contains(&10) || s.contains(&90) {
            Outcome::Fail
        } else {
            Outcome::Pass
        }
    };
    let mut r = Runner::new(&mut o);
    let res = ddmin(100, &NoClosure, &mut r).unwrap();
    assert_eq!(res.culprits.len(), 1);
    assert!(res.culprits == vec![10] || res.culprits == vec![90]);
}

#[test]
fn unresolved_configurations_do_not_mislead_the_search() {
    // 19 and 20 only exist together (every other mix cannot be built) and
    // straddle the first split, 33 is the real culprit.
    let mut o = |s: &Set| {
        if s.contains(&19) != s.contains(&20) {
            Outcome::Unresolved
        } else if s.contains(&33) {
            Outcome::Fail
        } else {
            Outcome::Pass
        }
    };
    let mut r = Runner::new(&mut o);
    let res = ddmin(40, &NoClosure, &mut r).unwrap();
    assert_eq!(res.culprits, vec![33]);
    assert!(
        r.stats.unresolved > 0,
        "the test was meant to hit unresolved states"
    );
}

#[test]
fn unresolved_pair_inside_the_culprit_is_kept_together() {
    // The failure needs 33 *and* the inseparable pair 19+20.
    let mut o = |s: &Set| {
        if s.contains(&19) != s.contains(&20) {
            Outcome::Unresolved
        } else if s.contains(&33) && s.contains(&19) {
            Outcome::Fail
        } else {
            Outcome::Pass
        }
    };
    let mut r = Runner::new(&mut o);
    let res = ddmin(40, &NoClosure, &mut r).unwrap();
    assert_eq!(res.culprits, vec![19, 20, 33]);
    assert!(r.stats.unresolved > 0);
}

#[test]
fn culprit_that_needs_a_companion_is_closed_over() {
    // 7 (an update) requires 8 (a newly added crate).
    let closure = DepClosure::new(10, [(7, 8)], []);
    let mut o = |s: &Set| {
        assert!(
            !s.contains(&7) || s.contains(&8),
            "proposed a configuration that cannot exist: {s:?}"
        );
        if s.contains(&7) {
            Outcome::Fail
        } else {
            Outcome::Pass
        }
    };
    let mut r = Runner::new(&mut o);
    let res = ddmin(10, &closure, &mut r).unwrap();
    assert_eq!(res.culprits, vec![7, 8]);
}

#[test]
fn forced_units_are_part_of_every_configuration_but_never_reported() {
    let closure = DepClosure::new(6, [], [0]);
    let mut o = |s: &Set| {
        assert!(s.contains(&0), "forced unit missing from {s:?}");
        if s.contains(&3) {
            Outcome::Fail
        } else {
            Outcome::Pass
        }
    };
    let mut r = Runner::new(&mut o);
    let res = ddmin(6, &closure, &mut r).unwrap();
    assert_eq!(res.culprits, vec![3]);
}

#[test]
fn precondition_failures_are_reported() {
    let mut always_fail = |_: &Set| Outcome::Fail;
    assert_eq!(
        ddmin(8, &NoClosure, &mut Runner::new(&mut always_fail)).unwrap_err(),
        DdError::BaselineFails
    );

    let mut always_pass = |_: &Set| Outcome::Pass;
    assert_eq!(
        ddmin(8, &NoClosure, &mut Runner::new(&mut always_pass)).unwrap_err(),
        DdError::BadPasses
    );

    let mut base_unresolved = |s: &Set| {
        if s.is_empty() {
            Outcome::Unresolved
        } else {
            Outcome::Fail
        }
    };
    assert_eq!(
        ddmin(8, &NoClosure, &mut Runner::new(&mut base_unresolved)).unwrap_err(),
        DdError::BaselineUnresolved
    );

    let mut bad_unresolved = |s: &Set| {
        if s.len() == 8 {
            Outcome::Unresolved
        } else {
            Outcome::Pass
        }
    };
    assert_eq!(
        ddmin(8, &NoClosure, &mut Runner::new(&mut bad_unresolved)).unwrap_err(),
        DdError::BadUnresolved
    );

    let mut any = |_: &Set| Outcome::Fail;
    assert_eq!(
        ddmin(0, &NoClosure, &mut Runner::new(&mut any)).unwrap_err(),
        DdError::NothingToBisect
    );
}

#[test]
fn a_fix_that_comes_with_the_break_is_a_precondition_failure() {
    // 1 breaks the build, 2 repairs it again: everything applied passes.
    let mut o = |s: &Set| {
        if s.contains(&1) && !s.contains(&2) {
            Outcome::Fail
        } else {
            Outcome::Pass
        }
    };
    assert_eq!(
        ddmin(4, &NoClosure, &mut Runner::new(&mut o)).unwrap_err(),
        DdError::BadPasses
    );
}

#[test]
fn budget_stops_the_search_but_keeps_a_failing_configuration() {
    let mut o = fails_if_all(vec![50]);
    let mut r = Runner::new(&mut o).with_budget(Some(5));
    let res = ddmin(100, &NoClosure, &mut r).unwrap();
    assert!(res.exhausted);
    assert!(res.culprits.len() > 1);
    assert!(res.culprits.contains(&50));
    assert_eq!(r.stats.executed, 5);

    let mut o = fails_if_all(vec![50]);
    let mut r = Runner::new(&mut o).with_budget(Some(1));
    assert_eq!(
        ddmin(100, &NoClosure, &mut r).unwrap_err(),
        DdError::Exhausted
    );
}

#[test]
fn cached_results_are_not_run_again() {
    let mut o = fails_if_all(vec![7, 8]);
    let mut first = Runner::new(&mut o);
    ddmin(20, &NoClosure, &mut first).unwrap();
    let cache = first.cache().clone();
    assert!(first.stats.executed > 0);

    let mut o2 = |_: &Set| -> Outcome { panic!("oracle must not run when everything is cached") };
    let mut second = Runner::new(&mut o2).with_cache(cache);
    let res = ddmin(20, &NoClosure, &mut second).unwrap();
    assert_eq!(res.culprits, vec![7, 8]);
    assert_eq!(second.stats.executed, 0);
}

/// Tiny deterministic PRNG so the property test needs no crates.
struct Xs(u64);
impl Xs {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn result_is_always_failing_and_one_minimal() {
    // Monotone failure: fails iff it contains one of several hidden sets.
    let mut rng = Xs(0x9E3779B97F4A7C15);
    for round in 0..400 {
        let n = 2 + rng.below(60);
        let groups: Vec<Vec<usize>> = (0..1 + rng.below(3))
            .map(|_| (0..1 + rng.below(3)).map(|_| rng.below(n)).collect())
            .collect();
        let fails = move |s: &Set| groups.iter().any(|g| g.iter().all(|x| s.contains(x)));
        let oracle_fn = fails.clone();
        let mut o = move |s: &Set| {
            if oracle_fn(s) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let mut r = Runner::new(&mut o);
        let res = ddmin(n, &NoClosure, &mut r).unwrap();
        let found = set(&res.culprits);

        assert!(fails(&found), "round {round}: {found:?} does not fail");
        for x in &found {
            let mut smaller = found.clone();
            smaller.remove(x);
            assert!(
                !fails(&smaller),
                "round {round}: {found:?} is not 1-minimal, {x} is not needed"
            );
        }
    }
}

mod find {
    use whichdep::dd::{find_culprits, DepClosure, NoClosure, Outcome, Runner, Set, Verdict};

    #[test]
    fn one_culprit_is_confirmed_by_reverting_it() {
        let mut o = |s: &Set| {
            if s.contains(&42) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let mut r = Runner::new(&mut o);
        let f = find_culprits(100, &NoClosure, &mut r, 5).unwrap();
        assert_eq!(f.verdict, Verdict::Verified);
        assert_eq!(f.culprits.len(), 1);
        assert_eq!(f.culprits[0].units, vec![42]);
    }

    #[test]
    fn independent_culprits_are_all_found() {
        let mut o = |s: &Set| {
            if s.contains(&10) || s.contains(&90) || (s.contains(&3) && s.contains(&4)) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let mut r = Runner::new(&mut o);
        let f = find_culprits(100, &NoClosure, &mut r, 5).unwrap();
        assert_eq!(f.verdict, Verdict::Verified);
        let mut sets: Vec<Vec<usize>> = f.culprits.iter().map(|c| c.units.clone()).collect();
        sets.sort();
        assert_eq!(sets, vec![vec![3, 4], vec![10], vec![90]]);
    }

    #[test]
    fn the_culprit_limit_leaves_the_verdict_incomplete() {
        let mut o = |s: &Set| {
            if s.contains(&10) || s.contains(&90) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let mut r = Runner::new(&mut o);
        let f = find_culprits(100, &NoClosure, &mut r, 1).unwrap();
        assert_eq!(f.verdict, Verdict::Incomplete);
        assert_eq!(f.culprits.len(), 1);
    }

    #[test]
    fn an_unresolvable_revert_is_reported_as_unverified() {
        // 5 breaks the build, but the state without 5 cannot be built at all
        // (6 needs 5's companion and is not applied then).
        let mut o = |s: &Set| {
            if s.contains(&5) {
                Outcome::Fail
            } else if s.contains(&6) {
                Outcome::Unresolved
            } else {
                Outcome::Pass
            }
        };
        let mut r = Runner::new(&mut o);
        let f = find_culprits(10, &NoClosure, &mut r, 5).unwrap();
        assert_eq!(f.culprits[0].units, vec![5]);
        assert_eq!(f.verdict, Verdict::Unverified);
    }

    #[test]
    fn reverting_a_culprit_also_reverts_what_requires_it() {
        // 7 requires 8; 8 is the culprit. The revert check must drop 7 too,
        // otherwise it would propose a configuration that cannot exist.
        let closure = DepClosure::new(10, [(7, 8)], []);
        let mut o = |s: &Set| {
            assert!(
                !s.contains(&7) || s.contains(&8),
                "impossible configuration {s:?}"
            );
            if s.contains(&8) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let mut r = Runner::new(&mut o);
        let f = find_culprits(10, &closure, &mut r, 5).unwrap();
        assert_eq!(f.culprits[0].units, vec![8]);
        assert_eq!(f.verdict, Verdict::Verified);
    }

    #[test]
    fn exhausting_the_budget_is_incomplete() {
        let mut o = |s: &Set| {
            if s.contains(&50) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let mut r = Runner::new(&mut o).with_budget(Some(5));
        let f = find_culprits(100, &NoClosure, &mut r, 5).unwrap();
        assert_eq!(f.verdict, Verdict::Incomplete);
        assert!(f.culprits[0].exhausted);
    }
}
