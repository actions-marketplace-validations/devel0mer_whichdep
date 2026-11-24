# whichdep

**Which dependency update broke your build?** `whichdep` finds it for you.

Your CI was green, then a `cargo update`, a Dependabot PR or a fresh install pulled in 30 new package versions and the tests went red. `git bisect` does not help: your code did not change, your lockfile did. `whichdep` takes the lockfile that worked and the one that does not, applies subsets of the differences to a copy of your project, runs your test command, and narrows the failure down to the smallest set of package updates that still breaks it. That is delta debugging (Zeller's ddmin) over packages instead of commits.

```console
$ whichdep --good v1.4.0 -- cargo test
whichdep 0.0.1 · cargo · Cargo.lock
  9 changes (8 updated, 1 new)

  #1   pass        good lockfile       exit 0
  #2   fail        bad lockfile        exit 101
  #3   pass        5 of 9 updates      exit 0
  ...

Cause
  gamma  1.0.0 → 1.0.1

Verified: The bad lockfile without this cause passes the tests again.

To put them back to the good versions:
  cargo update gamma@1.0.1 --precise 1.0.0
```

(The output above is illustrative; see `tests/e2e.rs` for runs against real tools.)

## Why not just bisect by hand?

- **Interactions.** Two updates that only break *together* come back as a pair, not as a wrong single answer.
- **Several causes.** After finding one it reverts it in the bad lockfile and checks; if the tests still fail it keeps going.
- **Invalid mixes are not blamed.** A half-applied update that Cargo cannot lock, or that `pip check` rejects, is classified *unresolved* and skipped, never counted as a failure.
- **Fast.** Roughly 2·log2(n) trials for one cause; identical rendered lockfiles are not run twice, and conclusive trial outcomes can be reused across runs.
- **Isolated.** Each trial starts from a fresh copy of the project. Build caches, logs and experiment history live outside your source tree.

## Install

```
cargo install --path .
```

No dependencies, builds offline. Needs `git`; `uv` for the pip ecosystem (falls back to `venv` + `pip`).

## Usage

```
whichdep --good <REV|FILE> [--bad <REV|FILE>] [--lockfile PATH] -- <test command>
whichdep --good <REV|FILE> --list          # only show what changed
whichdep adapters [--json]                 # list supported ecosystems
whichdep history [ID] [--json | --html]    # inspect store or export dashboard HTML
whichdep clean [--build-only]              # reclaim cache/store disk space
whichdep format --input rep.json --format github-comment
```

| Option | Meaning |
|---|---|
| `--good` | Lockfile that works: a git revision or a file |
| `--bad` | Lockfile that fails (default: working tree) |
| `--lockfile` | `Cargo.lock`, `uv.lock`, pinned `requirements.txt`, `package-lock.json`, or `pnpm-lock.yaml` (auto-detected) |
| `--ecosystem` | `cargo`, `uv`, `pip`, `npm`, `pnpm` (guessed from lockfile name) |
| `--strategy` | `ddmin` (default), `bisect`, `adaptive`, or `one-by-one` |
| `--repeat N` | Run the test N times per trial; disagreement = flaky = unresolved |
| `--timeout S` | Kill a run after S seconds; counts as a failure (finds hangs) |
| `--max-trials N`, `--max-causes N` | Budgets |
| `--prepare SCRIPT` | Replace the built-in environment setup (`none` skips) |
| `--json`, `--format FMT` | `text`, `json` (`whichdep-report-v2`), `markdown`, `github-comment`, `html` |
| `--out PATH` | Write the rendered report to a file |
| `--github-summary` | Append Markdown summary to `$GITHUB_STEP_SUMMARY` and emit CI annotations |
| `--keep`, `-v` | Keep trial workspaces, stream command output |
| `--isolate-target` | Isolate build target dirs per rendered lockfile instead of sharing per experiment |
| `--no-resume` | Re-run trials instead of reusing persisted conclusive results |

The test command's exit status: `0` pass, `125` cannot be tested (like `git bisect run`), anything else fail. Exit status of `whichdep`: `0` found, `1` no result, `2` usage/environment error.

Run it in the directory that contains the lockfile, with the *bad* state checked out. Only the lockfile varies between trials; code and manifest stay as they are.

## How it works

1. Parse both lockfiles and compute the changes (one unit per package whose locked versions differ).
2. Check the endpoints: good must pass, bad must fail.
3. ddmin over the units. Requirements between units (a new crate that needs a new major of another) are respected so impossible lockfiles are not proposed.
4. Each trial renders a lockfile, builds the environment, verifies that the resolver kept it as written, then runs your test command.
5. Revert what was found in the bad lockfile and test again to confirm.

Every trial is run in a fresh copy of the project. The reusable Cargo build
cache is stored under the whichdep cache directory, not in `./target`. The
persistent store keeps experiment metadata, diagnosis snapshots, command logs
and conclusive trial outcomes. Its default location is `$WHICHDEP_HOME`, then
`$XDG_CACHE_HOME/whichdep`, then `~/.cache/whichdep` (or the platform cache
location). `--no-resume` disables reading old outcomes but still records the
new run. `Unresolved` outcomes are never persisted. If a fresh run disagrees
with a previously cached result for the same key, that cache entry is removed.

## Using the Rust library

The search engine can be used without the CLI, Git handling or a process
runner. A caller supplies an `Experiment`, a dependency `Closure`, and any
implementation of the `Oracle` trait:

```rust
use whichdep::{Change, Engine, Experiment, NoClosure, Outcome, Set, Snapshot};

let experiment = Experiment::new(
    "example-1",
    "example regression",
    "custom",
    Snapshot::new("good", "good-state-fingerprint"),
    Snapshot::new("bad", "bad-state-fingerprint"),
    vec![Change {
        name: "library-a".into(),
        from: vec!["1.0".into()],
        to: vec!["2.0".into()],
    }],
    "test-oracle-v1",
);
let mut oracle = |changes: &Set| {
    if changes.contains(&0) { Outcome::Fail } else { Outcome::Pass }
};
let diagnosis = Engine::new()
    .run(&experiment, &NoClosure, &mut oracle)
    .expect("experiment runs");
println!("{:?}", diagnosis.findings);
```

For persistent history and trial caching, pass an `ExperimentStore` to
`Engine::with_store` and enable reuse with `.with_resume(true)`. The CLI does
this by default and prints an experiment ID that identifies its history folder.
Cache identity includes the endpoint fingerprints, change
units, ecosystem, oracle fingerprint and dependency-closure fingerprint.
Callers must make the oracle fingerprint cover every input that can affect
its result (tool versions, environment, source tree, command and settings).
Custom `Closure` implementations must return a stable `cache_fingerprint()`
to enable persistent trial reuse; otherwise history is still stored but
cross-run cache reuse is disabled. Fingerprints are non-cryptographic, so use
`--no-resume` for volatile or untrusted external oracles. The engine persists each conclusive `Pass` and `Fail` outcome immediately
when a trial finishes; unresolved states are retried. This allows a later run
to continue after an interrupted process, not only after a budget-limited run.

## Limitations (v0.1.0)

- Ecosystems: `Cargo.lock` (v3/v4), `uv.lock` (v1), pinned `requirements.txt`, `package-lock.json` (v2/v3), and `pnpm-lock.yaml` (v6/v9), plus custom adapters via `AdapterRegistry`. See [docs/PLATFORM.md](docs/PLATFORM.md) for the Adapter API, search strategies, machine-readable JSON schemas, GitHub Action ([action.yml](action.yml)), CI security model, and HTML dashboard export.
- Live regression reproductions are documented in [tests/regression_corpus/README.md](tests/regression_corpus/README.md) (Python `httpx`, Node `chalk`, Rust `rand`); automated offline CI tests use synthetic fixtures/registries.
- The persistent store saves fingerprints, metadata, trial outcomes and command logs, but not a full source archive. Use `--keep` to retain the exact temporary project/trial workspaces for manual replay.
- Workspace manifests that changed between good and bad are reported as a warning; the baseline may then be unresolved.
- Path dependencies outside the project directory are not copied.
- The fresh-workspace boundary is filesystem isolation, not a security sandbox: test and prepare commands still run as the current user and can access the network, home directory and external services. Never expose production secrets to a CI job running `whichdep`.
- Persistent resume is only as correct as the supplied Oracle and Closure fingerprints. External services, tool binaries changed in place, and environment inputs not captured by the caller may require `--no-resume`.
- On Windows, symbolic links inside the project tree are rejected rather than followed; process-tree termination on timeout uses `taskkill /T /F`.

## Related work

[culpip](https://pypi.org/project/culpip/) (Python only, alpha), where-broke (npm, one library's versions), debbisect (Debian snapshots), lockvet (explains lockfile diffs, runs no tests), EnvCause (ddmin over `.env`).

See [PLAN.md](PLAN.md) for the measurement plan and roadmap.

## License

Apache-2.0
