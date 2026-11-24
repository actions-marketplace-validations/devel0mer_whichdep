# Real-world regression corpus

This directory tracks **reproduced** dependency-regression cases. A case is
promoted to the corpus when it documents:

1. pinned good and bad lockfiles (or a reproducible way to obtain them),
2. the source/test reproducer and toolchain/environment metadata,
3. an oracle command that passes on the good state and fails on the bad state,
4. the upstream release/issue reference and maintainer-confirmed explanation,
5. the expected culprit set and observed `whichdep` results.

## Reproduced cases

### 1. Python / pip — `httpx 0.27.2 → 0.28.1` (`proxies=` removal)

- **Upstream reference:** `encode/httpx` 0.28.0 (Nov 2024) removed the
  deprecated `proxies=` parameter from `httpx.Client`, breaking downstream
  clients (`openai`, `anthropic`, `langchain`, etc.).
- **Environment:** macOS arm64, Python 3.15 + `pip` fallback (no `uv`),
  `whichdep 0.0.1` via `npx -y whichdep`, `--strategy adaptive`, oracle
  `pytest -q` calling `httpx.Client(proxies="http://127.0.0.1:8080")`.
- **Diff size:** 7 changes (6 updated, 1 removed: `sniffio` dropped by
  `httpx 0.28.1`).
- **Observed result:**
  - Isolated culprit: **`httpx 0.27.2 → 0.28.1`** (exact match).
  - Pin hint: `httpx==0.27.2`.
  - Trials: **7 trials** (2 pass, 4 fail, 1 unresolved), wall time **36s**.
  - **Key real-world finding:** Trial #7 (revert verification) returned
    `unresolved` (`httpx 0.27.2 requires sniffio, which is not installed`)
    because `pip freeze` omits `# via` comments, so `whichdep` honestly
    reported `Not verified` instead of misblaming `sniffio`. In `v0.0.2`,
    `PipPlan` added reverse `# via` closure edges for `Kind::Removed` so
    `pip-compile` / `uv pip compile` lockfiles automatically restore removed
    transitive dependencies on revert and reach `Verified`.

## Candidate cases to reproduce next

- [Bevy #12044](https://github.com/bevyengine/bevy/issues/12044): Bevy 0.12.1
  startup hang after fresh Cargo resolution selected `async-executor` 1.9.0.
- [Synapse AI v1.8.6 release notes](https://github.com/synapseorch-ai/synapse-ai/releases/tag/v1.8.6):
  fresh installs broke after `mcp` 2.0.0 (`synapseorch-ai`, not Matrix's
  `matrix-org/synapse`).
- Node (`package-lock.json` / `pnpm-lock.yaml`) real upstream breakage.
