//! Command line parsing (by hand: the crate has no dependencies).

use crate::platform::OutputFormat;
use crate::strategy::StrategyKind;
use std::time::Duration;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const HELP: &str = "\
whichdep - find which dependency update broke your tests

Give it a lockfile that works and one that does not. It applies subsets of the
differences to a copy of your project and runs your test command until it has
narrowed the failure down to the smallest set of package updates that still
breaks it (delta debugging, like `git bisect` but over packages instead of
commits). Cargo.lock, uv.lock, pinned requirements.txt, package-lock.json and
pnpm-lock.yaml are supported.

USAGE:
    whichdep --good <REV|FILE> [OPTIONS] -- <TEST COMMAND>...
    whichdep --good <REV|FILE> --list
    whichdep adapters [--json]
    whichdep history [EXPERIMENT_ID] [--json | --html] [--out <PATH>]
    whichdep clean [--build-only]
    whichdep format --input <REPORT.json> [--format <FMT>] [--out <PATH>]

EXAMPLES:
    whichdep --good v1.4.0 -- cargo test
    whichdep --good origin/main --strategy adaptive -- uv run pytest -x
    whichdep --good origin/main --format github-comment --out comment.md -- npm test
    whichdep --good HEAD~5 --repeat 3 --timeout 600 -- cargo test --release

OPTIONS:
    --good <REV|FILE>     The lockfile that is known to work: a git revision (the
                          lockfile is read from it) or a path to a file.
    --bad <REV|FILE>      The lockfile that fails.
                          [default: the one in the working tree]
    --lockfile <PATH>     The lockfile to bisect, relative to the current directory.
                          [default: auto-detected from supported lockfiles]
    --ecosystem <NAME>    cargo | uv | pip | npm | pnpm
                          [default: guessed from the lockfile name]
    --strategy <NAME>     ddmin | bisect | adaptive | one-by-one   [default: ddmin]
    --repeat <N>          Run the test command N times per trial. If the runs
                          disagree the trial counts as flaky (unresolved).
                          [default: 1]
    --timeout <SECONDS>   Kill a test run after this long. A timeout counts as a
                          failure, so hangs can be bisected too.
    --max-trials <N>      Give up after N trials.
    --max-culprits <N>    Stop after N independent culprits.   [default: 5]
    --prepare <SCRIPT>    Replace the built-in environment setup with a script
                          (run with `sh -c` in the project copy). `none` skips it.
    --list                Only show what changed between good and bad.
    --json                Print the result as JSON on stdout (`--format json`).
    --format <FMT>        text | json | markdown | github-comment | html
                          [default: text]
    --out <PATH>          Write the formatted report to a file as well as stdout.
    --github-summary      Append Markdown summary to $GITHUB_STEP_SUMMARY and
                          emit GitHub Actions workflow annotations.
    --keep                Keep the scratch directory (project and trial workspaces).
    --isolate-target      Use a separate build target directory per rendered lockfile
                          instead of sharing one cache per experiment.
    --no-resume           Re-run trials instead of reusing persisted results.
    -v, --verbose         Show the output of the commands while they run.
    -h, --help            Print this help.
    -V, --version         Print the version.

THE TEST COMMAND runs in a copy of the current directory, with the lockfile of
the trial in place. Its exit status decides: 0 = pass, 125 = cannot be tested
(like `git bisect run`), anything else = fail. The variables WHICHDEP=1,
WHICHDEP_TRIAL and WHICHDEP_LOCKFILE are set for it.

EXIT STATUS:
    0  culprit found (or --list / management command succeeded)
    1  no result: the good lockfile fails, the bad one passes, a trial budget ran out, ...
    2  usage or environment error
