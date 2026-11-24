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
            SearchResult::Found(f) if f.causes.iter().all(|c| !c.exhausted) => 0,
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
        if err == DdError::BadPasses
            && report.kind == "cargo"
            && (tail.contains("#[warn(") || tail.contains("warning:"))
        {
            let _ = writeln!(
                out,
                "  Compiler warnings were emitted; if CI treats warnings as errors, retry with RUSTFLAGS=\"-D warnings\"."
            );
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
            let many = f.causes.len() > 1;
            for (i, c) in f.causes.iter().enumerate() {
                let interaction = c.is_interaction();
                let companions = c.companions();
                let display_roots: &[usize] = if c.roots.is_empty() {
                    &c.units
                } else {
                    &c.roots
                };
                let head = match (many, interaction) {
                    (false, false) => "Root cause".to_string(),
                    (false, true) => {
                        "Root cause: these updates break the tests together".to_string()
                    }
                    (true, false) => format!("Root cause {} of {}", i + 1, f.causes.len()),
                    (true, true) => format!(
                        "Root cause {} of {}: these updates break the tests together",
                        i + 1,
                        f.causes.len()
                    ),
                };
                let _ = writeln!(o, "{}", ui.bold(&head));
                for &u in display_roots {
                    let _ = writeln!(o, "  {}", change_line(&report.changes[u]));
                }
                if !companions.is_empty() {
                    let _ = writeln!(o, "  Required companion updates:");
                    for &u in &companions {
                        let _ = writeln!(o, "    {}", change_line(&report.changes[u]));
                    }
                }
                if interaction {
                    let msg = if companions.is_empty() {
                        "  Leaving out any single one of them no longer reproduces the failure."
                    } else {
                        "  Leaving out any single root update no longer reproduces the failure."
                    };
                    let _ = writeln!(o, "{msg}");
                }
                if c.exhausted {
                    let _ = writeln!(o, "  Stopped early: the trial budget ran out, this set may not be minimal yet.");
                }
                let _ = writeln!(o);
            }
            let what = if many { "these causes" } else { "this cause" };
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
                        "{} The bad lockfile without {what} still fails: stopped by --max-trials or --max-causes.",
                        ui.yellow("Incomplete:")
                    );
                }
            }
            let pins = revert_commands(report);
            if !pins.is_empty() {
                let heading = if pins.len() == 1 {
                    "\nTo put it back to the good version:"
                } else {
                    "\nTo put them back to the good versions:"
                };
                let _ = writeln!(o, "{heading}");
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

/// Deduplicated revert / pin commands for the root units of all reported causes.
///
/// Required companion updates are excluded because reverting the root package
/// lets the package manager resolve its companions back automatically, whereas
/// pinning a companion while the root still requires the new major version fails.
pub fn revert_commands(report: &Report) -> Vec<String> {
    let SearchResult::Found(f) = &report.result else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for c in &f.causes {
        let targets: &[usize] = if c.roots.is_empty() {
            &c.units
        } else {
            &c.roots
        };
        for &u in targets {
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
    let change_arr = |units: &[usize]| -> Json {
        Json::Arr(
            units
                .iter()
                .map(|&u| {
                    change_json(
                        u,
                        &report.changes[u],
                        report.hints.get(u).and_then(|h| h.as_ref()),
                    )
                })
                .collect(),
        )
    };
    let (status, error, verdict, causes) = match &report.result {
        SearchResult::Found(f) => {
            let causes: Vec<Json> = f
                .causes
                .iter()
                .map(|c| {
                    let roots: &[usize] = if c.roots.is_empty() {
                        &c.units
                    } else {
                        &c.roots
                    };
                    let companions = c.companions();
                    Json::obj([
                        ("changes", change_arr(&c.units)),
                        ("roots", change_arr(roots)),
                        ("companions", change_arr(&companions)),
                        ("minimal", Json::Bool(!c.exhausted)),
                    ])
                })
                .collect();
            ("found", Json::Null, Json::str(f.verdict.as_str()), causes)
        }
        SearchResult::NoResult(e) => ("no-result", Json::str(e.to_string()), Json::Null, vec![]),
        SearchResult::Fatal(m) => ("error", Json::str(m), Json::Null, vec![]),
        SearchResult::ListOnly => ("list", Json::Null, Json::Null, vec![]),
    };
    let verified = matches!(
        &report.result,
        SearchResult::Found(f) if f.verdict == Verdict::Verified
    );
    let cause_sets = causes.len();
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
                ("cause_sets", Json::int(cause_sets)),
                ("verified", Json::Bool(verified)),
            ]),
        ),
        ("causes", Json::Arr(causes)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dd::{Cause, Findings};

    fn cargo_chain_report() -> Report {
        Report {
            kind: "cargo".to_string(),
            lockfile: "Cargo.lock".to_string(),
            strategy: "adaptive".to_string(),
            good: "HEAD".to_string(),
            bad: "working tree".to_string(),
            changes: vec![
                Change {
                    name: "getrandom".to_string(),
                    from: vec!["0.2.17".to_string()],
                    to: vec!["0.3.4".to_string()],
                },
                Change {
                    name: "rand".to_string(),
                    from: vec!["0.8.5".to_string()],
                    to: vec!["0.9.5".to_string()],
                },
                Change {
                    name: "rand_core".to_string(),
                    from: vec!["0.6.4".to_string()],
                    to: vec!["0.9.5".to_string()],
                },
            ],
            hints: vec![
                Some("cargo update getrandom@0.3.4 --precise 0.2.17".to_string()),
                Some("cargo update rand@0.9.5 --precise 0.8.5".to_string()),
                Some("cargo update rand_core@0.9.5 --precise 0.6.4".to_string()),
            ],
            warnings: vec![],
            result: SearchResult::Found(Findings {
                causes: vec![Cause {
                    units: vec![0, 1, 2],
                    roots: vec![1],
                    exhausted: false,
                }],
                verdict: Verdict::Verified,
            }),
            records: vec![],
            cached_trials: vec![],
            cache_hits: 0,
            secs: 1.0,
            kept: None,
            experiment_id: None,
        }
    }

    #[test]
    fn single_root_with_companions_is_not_reported_as_interaction() {
        let rep = cargo_chain_report();
        let out = text(&rep, Ui::plain());
        assert!(
            !out.contains("these updates break the tests together"),
            "{out}"
        );
        assert!(!out.contains("Leaving out any single one of them"), "{out}");
        assert!(out.contains("Root cause\n  rand  0.8.5 → 0.9.5\n"), "{out}");
        assert!(out.contains("Required companion updates:"), "{out}");
        assert!(out.contains("    getrandom  0.2.17 → 0.3.4\n"), "{out}");
        assert!(out.contains("    rand_core  0.6.4 → 0.9.5\n"), "{out}");
        assert!(
            out.contains(
                "To put it back to the good version:\n  cargo update rand@0.9.5 --precise 0.8.5\n"
            ),
            "{out}"
        );
        assert!(!out.contains("cargo update getrandom@"), "{out}");
        assert!(!out.contains("cargo update rand_core@"), "{out}");

        let parsed = Json::parse(&json(&rep)).unwrap();
        let cause = &parsed.get("causes").unwrap().as_arr().unwrap()[0];
        assert_eq!(cause.get("changes").unwrap().as_arr().unwrap().len(), 3);
        assert_eq!(cause.get("roots").unwrap().as_arr().unwrap().len(), 1);
        assert_eq!(cause.get("companions").unwrap().as_arr().unwrap().len(), 2);
        let cmds = parsed.get("revert_commands").unwrap().as_arr().unwrap();
        assert_eq!(cmds.len(), 1);
        assert_eq!(
            cmds[0].as_str(),
            Some("cargo update rand@0.9.5 --precise 0.8.5")
        );
    }

    #[test]
    fn bad_passes_with_cargo_deprecation_warning_suggests_deny_warnings() {
        let log_path =
            std::env::temp_dir().join(format!("whichdep-warn-hint-{}.log", std::process::id()));
        fs::write(
            &log_path,
            "warning: use of deprecated function `rand::thread_rng`\n= note: `#[warn(deprecated)]` on by default\n",
        )
        .unwrap();
        let mut rep = cargo_chain_report();
        rep.result = SearchResult::NoResult(DdError::BadPasses);
        rep.records = vec![Record {
            no: 2,
            config: vec![0, 1, 2],
            outcome: Outcome::Pass,
            reason: "exit 0".to_string(),
            secs: 1.0,
            reused_from: None,
            log: Some(log_path.clone()),
        }];
        let out = text(&rep, Ui::plain());
        let _ = fs::remove_file(&log_path);
        assert!(
            out.contains("RUSTFLAGS=\"-D warnings\""),
            "expected -D warnings hint in:\n{out}"
        );
    }
}
