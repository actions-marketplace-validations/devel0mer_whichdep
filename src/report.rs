//! The result of a run, as text for people and as JSON for machines.

use crate::dd::{DdError, Findings, Outcome, Verdict};
use crate::diagnosis::TrialEvidence;
use crate::json::Json;
use crate::model::Change;
use crate::trial::Record;
use crate::ui::{self, Ui};
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;

pub enum SearchResult {
    Found(Findings),
    /// The endpoints did not behave: the good lockfile fails, the bad one
    /// passes, one of them could not be built, ...
    NoResult(DdError),
    /// Something made the run impossible (the test command does not exist).
    Fatal(String),
    /// `--list`: nothing was run.
    ListOnly,
}

pub struct Report {
    pub kind: String,
    pub lockfile: String,
    pub strategy: String,
    pub good: String,
    pub bad: String,
    pub changes: Vec<Change>,
    /// Per change: a command that puts it back to the good version.
    pub hints: Vec<Option<String>>,
    pub warnings: Vec<String>,
    pub result: SearchResult,
    pub records: Vec<Record>,
    /// All known trial evidence from the engine (including resumed cache hits).
    pub cached_trials: Vec<TrialEvidence>,
    /// Configurations the search asked for twice or loaded from persistent cache.
    pub cache_hits: usize,
    pub secs: f64,
    pub kept: Option<PathBuf>,
    /// Stable identifier used to locate persisted experiment history.
    pub experiment_id: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub run: usize,
    pub reused: usize,
    pub pass: usize,
    pub fail: usize,
    pub unresolved: usize,
}

impl Report {
    pub fn stats(&self) -> Stats {
        let mut s = Stats {
            reused: self.cache_hits,
            ..Stats::default()
        };
        if self.records.is_empty() && !self.cached_trials.is_empty() {
            for t in &self.cached_trials {
                match t.outcome {
                    Outcome::Pass => s.pass += 1,
                    Outcome::Fail => s.fail += 1,
                    Outcome::Unresolved => s.unresolved += 1,
                }
            }
            return s;
        }
        for r in &self.records {
            if r.reused_from.is_some() {
                s.reused += 1;
                continue;
            }
            s.run += 1;
            match r.outcome {
                Outcome::Pass => s.pass += 1,
                Outcome::Fail => s.fail += 1,
                Outcome::Unresolved => s.unresolved += 1,
            }
        }
        s
    }

    pub fn exit_code(&self) -> i32 {
        match &self.result {
            SearchResult::Found(f) if f.culprits.iter().all(|c| !c.exhausted) => 0,
            SearchResult::ListOnly => 0,
            SearchResult::Fatal(_) => 2,
            _ => 1,
        }
    }
}

pub fn change_line(c: &Change) -> String {
    match (c.from.is_empty(), c.to.is_empty()) {
        (false, false) => format!("{}  {} → {}", c.name, c.from.join(", "), c.to.join(", ")),
        (true, false) => format!("{}  {} (new)", c.name, c.to.join(", ")),
        (false, true) => format!("{}  {} (removed)", c.name, c.from.join(", ")),
        (true, true) => c.name.clone(),
    }
}

/// `3 updated, 1 new, 1 removed`
pub fn summarize(changes: &[Change]) -> String {
    let updated = changes
        .iter()
        .filter(|c| !c.from.is_empty() && !c.to.is_empty())
        .count();
    let added = changes.iter().filter(|c| c.from.is_empty()).count();
    let removed = changes.iter().filter(|c| c.to.is_empty()).count();
    let mut parts = Vec::new();
    if updated > 0 {
        parts.push(format!("{updated} updated"));
    }
    if added > 0 {
        parts.push(format!("{added} new"));
    }
    if removed > 0 {
        parts.push(format!("{removed} removed"));
    }
    parts.join(", ")
}

fn log_tail(rec: Option<&Record>, lines: usize) -> String {
    let Some(path) = rec.and_then(|r| r.log.as_ref()) else {
        return String::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };
    let all: Vec<&str> = text.lines().collect();
    let from = all.len().saturating_sub(lines);
    let mut out = String::new();
    for l in &all[from..] {
        let _ = writeln!(out, "    {l}");
    }
    out
}

