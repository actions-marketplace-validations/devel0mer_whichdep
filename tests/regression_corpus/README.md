# Real-world regression corpus

This directory is reserved for **reproduced, runnable** dependency-regression
cases. A case is only promoted to the corpus when it contains:

1. a pinned good and bad lockfile (or a reproducible way to obtain them),
2. the source revision and toolchain/environment metadata,
3. an oracle command that passes on the good state and fails on the bad state,
4. the upstream issue/release reference and maintainer-confirmed explanation,
5. an expected culprit set and a test that runs `whichdep` against the fixture.

## Current status

No upstream case has yet met all of those requirements in this repository.
The existing Cargo and Python end-to-end tests use real tools where installed,
but synthetic local packages/registries; they are integration tests, not
historical production regressions.

Candidate cases to reproduce next:

- [Bevy #12044](https://github.com/bevyengine/bevy/issues/12044): Bevy 0.12.1
  users reported a startup hang after a fresh Cargo resolution selected
  `async-executor` 1.9.0; the maintainer said the version was yanked while the
  issue was investigated. Reproduction artifacts and a stable oracle are
  still needed.
- [Synapse AI v1.8.6 release notes](https://github.com/synapseorch-ai/synapse-ai/releases/tag/v1.8.6): the project reports fresh installs broke after
  `mcp` 2.0.0 and documents compatibility and smoke-test fixes. This is
  **synapseorch-ai**, not Matrix's `matrix-org/synapse`; exact before/after
  lockfiles and an independently runnable fixture are still needed.

Do not describe a candidate as a passing regression-corpus test until its
fixture runs locally and the expected culprit is independently confirmed.