";

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub good: Option<String>,
    pub bad: Option<String>,
    pub lockfile: Option<String>,
    pub ecosystem: Option<String>,
    pub strategy: StrategyKind,
    pub repeat: usize,
    pub timeout: Option<Duration>,
    pub max_trials: Option<usize>,
    pub max_culprits: usize,
    pub prepare: Option<String>,
    pub list: bool,
    pub json: bool,
    pub format: OutputFormat,
    pub out: Option<String>,
    pub github_summary: bool,
    pub keep: bool,
    pub isolate_target: bool,
    pub resume: bool,
    pub verbose: bool,
    pub test: Vec<String>,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            good: None,
            bad: None,
            lockfile: None,
            ecosystem: None,
            strategy: StrategyKind::Ddmin,
            repeat: 1,
            timeout: None,
            max_trials: None,
            max_culprits: 5,
            prepare: None,
            list: false,
            json: false,
            format: OutputFormat::Text,
            out: None,
            github_summary: false,
            keep: false,
            isolate_target: false,
            resume: true,
            verbose: false,
            test: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Run(Args),
    Adapters {
        json: bool,
    },
    History {
        experiment_id: Option<String>,
        format: OutputFormat,
        out: Option<String>,
    },
    Clean {
        build_only: bool,
    },
    Format {
        input: String,
        format: OutputFormat,
        out: Option<String>,
    },
    Help,
    Version,
}

fn positive(flag: &str, v: &str) -> Result<usize, String> {
    match v.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "{flag} needs a whole number of at least 1, got `{v}`"
        )),
    }
}

fn next_flag_value<I: Iterator<Item = String>>(
    flag: &str,
    inline: Option<String>,
    it: &mut I,
) -> Result<String, String> {
    match inline {
        Some(v) => Ok(v),
        None => it.next().ok_or_else(|| format!("{flag} needs a value")),
    }
}

fn parse_adapters<I: Iterator<Item = String>>(it: I) -> Result<Command, String> {
    let mut json = false;
    for arg in it {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--json" => json = true,
            other => return Err(format!("unknown option `{other}` for `whichdep adapters`")),
        }
    }
    Ok(Command::Adapters { json })
}

fn parse_history<I: Iterator<Item = String>>(mut it: I) -> Result<Command, String> {
    let mut experiment_id = None;
    let mut format = OutputFormat::Text;
    let mut out = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--json" => format = OutputFormat::Json,
            "--html" => format = OutputFormat::Html,
            _ if arg.starts_with("--") => {
                let (flag, inline) = match arg.split_once('=') {
                    Some((f, v)) => (f.to_string(), Some(v.to_string())),
                    None => (arg.clone(), None),
                };
                match flag.as_str() {
                    "--format" => {
                        let v = next_flag_value(&flag, inline, &mut it)?;
                        format = OutputFormat::parse(&v)?;
                    }
                    "--out" => {
                        out = Some(next_flag_value(&flag, inline, &mut it)?);
                    }
                    _ => return Err(format!("unknown option `{flag}` for `whichdep history`")),
                }
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown option `{arg}` for `whichdep history`"));
            }
            id => {
                if experiment_id.is_some() {
                    return Err(format!(
                        "unexpected extra argument `{id}` for `whichdep history`"
                    ));
                }
                experiment_id = Some(id.to_string());
            }
        }
    }
    Ok(Command::History {
        experiment_id,
        format,
        out,
    })
}

fn parse_clean<I: Iterator<Item = String>>(it: I) -> Result<Command, String> {
    let mut build_only = false;
    for arg in it {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--build-only" => build_only = true,
            "--all" => build_only = false,
            other => return Err(format!("unknown option `{other}` for `whichdep clean`")),
        }
    }
    Ok(Command::Clean { build_only })
}

