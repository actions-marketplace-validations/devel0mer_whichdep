# whichdep

**`git bisect` for lockfiles** — isolate which dependency update broke your build across **Cargo**, **uv**, **pip**, **npm**, and **pnpm**.

[![CI](https://github.com/devel0mer/whichdep/actions/workflows/ci.yml/badge.svg)](https://github.com/devel0mer/whichdep/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/whichdep.svg)](https://crates.io/crates/whichdep)
[![PyPI](https://img.shields.io/pypi/v/whichdep.svg)](https://pypi.org/project/whichdep/)
[![npm](https://img.shields.io/npm/v/whichdep.svg)](https://www.npmjs.com/package/whichdep)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Your CI was green yesterday. Today a `cargo update`, a Dependabot PR, or a fresh install pulled in 20 new package versions and your tests went red. `git bisect` does not help: your code did not change, your lockfile did.

`whichdep` takes the lockfile that worked and the one that fails, applies closed subsets of the diff to an isolated copy of your project, runs your test command, and narrows the failure down to the **smallest verified root cause** using closure-aware delta debugging (Zeller's `ddmin` + adaptive binary search).

![whichdep isolating a breaking npm dependency update in 7 trials](https://raw.githubusercontent.com/devel0mer/whichdep/main/assets/demo.gif)

---

## Install

No runtime dependencies — prebuilt native binaries for Linux (`x86_64`, `aarch64`), macOS (`arm64`, `x86_64`), and Windows (`x86_64`):

```sh
# Node.js / npm (zero-install via npx)
npx whichdep --good HEAD -- npm test

# Python (PyPI / uv)
uvx whichdep --good HEAD -- pytest
# or: pip install whichdep

# Rust (crates.io)
cargo install whichdep
```

---

## Why not bisect dependencies by hand?

- **Closure-aware root causes.** When a major update pulls in required companion packages (for example `rand 0.9` pulling `rand_core`, `rand_chacha`, and `getrandom`), `whichdep` keeps the lockfile valid during search, separates the **root cause** from its **required companion updates**, and emits only the root revert command:
  ```text
  Root cause
    rand  0.8.5 → 0.9.5
    Required companion updates:
      getrandom  0.2.17 → 0.3.4
      rand_chacha  0.3.1 → 0.9.0
      rand_core  0.6.4 → 0.9.5

  Verified: The bad lockfile without this cause passes the tests again.

  To put it back to the good version:
    cargo update rand@0.9.5 --precise 0.8.5
  ```
- **Genuine interactions & multiple causes.** Two independent updates that only break *together* come back as a pair. After finding one cause, `whichdep` reverts it in the bad lockfile and verifies — if the tests still fail, it keeps searching for additional independent causes.
- **Never confidently wrong.** Half-applied lockfiles that Cargo, npm, pnpm, uv, or `pip check` reject are classified `unresolved` and skipped, never blamed. Flaky tests can be guarded with `--repeat 3` (disagreement = `unresolved`, driving false causes to 0% in E4 simulations).
- **Fast & resumable.** Roughly `2·log2(n)` trials for a single cause with `--strategy adaptive`, shared external build caches (never polluting `./target`), and persistent cross-run caching of conclusive trial outcomes.
- **CI-ready.** Ships with a composite [GitHub Action](action.yml), sticky PR comments, `$GITHUB_STEP_SUMMARY`, machine-readable JSON (`whichdep-report-v2`), and offline HTML reports.

---

## Supported ecosystems

| Ecosystem | Lockfile | Prepare step (default) | Revert / pin hint |
|---|---|---|---|
| **Cargo** | `Cargo.lock` (v3/v4) | `cargo fetch -q` | `cargo update <crate>@<to> --precise <from>` |
| **uv** | `uv.lock` (v1) | `uv sync --frozen` | `uv add "<pkg>==<from>"` |
| **pip** | pinned `requirements.txt` | `uv pip install` / `venv + pip` | `<pkg>==<from>` |
| **npm** | `package-lock.json` (v2/v3) | `npm ci --no-audit --no-fund --prefer-offline` | `npm install --save-exact <pkg>@<from>` |
| **pnpm** | `pnpm-lock.yaml` (v6/v9) | `pnpm install --frozen-lockfile --prefer-offline` | `pnpm add --save-exact <pkg>@<from>` |

Lockfiles are auto-detected in the current directory. Custom ecosystems can also be registered via `AdapterRegistry` in the Rust library.

---

## Live-registry validations

End-to-end validations against real PyPI, npm, and crates.io lockfiles using minimal synthetic reproducer harnesses (see [tests/regression_corpus/README.md](tests/regression_corpus/README.md) and [PLAN.md](PLAN.md) for full details and upcoming upstream E1 repository benchmarks):

| Ecosystem | Breaking update | Changes | Trials | Unresolved | Wall time | Result |
|---|---|---:|---:|---:|---:|---|
| **Python (`pip`)** | `httpx 0.27.2 → 0.28.1` (`proxies=` removed) | 7 | 7 | 1 | 36s | Exact root cause |
| **Node (`npm`)** | `chalk 4.1.2 → 6.0.1` (CJS → ESM) | 14 | 7 | 0 | 4.2s | Exact root cause · **Verified** |
| **Rust (`cargo`)** | `rand 0.8.5 → 0.9.5` (`thread_rng` under `-D warnings`) | 9 | 6 | 0 | 6.0s | Exact root cause + companions · **Verified** |

---

## Usage

Run `whichdep` in the directory containing your lockfile with the **bad** state checked out. Only the lockfile varies between trials; your source code and manifest stay untouched.

```sh
whichdep --good <REV|FILE> [--bad <REV|FILE>] [--lockfile PATH] -- <test command>
whichdep --good <REV|FILE> --list          # only show what changed
whichdep adapters [--json]                 # list supported ecosystems
whichdep history [ID] [--json | --html]    # inspect store or export dashboard HTML
whichdep clean [--build-only]              # reclaim cache/store disk space
whichdep format --input rep.json --format github-comment
```

| Option | Meaning |
|---|---|
| `--good` | Lockfile that works: a git revision or a file path |
| `--bad` | Lockfile that fails (default: working tree) |
| `--lockfile` | `Cargo.lock`, `uv.lock`, pinned `requirements.txt`, `package-lock.json`, or `pnpm-lock.yaml` (auto-detected) |
| `--ecosystem` | `cargo`, `uv`, `pip`, `npm`, `pnpm` (guessed from lockfile name) |
| `--strategy` | `ddmin` (default), `bisect`, `adaptive`, or `one-by-one` |
| `--repeat N` | Run the test `N` times per trial; disagreement = flaky = `unresolved` |
| `--timeout S` | Kill a run after `S` seconds; counts as a failure (isolates hangs) |
| `--max-trials N`, `--max-causes N` | Search budgets |
| `--prepare SCRIPT` | Replace the built-in environment setup (`none` skips) |
| `--json`, `--format FMT` | `text`, `json` (`whichdep-report-v2`), `markdown`, `github-comment`, `html` |
| `--out PATH` | Write the rendered report to a file |
| `--github-summary` | Append Markdown summary to `$GITHUB_STEP_SUMMARY` and emit CI annotations |
| `--keep`, `-v` | Keep trial workspaces, stream command output |
| `--isolate-target` | Isolate build target dirs per rendered lockfile instead of sharing per experiment |
| `--no-resume` | Re-run trials instead of reusing persisted conclusive results |

Exit codes:
- **Test command:** `0` = pass, `125` = cannot be tested (`unresolved`, like `git bisect run`), anything else = fail.
- **`whichdep`:** `0` = cause found, `1` = no result / incomplete, `2` = usage or environment error.

---

## GitHub Actions (Dependabot / Renovate)

Use the built-in composite action to bisect failing dependency PRs and export a sticky PR comment (see [.github/workflows/whichdep-dependabot.yml](.github/workflows/whichdep-dependabot.yml) and [docs/PLATFORM.md](docs/PLATFORM.md) for the recommended two-job security model):

```yaml
- uses: devel0mer/whichdep@v0.1.0
  with:
    good: origin/${{ github.base_ref }}
    strategy: adaptive
    test-command: cargo test -q
    format: json
    out: whichdep-report.json
    github-summary: "true"
```

---

## How it works

1. Parse both lockfiles and compute atomic change units (one unit per package whose locked versions differ).
2. Check the endpoints: `good` must pass, `bad` must fail.
3. Run `ddmin` (or `adaptive` / `bisect` / `one-by-one`) over the units while respecting dependency-closure edges so impossible lockfiles are never proposed.
4. For each trial, render the partial lockfile into a fresh copy of the project, prepare the environment, verify that the package manager kept the lockfile as written, and run your test command.
5. Separate root causes from required companion updates, revert the causes in the bad lockfile, and re-run to confirm (`Verified`).

Every trial runs in an isolated scratch copy of the project. Reusable build caches, trial logs, and experiment history live outside your source tree under `$WHICHDEP_HOME` (default: `$XDG_CACHE_HOME/whichdep` or `~/.cache/whichdep`). `Unresolved` outcomes are never persisted, and any fresh run that disagrees with a cached outcome invalidates that cache entry.

---

## Using the Rust library

The core search engine is usable without the CLI, Git, or process execution. Supply an `Experiment`, a `Closure`, and any `Oracle`:

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

See [docs/PLATFORM.md](docs/PLATFORM.md) for custom `Adapter` and `SearchStrategy` implementations, persistent `ExperimentStore` usage, and JSON schemas.

---

## Limitations (v0.1.0)

- **Ecosystems:** `Cargo.lock` (v3/v4), `uv.lock` (v1), pinned `requirements.txt`, `package-lock.json` (v2/v3), and `pnpm-lock.yaml` (v6/v9), plus custom adapters via `AdapterRegistry`.
- **Workspace manifests:** Manifest edits between good and bad are reported as a warning; the baseline may then be `unresolved`. Path dependencies outside the project directory are not copied.
- **Isolation boundary:** Trial workspaces provide filesystem isolation, not an OS security sandbox. Test and prepare commands run as the current user — never expose production secrets to a CI job running `whichdep`.
- **Cache correctness:** Persistent resume is only as accurate as the supplied oracle and closure fingerprints; pass `--no-resume` for volatile external services or untracked environment inputs.
- **Windows:** Symbolic links inside the project tree are rejected rather than followed; process-tree termination on timeout uses `taskkill /T /F`.

## Related work

[culpip](https://pypi.org/project/culpip/) (Python only, alpha), `where-broke` (npm, single library), `debbisect` (Debian snapshots), `lockvet` (explains lockfile diffs, runs no tests), `EnvCause` (ddmin over `.env`).

See [PLAN.md](PLAN.md) for the measurement plan, E4 flakiness results, and roadmap.

## License

Apache-2.0 — see [LICENSE](LICENSE).
