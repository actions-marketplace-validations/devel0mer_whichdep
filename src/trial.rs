//! The oracle that delta debugging asks: "does this configuration of the
//! lockfile make the test command fail?". Answering it means writing the
//! lockfile, building the environment, checking that the resolver kept it as
//! written and running the test command.

use crate::dd::{Oracle, Outcome, Set};
use crate::exec::{self, Spec, Status};
use crate::fingerprint;
use crate::model::Ecosystem;
use crate::store::StoreLock;
use crate::ui::{self, Ui};
use crate::workspace;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How the environment for a trial is built from the written lockfile.
#[derive(Clone, Debug)]
pub enum Prepare {
    /// The ecosystem's own script.
    Default,
    /// The user's script, run with `sh -c`.
    Script(String),
    /// Nothing: the test command does it itself.
    Skip,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub test: Vec<String>,
    pub prepare: Prepare,
    /// The test command runs this many times per trial; unless every run
    /// agrees the trial is unresolved (flaky).
    pub repeat: usize,
    pub timeout: Option<Duration>,
    /// Separate upper bound for environment preparation (fetch/install/setup).
    pub prepare_timeout: Option<Duration>,
    /// Show the output of the commands while they run.
    pub verbose: bool,
    /// Keep each trial's workspace directory instead of resetting/deleting it
    /// after execution.
    pub keep_trials: bool,
    /// Isolate build target directories per rendered lockfile instead of
    /// sharing one build cache per experiment.
    pub isolate_target: bool,
}

/// What happened in one trial.
#[derive(Clone, Debug)]
pub struct Record {
    pub no: usize,
    pub config: Vec<usize>,
    pub outcome: Outcome,
    pub reason: String,
    pub secs: f64,
    pub log: Option<PathBuf>,
    /// Another trial wrote exactly the same lockfile, so nothing ran.
    pub reused_from: Option<usize>,
}

/// The line of a command's output that says best why it failed: the first
/// `error...` line, otherwise the last line (`pip check` and friends do not
/// say "error").
fn shell_command(script: &str) -> (&'static str, Vec<String>) {
    #[cfg(windows)]
    {
        ("cmd", vec!["/C".to_string(), script.to_string()])
    }
    #[cfg(not(windows))]
    {
        ("sh", vec!["-c".to_string(), script.to_string()])
    }
}

fn failure_hint(log: &Path) -> Option<String> {
    let text = fs::read_to_string(log).ok()?;
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("# "))
        .collect();
    let pick = lines
        .iter()
        .find(|l| l.to_ascii_lowercase().starts_with("error"))
        .or_else(|| lines.last())?;
    let mut hint: String = pick.chars().take(110).collect();
    if pick.chars().count() > 110 {
        hint.push('…');
    }
    Some(hint)
}

pub struct Trials<'a> {
    eco: &'a dyn Ecosystem,
    workdir: PathBuf,
    shared: PathBuf,
    logs: PathBuf,
    settings: Settings,
    ui: Ui,
    by_content: HashMap<String, usize>,
    pub records: Vec<Record>,
    /// Something that makes every further trial pointless, such as a test
    /// command that does not exist.
    pub fatal: Option<String>,
}

impl<'a> Trials<'a> {
    pub fn new(
        eco: &'a dyn Ecosystem,
        workdir: PathBuf,
        shared: PathBuf,
        logs: PathBuf,
        settings: Settings,
    ) -> Self {
        Trials {
            eco,
            workdir,
            shared,
            logs,
            settings,
            ui: Ui::stderr(),
            by_content: HashMap::new(),
            records: Vec::new(),
            fatal: None,
        }
    }

    pub fn with_ui(mut self, ui: Ui) -> Self {
        self.ui = ui;
        self
    }

    fn env(&self, no: usize, trial_dir: &Path, build_cache: &Path) -> Vec<(String, String)> {
        let mut env = self.eco.env(trial_dir, build_cache);
        env.push(("WHICHDEP".into(), "1".into()));
        env.push(("WHICHDEP_TRIAL".into(), no.to_string()));
        env.push(("WHICHDEP_LOCKFILE".into(), self.eco.lockfile().to_string()));
        env
    }

