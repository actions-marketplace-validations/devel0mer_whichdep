//! Pluggable regression search strategies over lockfile changes.
//!
//! All strategies share the same [`Runner`] memoisation cache and persistent
//! trial store, so switching strategies between runs reuses conclusive trial
//! outcomes automatically.

use crate::dd::{
    check_endpoints, find_culprits, minimize, Closure, Culprit, DdError, Exhausted, Findings,
    MinResult, Outcome, Runner, Set, Verdict,
};
use crate::oracle::Oracle;
use std::fmt;

/// Built-in search strategies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StrategyKind {
    /// Zeller's delta debugging (`ddmin`) with multi-culprit verification.
    #[default]
    Ddmin,
    /// Fast `O(log2 n)` binary search over change halves. Stops early if an
    /// interaction pair or unresolved split prevents pure halving.
    Bisect,
    /// Binary search first for fast single-culprit isolation, automatically
    /// falling back to `ddmin` (reusing the warm cache) on interactions or
    /// unresolved splits.
    Adaptive,
    /// Tests single changes (and single-change reverts) linearly. Helpful for
    /// small diffs, non-monotone failures, or high-unresolved lockfiles.
    OneByOne,
}

impl StrategyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ddmin => "ddmin",
            Self::Bisect => "bisect",
            Self::Adaptive => "adaptive",
            Self::OneByOne => "one-by-one",
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        match text.to_ascii_lowercase().as_str() {
            "ddmin" => Ok(Self::Ddmin),
            "bisect" | "binary" => Ok(Self::Bisect),
            "adaptive" | "hybrid" => Ok(Self::Adaptive),
            "one-by-one" | "one_by_one" | "sequential" | "linear" => Ok(Self::OneByOne),
            other => Err(format!(
                "unknown search strategy `{other}`: use ddmin, bisect, adaptive or one-by-one"
            )),
        }
    }
}

impl fmt::Display for StrategyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Extensible contract for custom regression search algorithms.
pub trait SearchStrategy {
    fn name(&self) -> &'static str;

    fn search<O: Oracle, C: Closure>(
        &self,
        n: usize,
        closure: &C,
        runner: &mut Runner<'_, O>,
        max_culprits: usize,
    ) -> Result<Findings, DdError>;
}

impl SearchStrategy for StrategyKind {
    fn name(&self) -> &'static str {
        self.as_str()
    }

    fn search<O: Oracle, C: Closure>(
        &self,
        n: usize,
        closure: &C,
        runner: &mut Runner<'_, O>,
        max_culprits: usize,
    ) -> Result<Findings, DdError> {
        match self {
            Self::Ddmin => find_culprits(n, closure, runner, max_culprits),
            Self::Bisect => run_with_minimizer(n, closure, runner, max_culprits, |start, c, r| {
                bisect_minimize(start, c, r, false)
            }),
            Self::Adaptive => {
                run_with_minimizer(n, closure, runner, max_culprits, |start, c, r| {
                    bisect_minimize(start, c, r, true)
                })
            }
            Self::OneByOne => one_by_one_search(n, closure, runner, max_culprits),
        }
    }
}

