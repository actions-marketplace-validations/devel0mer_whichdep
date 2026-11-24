# Feasibility and measurement plan

The engine and both adapters are tested against synthetic data and tiny local registries. What is not known yet is how it behaves on **real breakages**. The sandbox this was built in has no registry access, so these experiments run on a normal machine.

## Hypotheses
- H1: on real lockfile diffs the culprit is found correctly.
- H2: the number of trials stays near 2·log2(n) + 3.
- H3: unresolved trials do not dominate (they cost little but can coarsen the answer).
- H4: it is clearly faster than manual bisecting.

## Experiments
**E1. Real breakages (3-4 first, then 15-20).** Reproduce: Bevy issue #12044 (async-executor 1.9.0 lock-up, Cargo), Synapse v1.8.6 (`mcp` 2.0.0 broke new installs, pip), plus recent Dependabot/`cargo update` failures found by searching issues. For each: pin good/bad lockfiles, run `whichdep`, record found culprit vs. the one the maintainers identified, trials, wall time.

**E2. Unresolved rate.** Take 50 recent lockfile-update PRs from 10 popular Rust and 10 Python repos. Run `--list` and a full search with a trivial always-pass/fail oracle to count unresolved configurations and final set sizes, without needing real test suites.

**E3. Trial cost.** Measure seconds per trial vs. number of changed packages, cold vs. warm cache (external per-project build cache, uv cache). Compare against a manual `cargo update -p X --precise` loop.

**E4. Flakiness.** Inject tests that fail with probability p in good and bad states; measure wrong answers and trials for `--repeat` 1, 3, 5.

## Success criteria
- Correct culprit in at least 80% of E1 cases, never a confidently wrong one (wrong cases must end unverified or incomplete).
- Median trials at most 2·log2(n)+3 for single culprits.
- Unresolved at most 25% of trials in E2.
- At least 3x faster than the manual loop in E3.

## Risks
- Usage may be as rare as `git bisect`: mitigate with a GitHub Action that comments the culprit on Dependabot/Renovate PRs.
- culpip or AI-generated clones: differentiate on Cargo, interactions, multi-culprit and the verified revert.
- Partial lockfile application is intricate: keep ecosystems few, classify invalid states as unresolved.
- Persistent cache correctness depends on a complete oracle fingerprint; external services and untracked environment inputs may require `--no-resume`.
- Trial workspaces isolate project files, but are not an OS security sandbox.
- Unverified: name availability on crates.io, competitor star counts, whether GitHub or Renovate build this in.

## Roadmap
1. Run E1, write the regression corpus and a benchmark post (dev.to / HN).
2. uv.lock, then npm/pnpm.
3. GitHub Action for dependency PRs.
4. Windows support, `--in-place` mode for out-of-tree path dependencies.

## Regression corpus status (2026-11-24)

The repository now has `tests/regression_corpus/README.md` as the admission
criteria and candidate register. The Bevy and Synapse AI cases are documented
upstream reports, not yet runnable local fixtures. The synthetic local-registry
end-to-end tests must not be counted as real-world regression reproductions.
The corpus milestone therefore remains open.
