//! Running commands: the output goes to a log file (and optionally to the
//! terminal) and a timeout kills the whole process group, not just the
//! shell that started it.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Exited(i32),
    Signaled(i32),
    TimedOut,
    /// The program could not be started at all (not found, not executable).
    NotRunnable(String),
}

impl Status {
    pub fn describe(&self) -> String {
        match self {
            Status::Exited(c) => format!("exit {c}"),
            Status::Signaled(s) => format!("killed by signal {s}"),
            Status::TimedOut => "timed out".to_string(),
            Status::NotRunnable(e) => e.clone(),
        }
    }
}

pub struct Spec<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: &'a Path,
    pub env: &'a [(String, String)],
    pub timeout: Option<Duration>,
    /// Output is appended here.
    pub log: &'a Path,
    /// Also copy the output to our stderr while it arrives.
    pub echo: bool,
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: a plain libc call. A negative pid addresses the process group
    // that `process_group(0)` created for the child; if nothing is left in it
    // the call just fails with ESRCH.
    unsafe {
        kill(-(pid as i32), 9);
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

fn pump<R: Read + Send + 'static>(mut src: R, log: Arc<Mutex<File>>, echo: bool) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            let n = match src.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if let Ok(mut f) = log.lock() {
                let _ = f.write_all(&buf[..n]);
            }
            if echo {
                let _ = std::io::stderr().write_all(&buf[..n]);
            }
        }
    })
}

pub fn run(spec: &Spec) -> (Status, Duration) {
    let start = Instant::now();
    let log = match OpenOptions::new().create(true).append(true).open(spec.log) {
        Ok(f) => Arc::new(Mutex::new(f)),
        Err(e) => {
            return (
                Status::NotRunnable(format!("cannot open {}: {e}", spec.log.display())),
                start.elapsed(),
            );
        }
    };
    let mut cmd = Command::new(spec.program);
    cmd.args(spec.args)
        .current_dir(spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in spec.env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                Status::NotRunnable(format!("cannot run `{}`: {e}", spec.program)),
                start.elapsed(),
            );
        }
    };
    let pid = child.id();
    let out = child.stdout.take().expect("stdout is piped");
    let err = child.stderr.take().expect("stderr is piped");
    let readers = [
        pump(out, log.clone(), spec.echo),
        pump(err, log.clone(), spec.echo),
    ];

    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                break match st.code() {
                    Some(c) => Status::Exited(c),
                    None => {
                        #[cfg(unix)]
                        let sig = st.signal().unwrap_or(0);
                        #[cfg(not(unix))]
                        let sig = 0;
                        Status::Signaled(sig)
                    }
                };
            }
            Ok(None) => {
                if spec.timeout.is_some_and(|t| start.elapsed() >= t) {
                    kill_group(pid);
                    let _ = child.kill();
                    let _ = child.wait();
                    break Status::TimedOut;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                break Status::NotRunnable(format!("waiting for `{}` failed: {e}", spec.program))
            }
        }
    };
    // Whatever the command left running in the background must not keep our
    // pipes open (or keep running into the next trial).
    kill_group(pid);
    for r in readers {
        let _ = r.join();
    }
    (status, start.elapsed())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("whichdep-exec-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn sh(script: &str, timeout: Option<Duration>, log: &Path) -> (Status, Duration) {
        let args = ["-c".to_string(), script.to_string()];
        run(&Spec {
            program: "sh",
            args: &args,
            cwd: Path::new("."),
            env: &[],
            timeout,
            log,
            echo: false,
        })
    }

    #[test]
    fn exit_codes_and_both_streams_are_captured() {
        let log = tmp("codes");
        let (st, _) = sh("echo out; echo err >&2; exit 3", None, &log);
        assert_eq!(st, Status::Exited(3));
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("out") && text.contains("err"), "{text}");
        let _ = std::fs::remove_file(&log);
    }

    #[test]
    fn a_signal_is_reported() {
        let log = tmp("signal");
        let (st, _) = sh("kill -9 $$", None, &log);
        assert_eq!(st, Status::Signaled(9));
        let _ = std::fs::remove_file(&log);
    }

    #[test]
    fn a_timeout_kills_the_whole_group_quickly() {
        let log = tmp("timeout");
        let (st, took) = sh(
            "sleep 30 & sleep 30",
            Some(Duration::from_millis(300)),
            &log,
        );
        assert_eq!(st, Status::TimedOut);
        assert!(took < Duration::from_secs(5), "took {took:?}");
        let _ = std::fs::remove_file(&log);
    }

    #[test]
    fn background_processes_do_not_hang_us() {
        let log = tmp("background");
        let (st, took) = sh("sleep 30 & echo done", None, &log);
        assert_eq!(st, Status::Exited(0));
        assert!(took < Duration::from_secs(5), "took {took:?}");
        let _ = std::fs::remove_file(&log);
    }

    #[test]
    fn a_missing_program_is_not_a_test_failure() {
        let log = tmp("missing");
        let (st, _) = run(&Spec {
            program: "definitely-not-a-program-whichdep",
            args: &[],
            cwd: Path::new("."),
            env: &[],
            timeout: None,
            log: &log,
            echo: false,
        });
        assert!(matches!(st, Status::NotRunnable(_)), "{st:?}");
        let _ = std::fs::remove_file(&log);
    }

    #[test]
    fn the_environment_reaches_the_child() {
        let log = tmp("env");
        let args = ["-c".to_string(), "echo $WHICHDEP_X".to_string()];
        let env = [("WHICHDEP_X".to_string(), "hello".to_string())];
        let (st, _) = run(&Spec {
            program: "sh",
            args: &args,
            cwd: Path::new("."),
            env: &env,
            timeout: None,
            log: &log,
            echo: false,
        });
        assert_eq!(st, Status::Exited(0));
        assert!(std::fs::read_to_string(&log).unwrap().contains("hello"));
        let _ = std::fs::remove_file(&log);
    }
}
