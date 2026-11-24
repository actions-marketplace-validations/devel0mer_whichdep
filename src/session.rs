//! One run of the tool: read the two lockfiles, plan the changes, let the
//! search engine drive the trials, and render platform reports.

use crate::adapter::AdapterRegistry;
use crate::cli::{Args, VERSION};
use crate::dd::{Culprit, Findings, Verdict};
use crate::engine::{Engine, EngineError};
use crate::fingerprint;
use crate::git;
use crate::json::Json;
use crate::model::{Change, Experiment, Snapshot};
use crate::platform::{self, OutputFormat};
use crate::report::{self, Report, SearchResult};
use crate::store::ExperimentStore;
use crate::trial::{Prepare, Settings, Trials};
use crate::ui::Ui;
use crate::workspace::{self, Scratch};
use std::env;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// The lockfile text and a label for it: a file, or the file at a revision.
fn read_side(cwd: &Path, spec: &str, lockfile: &str) -> Result<(String, String), String> {
    let as_file = cwd.join(spec);
    if as_file.is_file() {
        let text = fs::read_to_string(&as_file).map_err(|e| format!("cannot read {spec}: {e}"))?;
        return Ok((text, spec.to_string()));
    }
    let text = git::show(cwd, spec, lockfile)?;
    let label = match git::short(cwd, spec) {
        Some(h) if h != spec => format!("{spec} ({h})"),
        _ => spec.to_string(),
    };
    Ok((text, label))
}

fn header(
    ui: Ui,
    kind: &str,
    lockfile: &str,
    strategy: &str,
    good: &str,
    bad: &str,
    changes: &[Change],
) {
    eprintln!(
        "{} {} · {} · {} · {}",
        ui.bold("whichdep"),
        VERSION,
        kind,
        lockfile,
        strategy
    );
    eprintln!("  good  {good}");
    eprintln!("  bad   {bad}");
    eprintln!(
        "  {} changes ({})",
        changes.len(),
        report::summarize(changes)
    );
}

pub fn run(args: Args) -> i32 {
    match execute(&args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("error: {msg}");
            2
        }
    }
}

pub fn run_adapters(json: bool) -> i32 {
    let registry = AdapterRegistry::builtin();
    if json {
        let items: Vec<Json> = registry
            .all()
            .map(|a| {
                Json::obj([
                    ("id", Json::str(a.id())),
                    ("aliases", Json::strs(a.aliases().iter().copied())),
                    ("description", Json::str(a.description())),
                    ("lockfiles", Json::strs(a.lockfiles().iter().copied())),
                ])
            })
            .collect();
        print!(
            "{}",
            Json::obj([
                ("schema", Json::str("whichdep-adapters-v1")),
                ("adapters", Json::Arr(items)),
            ])
            .pretty()
        );
    } else {
        for a in registry.all() {
            println!(
                "{:<8} {:<22} {}",
                a.id(),
                a.lockfiles().join(", "),
                a.description()
            );
        }
    }
    0
}

pub fn run_history(experiment_id: Option<&str>, format: OutputFormat, out: Option<&str>) -> i32 {
    match execute_history(experiment_id, format, out) {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("error: {msg}");
            2
        }
    }
}

