//! whichdep: find which dependency update broke your build.
//!
//! Given a *good* and a *bad* lockfile, `whichdep` applies subsets of the
//! changes on top of the good one and runs your test command until it has
//! isolated the smallest set of changes that makes the tests fail.

pub mod adapter;
pub mod cargo;
pub mod cli;
pub mod dd;
pub mod diagnosis;
pub mod engine;
pub mod exec;
pub mod fingerprint;
pub mod git;
pub mod json;
pub mod model;
pub mod npm;
pub mod oracle;
pub mod pip;
pub mod platform;
pub mod pnpm;
pub mod report;
pub mod session;
pub mod store;
pub mod strategy;
pub mod trial;
pub mod ui;
pub mod uv;
pub mod workspace;

// Primary library API: callers can run experiments without using the CLI.
pub use adapter::{Adapter, AdapterRegistry, CargoAdapter, PipAdapter};
pub use dd::{Closure, DepClosure, NoClosure};
pub use diagnosis::{Diagnosis, Finding, TrialEvidence, Verification, VerificationStatus};
pub use engine::{Engine, EngineError};
pub use model::{Change, Ecosystem, Experiment, Snapshot};
pub use npm::NpmAdapter;
pub use oracle::{Oracle, Outcome, Set};
pub use platform::{OutputFormat, STICKY_COMMENT_MARKER};
pub use pnpm::PnpmAdapter;
pub use store::{ExperimentStore, ExperimentSummary};
pub use strategy::{SearchStrategy, StrategyKind};
pub use uv::UvAdapter;
