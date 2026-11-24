# Feasibility and measurement plan

The engine and both adapters are tested against synthetic data and tiny local registries. What is not known yet is how it behaves on **real breakages**. The sandbox this was built in has no registry access, so these experiments run on a normal machine.

## Hypotheses
- H1: on real lockfile diffs the cause is found correctly.
- H2: the number of trials stays near 2·log2(n) + 3.
- H3: unresolved trials do not dominate (they cost little but can coarsen the answer).
- H4: it is clearly faster than manual bisecting.

## Experiments
**E1. Real breakages (3-4 first, then 15-20).** Reproduce: Bevy issue #12044 (async-executor 1.9.0 lock-up, Cargo), Synapse v1.8.6 (`mcp` 2.0.0 broke new installs, pip), plus recent Dependabot/`cargo update` failures found by searching issues. For each: pin good/bad lockfiles, run `whichdep`, record found cause vs. the one the maintainers identified, trials, wall time.

**E2. Unresolved rate.** Take 50 recent lockfile-update PRs from 10 popular Rust and 10 Python repos. Run `--list` and a full search with a trivial always-pass/fail oracle to count unresolved configurations and final set sizes, without needing real test suites.

**E3. Trial cost.** Measure seconds per trial vs. number of changed packages, cold vs. warm cache (external per-project build cache, uv cache). Compare against a manual `cargo update -p X --precise` loop.

**E4. Flakiness.** Inject tests that fail with probability p in good and bad states; measure wrong answers and trials for `--repeat` 1, 3, 5.

## Success criteria
- Correct cause in at least 80% of E1 cases, never a confidently wrong one (wrong cases must end unverified or incomplete).
- Median trials at most 2·log2(n)+3 for single causes.
- Unresolved at most 25% of trials in E2.
- At least 3x faster than the manual loop in E3.

## Risks
- Usage may be as rare as `git bisect`: mitigate with a GitHub Action that comments the cause on Dependabot/Renovate PRs.
- culpip or AI-generated clones: differentiate on Cargo, interactions, multi-cause and the verified revert.
- Partial lockfile application is intricate: keep ecosystems few, classify invalid states as unresolved.
- Persistent cache correctness depends on a complete oracle fingerprint; external services and untracked environment inputs may require `--no-resume`.
- Trial workspaces isolate project files, but are not an OS security sandbox.
- Unverified: name availability on crates.io, competitor star counts, whether GitHub or Renovate build this in.

## Roadmap
1. Run E1, write the regression corpus and a benchmark post (dev.to / HN).
2. uv.lock, then npm/pnpm.
3. GitHub Action for dependency PRs.
4. Windows support, `--in-place` mode for out-of-tree path dependencies.

## Observed Measurements (2026-11-24)

### Live-Registry Synthetic Validations (pre-E1 — Python, Node, Rust)
Minimal synthetic harnesses against real PyPI, npm, and crates.io packages/lockfiles (full upstream E1 repos such as Bevy #12044 and Synapse AI v1.8.6 remain next):

| Ecosystem | Live-registry case | Changes | Trials | Unresolved | Wall time | Verdict |
|---|---|---:|---:|---:|---:|---|
| Python (`pip`) | `httpx 0.27.2 → 0.28.1` (`proxies=` removed) | 7 | 7 | 1 (`sniffio`) | 36s | Exact match (`Removed` closure added in `v0.0.2`) |
| Node (`npm`) | `chalk 4.1.2 → 6.0.1` (CJS → ESM) | 14 | 7 | 0 | 4.2s | Exact match · **Verified** (~580ms/trial) |
| Rust (`cargo`) | `rand 0.8.5 → 0.9.5` (root + companions `rand_core`/`rand_chacha`/`getrandom`) | 9 | 6 | 0 | 6.0s | Exact match · **Verified** (~150ms warm trial; root/companion split added in `v0.1.0`) |

See `tests/regression_corpus/README.md` for full reproduction details.

### E4 (Flakiness simulation — `tests/flakiness_e4.rs`)
Measured across 300 seeded rounds per cell (`n = 20`, 1 true cause, symmetric flake probability `p`, `strategy = adaptive`):

| `p` | `--repeat` | Exact verified | Coarsened superset | False cause | Safe incomplete / no-result | Avg test runs |
|---:|---:|---:|---:|---:|---:|---:|
| 0.01 | 1 | 93.3% | 5.3% | 0.0% | 1.3% | 10.9 |
| 0.01 | 3 | 88.7% | 4.0% | 0.0% | 7.3% | 28.5 |
| 0.01 | 5 | 82.3% | 4.0% | 0.0% | 13.7% | 45.4 |
| 0.05 | 1 | 74.0% | 17.3% | 0.7% | 8.0% | 12.3 |
| 0.05 | 3 | 56.7% | 10.7% | 0.0% | 32.7% | 25.0 |
| 0.05 | 5 | 38.0% | 10.0% | 0.0% | 52.0% | 34.6 |
| 0.10 | 1 | 49.7% | 31.7% | 1.3% | 17.3% | 14.4 |
| 0.10 | 3 | 29.7% | 9.7% | 0.0% | 60.7% | 19.7 |
| 0.10 | 5 | 14.7% | 7.7% | 0.0% | 77.7% | 25.4 |
| 0.20 | 1 | 17.7% | 31.3% | 8.7% | 42.3% | 12.1 |
| 0.20 | 3 | 6.0% | 7.3% | 0.0% | 86.7% | 15.8 |
| 0.20 | 5 | 1.7% | 3.3% | 0.0% | 95.0% | 15.8 |

**Takeaway:** `--repeat 3` is the recommended CI default for flaky suites: it drove false causes to **0.0%** across all tested flake rates (up to `p = 0.20`) while keeping substantially higher completion rates than `--repeat 5`.