fn explain(report: &Report, err: DdError, ui: Ui) -> String {
    let total = report.changes.len();
    let baseline_rec = report.records.iter().find(|r| r.config.is_empty());
    let target_rec = report.records.iter().find(|r| r.config.len() == total);
    let (what, rec, hint) = match err {
        DdError::BaselineFails => (
            "The good lockfile already fails, so the failure is not caused by the lockfile changes.",
            baseline_rec,
            "Check that --good really is a state in which the tests pass with the code and manifest in the working tree.",
        ),
        DdError::BaselineUnresolved => (
            "The good lockfile could not be built or tested.",
            baseline_rec,
            "Usually the manifest in the working tree no longer matches the good lockfile; try a --good closer to the bad state.",
        ),
        DdError::BadPasses => (
            "The bad lockfile passes, so the failure does not reproduce here.",
            target_rec,
            "If the failure is intermittent try --repeat 3; if it only happens in CI, check that the environment matches.",
        ),
        DdError::BadUnresolved => (
            "The bad lockfile could not be built or tested.",
            target_rec,
            "Look at the log below; --prepare lets you replace the environment setup.",
        ),
        DdError::NothingToBisect => ("Good and bad do not differ, there is nothing to bisect.", None, ""),
        DdError::Exhausted => (
            "The trial budget ran out before both lockfiles were checked.",
            None,
            "Raise --max-trials.",
        ),
        DdError::Aborted => (
            "The search aborted due to a fatal oracle error.",
            report.records.last(),
            "",
        ),
    };
    let mut out = String::new();
    let _ = writeln!(out, "{}", ui.bold("No result"));
    let _ = writeln!(out, "  {what}");
    if let Some(r) = rec {
        let _ = writeln!(out, "  Trial #{}: {}", r.no, r.reason);
        let tail = log_tail(Some(r), 15);
        if !tail.is_empty() {
            let _ = writeln!(out, "  Last lines of its output:\n{tail}");
        }
    }
    if !hint.is_empty() {
        let _ = writeln!(out, "  {hint}");
    }
    out
}

pub fn text(report: &Report, ui: Ui) -> String {
    let mut o = String::new();
    match &report.result {
        SearchResult::ListOnly => {
            let _ = writeln!(
                o,
                "{} changes between good and bad ({}):",
                report.changes.len(),
                summarize(&report.changes)
            );
            for c in &report.changes {
                let _ = writeln!(o, "  {}", change_line(c));
            }
            for w in &report.warnings {
                let _ = writeln!(o, "warning: {w}");
            }
            return o;
        }
        SearchResult::Fatal(msg) => {
            let _ = writeln!(o, "{} {msg}", ui.red("error:"));
            return o;
        }
        SearchResult::NoResult(err) => {
            o.push_str(&explain(report, *err, ui));
        }
        SearchResult::Found(f) => {
            let many = f.culprits.len() > 1;
            for (i, c) in f.culprits.iter().enumerate() {
                let head = match (many, c.units.len() > 1) {
                    (false, false) => "Culprit".to_string(),
                    (false, true) => "Culprit: these updates break the tests together".to_string(),
                    (true, false) => format!("Culprit {} of {}", i + 1, f.culprits.len()),
                    (true, true) => format!(
                        "Culprit {} of {}: these updates break the tests together",
                        i + 1,
                        f.culprits.len()
                    ),
                };
                let _ = writeln!(o, "{}", ui.bold(&head));
                for &u in &c.units {
                    let _ = writeln!(o, "  {}", change_line(&report.changes[u]));
                }
                if c.units.len() > 1 {
                    let _ = writeln!(
                        o,
                        "  Leaving out any single one of them no longer reproduces the failure."
                    );
                }
                if c.exhausted {
                    let _ = writeln!(o, "  Stopped early: the trial budget ran out, this set may not be minimal yet.");
                }
                let _ = writeln!(o);
            }
            let what = if many {
                "these culprits"
            } else {
                "this culprit"
            };
            match f.verdict {
                Verdict::Verified => {
                    let _ = writeln!(
                        o,
                        "{} The bad lockfile without {what} passes the tests again.",
                        ui.green("Verified:")
                    );
                }
                Verdict::Unverified => {
                    let _ = writeln!(
                        o,
                        "{} The bad lockfile without {what} could not be built or tested.",
                        ui.yellow("Not verified:")
                    );
                }
                Verdict::Incomplete => {
                    let _ = writeln!(
                        o,
                        "{} The bad lockfile without {what} still fails: stopped by --max-trials or --max-culprits.",
                        ui.yellow("Incomplete:")
                    );
                }
            }
            let pins: Vec<&String> = f
                .culprits
                .iter()
                .flat_map(|c| c.units.iter())
                .filter_map(|&u| report.hints.get(u).and_then(|h| h.as_ref()))
                .collect();
            if !pins.is_empty() {
                let _ = writeln!(o, "\nTo put them back to the good versions:");
                for p in pins {
                    let _ = writeln!(o, "  {p}");
                }
            }
        }
    }
    for w in &report.warnings {
        let _ = writeln!(o, "\nwarning: {w}");
    }
    let s = report.stats();
    let mut line = format!("{} trials", s.run);
    if s.reused > 0 {
        let _ = write!(line, " (+{} reused)", s.reused);
    }
    let _ = write!(
        line,
        " · {} pass · {} fail · {} unresolved · {}",
        s.pass,
        s.fail,
        s.unresolved,
        ui::duration(report.secs)
    );
    let _ = writeln!(o, "\n{}", ui.dim(&line));
    if let Some(id) = &report.experiment_id {
        let _ = writeln!(o, "{}", ui.dim(&format!("experiment history: {id}")));
    }
    if let Some(k) = &report.kept {
        let _ = writeln!(
            o,
            "{}",
            ui.dim(&format!("scratch directory kept: {}", k.display()))
        );
    }
    o
}