fn execute_history(
    experiment_id: Option<&str>,
    format: OutputFormat,
    out: Option<&str>,
) -> Result<(), String> {
    let store = ExperimentStore::open(ExperimentStore::default_root())
        .map_err(|e| format!("cannot open experiment store: {e}"))?;

    let rendered = if let Some(id) = experiment_id {
        let runs = store
            .load_diagnoses(id)
            .map_err(|e| format!("cannot read history for `{id}`: {e}"))?;
        if format == OutputFormat::Json {
            let items: Vec<Json> = runs
                .iter()
                .map(|d| {
                    Json::obj([
                        ("strategy", Json::str(&d.strategy)),
                        ("verification", Json::str(d.verification.status.as_str())),
                        ("findings", Json::int(d.findings.len())),
                        ("executed_trials", Json::int(d.executed_trials)),
                        ("cache_hits", Json::int(d.cache_hits)),
                        ("elapsed_seconds", Json::Float(d.elapsed_seconds)),
                    ])
                })
                .collect();
            Json::obj([
                ("schema", Json::str("whichdep-history-runs-v1")),
                ("experiment_id", Json::str(id)),
                ("runs", Json::Arr(items)),
            ])
            .pretty()
        } else {
            let mut s = format!("Experiment {id} ({} runs):\n", runs.len());
            for (idx, d) in runs.iter().enumerate() {
                s.push_str(&format!(
                    "  run #{:<2}  {:<10}  {:<12}  {} findings · {} run (+{} cached)\n",
                    idx + 1,
                    d.strategy,
                    d.verification.status.as_str(),
                    d.findings.len(),
                    d.executed_trials,
                    d.cache_hits
                ));
            }
            s
        }
    } else {
        let summaries = store
            .list_experiments()
            .map_err(|e| format!("cannot list experiments: {e}"))?;
        match format {
            OutputFormat::Json => {
                let items: Vec<Json> = summaries
                    .iter()
                    .map(|s| {
                        Json::obj([
                            ("id", Json::str(&s.id)),
                            ("ecosystem", Json::str(&s.ecosystem)),
                            ("description", Json::str(&s.description)),
                            ("baseline_label", Json::str(&s.baseline_label)),
                            ("target_label", Json::str(&s.target_label)),
                            ("unit_count", Json::int(s.unit_count)),
                            ("runs", Json::int(s.runs)),
                            (
                                "latest_verification",
                                Json::opt_str(s.latest_verification.clone()),
                            ),
                            ("latest_strategy", Json::opt_str(s.latest_strategy.clone())),
                        ])
                    })
                    .collect();
                Json::obj([
                    ("schema", Json::str("whichdep-dashboard-v1")),
                    ("experiments", Json::Arr(items)),
                ])
                .pretty()
            }
            OutputFormat::Html => platform::dashboard_html(&summaries),
            _ => {
                if summaries.is_empty() {
                    "No persisted experiments found.\n".to_string()
                } else {
                    let mut s = format!("{} persisted experiments:\n", summaries.len());
                    for item in &summaries {
                        s.push_str(&format!(
                            "  {}  {:<6}  {:>2} runs  {:<12}  {}\n",
                            item.id,
                            item.ecosystem,
                            item.runs,
                            item.latest_verification.as_deref().unwrap_or("none"),
                            item.description
                        ));
                    }
                    s
                }
            }
        }
    };

    if let Some(path) = out {
        fs::write(path, &rendered).map_err(|e| format!("cannot write {path}: {e}"))?;
    }
    print!("{rendered}");
    Ok(())
}

pub fn run_clean(build_only: bool) -> i32 {
    let store = match ExperimentStore::open(ExperimentStore::default_root()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot open experiment store: {e}");
            return 2;
        }
    };
    let result = if build_only {
        store.clean_build_cache()
    } else {
        store.clean_all()
    };
    match result {
        Ok(bytes) => {
            let scope = if build_only {
                "build caches"
            } else {
                "build caches and experiment history"
            };
            println!(
                "cleaned {scope} under {} (reclaimed {} bytes)",
                store.root().display(),
                bytes
            );
            0
        }
        Err(e) => {
            eprintln!("error: cleanup failed: {e}");
            2
        }
    }
}

pub fn run_format(input: &str, format: OutputFormat, out: Option<&str>) -> i32 {
    let text = match fs::read_to_string(input) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {input}: {e}");
            return 2;
        }
    };
    let rendered = match platform::format_from_report_json(&text, format) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Some(path) = out {
        if let Err(e) = fs::write(path, &rendered) {
            eprintln!("error: cannot write {path}: {e}");
            return 2;
        }
    }
    print!("{rendered}");
    0
}