    fn label(&self, config: &Set) -> String {
        let total = self.eco.changes().len();
        if config.is_empty() {
            "good lockfile".to_string()
        } else if config.len() == total {
            "bad lockfile".to_string()
        } else if config.len() <= 3 {
            config
                .iter()
                .map(|&i| self.eco.changes()[i].name.clone())
                .collect::<Vec<_>>()
                .join(" + ")
        } else {
            format!("{} of {} updates", config.len(), total)
        }
    }

    fn describe(&self, config: &Set) -> String {
        if config.is_empty() {
            return "nothing applied (the good lockfile)".to_string();
        }
        config
            .iter()
            .map(|&i| self.eco.changes()[i].describe())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn show(&self, rec: &Record) {
        let outcome = match rec.outcome {
            Outcome::Pass => self.ui.green("pass      "),
            Outcome::Fail => self.ui.red("fail      "),
            Outcome::Unresolved => self.ui.yellow("unresolved"),
        };
        let set: Set = rec.config.iter().copied().collect();
        let time = if rec.reused_from.is_some() {
            String::new()
        } else {
            format!("  {}", ui::duration(rec.secs))
        };
        eprintln!(
            "  {} {outcome}  {:<34} {}{}",
            self.ui.dim(&format!("#{:<3}", rec.no)),
            self.label(&set),
            self.ui.dim(&rec.reason),
            self.ui.dim(&time)
        );
    }

    fn log_line(&self, log: &Path, line: &str) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(f, "{line}");
        }
    }

    fn fatal_err(&mut self, msg: String) -> (Outcome, String) {
        self.fatal = Some(msg.clone());
        (Outcome::Unresolved, msg)
    }

    fn execute(
        &mut self,
        no: usize,
        config: &Set,
        text: &str,
        log: &Path,
        workdir: &Path,
    ) -> (Outcome, String) {
        let lock_path = workdir.join(self.eco.lockfile());
        if let Some(parent) = lock_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::write(&lock_path, text) {
            return self.fatal_err(format!("cannot write {}: {e}", lock_path.display()));
        }
        self.log_line(log, &format!("# trial {no}: {}", self.describe(config)));
        // By default, share one external build cache per experiment so Cargo
        // (and package-manager stores) reuse unchanged dependency artifacts
        // across trials rather than doing a cold build every time. Callers can
        // opt into per-lockfile target isolation when needed.
        let build_cache = if self.settings.isolate_target {
            self.shared
                .join("targets")
                .join(fingerprint::hash(text.as_bytes()))
        } else {
            self.shared.join("shared-cache")
        };
        if let Err(e) = fs::create_dir_all(&build_cache) {
            return self.fatal_err(format!(
                "cannot create build cache {}: {e}",
                build_cache.display()
            ));
        }
        let _cache_lock = match StoreLock::acquire(&build_cache.join(".lock")) {
            Ok(guard) => guard,
            Err(e) => {
                return self.fatal_err(format!(
                    "cannot acquire build cache lock {}: {e}",
                    build_cache.display()
                ));
            }
        };
        let env = self.env(no, workdir, &build_cache);
        let echo = self.settings.verbose;

        // 1. Build the environment and check that the resolver left the
        //    lockfile alone.
        let script = match &self.settings.prepare {
            Prepare::Default => Some(self.eco.default_prepare().to_string()),
            Prepare::Script(s) => Some(s.clone()),
            Prepare::Skip => None,
        };
        if let Some(script) = script {
            self.log_line(log, "# prepare");
            let (program, args) = shell_command(&script);
            let (st, _) = exec::run(&Spec {
                program,
                args: &args,
                cwd: workdir,
                env: &env,
                timeout: self.settings.prepare_timeout,
                log,
                echo,
            });
            match st {
                Status::Exited(0) => {}
                Status::NotRunnable(e) => return self.fatal_err(e),
                other => {
                    if let Ok(log_text) = fs::read_to_string(log) {
                        self.eco.learn_from_prepare_failure(config, &log_text);
                    }
                    let why = match failure_hint(log) {
                        Some(h) => format!("environment setup failed: {h}"),
                        None => format!("environment setup failed ({})", other.describe()),
                    };
                    return (Outcome::Unresolved, why);
                }
            }
            match fs::read_to_string(&lock_path) {
                Ok(after) => {
                    if let Err(why) = self.eco.verify(config, &after) {
                        self.log_line(log, &format!("# {why}"));
                        return (Outcome::Unresolved, why);
                    }
                }
                Err(e) => {
                    return (
                        Outcome::Unresolved,
                        format!("cannot read the lockfile back: {e}"),
                    )
                }
            }
        }

        // 2. Run the test command.
        let repeat = self.settings.repeat.max(1);
        let test = self.settings.test.clone();
        let mut first: Option<(Outcome, String)> = None;
        for i in 0..repeat {
            self.log_line(
                log,
                &format!("# test run {}/{repeat}: {}", i + 1, test.join(" ")),
            );
            let (st, _) = exec::run(&Spec {
                program: &test[0],
                args: &test[1..],
                cwd: workdir,
                env: &env,
                timeout: self.settings.timeout,
                log,
                echo,
            });
            let (o, why) = match st {
                Status::NotRunnable(e) => return self.fatal_err(e),
                Status::Exited(0) => (Outcome::Pass, "exit 0".to_string()),
                Status::Exited(125) => {
                    return (
                        Outcome::Unresolved,
                        "exit 125: the test command skipped this configuration".to_string(),
                    );
                }
                Status::Exited(c) => (Outcome::Fail, format!("exit {c}")),
                Status::Signaled(s) => (Outcome::Fail, format!("killed by signal {s}")),
                Status::TimedOut => (
                    Outcome::Fail,
                    format!(
                        "timed out after {}",
                        ui::duration(self.settings.timeout.map_or(0.0, |t| t.as_secs_f64()))
                    ),
                ),
            };
            match &first {
                None => first = Some((o, why)),
                Some((fo, fwhy)) if *fo != o => {
                    return (
                        Outcome::Unresolved,
                        format!(
                            "flaky: run 1 {} ({fwhy}), run {} {} ({why})",
                            fo.as_str(),
                            i + 1,
                            o.as_str()
                        ),
                    );
                }
                Some(_) => {}
            }
        }
        let (o, why) = first.expect("at least one run");
        let why = if repeat > 1 {
            format!("{why} (x{repeat})")
        } else {
            why
        };
        (o, why)
    }
}

