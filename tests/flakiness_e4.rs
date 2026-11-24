//! Experiment E4 (from PLAN.md): Flakiness resistance under `--repeat 1, 3, 5`.
//!
//! Injects symmetric test flakiness with probability `p` (a passing config
//! flakes to fail with probability `p`; a failing config flakes to pass with
//! probability `p`) and measures how often `whichdep` produces a confidently
//! wrong (`Verified` with wrong cause) answer vs. correct or safely
//! unverified/no-result outcomes.

use whichdep::dd::{NoClosure, Outcome, Runner, Set, Verdict};
use whichdep::strategy::{SearchStrategy, StrategyKind};

struct XorShift64(u64);

impl XorShift64 {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_f64(&mut self) -> f64 {
        // 53 bits of uniform precision in [0, 1).
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }
}

#[derive(Default, Debug)]
struct E4Metrics {
    rounds: usize,
    exact_verified: usize,
    coarsened_superset: usize,
    false_cause: usize,
    safe_incomplete_or_no_result: usize,
    total_invocations: usize,
}

fn run_e4(p: f64, repeat: usize, rounds: usize, strategy: StrategyKind) -> E4Metrics {
    let n = 20usize;
    let mut rng = XorShift64(0xD1B5_4A32_D192_ED03 ^ ((p.to_bits()) ^ (repeat as u64)));
    let mut metrics = E4Metrics {
        rounds,
        ..E4Metrics::default()
    };

    for r in 0..rounds {
        let true_cause = r % n;
        let mut invocations = 0usize;

        let mut oracle = |config: &Set| -> Outcome {
            let truly_fails = config.contains(&true_cause);
            let mut first: Option<Outcome> = None;
            for _ in 0..repeat {
                invocations += 1;
                let flaked = rng.next_f64() < p;
                let observed = match (truly_fails, flaked) {
                    (true, false) | (false, true) => Outcome::Fail,
                    (false, false) | (true, true) => Outcome::Pass,
                };
                match first {
                    None => first = Some(observed),
                    Some(prev) if prev != observed => return Outcome::Unresolved,
                    Some(_) => {}
                }
            }
            first.unwrap()
        };

        let mut runner = Runner::new(&mut oracle);
        let res = strategy.search(n, &NoClosure, &mut runner, 5);
        drop(runner);
        metrics.total_invocations += invocations;

        match res {
            Ok(found) if found.verdict == Verdict::Verified => {
                if found.causes.len() == 1 && found.causes[0].units == vec![true_cause] {
                    metrics.exact_verified += 1;
                } else if found.causes.iter().any(|c| c.units.contains(&true_cause)) {
                    // Contains the true cause, coarsened by flaky Unresolved trials.
                    metrics.coarsened_superset += 1;
                } else {
                    // Blamed only innocent package(s) while missing the true cause.
                    metrics.false_cause += 1;
                }
            }
            Ok(_) | Err(_) => {
                metrics.safe_incomplete_or_no_result += 1;
            }
        }
    }
    metrics
}

#[test]
fn e4_repeat_drastically_reduces_false_causes_under_flakiness() {
    let rounds = 300;
    let probs = [0.01, 0.05, 0.10, 0.20];
    let repeats = [1usize, 3, 5];

    eprintln!(
        "\nE4 Flakiness Matrix (n=20, rounds={rounds}, strategy=adaptive)\n\
         |   p  | repeat | exact verified | coarsened superset | false cause | safe incomplete/none | avg runs |"
    );
    eprintln!(
        "|------|--------|----------------|--------------------|---------------|----------------------|----------|"
    );

    let mut false_p05_r1 = 0;
    let mut false_p05_r3 = 0;
    let mut false_p05_r5 = 0;
    let mut false_p10_r1 = 0;
    let mut false_p10_r3 = 0;
    let mut false_p10_r5 = 0;

    for &p in &probs {
        for &repeat in &repeats {
            let m = run_e4(p, repeat, rounds, StrategyKind::Adaptive);
            let pct = |x: usize| (x as f64) * 100.0 / (m.rounds as f64);
            let avg_runs = (m.total_invocations as f64) / (m.rounds as f64);
            eprintln!(
                "| {p:>4.2} | {repeat:>6} | {:>13.1}% | {:>17.1}% | {:>12.1}% | {:>19.1}% | {:>8.1} |",
                pct(m.exact_verified),
                pct(m.coarsened_superset),
                pct(m.false_cause),
                pct(m.safe_incomplete_or_no_result),
                avg_runs
            );
            if (p - 0.05).abs() < 1e-9 {
                match repeat {
                    1 => false_p05_r1 = m.false_cause,
                    3 => false_p05_r3 = m.false_cause,
                    5 => false_p05_r5 = m.false_cause,
                    _ => {}
                }
            }
            if (p - 0.10).abs() < 1e-9 {
                match repeat {
                    1 => false_p10_r1 = m.false_cause,
                    3 => false_p10_r3 = m.false_cause,
                    5 => false_p10_r5 = m.false_cause,
                    _ => {}
                }
            }
        }
    }

    assert!(
        false_p05_r3 == 0 && false_p05_r5 == 0,
        "repeat>=3 should eliminate false causes at p=0.05 (r1={false_p05_r1}, r3={false_p05_r3}, r5={false_p05_r5})"
    );
    assert!(
        false_p10_r3 <= 1 && false_p10_r5 == 0 && false_p10_r5 < false_p10_r1,
        "repeat=5 should eliminate false causes at p=0.10 (r1={false_p10_r1}, r3={false_p10_r3}, r5={false_p10_r5})"
    );
}