fn execute(a: &Args) -> Result<i32, String> {
    let started = Instant::now();
    let ui = Ui::stderr();
    let cwd = env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    let registry = AdapterRegistry::builtin();
    let (lockfile, adapter) =
        registry.resolve(&cwd, a.lockfile.as_deref(), a.ecosystem.as_deref())?;
    let good_spec = a
        .good
        .as_deref()
        .ok_or_else(|| "--good is required".to_string())?;
    let (good_text, good_label) = read_side(&cwd, good_spec, &lockfile)?;
    let (bad_text, bad_label) = match a.bad.as_deref() {
        Some(spec) => read_side(&cwd, spec, &lockfile)?,
        None => {
            let t = fs::read_to_string(cwd.join(&lockfile))
                .map_err(|e| format!("cannot read {lockfile}: {e}"))?;
            (t, "working tree".to_string())
        }
    };

    let plan = adapter.plan(&lockfile, &good_text, &bad_text)?;
    let kind_name = plan.kind();
    let changes: Vec<Change> = plan.changes().to_vec();
    let hints: Vec<Option<String>> = (0..changes.len()).map(|i| plan.pin_hint(i)).collect();
    let warnings: Vec<String> = plan.warnings().to_vec();

    let mut rep = Report {
        kind: kind_name.to_string(),
        lockfile: lockfile.clone(),
        strategy: a.strategy.as_str().to_string(),
        good: good_label.clone(),
        bad: bad_label.clone(),
        changes: changes.clone(),
        hints,
        warnings,
        result: SearchResult::ListOnly,
        records: Vec::new(),
        cached_trials: Vec::new(),
        cache_hits: 0,
        secs: 0.0,
        kept: None,
        experiment_id: None,
    };

    if a.list {
        return finish(a, rep, started);
    }
    header(
        ui,
        kind_name,
        &lockfile,
        a.strategy.as_str(),
        &good_label,
        &bad_label,
        &changes,
    );
    for w in plan.warnings() {
        eprintln!("  warning: {w}");
    }
    if changes.is_empty() {
        rep.result = SearchResult::NoResult(crate::dd::DdError::NothingToBisect);
        return finish(a, rep, started);
    }

    let scratch =
        Scratch::new(a.keep).map_err(|e| format!("cannot create a scratch directory: {e}"))?;
    let io = |what: &str| {
        let what = what.to_string();
        move |e: std::io::Error| format!("{what}: {e}")
    };
    let work = scratch
        .dir("work")
        .map_err(io("cannot create the project template"))?;
    workspace::copy_project(&cwd, &work).map_err(io("cannot copy the project"))?;
    let project_fingerprint =
        workspace::fingerprint_tree(&work).map_err(io("cannot fingerprint the copied project"))?;

    let prepare_description = match a.prepare.as_deref() {
        None => plan.default_prepare().to_string(),
        Some("none") => "<prepare skipped>".to_string(),
        Some(script) => script.to_string(),
    };
    let mut oracle_material = format!(
        "engine={}\necosystem={}\nproject={}\ncwd={}\nos={}\narch={}\ntest={:?}\nprepare={}\nrepeat={}\ntimeout={:?}\nprepare-timeout=600s\n",
        VERSION,
        kind_name,
        project_fingerprint,
        cwd.canonicalize().unwrap_or_else(|_| cwd.clone()).display(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.test,
        prepare_description,
        a.repeat.max(1),
        a.timeout,
    );
    for key in [
        "PATH",
        "CARGO_HOME",
        "CARGO_TARGET_DIR",
        "RUSTUP_HOME",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "PYTHONPATH",
        "PIP_INDEX_URL",
        "UV_INDEX_URL",
        "NPM_CONFIG_REGISTRY",
        "NODE_OPTIONS",
        "CI",
    ] {
        oracle_material.push_str(key);
        oracle_material.push('=');
        oracle_material.push_str(&std::env::var(key).unwrap_or_default());
        oracle_material.push('\n');
    }
    let oracle_fingerprint = fingerprint::hash(oracle_material.as_bytes());

    let store = ExperimentStore::open(ExperimentStore::default_root())
        .map_err(io("cannot open the experiment store"))?;
    let shared = store
        .build_cache_dir(&format!(
            "{}:{}:{}",
            project_fingerprint, oracle_fingerprint, kind_name
        ))
        .map_err(io("cannot create the external build cache"))?;
    let settings = Settings {
        test: a.test.clone(),
        prepare: match a.prepare.as_deref() {
            None => Prepare::Default,
            Some("none") => Prepare::Skip,
            Some(script) => Prepare::Script(script.to_string()),
        },
        repeat: a.repeat,
        timeout: a.timeout,
        prepare_timeout: Some(Duration::from_secs(600)),
        verbose: a.verbose,
        keep_trials: a.keep,
        isolate_target: a.isolate_target,
    };

    let mut experiment = Experiment::new(
        "pending",
        format!("{kind_name} dependency regression: {good_label} -> {bad_label}"),
        kind_name,
        Snapshot::new(&good_label, fingerprint::hash(good_text.as_bytes())),
        Snapshot::new(&bad_label, fingerprint::hash(bad_text.as_bytes())),
        changes.clone(),
        oracle_fingerprint,
    )
    .with_budget(a.max_trials, a.max_culprits);
    experiment.id = format!(
        "exp-{}",
        fingerprint::hash(experiment.cache_namespace().as_bytes())
    );
    let logs = store
        .create_run_artifacts_dir(&experiment.id)
        .map_err(io("cannot create the persistent trial-log directory"))?;

    eprintln!();
    let mut trials = Trials::new(plan.as_ref(), work, shared, logs, settings).with_ui(ui);
    rep.experiment_id = Some(experiment.id.clone());
    let engine_run = Engine::new()
        .with_store(store)
        .with_resume(a.resume)
        .with_strategy(a.strategy)
        .run(&experiment, plan.closure(), &mut trials);
    eprintln!();

    match engine_run {
        Ok(diagnosis) => {
            let search = if let Some(error) = diagnosis.search_error {
                Err(error)
            } else {
                let culprits = diagnosis
                    .findings
                    .iter()
                    .map(|f| Culprit {
                        units: f.units.clone(),
                        exhausted: !f.one_minimal,
                    })
                    .collect();
                let verdict = match diagnosis.verification.status {
                    crate::diagnosis::VerificationStatus::Verified => Verdict::Verified,
                    crate::diagnosis::VerificationStatus::Unverified => Verdict::Unverified,
                    crate::diagnosis::VerificationStatus::Incomplete => Verdict::Incomplete,
                    crate::diagnosis::VerificationStatus::NotAttempted => Verdict::Unverified,
                };
                Ok(Findings { culprits, verdict })
            };
            rep.result = match (trials.fatal.take(), search) {
                (Some(msg), _) => SearchResult::Fatal(msg),
                (None, Ok(f)) => SearchResult::Found(f),
                (None, Err(e)) => SearchResult::NoResult(e),
            };
            rep.cache_hits = diagnosis.cache_hits;
            rep.cached_trials = diagnosis.trials;
        }
        Err(EngineError::OracleFatal(msg)) => {
            rep.result = SearchResult::Fatal(msg);
        }
        Err(EngineError::Storage(e)) => {
            return Err(format!("experiment storage failed: {e}"));
        }
    }
    rep.records = std::mem::take(&mut trials.records);
    if scratch.kept() {
        rep.kept = Some(scratch.root().to_path_buf());
    }
    finish(a, rep, started)
}

fn finish(a: &Args, mut rep: Report, started: Instant) -> Result<i32, String> {
    rep.secs = started.elapsed().as_secs_f64();
    let format = if a.json { OutputFormat::Json } else { a.format };
    let rendered = match format {
        OutputFormat::Text => report::text(&rep, Ui::stdout()),
        OutputFormat::Json => report::json(&rep),
        OutputFormat::Markdown => platform::markdown(&rep),
        OutputFormat::GithubComment => platform::github_comment(&rep),
        OutputFormat::Html => platform::html_report(&rep),
    };
    if let Some(out_path) = &a.out {
        fs::write(out_path, &rendered)
            .map_err(|e| format!("cannot write report to {out_path}: {e}"))?;
    }
    if a.github_summary {
        let _ = platform::write_github_summary(&rep);
        platform::emit_github_annotations(&rep);
    }
    print!("{rendered}");
    Ok(rep.exit_code())
}
