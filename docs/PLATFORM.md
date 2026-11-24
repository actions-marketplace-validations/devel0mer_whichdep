# whichdep Developer Platform & Ecosystem Guide

## Supported Ecosystems (Phase 3)

Run `whichdep adapters` (or `whichdep adapters --json`) to inspect all registered adapters:

| Adapter ID | Aliases | Lockfiles | Default Prepare | Pin / Revert Hint |
|---|---|---|---|---|
| `cargo` | — | `Cargo.lock` (v3, v4) | `cargo fetch -q` | `cargo update <pkg>@<bad> --precise <good>` |
| `uv` | `uv-lock` | `uv.lock` (v1) | `uv sync --locked --quiet && uv pip check` | `uv lock --upgrade-package <pkg>==<good>` |
| `pip` | `requirements` | `requirements.txt`, `*.txt` | `uv pip sync` + `uv pip check` (falls back to `venv` + `pip`) | `<pkg>==<good>` |
| `npm` | `package-lock` | `package-lock.json` (v2, v3) | `npm ci --no-audit --no-fund --prefer-offline` | `npm install --save-exact <pkg>@<good>` |
| `pnpm` | `pnpm-lock` | `pnpm-lock.yaml` (v6, v9) | `pnpm install --frozen-lockfile --store-dir "$WHICHDEP_PNPM_STORE"` | `pnpm add --save-exact <pkg>@<good>` |

### Custom Adapter API

Library callers can register their own ecosystems into `AdapterRegistry`:

```rust
use whichdep::{Adapter, AdapterRegistry, Ecosystem};

let mut registry = AdapterRegistry::builtin();
// registry.register(Box::new(MyCustomAdapter));
```

## Search Strategies (Phase 3)

Select a strategy with `--strategy <NAME>` or `Engine::with_strategy(...)`:

- **`ddmin`** *(default)*: Zeller's delta debugging. Finds 1-minimal sets including interacting pairs/triples and multiple independent causes.
- **`bisect`**: Fast $O(\log_2 n)$ binary halving over change units. Minimizes trials when a single package broke the build; honestly marks the finding incomplete if an interaction pair or unresolved boundary prevents pure halving.
- **`adaptive`**: Runs fast binary halving first and seamlessly hands off to `ddmin` (reusing the warm trial cache) only if a split stalls on an interaction or unresolved boundary. Recommended for CI.
- **`one-by-one`**: Linear single-unit and single-revert probes. Useful for very small diffs ($n \le 6$), non-monotone failures, or high-unresolved lockfiles.

Switching strategies on the same experiment reuses all previously cached conclusive trial outcomes automatically.

## CI, PR Comments & Machine-Readable API (Phase 4)

### Output Formats

- `--format text` *(default)*: Human-readable terminal summary.
- `--format json` (or `--json`): Versioned machine-readable payload (`schema: "whichdep-report-v2"`) including `summary`, `causes`, `revert_commands`, `changes`, `warnings`, `strategy`, and `trials`.
- `--format markdown`: GitHub-Flavored Markdown table + collapsible trial log.
- `--format github-comment`: Sticky PR comment prefixed with `<!-- whichdep-report -->` so CI can update one comment instead of posting duplicates.
- `--format html`: Self-contained, offline single-file HTML report embedding the `whichdep-report-v2` JSON payload in `<script type="application/json" id="whichdep-data">`.
- `--out <PATH>`: Persist the rendered report to a file alongside stdout.
- `--github-summary`: Append the Markdown summary to `$GITHUB_STEP_SUMMARY` and emit GitHub Actions `::error` / `::warning` / `::notice` annotations.

### Re-rendering & Store Management

```sh
whichdep adapters --json
whichdep format --input report.json --format github-comment --out comment.md
whichdep history --json
whichdep history --html --out dashboard.html
whichdep clean --build-only
```

### GitHub Action & Security Model

See [action.yml](../action.yml) and [.github/workflows/whichdep-dependabot.yml](../.github/workflows/whichdep-dependabot.yml).

> **Security Warning (Threat Model):**
> `whichdep` isolates project files per trial, **not** OS permissions, environment variables, or network access. During a Dependabot/Renovate PR, untrusted dependency updates execute arbitrary code (`build.rs`, proc-macros, `postinstall` scripts, Python build hooks, and your test suite) as the CI runner user.
>
> - **Never** expose production secrets, deploy keys, or registry publish tokens to a CI job that runs `whichdep`.
> - **Never** run `whichdep` under `pull_request_target` on untrusted fork PRs.
> - **Prefer the two-job pattern** shown in [whichdep-dependabot.yml](../.github/workflows/whichdep-dependabot.yml): run `whichdep` with `permissions: contents: read` and `post-comment: "false"`, upload `whichdep-comment.md` as an artifact, and post the sticky PR comment from a separate job that does not check out PR code.
> - For Node ecosystems where lifecycle scripts are not required for tests, consider overriding `--prepare "npm ci --ignore-scripts --no-audit --no-fund --prefer-offline"`.
> - **Concurrent CI safety:** `ExperimentStore` and shared build caches use OS-level exclusive file locks (`std::fs::File::lock`) so parallel runs sharing `WHICHDEP_HOME` on self-hosted runners do not corrupt trial caches or `history.log`.

### Web Dashboard Foundation

The persistent `ExperimentStore` exposes two versioned JSON contracts designed to power a standalone web dashboard without adding web-server dependencies to the core CLI:

1. **`whichdep-dashboard-v1`** (`whichdep history --json` / embedded in `whichdep history --html`): index of all experiments, ecosystems, strategies, run counts, and latest verification statuses.
2. **`whichdep-report-v2`** & **`whichdep-diagnosis-v1`** (`whichdep --json` / `runs/*.json` / `Diagnosis::from_json`): full per-run cause sets, trial evidence timeline, and revert commands.