fn change_json(i: usize, c: &Change, hint: Option<&String>) -> Json {
    Json::obj([
        ("id", Json::int(i)),
        ("package", Json::str(&c.name)),
        ("from", Json::strs(c.from.iter().cloned())),
        ("to", Json::strs(c.to.iter().cloned())),
        ("pin", Json::opt_str(hint.cloned())),
    ])
}

/// Deduplicated revert / pin commands for all reported culprit units.
pub fn revert_commands(report: &Report) -> Vec<String> {
    let SearchResult::Found(f) = &report.result else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for c in &f.culprits {
        for &u in &c.units {
            if let Some(Some(hint)) = report.hints.get(u) {
                if !out.contains(hint) {
                    out.push(hint.clone());
                }
            }
        }
    }
    out
}

pub fn json(report: &Report) -> String {
    let (status, error, verdict, culprits) = match &report.result {
        SearchResult::Found(f) => {
            let culprits: Vec<Json> = f
                .culprits
                .iter()
                .map(|c| {
                    Json::obj([
                        (
                            "changes",
                            Json::Arr(
                                c.units
                                    .iter()
                                    .map(|&u| {
                                        change_json(
                                            u,
                                            &report.changes[u],
                                            report.hints.get(u).and_then(|h| h.as_ref()),
                                        )
                                    })
                                    .collect(),
                            ),
                        ),
                        ("minimal", Json::Bool(!c.exhausted)),
                    ])
                })
                .collect();
            ("found", Json::Null, Json::str(f.verdict.as_str()), culprits)
        }
        SearchResult::NoResult(e) => ("no-result", Json::str(e.to_string()), Json::Null, vec![]),
        SearchResult::Fatal(m) => ("error", Json::str(m), Json::Null, vec![]),
        SearchResult::ListOnly => ("list", Json::Null, Json::Null, vec![]),
    };
    let verified = matches!(
        &report.result,
        SearchResult::Found(f) if f.verdict == Verdict::Verified
    );
    let culprit_sets = culprits.len();
    let revert_cmds = revert_commands(report);
    let s = report.stats();
    let name_of = |config: &[usize]| -> Json {
        Json::Arr(
            config
                .iter()
                .filter_map(|&i| report.changes.get(i))
                .map(|c| Json::str(&c.name))
                .collect(),
        )
    };
    let trials: Vec<Json> = report
        .records
        .iter()
        .map(|r| {
            Json::obj([
                ("no", Json::int(r.no)),
                ("applied", name_of(&r.config)),
                ("outcome", Json::str(r.outcome.as_str())),
                ("reason", Json::str(&r.reason)),
                ("seconds", Json::Float(r.secs)),
                ("reused_from", r.reused_from.map_or(Json::Null, Json::int)),
                (
                    "log",
                    Json::opt_str(r.log.as_ref().map(|p| p.display().to_string())),
                ),
            ])
        })
        .collect();
    Json::obj([
        ("schema", Json::str("whichdep-report-v2")),
        ("tool", Json::str("whichdep")),
        ("experiment_id", Json::opt_str(report.experiment_id.clone())),
        ("version", Json::str(crate::cli::VERSION)),
        ("status", Json::str(status)),
        ("error", error),
        ("verdict", verdict),
        ("ecosystem", Json::str(&report.kind)),
        ("lockfile", Json::str(&report.lockfile)),
        ("strategy", Json::str(&report.strategy)),
        ("good", Json::str(&report.good)),
        ("bad", Json::str(&report.bad)),
        (
            "summary",
            Json::obj([
                ("total_changes", Json::int(report.changes.len())),
                ("culprit_sets", Json::int(culprit_sets)),
                ("verified", Json::Bool(verified)),
            ]),
        ),
        ("culprits", Json::Arr(culprits)),
        ("revert_commands", Json::strs(revert_cmds)),
        (
            "changes",
            Json::Arr(
                report
                    .changes
                    .iter()
                    .enumerate()
                    .map(|(i, c)| change_json(i, c, report.hints.get(i).and_then(|h| h.as_ref())))
                    .collect(),
            ),
        ),
        ("warnings", Json::strs(report.warnings.iter().cloned())),
        (
            "trials",
            Json::obj([
                ("run", Json::int(s.run)),
                ("reused", Json::int(s.reused)),
                ("pass", Json::int(s.pass)),
                ("fail", Json::int(s.fail)),
                ("unresolved", Json::int(s.unresolved)),
                ("seconds", Json::Float(report.secs)),
                ("list", Json::Arr(trials)),
            ]),
        ),
    ])
    .pretty()
}
