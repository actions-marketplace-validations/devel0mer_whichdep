//! The minimal, CLI-independent contract for evaluating a configuration.
//!
//! An oracle can be a test command, a build, a benchmark, a remote CI job, or
//! any other deterministic (or explicitly repeatable) predicate.

use std::collections::BTreeSet;

/// A configuration: the indices of the selected change units.
pub type Set = BTreeSet<usize>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    Pass,
    Fail,
    Unresolved,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Evaluates one proposed set of changes.
pub trait Oracle {
    fn test(&mut self, config: &Set) -> Outcome;

    /// If the oracle encountered an unrecoverable environment/tool error (such
    /// as a missing test binary or unwritable workspace), return the message so
    /// the search aborts immediately without persisting bogus outcomes.
    fn fatal_error(&self) -> Option<&str> {
        None
    }
}

impl<F> Oracle for F
where
    F: FnMut(&Set) -> Outcome,
{
    fn test(&mut self, config: &Set) -> Outcome {
        self(config)
    }
}