impl Oracle for Trials<'_> {
    fn test(&mut self, config: &Set) -> Outcome {
        if self.fatal.is_some() {
            return Outcome::Unresolved;
        }
        let no = self.records.len() + 1;
        let text = self.eco.render(config);
        let key = text.clone();
        let cfg: Vec<usize> = config.iter().copied().collect();

        if let Some(&ri) = self.by_content.get(&key) {
            let prev = self.records[ri].clone();
            let rec = Record {
                no,
                config: cfg,
                outcome: prev.outcome,
                reason: format!("same lockfile as #{}", prev.no),
                secs: 0.0,
                log: prev.log.clone(),
                reused_from: Some(prev.no),
            };
            self.show(&rec);
            self.records.push(rec);
            return prev.outcome;
        }

        let started = Instant::now();
        let log = self.logs.join(format!("trial-{no:03}.log"));
        let trial_dir = if self.settings.keep_trials {
            self.workdir
                .parent()
                .unwrap_or(&self.workdir)
                .join("trials")
                .join(format!("trial-{no:03}"))
        } else {
            // Reusing a single freshly recreated workspace path keeps Cargo
            // path fingerprints stable across trials and prevents temp disk
            // usage from multiplying with the number of trials.
            self.workdir.parent().unwrap_or(&self.workdir).join("trial")
        };
        if trial_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&trial_dir) {
                return self
                    .fatal_err(format!(
                        "cannot reset isolated trial workspace {}: {e}",
                        trial_dir.display()
                    ))
                    .0;
            }
        }
        if let Err(e) = workspace::copy_snapshot(&self.workdir, &trial_dir) {
            return self
                .fatal_err(format!(
                    "cannot create isolated trial workspace {}: {e}",
                    trial_dir.display()
                ))
                .0;
        }
        let (outcome, reason) = self.execute(no, config, &text, &log, &trial_dir);
        if !self.settings.keep_trials {
            let _ = fs::remove_dir_all(&trial_dir);
        }
        let rec = Record {
            no,
            config: cfg,
            outcome,
            reason,
            secs: started.elapsed().as_secs_f64(),
            log: Some(log),
            reused_from: None,
        };
        self.show(&rec);
        self.by_content.insert(key, self.records.len());
        self.records.push(rec);
        outcome
    }

    fn fatal_error(&self) -> Option<&str> {
        self.fatal.as_deref()
    }
}
