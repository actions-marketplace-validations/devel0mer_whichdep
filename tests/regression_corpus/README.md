# Real-world regression corpus

This directory tracks **reproduced** dependency-regression cases. A case is
promoted to the corpus when it documents:

1. pinned good and bad lockfiles (or a reproducible way to obtain them),
2. the source/test reproducer and toolchain/environment metadata,
3. an oracle command that passes on the good state and fails on the bad state,
4. the upstream release/issue reference and maintainer-confirmed explanation,
5. the expected cause set and observed `whichdep` results.

## Live-registry synthetic validations

The cases below resolve real packages and real lockfiles from PyPI, npm, and
crates.io for known upstream breaking releases, using minimal synthetic
reproducer harnesses to validate end-to-end lockfile rewriting, closure
handling, and verification. Full upstream repository reproductions (E1) are
tracked separately below.

### 1. Python (`pip` / `requirements.txt`) — `httpx 0.27.2 → 0.28.1` (`proxies=` removal)

- **Upstream reference:** `encode/httpx` 0.28.0 (Nov 2024) removed the
  deprecated `proxies=` parameter from `httpx.Client`, breaking downstream
  clients (`openai`, `anthropic`, `langchain`, etc.).
- **Environment:** macOS arm64, Python 3.15 + `pip` fallback (no `uv`),
  `whichdep 0.0.1` via `npx -y whichdep`, `--strategy adaptive`, oracle
  `pytest -q` calling `httpx.Client(proxies="http://127.0.0.1:8080")`.
- **Diff size:** 7 changes (6 updated, 1 removed: `sniffio` dropped by
  `httpx 0.28.1`).
- **Observed result:**
  - Isolated cause: **`httpx 0.27.2 → 0.28.1`** (exact match).
  - Pin hint: `httpx==0.27.2`.
  - Trials: **7 trials** (2 pass, 4 fail, 1 unresolved), wall time **36s**.
  - **Key finding:** Trial #7 (revert verification) returned `unresolved`
    (`httpx 0.27.2 requires sniffio, which is not installed`) because
    `pip freeze` omits `# via` comments, so `whichdep` honestly reported
    `Not verified` instead of misblaming `sniffio`. In `v0.0.2`, `PipPlan`
    added reverse `# via` closure edges for `Kind::Removed` so `pip-compile` /
    `uv pip compile` lockfiles automatically restore removed transitive
    dependencies on revert and reach `Verified`.

### 2. Node.js (`npm` / `package-lock.json` v3) — `chalk 4.1.2 → 6.0.1` (CJS → ESM)

- **Upstream reference:** `chalk` 5+/6+ dropped CommonJS `require("chalk")`
  default export support in favor of pure ESM, breaking CJS consumers.
- **Environment:** macOS arm64, Node.js + `npm ci --no-audit --no-fund
  --prefer-offline`, `whichdep 0.0.2` via `npx -y whichdep@latest`,
  `--strategy adaptive`, oracle `node test.js` (`const chalk = require("chalk");
  chalk.green("ok")`).
- **Diff size:** 14 changes (7 updated, 7 removed — including `chalk` 4's
  transitive dependencies `ansi-styles`, `supports-color`, `color-convert`,
  `color-name`, `has-flag`).
- **Observed result:**
  - Isolated cause: **`chalk 4.1.2 → 6.0.1`** (exact match).
  - Verdict: **`Verified`** — reverting `chalk` automatically restored its
    removed transitive dependencies via `NpmPlan`'s reverse closure, and
    `npm ci` + `node test.js` passed in 567ms.
  - Trials: **7 trials** (2 pass, 5 fail, **0 unresolved**), ~560–723ms per
    trial, wall time **4.2s**.

### 3. Rust (`cargo` / `Cargo.lock` v4) — `rand 0.8.5 → 0.9.5` (`thread_rng` deprecation under `-D warnings`)

- **Upstream reference:** `rand` 0.9.0 (Jan 2025) deprecated `rand::thread_rng()`
  in favor of `rand::rng()`, breaking Rust CI builds running with
  `RUSTFLAGS="-D warnings"`.
- **Environment:** macOS arm64, Rust stable, `RUSTFLAGS="-D warnings"`,
  `whichdep 0.0.2` via `npx -y whichdep@latest`, `--strategy adaptive`,
  oracle `cargo test -q`.
