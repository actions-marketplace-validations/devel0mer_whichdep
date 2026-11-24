//! Stable, machine-readable results produced by the core engine.

use crate::dd::{DdError, Findings, Outcome, Verdict};
use crate::json::Json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationStatus {
    Verified,
    Unverified,
    Incomplete,
    NotAttempted,
}

impl VerificationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Incomplete => "incomplete",
            Self::NotAttempted => "not-attempted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verification {
    pub status: VerificationStatus,
    pub detail: String,
}

impl Verification {
    pub fn from_verdict(verdict: Verdict) -> Self {
        let (status, detail) = match verdict {
            Verdict::Verified => (
                VerificationStatus::Verified,
                "Reverting the reported findings made the target configuration pass.".to_string(),
            ),
            Verdict::Unverified => (
                VerificationStatus::Unverified,
                "The revert configuration could not be evaluated.".to_string(),
            ),
            Verdict::Incomplete => (
                VerificationStatus::Incomplete,
                "The search stopped before the findings could be fully verified.".to_string(),
            ),
        };
        Self { status, detail }
    }

    pub fn not_attempted(detail: impl Into<String>) -> Self {
        Self {
            status: VerificationStatus::NotAttempted,
            detail: detail.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub units: Vec<usize>,
    /// Top-level units in `units` not required by another unit in `units`.
    pub roots: Vec<usize>,
    /// True means the search completed its 1-minimality checks for this set.
    pub one_minimal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrialEvidence {
    pub config: Vec<usize>,
    pub outcome: Outcome,
}

#[derive(Clone, Debug)]
pub struct Diagnosis {
    pub experiment_id: String,
    pub strategy: String,
    pub findings: Vec<Finding>,
    pub verification: Verification,
    pub search_error: Option<DdError>,
    /// Cached outcomes known to this run, including outcomes loaded on resume.
    pub trials: Vec<TrialEvidence>,
    pub executed_trials: usize,
    pub cache_hits: usize,
    pub elapsed_seconds: f64,
}

impl Diagnosis {
    pub fn from_search(
        experiment_id: impl Into<String>,
        result: Result<Findings, DdError>,
        trials: Vec<TrialEvidence>,
        executed_trials: usize,
        cache_hits: usize,
        elapsed_seconds: f64,
    ) -> Self {
        match result {
            Ok(found) => Self {
                experiment_id: experiment_id.into(),
                strategy: "ddmin".to_string(),
                findings: found
                    .causes
                    .iter()
                    .map(|c| Finding {
                        units: c.units.clone(),
                        roots: c.roots.clone(),
                        one_minimal: !c.exhausted,
                    })
                    .collect(),
                verification: Verification::from_verdict(found.verdict),
                search_error: None,
                trials,
                executed_trials,
                cache_hits,
                elapsed_seconds,
            },
            Err(error) => Self {
                experiment_id: experiment_id.into(),
                strategy: "ddmin".to_string(),
                findings: Vec::new(),
                verification: Verification::not_attempted(error.to_string()),
                search_error: Some(error),
                trials,
                executed_trials,
                cache_hits,
                elapsed_seconds,
            },
        }
    }

    pub fn with_strategy(mut self, strategy: impl Into<String>) -> Self {
        self.strategy = strategy.into();
        self
    }

    /// Serialize the diagnosis without pulling a serialization dependency
    /// into the core crate.
    pub fn to_json(&self) -> String {
        let findings = self
            .findings
            .iter()
            .map(|f| {
                Json::obj([
                    (
                        "units",
                        Json::Arr(f.units.iter().map(|&n| Json::int(n)).collect()),
                    ),
                    (
                        "roots",
                        Json::Arr(f.roots.iter().map(|&n| Json::int(n)).collect()),
                    ),
                    ("one_minimal", Json::Bool(f.one_minimal)),
                ])
            })
            .collect();
        let trials = self
            .trials
            .iter()
            .map(|t| {
                Json::obj([
                    (
                        "config",
                        Json::Arr(t.config.iter().map(|&n| Json::int(n)).collect()),
                    ),
                    ("outcome", Json::str(t.outcome.as_str())),
                ])
            })
            .collect();
        Json::obj([
            ("schema", Json::str("whichdep-diagnosis-v1")),
            ("experiment_id", Json::str(self.experiment_id.clone())),
            ("strategy", Json::str(self.strategy.clone())),
            (
                "search_error",
                Json::opt_str(self.search_error.map(|e| e.to_string())),
            ),
            ("verification", Json::str(self.verification.status.as_str())),
            (
                "verification_detail",
                Json::str(self.verification.detail.clone()),
            ),
            ("findings", Json::Arr(findings)),
            ("trials", Json::Arr(trials)),
            ("executed_trials", Json::int(self.executed_trials)),
            ("cache_hits", Json::int(self.cache_hits)),
            ("elapsed_seconds", Json::Float(self.elapsed_seconds)),
        ])
        .pretty()
    }

    /// Parse a previously persisted diagnosis snapshot from JSON.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let root = Json::parse(text)?;
        let experiment_id = root
            .get("experiment_id")
            .and_then(Json::as_str)
            .ok_or_else(|| "diagnosis JSON is missing `experiment_id`".to_string())?
            .to_string();
        let strategy = root
            .get("strategy")
            .and_then(Json::as_str)
            .unwrap_or("ddmin")
            .to_string();
        let status = match root.get("verification").and_then(Json::as_str) {
            Some("verified") => VerificationStatus::Verified,
            Some("unverified") => VerificationStatus::Unverified,
            Some("incomplete") => VerificationStatus::Incomplete,
            _ => VerificationStatus::NotAttempted,
        };
        let detail = root
            .get("verification_detail")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        let mut findings = Vec::new();
        if let Some(arr) = root.get("findings").and_then(Json::as_arr) {
            for item in arr {
                let units: Vec<usize> = item
                    .get("units")
                    .and_then(Json::as_arr)
                    .map(|u| {
                        u.iter()
                            .filter_map(Json::as_i64)
                            .filter_map(|n| usize::try_from(n).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                let roots = item
                    .get("roots")
                    .and_then(Json::as_arr)
                    .map(|u| {
                        u.iter()
                            .filter_map(Json::as_i64)
                            .filter_map(|n| usize::try_from(n).ok())
                            .collect()
                    })
                    .unwrap_or_else(|| units.clone());
                let one_minimal = item
                    .get("one_minimal")
                    .and_then(Json::as_bool)
                    .unwrap_or(false);
                findings.push(Finding {
                    units,
                    roots,
                    one_minimal,
                });
            }
        }
        let mut trials = Vec::new();
        if let Some(arr) = root.get("trials").and_then(Json::as_arr) {
            for item in arr {
                let config = item
                    .get("config")
                    .and_then(Json::as_arr)
                    .map(|c| {
                        c.iter()
                            .filter_map(Json::as_i64)
                            .filter_map(|n| usize::try_from(n).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                let outcome = match item.get("outcome").and_then(Json::as_str) {
                    Some("pass") => Outcome::Pass,
                    Some("fail") => Outcome::Fail,
                    _ => Outcome::Unresolved,
                };
                trials.push(TrialEvidence { config, outcome });
            }
        }
        let search_error =
            root.get("search_error")
                .and_then(Json::as_str)
                .and_then(|msg| match msg {
                    s if s.contains("nothing to bisect") => Some(DdError::NothingToBisect),
                    s if s.contains("already fails") => Some(DdError::BaselineFails),
                    s if s.contains("baseline") && s.contains("could not be evaluated") => {
                        Some(DdError::BaselineUnresolved)
                    }
                    s if s.contains("bad state passes") => Some(DdError::BadPasses),
                    s if s.contains("bad state") && s.contains("could not be evaluated") => {
                        Some(DdError::BadUnresolved)
                    }
                    s if s.contains("exhausted") => Some(DdError::Exhausted),
                    s if s.contains("aborted") => Some(DdError::Aborted),
                    _ => None,
                });
        let executed_trials = root
            .get("executed_trials")
            .and_then(Json::as_i64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        let cache_hits = root
            .get("cache_hits")
            .and_then(Json::as_i64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        let elapsed_seconds = root
            .get("elapsed_seconds")
            .and_then(Json::as_f64)
            .unwrap_or(0.0);

        Ok(Self {
            experiment_id,
            strategy,
            findings,
            verification: Verification { status, detail },
            search_error,
            trials,
            executed_trials,
            cache_hits,
            elapsed_seconds,
        })
    }
}