fn run_with_minimizer<O, C, F>(
    n: usize,
    closure: &C,
    runner: &mut Runner<'_, O>,
    max_culprits: usize,
    mut minimizer: F,
) -> Result<Findings, DdError>
where
    O: Oracle,
    C: Closure,
    F: FnMut(Set, &C, &mut Runner<'_, O>) -> MinResult,
{
    let base = closure.up(&Set::new());
    let mut remaining = check_endpoints(n, closure, runner)?;
    let mut culprits: Vec<Culprit> = Vec::new();
    let max_culprits = max_culprits.max(1);

    let verdict = loop {
        let m = minimizer(remaining.clone(), closure, runner);
        if runner.aborted() {
            return Err(DdError::Aborted);
        }
        let exhausted = m.exhausted;
        let mut next = remaining.clone();
        for u in &m.culprits {
            next.remove(u);
        }
        culprits.push(Culprit {
            units: m.culprits,
            exhausted,
        });
        if exhausted {
            break Verdict::Incomplete;
        }
        let next = closure.down(&next);
        if next == base {
            break Verdict::Verified;
        }
        match runner.run(&next) {
            Err(Exhausted) if runner.aborted() => return Err(DdError::Aborted),
            Err(Exhausted) => break Verdict::Incomplete,
            Ok(Outcome::Pass) => break Verdict::Verified,
            Ok(Outcome::Unresolved) => break Verdict::Unverified,
            Ok(Outcome::Fail) => {
                if culprits.len() >= max_culprits {
                    break Verdict::Incomplete;
                }
                remaining = next;
            }
        }
    };

    Ok(Findings { culprits, verdict })
}

/// Binary halving minimiser. When `fallback_to_ddmin` is true, stalling on an
/// interaction pair or unresolved boundary seamlessly hands off to [`minimize`].
fn bisect_minimize<O: Oracle, C: Closure>(
    start: Set,
    closure: &C,
    runner: &mut Runner<'_, O>,
    fallback_to_ddmin: bool,
) -> MinResult {
    let base = closure.up(&Set::new());
    let mut cur = start;

    loop {
        let delta: Vec<usize> = cur.difference(&base).copied().collect();
        if delta.len() <= 1 {
            return MinResult {
                culprits: delta,
                config: cur,
                exhausted: false,
            };
        }
        let mid = delta.len() / 2;
        let left = closure.up(&delta[..mid].iter().copied().collect());
        let right = closure.up(&delta[mid..].iter().copied().collect());

        let mut progressed = false;
        for half in [left, right] {
            if half == cur || half == base {
                continue;
            }
            match runner.run(&half) {
                Err(Exhausted) => {
                    let culprits = cur.difference(&base).copied().collect();
                    return MinResult {
                        culprits,
                        config: cur,
                        exhausted: true,
                    };
                }
                Ok(Outcome::Fail) => {
                    cur = half;
                    progressed = true;
                    break;
                }
                Ok(Outcome::Pass | Outcome::Unresolved) => {}
            }
        }
        if !progressed {
            if fallback_to_ddmin {
                return minimize(cur, closure, runner);
            }
            // Pure bisect cannot isolate an interaction or unresolved split;
            // mark as not proven 1-minimal rather than misreporting.
            let culprits = cur.difference(&base).copied().collect();
            return MinResult {
                culprits,
                config: cur,
                exhausted: true,
            };
        }
    }
}

/// Linear single-unit and single-revert search.
fn one_by_one_search<O: Oracle, C: Closure>(
    n: usize,
    closure: &C,
    runner: &mut Runner<'_, O>,
    max_culprits: usize,
) -> Result<Findings, DdError> {
    let base = closure.up(&Set::new());
    let mut remaining = check_endpoints(n, closure, runner)?;
    let max_culprits = max_culprits.max(1);
    let mut culprits: Vec<Culprit> = Vec::new();

    // Pass 1: probe individual closed units `up({u})`.
    let initial_delta: Vec<usize> = remaining.difference(&base).copied().collect();
    for u in initial_delta {
        if !remaining.contains(&u) {
            continue;
        }
        let single = closure.up(&[u].into_iter().collect());
        match runner.run(&single) {
            Err(Exhausted) if runner.aborted() => return Err(DdError::Aborted),
            Err(Exhausted) => {
                if culprits.is_empty() {
                    let units = remaining.difference(&base).copied().collect();
                    culprits.push(Culprit {
                        units,
                        exhausted: true,
                    });
                }
                return Ok(Findings {
                    culprits,
                    verdict: Verdict::Incomplete,
                });
            }
            Ok(Outcome::Fail) => {
                let units: Vec<usize> = single.difference(&base).copied().collect();
                for x in &units {
                    remaining.remove(x);
                }
                remaining = closure.down(&remaining);
                culprits.push(Culprit {
                    units,
                    exhausted: false,
                });
                if remaining == base {
                    return Ok(Findings {
                        culprits,
                        verdict: Verdict::Verified,
                    });
                }
                if culprits.len() >= max_culprits {
                    match runner.run(&remaining) {
                        Err(Exhausted) if runner.aborted() => return Err(DdError::Aborted),
                        Err(Exhausted) => {
                            return Ok(Findings {
                                culprits,
                                verdict: Verdict::Incomplete,
                            });
                        }
                        Ok(Outcome::Pass) => {
                            return Ok(Findings {
                                culprits,
                                verdict: Verdict::Verified,
                            });
                        }
                        Ok(Outcome::Unresolved) => {
                            return Ok(Findings {
                                culprits,
                                verdict: Verdict::Unverified,
                            });
                        }
                        Ok(Outcome::Fail) => {
                            return Ok(Findings {
                                culprits,
                                verdict: Verdict::Incomplete,
                            });
                        }
                    }
                }
            }
            Ok(Outcome::Pass | Outcome::Unresolved) => {}
        }
    }

    // Check whether reverting all individually found culprits fixed the failure.
    if !culprits.is_empty() {
        match runner.run(&remaining) {
            Err(Exhausted) if runner.aborted() => return Err(DdError::Aborted),
            Err(Exhausted) => {
                return Ok(Findings {
                    culprits,
                    verdict: Verdict::Incomplete,
                })
            }
            Ok(Outcome::Pass) => {
                return Ok(Findings {
                    culprits,
                    verdict: Verdict::Verified,
                })
            }
            Ok(Outcome::Unresolved) => {
                return Ok(Findings {
                    culprits,
                    verdict: Verdict::Unverified,
                });
            }
            Ok(Outcome::Fail) => {}
        }
    }

    // Pass 2: single-unit revert probe on `remaining` to isolate interacting sets
    // (a unit whose removal makes `remaining` pass is necessary for the failure).
    let rem_delta: Vec<usize> = remaining.difference(&base).copied().collect();
    let mut necessary = Set::new();
    for u in rem_delta {
        let mut without = remaining.clone();
        without.remove(&u);
        let without = closure.down(&without);
        if without == base {
            necessary.insert(u);
            continue;
        }
        match runner.run(&without) {
            Err(Exhausted) if runner.aborted() => return Err(DdError::Aborted),
            Err(Exhausted) => {
                let units = remaining.difference(&base).copied().collect();
                culprits.push(Culprit {
                    units,
                    exhausted: true,
                });
                return Ok(Findings {
                    culprits,
                    verdict: Verdict::Incomplete,
                });
            }
            Ok(Outcome::Pass) => {
                necessary.insert(u);
            }
            Ok(Outcome::Fail | Outcome::Unresolved) => {}
        }
    }

    if !necessary.is_empty() {
        let closed = closure.up(&necessary);
        match runner.run(&closed) {
            Err(Exhausted) if runner.aborted() => return Err(DdError::Aborted),
            Err(Exhausted) => {
                let units = remaining.difference(&base).copied().collect();
                culprits.push(Culprit {
                    units,
                    exhausted: true,
                });
                return Ok(Findings {
                    culprits,
                    verdict: Verdict::Incomplete,
                });
            }
            Ok(Outcome::Fail) => {
                let units: Vec<usize> = closed.difference(&base).copied().collect();
                let mut after = remaining.clone();
                for x in &units {
                    after.remove(x);
                }
                let after = closure.down(&after);
                culprits.push(Culprit {
                    units,
                    exhausted: false,
                });
                if after == base {
                    return Ok(Findings {
                        culprits,
                        verdict: Verdict::Verified,
                    });
                }
                let verdict = match runner.run(&after) {
                    Err(Exhausted) if runner.aborted() => return Err(DdError::Aborted),
                    Err(Exhausted) | Ok(Outcome::Fail) => Verdict::Incomplete,
                    Ok(Outcome::Pass) => Verdict::Verified,
                    Ok(Outcome::Unresolved) => Verdict::Unverified,
                };
                return Ok(Findings { culprits, verdict });
            }
            Ok(Outcome::Pass | Outcome::Unresolved) => {}
        }
    }

    // Fall back to ddmin on whatever complex interaction remains.
    let m = minimize(remaining.clone(), closure, runner);
    if runner.aborted() {
        return Err(DdError::Aborted);
    }
    let exhausted = m.exhausted;
    let mut after = remaining;
    for u in &m.culprits {
        after.remove(u);
    }
    culprits.push(Culprit {
        units: m.culprits,
        exhausted,
    });
    if exhausted {
        return Ok(Findings {
            culprits,
            verdict: Verdict::Incomplete,
        });
    }
    let after = closure.down(&after);
    let verdict = if after == base {
        Verdict::Verified
    } else {
        match runner.run(&after) {
            Err(Exhausted) if runner.aborted() => return Err(DdError::Aborted),
            Err(Exhausted) | Ok(Outcome::Fail) => Verdict::Incomplete,
            Ok(Outcome::Pass) => Verdict::Verified,
            Ok(Outcome::Unresolved) => Verdict::Unverified,
        }
    };
    Ok(Findings { culprits, verdict })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dd::{DepClosure, NoClosure};

    #[test]
    fn bisect_and_adaptive_find_single_culprit_in_logarithmic_trials() {
        for strategy in [StrategyKind::Bisect, StrategyKind::Adaptive] {
            let mut oracle = |s: &Set| {
                if s.contains(&42) {
                    Outcome::Fail
                } else {
                    Outcome::Pass
                }
            };
            let mut runner = Runner::new(&mut oracle);
            let found = strategy.search(100, &NoClosure, &mut runner, 5).unwrap();
            assert_eq!(found.verdict, Verdict::Verified);
            assert_eq!(found.culprits.len(), 1);
            assert_eq!(found.culprits[0].units, vec![42]);
            assert!(!found.culprits[0].exhausted);
            assert!(
                runner.stats.executed <= 18,
                "{strategy}: {} trials",
                runner.stats.executed
            );
        }
    }

    #[test]
    fn adaptive_falls_back_to_ddmin_for_interacting_pairs_while_bisect_flags_incomplete() {
        let mut oracle = |s: &Set| {
            if s.contains(&7) && s.contains(&71) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let mut r_adaptive = Runner::new(&mut oracle);
        let adaptive = StrategyKind::Adaptive
            .search(100, &NoClosure, &mut r_adaptive, 5)
            .unwrap();
        assert_eq!(adaptive.verdict, Verdict::Verified);
        assert_eq!(adaptive.culprits[0].units, vec![7, 71]);
        assert!(!adaptive.culprits[0].exhausted);
        drop(r_adaptive);

        let mut r_bisect = Runner::new(&mut oracle);
        let bisect = StrategyKind::Bisect
            .search(100, &NoClosure, &mut r_bisect, 5)
            .unwrap();
        assert_eq!(bisect.verdict, Verdict::Incomplete);
        assert!(bisect.culprits[0].exhausted);
    }

    #[test]
    fn one_by_one_finds_single_multiple_and_pair_culprits() {
        let mut single = |s: &Set| {
            if s.contains(&2) || s.contains(&5) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let mut r = Runner::new(&mut single);
        let found = StrategyKind::OneByOne
            .search(6, &NoClosure, &mut r, 5)
            .unwrap();
        assert_eq!(found.verdict, Verdict::Verified);
        let mut sets: Vec<Vec<usize>> = found.culprits.iter().map(|c| c.units.clone()).collect();
        sets.sort();
        assert_eq!(sets, vec![vec![2], vec![5]]);

        let mut pair = |s: &Set| {
            if s.contains(&1) && s.contains(&4) {
                Outcome::Fail
            } else {
                Outcome::Pass
            }
        };
        let mut r2 = Runner::new(&mut pair);
        let found2 = StrategyKind::OneByOne
            .search(6, &NoClosure, &mut r2, 5)
            .unwrap();
        assert_eq!(found2.verdict, Verdict::Verified);
        assert_eq!(found2.culprits[0].units, vec![1, 4]);
    }

    #[test]
    fn strategies_respect_dependency_closures() {
        let closure = DepClosure::new(10, [(4, 5)], []);
        for strategy in [
            StrategyKind::Ddmin,
            StrategyKind::Adaptive,
            StrategyKind::OneByOne,
        ] {
            let mut oracle = |s: &Set| {
                assert!(!s.contains(&4) || s.contains(&5), "violated closure: {s:?}");
                if s.contains(&4) {
                    Outcome::Fail
                } else {
                    Outcome::Pass
                }
            };
            let mut runner = Runner::new(&mut oracle);
            let found = strategy.search(10, &closure, &mut runner, 5).unwrap();
            assert_eq!(found.verdict, Verdict::Verified);
            assert_eq!(found.culprits[0].units, vec![4, 5]);
        }
    }
}