fn parse_format<I: Iterator<Item = String>>(mut it: I) -> Result<Command, String> {
    let mut input: Option<String> = None;
    let mut format = OutputFormat::GithubComment;
    let mut out = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            _ if arg.starts_with("--") => {
                let (flag, inline) = match arg.split_once('=') {
                    Some((f, v)) => (f.to_string(), Some(v.to_string())),
                    None => (arg.clone(), None),
                };
                match flag.as_str() {
                    "--input" => input = Some(next_flag_value(&flag, inline, &mut it)?),
                    "--format" => {
                        let v = next_flag_value(&flag, inline, &mut it)?;
                        format = OutputFormat::parse(&v)?;
                    }
                    "--out" => out = Some(next_flag_value(&flag, inline, &mut it)?),
                    _ => return Err(format!("unknown option `{flag}` for `whichdep format`")),
                }
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown option `{arg}` for `whichdep format`"));
            }
            positional => {
                if input.is_some() {
                    return Err(format!(
                        "unexpected extra argument `{positional}` for `whichdep format`"
                    ));
                }
                input = Some(positional.to_string());
            }
        }
    }
    let input =
        input.ok_or_else(|| "`whichdep format` requires --input <REPORT.json>".to_string())?;
    Ok(Command::Format { input, format, out })
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
    let mut it = args.into_iter().peekable();
    if let Some(first) = it.peek().map(String::as_str) {
        match first {
            "adapters" => {
                it.next();
                return parse_adapters(it);
            }
            "history" => {
                it.next();
                return parse_history(it);
            }
            "clean" => {
                it.next();
                return parse_clean(it);
            }
            "format" => {
                it.next();
                return parse_format(it);
            }
            _ => {}
        }
    }

    let mut a = Args::default();
    while let Some(arg) = it.next() {
        if arg == "--" {
            a.test = it.by_ref().collect();
            break;
        }
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--list" => a.list = true,
            "--json" => {
                a.json = true;
                a.format = OutputFormat::Json;
            }
            "--github-summary" => a.github_summary = true,
            "--keep" => a.keep = true,
            "--isolate-target" => a.isolate_target = true,
            "--no-resume" => a.resume = false,
            "-v" | "--verbose" => a.verbose = true,
            _ if arg.starts_with("--") => {
                let (flag, inline) = match arg.split_once('=') {
                    Some((f, v)) => (f.to_string(), Some(v.to_string())),
                    None => (arg.clone(), None),
                };
                let known = [
                    "--good",
                    "--bad",
                    "--lockfile",
                    "--ecosystem",
                    "--strategy",
                    "--format",
                    "--out",
                    "--repeat",
                    "--timeout",
                    "--max-trials",
                    "--max-culprits",
                    "--prepare",
                ];
                if !known.contains(&flag.as_str()) {
                    return Err(format!("unknown option `{flag}`"));
                }
                let value = next_flag_value(&flag, inline, &mut it)?;
                match flag.as_str() {
                    "--good" => a.good = Some(value),
                    "--bad" => a.bad = Some(value),
                    "--lockfile" => a.lockfile = Some(value),
                    "--ecosystem" => a.ecosystem = Some(value),
                    "--strategy" => a.strategy = StrategyKind::parse(&value)?,
                    "--format" => {
                        a.format = OutputFormat::parse(&value)?;
                        a.json = a.format == OutputFormat::Json;
                    }
                    "--out" => a.out = Some(value),
                    "--prepare" => a.prepare = Some(value),
                    "--repeat" => a.repeat = positive(&flag, &value)?,
                    "--max-trials" => a.max_trials = Some(positive(&flag, &value)?),
                    "--max-culprits" => a.max_culprits = positive(&flag, &value)?,
                    "--timeout" => match value.parse::<f64>() {
                        Ok(s) if s.is_finite() && s > 0.0 => a.timeout = Some(Duration::from_secs_f64(s)),
                        _ => return Err(format!("--timeout needs a number of seconds above 0, got `{value}`")),
                    },
                    _ => unreachable!("checked against the list above"),
                }
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option `{arg}`")),
            _ => {
                return Err(format!(
                    "unexpected argument `{arg}`; the test command goes after `--`, for example: whichdep --good v1.0 -- cargo test"
                ))
            }
        }
    }
    if a.good.is_none() {
        return Err(
            "--good is required: the git revision or file with the lockfile that works".to_string(),
        );
    }
    if a.test.is_empty() && !a.list {
        return Err(
            "no test command: put it after `--`, for example: whichdep --good v1.0 -- cargo test"
                .to_string(),
        );
    }
    Ok(Command::Run(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    fn run(args: &[&str]) -> Args {
        match p(args).unwrap() {
            Command::Run(a) => a,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_test_command_is_everything_after_the_double_dash() {
        let a = run(&["--good", "v1", "--", "cargo", "test", "--release", "--good"]);
        assert_eq!(a.good.as_deref(), Some("v1"));
        assert_eq!(a.test, vec!["cargo", "test", "--release", "--good"]);
    }

    #[test]
    fn values_can_be_attached_with_an_equals_sign() {
        let a = run(&[
            "--good=v1",
            "--strategy=adaptive",
            "--format=github-comment",
            "--repeat=3",
            "--timeout=2.5",
            "--max-trials",
            "40",
            "--",
            "true",
        ]);
        assert_eq!(a.strategy, StrategyKind::Adaptive);
        assert_eq!(a.format, OutputFormat::GithubComment);
        assert_eq!(a.repeat, 3);
        assert_eq!(a.timeout, Some(Duration::from_millis(2500)));
        assert_eq!(a.max_trials, Some(40));
    }

    #[test]
    fn flags_and_defaults() {
        let a = run(&[
            "--good",
            "v1",
            "--json",
            "--keep",
            "-v",
            "--github-summary",
            "--",
            "true",
        ]);
        assert!(a.json && a.keep && a.verbose && a.github_summary && !a.list && a.resume);
        assert_eq!(a.format, OutputFormat::Json);
        assert_eq!(a.strategy, StrategyKind::Ddmin);
        assert_eq!((a.repeat, a.max_culprits), (1, 5));
        assert!(!run(&["--good", "v1", "--no-resume", "--", "true"]).resume);
    }

    #[test]
    fn parses_platform_subcommands() {
        assert_eq!(
            p(&["adapters", "--json"]).unwrap(),
            Command::Adapters { json: true }
        );
        assert_eq!(
            p(&["history", "--html", "--out", "dash.html"]).unwrap(),
            Command::History {
                experiment_id: None,
                format: OutputFormat::Html,
                out: Some("dash.html".into()),
            }
        );
        assert_eq!(
            p(&["clean", "--build-only"]).unwrap(),
            Command::Clean { build_only: true }
        );
        assert_eq!(
            p(&["format", "--input", "rep.json", "--format", "markdown"]).unwrap(),
            Command::Format {
                input: "rep.json".into(),
                format: OutputFormat::Markdown,
                out: None,
            }
        );
    }

    #[test]
    fn list_does_not_need_a_test_command() {
        assert!(run(&["--good", "v1", "--list"]).list);
    }

    #[test]
    fn mistakes_get_helpful_messages() {
        assert!(p(&["--", "true"])
            .unwrap_err()
            .contains("--good is required"));
        assert!(p(&["--good", "v1"])
            .unwrap_err()
            .contains("no test command"));
        assert!(p(&["--good", "v1", "cargo", "test"])
            .unwrap_err()
            .contains("after `--`"));
        assert!(p(&["--good", "v1", "--nope", "--", "x"])
            .unwrap_err()
            .contains("unknown option `--nope`"));
        assert!(p(&["--good"]).unwrap_err().contains("needs a value"));
        assert!(p(&["--good", "v1", "--repeat", "0", "--", "x"])
            .unwrap_err()
            .contains("at least 1"));
        assert!(p(&["--good", "v1", "--timeout", "abc", "--", "x"])
            .unwrap_err()
            .contains("--timeout"));
        assert!(p(&["--good", "v1", "--strategy", "magic", "--", "x"])
            .unwrap_err()
            .contains("unknown search strategy"));
    }

    #[test]
    fn help_and_version_win() {
        assert_eq!(p(&["--help"]).unwrap(), Command::Help);
        assert_eq!(p(&["--good", "v1", "-V"]).unwrap(), Command::Version);
    }
}