- **Diff size:** 9 changes (9 updated crates from crates.io).
- **Observed result:**
  - Isolated coupled major-bump set: root **`rand 0.8.5 → 0.9.5`** with
    required companions `getrandom 0.2.17 → 0.3.4`,
    `rand_chacha 0.3.1 → 0.9.0`, `rand_core 0.6.4 → 0.9.5`.
  - Verdict: **`Verified`** (reverting the coupled `rand` 0.9 group while
    keeping the other 5 updated crates passed in 615ms).
  - Trials: **6 trials** (2 pass, 4 fail, **0 unresolved**), warm incremental
    trials **143–154ms**, total wall time **6.0s**.
  - **Key finding:** On `v0.0.2`, this run exposed two UX issues — a closure
    chain was labelled as a multi-package interaction and emitted per-companion
    `cargo update` hints. Fixed in `v0.1.0` via `Closure::roots`, which
    separates the root (`rand`) from required companion updates and emits only
    `cargo update rand@0.9.5 --precise 0.8.5`. Also verified `RUSTFLAGS`
    cache-key isolation and added a `-D warnings` hint when `BadPasses` sees
    Cargo compiler warnings.

## Upstream E1 reproductions (full repos)

### 4. Rust (`cargo` / `Cargo.lock`) — Bevy #12044 (`async-executor 1.8.0 → 1.9.0` deadlock)

- **Upstream reference:** [Bevy #12044](https://github.com/bevyengine/bevy/issues/12044)
  — Bevy `v0.12.1` startup/task-pool lockup after Cargo resolved yanked
  `async-executor 1.9.0` (which re-introduced a `ThreadLocal` local-queue
  optimization later reverted in `1.9.1`).
- **Fixtures:** `bevy_12044/Cargo.good.lock`, `bevy_12044/Cargo.bad.lock`,
  `bevy_12044/issue_12044.rs` (placed at `crates/bevy_tasks/tests/issue_12044.rs`).
- **Environment:** macOS arm64, Rust stable, `whichdep 0.1.0`, full 590-crate
  `bevyengine/bevy` `v0.12.1` workspace, oracle
  `cargo test -p bevy_tasks --features multi-threaded --test issue_12044`.
- **Diff size:** 16 updated crates across the 590-crate workspace.
- **Observed results (`--no-resume`):**
  - `adaptive`: **8 trials** (3 pass, 5 fail, **0 unresolved**), **36s** (cold→warm).
  - `ddmin`: **8 trials** (3 pass, 5 fail, **0 unresolved**), **18s** (warm cache: ~850ms/pass, ~2.8s/fail including 2s timeout).
  - Isolated cause: **`async-executor 1.8.0 → 1.9.0`** (exact match).
  - Verdict: **`Verified`** — `cargo update async-executor@1.9.0 --precise 1.8.0`.

### 5. Python (`pip` / `requirements.txt` with `# via`) — Synapse AI v1.8.6 (`mcp 1.30.0 → 2.3.0`)

- **Upstream reference:** [Synapse AI v1.8.6 release notes](https://github.com/synapseorch-ai/synapse-ai/releases/tag/v1.8.6)
  — `synapseorch-ai/synapse-ai` `v1.8.5` broke on fresh installs when `mcp`
  bumped to `2.x` (`backend/tools/time.py` fails at import with
  `AttributeError: 'Server' object has no attribute 'list_tools'`).
- **Fixtures:** `synapse_v1_8_6/requirements.good.txt`,
  `synapse_v1_8_6/requirements.bad.txt` (60 packages with `# via` edges).
- **Environment:** macOS arm64, Python 3.15 + `pip`, `whichdep 0.1.0`,
  `synapseorch-ai/synapse-ai` `v1.8.5`, oracle
  `python -c "import sys; sys.path.insert(0, 'backend'); import tools.time"`.
- **Diff size:** 31 changes (19 updated, 8 new, 4 removed).
- **Observed results (`--no-resume`):**
  - `adaptive`: **29 trials** (+12 reused; 17 pass, 7 fail, **5 unresolved** = 17.2%), **5m 25s**.
  - `ddmin`: **29 trials** (+11 reused; 17 pass, 7 fail, **5 unresolved** = 17.2%), **5m 35s**.
  - Isolated cause: **`mcp 1.30.0 → 2.3.0`** + required version companion
    `idna 3.10 → 3.20` (`mcp 2.3.0 → httpx2 2.16.2` requires `idna>=3.18`) +
    5 new companions (`httpcore2`, `httpx2`, `mcp-types`, `opentelemetry-api`,
    `truststore`).
  - Verdict: **`Not verified`** (honest: reverting `mcp` alone failed `google-genai 1.2.0` vs `websockets 17.4` constraint).
  - **Key finding:** Without `# via` comments (`pip freeze`), the same 31-change diff stalled past 54 trials with >60% unresolved; `# via` edges cut unresolved to 17.2%.

## Next E1 targets

- `uv.lock` and `pnpm-lock.yaml` live upstream reproductions.
