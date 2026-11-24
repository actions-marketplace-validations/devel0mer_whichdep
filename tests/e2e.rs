//! End to end: the real binary against throw-away projects.
//!
//! The requirements tests need nothing but `sh` and `git`. The uv and cargo
//! tests build tiny local package indexes (a wheelhouse, a vendored registry)
//! so the real tools run without network access; they are skipped when the
//! tool is not installed.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A temporary directory that disappears with the test, holding both the
/// project worktree and an isolated `WHICHDEP_HOME` so tests never pollute the
/// developer's real cache.
struct Dir {
    root: PathBuf,
    project: PathBuf,
}

impl Dir {
    fn new(name: &str) -> Dir {
        let root = std::env::temp_dir().join(format!("whichdep-e2e-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let project = root.join("project");
        let home = root.join("home");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&home).unwrap();
        Dir { root, project }
    }
    fn path(&self) -> &Path {
        &self.project
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

fn whichdep(dir: &Path, args: &[&str]) -> Run {
    let home = dir.parent().unwrap_or(dir).join("home");
    let o = Command::new(env!("CARGO_BIN_EXE_whichdep"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("WHICHDEP_HOME", home)
        .output()
        .expect("the whichdep binary runs");
    Run {
        code: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A git repository whose HEAD holds the `good` lockfile and whose working
/// tree holds the `bad` one. `files` are extra project files (committed).
fn repo(dir: &Dir, lockfile: &str, good: &str, bad: &str, files: &[(&str, &str)]) {
    let p = dir.path();
    git(p, &["init", "-q", "."]);
    git(p, &["config", "user.email", "t@example.com"]);
    git(p, &["config", "user.name", "t"]);
    fs::write(p.join(lockfile), good).unwrap();
    for (name, content) in files {
        let f = p.join(name);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, content).unwrap();
    }
    git(p, &["add", "-A", "."]);
    git(p, &["commit", "-qm", "good"]);
    fs::write(p.join(lockfile), bad).unwrap();
}

/// The "test suite": a shell script that reads the lockfile of the trial.
fn script(body: &str) -> [(&'static str, String); 1] {
    [(
        "check.sh",
        format!("has() {{ grep -q \"^$1==$2\\$\" requirements.txt; }}\n{body}\n"),
    )]
}

fn with_script(dir: &Dir, good: &str, bad: &str, body: &str) {
    let [(name, text)] = script(body);
    repo(dir, "requirements.txt", good, bad, &[(name, &text)]);
}

const GOOD8: &str = "alpha==1.0.0\nbadlib==1.0.0\nbeta==2.1.0\ndelta==0.9.1\nepsilon==3.4.5\ngamma==1.2.0\nkappa==0.3.0\nlambda==4.0.0\n";
const BAD8: &str = "alpha==1.0.2\nbadlib==1.1.0\nbeta==2.2.0\ndelta==0.9.3\nepsilon==3.5.0\ngamma==1.2.1\nkappa==0.3.1\nlambda==4.1.0\nnewcomer==0.1.0\n";

#[test]
fn finds_a_single_cause() {
    let d = Dir::new("single");
    with_script(
        &d,
        GOOD8,
        BAD8,
        "if has badlib 1.1.0; then echo 'badlib 1.1.0 breaks the API'; exit 1; fi",
    );
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(
        r.out.contains("Root cause\n  badlib  1.0.0 → 1.1.0\n"),
        "{}",
        r.out
    );
    assert!(r.out.contains("Verified:"), "{}", r.out);
    assert!(
        r.out.contains("badlib==1.0.0"),
        "the pin hint is missing:\n{}",
        r.out
    );
    assert!(r.err.contains("9 changes"), "{}", r.err);
}

#[test]
fn finds_a_pair_that_only_breaks_together() {
    let d = Dir::new("pair");
    with_script(
        &d,
        "bar==1.0\nbaz==1.0\nfoo==1.0\nqux==1.0\n",
        "bar==2.0\nbaz==2.0\nfoo==2.0\nqux==2.0\n",
        "if has foo 2.0 && has bar 2.0; then exit 1; fi",
    );
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(
        r.out.contains("these updates break the tests together"),
        "{}",
        r.out
    );
    assert!(
        r.out.contains("  bar  1.0 → 2.0\n") && r.out.contains("  foo  1.0 → 2.0\n"),
        "{}",
        r.out
    );
    assert!(
        !r.out.contains("  baz  1.0 → 2.0\n") && !r.out.contains("  qux  1.0 → 2.0\n"),
        "{}",
        r.out
    );
}

#[test]
fn finds_independent_causes() {
    let d = Dir::new("independent");
    with_script(
        &d,
        "a==1\nb==1\nc==1\nd==1\ne==1\nf==1\n",
        "a==2\nb==2\nc==2\nd==2\ne==2\nf==2\n",
        "if has b 2 || has e 2; then exit 1; fi",
    );
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(
        r.out.contains("Root cause 1 of 2") && r.out.contains("Root cause 2 of 2"),
        "{}",
        r.out
    );
    assert!(
        r.out.contains("  b  1 → 2\n") && r.out.contains("  e  1 → 2\n"),
        "{}",
        r.out
    );
    assert!(r.out.contains("Verified:"), "{}", r.out);
}

#[test]
fn configurations_that_cannot_be_tested_are_skipped_not_blamed() {
    let d = Dir::new("unresolved");
    // foo and bar only work as a pair (exit 125 = cannot be tested), baz is the cause.
    with_script(
        &d,
        "foo==1\nbaz==1\nqux==1\nbar==1\n",
        "foo==2\nbaz==2\nqux==2\nbar==2\n",
        "f=0; b=0; has foo 2 && f=1; has bar 2 && b=1\n[ $f != $b ] && exit 125\nif has baz 2; then exit 1; fi",
    );
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(r.out.contains("Root cause\n  baz  1 → 2\n"), "{}", r.out);
    assert!(
        !r.out.contains(" 0 unresolved"),
        "the search was meant to hit untestable states:\n{}",
        r.out
    );
}

#[test]
fn a_failing_good_lockfile_is_reported_with_its_output() {
    let d = Dir::new("goodfails");
    with_script(&d, GOOD8, BAD8, "echo 'the harness is broken'; exit 1");
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 1, "{}\n{}", r.out, r.err);
    assert!(
        r.out.contains("The good lockfile already fails"),
        "{}",
        r.out
    );
    assert!(r.out.contains("the harness is broken"), "{}", r.out);
}

#[test]
fn a_passing_bad_lockfile_is_reported() {
    let d = Dir::new("badpasses");
    with_script(&d, GOOD8, BAD8, "exit 0");
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 1, "{}\n{}", r.out, r.err);
    assert!(r.out.contains("The bad lockfile passes"), "{}", r.out);
}

#[test]
fn json_output_is_machine_readable() {
    let d = Dir::new("json");
    with_script(&d, GOOD8, BAD8, "if has badlib 1.1.0; then exit 1; fi");
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--json",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(
        r.out.starts_with("{\n") && r.out.ends_with("}\n"),
        "stdout must be only JSON:\n{}",
        r.out
    );
    for needle in [
        "\"status\": \"found\"",
        "\"verdict\": \"verified\"",
        "\"ecosystem\": \"pip\"",
        "\"package\": \"badlib\"",
        "\"pin\": \"badlib==1.0.0\"",
        "\"outcome\": \"fail\"",
    ] {
        assert!(r.out.contains(needle), "missing {needle}:\n{}", r.out);
    }
}

#[test]
fn list_shows_the_changes_and_runs_nothing() {
    let d = Dir::new("list");
    with_script(&d, GOOD8, BAD8, "exit 1");
    let r = whichdep(d.path(), &["--good", "HEAD", "--list"]);
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(
        r.out
            .contains("9 changes between good and bad (8 updated, 1 new)"),
        "{}",
        r.out
    );
    assert!(r.out.contains("  newcomer  0.1.0 (new)"), "{}", r.out);
}

#[test]
fn a_missing_test_command_is_an_error_not_a_failure() {
    let d = Dir::new("missing");
    with_script(&d, GOOD8, BAD8, "exit 0");
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "definitely-not-a-program",
        ],
    );
    assert_eq!(r.code, 2, "{}\n{}", r.out, r.err);
    assert!(
        r.out.contains("cannot run `definitely-not-a-program`"),
        "{}",
        r.out
    );
}

#[test]
fn the_project_is_left_exactly_as_it_was() {
    let d = Dir::new("untouched");
    with_script(&d, GOOD8, BAD8, "if has badlib 1.1.0; then exit 1; fi");
    let before = git(d.path(), &["status", "--porcelain"]);
    let lock_before = fs::read_to_string(d.path().join("requirements.txt")).unwrap();
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert_eq!(git(d.path(), &["status", "--porcelain"]), before);
    assert_eq!(
        fs::read_to_string(d.path().join("requirements.txt")).unwrap(),
        lock_before
    );
    let entries: Vec<String> = fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !entries
            .iter()
            .any(|n| n.contains("whichdep") || n.starts_with("target")),
        "{entries:?}"
    );
}

#[test]
fn every_trial_starts_from_a_clean_project_workspace() {
    let d = Dir::new("fresh-trials");
    with_script(
        &d,
        "alpha==1.0\n",
        "alpha==2.0\n",
        "test ! -e .whichdep-state || exit 1; touch .whichdep-state; exit 0",
    );
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 1, "{}\n{}", r.out, r.err);
    assert!(
        r.out.contains("The bad lockfile passes"),
        "trials appear to share mutable project state:\n{}",
        r.out
    );
}

#[test]
fn a_timeout_counts_as_a_failure() {
    let d = Dir::new("timeout");
    with_script(&d, GOOD8, BAD8, "if has badlib 1.1.0; then sleep 30; fi");
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--timeout",
            "1",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(
        r.out.contains("Root cause\n  badlib  1.0.0 → 1.1.0\n"),
        "{}",
        r.out
    );
    assert!(r.err.contains("timed out after 1.0s"), "{}", r.err);
}

#[test]
fn disagreeing_repeats_make_a_trial_flaky_not_failed() {
    let d = Dir::new("flaky");
    // Alternates pass/fail on every invocation, so no two runs agree.
    with_script(&d, GOOD8, BAD8, "n=$(cat .n 2>/dev/null || echo 0); echo $((n+1)) > .n; [ $((n % 2)) = 0 ] && exit 0; exit 1");
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--repeat",
            "2",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 1, "{}\n{}", r.out, r.err);
    assert!(r.err.contains("flaky"), "{}", r.err);
    assert!(r.out.contains("could not be built or tested"), "{}", r.out);
}

#[test]
fn good_can_be_a_file_and_bad_a_revision() {
    let d = Dir::new("files");
    with_script(&d, GOOD8, BAD8, "if has badlib 1.1.0; then exit 1; fi");
    git(d.path(), &["add", "-A", "."]);
    git(d.path(), &["commit", "-qm", "bad"]);
    fs::write(d.path().join("old.txt"), GOOD8).unwrap();
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "old.txt",
            "--bad",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(r.out.contains("  badlib  1.0.0 → 1.1.0\n"), "{}", r.out);
    assert!(
        r.err.contains("good  old.txt") && r.err.contains("bad   HEAD"),
        "{}",
        r.err
    );
}

#[test]
fn usage_errors_exit_with_2() {
    let d = Dir::new("usage");
    let r = whichdep(d.path(), &["--", "true"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("--good is required"), "{}", r.err);
    let r = whichdep(d.path(), &["--good", "HEAD", "--", "true"]);
    assert_eq!(r.code, 2);
    assert!(
        r.err.contains("no supported lockfile") && r.err.contains("Cargo.lock"),
        "{}",
        r.err
    );
}

// ---------------------------------------------------------------------------
// uv and a wheelhouse

const MAKE_WHEELS: &str = r#"
import zipfile, hashlib, base64, os
os.makedirs('wheelhouse', exist_ok=True)
def wheel(name, version, requires=(), body=''):
    d = f'{name}-{version}.dist-info'
    files = {
        f'{name}/__init__.py': f'VERSION = "{version}"\n{body}',
        f'{d}/METADATA': f'Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n' + ''.join(f'Requires-Dist: {r}\n' for r in requires),
        f'{d}/WHEEL': 'Wheel-Version: 1.0\nGenerator: whichdep-tests\nRoot-Is-Purelib: true\nTag: py3-none-any\n',
    }
    rec = ''
    for k, v in files.items():
        h = base64.urlsafe_b64encode(hashlib.sha256(v.encode()).digest()).rstrip(b'=').decode()
        rec += f'{k},sha256={h},{len(v.encode())}\n'
    files[f'{d}/RECORD'] = rec + f'{d}/RECORD,,\n'
    with zipfile.ZipFile(f'wheelhouse/{name}-{version}-py3-none-any.whl', 'w') as z:
        for k, v in files.items():
            z.writestr(k, v)
wheel('fakeparse', '1.0'); wheel('fakeparse', '2.0')
wheel('fakeweb', '1.0', ['fakeparse<2']); wheel('fakeweb', '1.1', ['fakeparse'])
wheel('fakeutil', '1.0'); wheel('fakeutil', '1.1')
wheel('badlib', '1.0', body='def answer():\n    return 42\n')
wheel('badlib', '1.1', body='def answer():\n    return 41\n')
"#;

#[test]
fn uv_installs_real_wheels_and_untestable_mixes_are_skipped() {
    if !have("uv") || !have("python3") {
        eprintln!("skipped: uv or python3 is not installed");
        return;
    }
    let d = Dir::new("uv");
    fs::write(d.path().join("make_wheels.py"), MAKE_WHEELS).unwrap();
    let st = Command::new("python3")
        .args(["-I", "make_wheels.py"])
        .current_dir(d.path())
        .status()
        .unwrap();
    assert!(st.success());
    let head = "--no-index\n--find-links wheelhouse\n";
    let good = format!("{head}badlib==1.0\nfakeparse==1.0\nfakeutil==1.0\nfakeweb==1.0\n");
    let bad = format!("{head}badlib==1.1\nfakeparse==2.0\nfakeutil==1.1\nfakeweb==1.1\n");
    repo(
        &d,
        "requirements.txt",
        &good,
        &bad,
        &[(
            "check.py",
            "import badlib, sys\nsys.exit(0 if badlib.answer() == 42 else 1)\n",
        )],
    );
    git(d.path(), &["add", "-A", "."]);
    // the wheels are untracked: they are still part of the project copy
    let r = whichdep(d.path(), &["--good", "HEAD", "--", "python", "check.py"]);
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(
        r.out.contains("Root cause\n  badlib  1.0 → 1.1\n"),
        "{}",
        r.out
    );
    // fakeparse 2.0 next to fakeweb 1.0 is rejected by `uv pip check`
    assert!(
        r.err.contains("unresolved") && r.err.contains("requires `fakeparse<2`"),
        "{}",
        r.err
    );
}

// ---------------------------------------------------------------------------
// cargo and a vendored registry

fn vendored_crate(root: &Path, name: &str, version: &str, deps: &[(&str, &str)], ok: bool) {
    let dir = root.join("vendor").join(format!("{name}-{version}"));
    fs::create_dir_all(dir.join("src")).unwrap();
    let deps_toml: String = deps
        .iter()
        .map(|(n, r)| format!("{n} = \"{r}\"\n"))
        .collect();
    fs::write(
        dir.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n\n[dependencies]\n{deps_toml}"),
    )
    .unwrap();
    let uses: String = deps
        .iter()
        .map(|(n, _)| format!("#[allow(unused_imports)] use {n} as _;\n"))
        .collect();
    fs::write(
        dir.join("src/lib.rs"),
        format!("{uses}pub fn ok() -> bool {{ {ok} }}\n"),
    )
    .unwrap();
    fs::write(
        dir.join(".cargo-checksum.json"),
        "{\"files\": {}, \"package\": null}",
    )
    .unwrap();
}

/// (name, version, dependencies as Cargo.lock writes them)
type Pkg<'a> = (&'a str, &'a str, &'a [&'a str]);

fn cargo_lock(pkgs: &[Pkg]) -> String {
    let mut out = String::from("# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n");
    for (name, version, deps) in pkgs {
        out.push_str(&format!(
            "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n"
        ));
        if *name != "app" {
            out.push_str("source = \"registry+https://github.com/rust-lang/crates.io-index\"\n");
        }
        if !deps.is_empty() {
            out.push_str("dependencies = [\n");
            for d in deps.iter() {
                out.push_str(&format!(" \"{d}\",\n"));
            }
            out.push_str("]\n");
        }
    }
    out
}

/// (name, version, dependencies as Cargo.toml writes them)
type CrateSpec<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

/// A project that depends on five vendored crates; `broken` names the crate
/// version whose `ok()` returns false.
fn cargo_project(d: &Dir, broken: (&str, &str)) {
    let p = d.path();
    let crates: [CrateSpec<'_>; 11] = [
        ("alpha", "1.0.0", &[]),
        ("alpha", "1.1.0", &[]),
        ("beta", "1.0.0", &[("alpha", "1.0")]),
        ("beta", "1.1.0", &[("alpha", "1.1"), ("epsilon", "1")]),
        ("epsilon", "1.0.0", &[]),
        ("gamma", "1.0.0", &[]),
        ("gamma", "1.0.1", &[]),
        ("delta", "1.0.0", &[]),
        ("delta", "1.0.1", &[]),
        ("kappa", "0.3.0", &[]),
        ("kappa", "0.3.1", &[]),
    ];
    for (name, version, deps) in crates {
        vendored_crate(p, name, version, deps, (name, version) != broken);
    }
    let files = [
        (
            ".cargo/config.toml",
            "[source.crates-io]\nreplace-with = \"vendored\"\n\n[source.vendored]\ndirectory = \"vendor\"\n",
        ),
        (
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nalpha = \"1\"\nbeta = \"1\"\ndelta = \"1\"\ngamma = \"1\"\nkappa = \"0.3\"\n",
        ),
        (
            "src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn every_dependency_works() {\n        assert!(alpha::ok() && beta::ok() && delta::ok() && gamma::ok() && kappa::ok());\n    }\n}\n",
        ),
        (".gitignore", "target\n"),
    ];
    let app_deps: &[&str] = &["alpha", "beta", "delta", "gamma", "kappa"];
    let good = cargo_lock(&[
        ("alpha", "1.0.0", &[]),
        ("app", "0.1.0", app_deps),
        ("beta", "1.0.0", &["alpha"]),
        ("delta", "1.0.0", &[]),
        ("gamma", "1.0.0", &[]),
        ("kappa", "0.3.0", &[]),
    ]);
    let bad = cargo_lock(&[
        ("alpha", "1.1.0", &[]),
        ("app", "0.1.0", app_deps),
        ("beta", "1.1.0", &["alpha", "epsilon"]),
        ("delta", "1.0.1", &[]),
        ("epsilon", "1.0.0", &[]),
        ("gamma", "1.0.1", &[]),
        ("kappa", "0.3.1", &[]),
    ]);
    repo(d, "Cargo.lock", &good, &bad, &files);
    git(p, &["add", "-A", "."]);
    git(p, &["commit", "-qm", "vendored crates", "--allow-empty"]);
    // `repo` left the bad lockfile in the working tree, the commit above
    // recorded it: put the good one back into HEAD's history instead.
    fs::write(p.join("Cargo.lock"), &good).unwrap();
    git(p, &["commit", "-qam", "good lockfile"]);
    fs::write(p.join("Cargo.lock"), &bad).unwrap();
}

#[test]
fn cargo_finds_the_crate_that_broke_the_build() {
    if !have("cargo") {
        eprintln!("skipped: cargo is not installed");
        return;
    }
    let d = Dir::new("cargo-single");
    cargo_project(&d, ("gamma", "1.0.1"));
    let r = whichdep(d.path(), &["--good", "HEAD", "--", "cargo", "test", "-q"]);
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(
        r.out.contains("Root cause\n  gamma  1.0.0 → 1.0.1\n"),
        "{}",
        r.out
    );
    assert!(
        r.out.contains("cargo update gamma@1.0.1 --precise 1.0.0"),
        "{}",
        r.out
    );
    assert!(r.out.contains("Verified:"), "{}", r.out);
    // The build cache is outside the project tree; no target directory is created here.
    assert!(!d.path().join("target/whichdep").exists());
}

#[test]
fn cargo_reports_a_crate_that_needs_its_companion_as_a_pair() {
    if !have("cargo") {
        eprintln!("skipped: cargo is not installed");
        return;
    }
    let d = Dir::new("cargo-pair");
    // beta 1.1.0 is the regression, but it requires alpha ^1.1: beta alone
    // cannot be locked, so Cargo rewrites the lockfile and the trial is
    // classified as unresolved.
    cargo_project(&d, ("beta", "1.1.0"));
    let r = whichdep(d.path(), &["--good", "HEAD", "--", "cargo", "test", "-q"]);
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(
        r.out.contains("these updates break the tests together"),
        "{}",
        r.out
    );
    assert!(
        r.out.contains("  alpha  1.0.0 → 1.1.0\n") && r.out.contains("  beta  1.0.0 → 1.1.0\n"),
        "{}",
        r.out
    );
    assert!(!r.out.contains(" 0 unresolved"), "{}", r.out);
}

// ---------------------------------------------------------------------------
// Phase 3 & 4: resume, strategies, uv/npm/pnpm ecosystems, platform outputs

#[test]
fn cli_resumes_from_persistent_cache_and_no_resume_reruns() {
    let d = Dir::new("resume-cli");
    with_script(&d, GOOD8, BAD8, "if has badlib 1.1.0; then exit 1; fi");

    let first = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(first.code, 0, "{}\n{}", first.out, first.err);
    assert!(
        first.out.contains("Root cause\n  badlib  1.0.0 → 1.1.0\n"),
        "{}",
        first.out
    );

    let second = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(second.code, 0, "{}\n{}", second.out, second.err);
    assert!(
        second.out.contains("0 trials (+"),
        "expected full cache hit:\n{}",
        second.out
    );
    assert!(
        !second.out.contains("0 pass · 0 fail"),
        "cached stats must reflect pass/fail:\n{}",
        second.out
    );

    let fresh = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--no-resume",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(fresh.code, 0, "{}\n{}", fresh.out, fresh.err);
    assert!(
        !fresh.out.contains("0 trials (+"),
        "--no-resume must re-execute trials:\n{}",
        fresh.out
    );
}

#[test]
fn search_strategies_work_end_to_end() {
    for strategy in ["bisect", "adaptive", "one-by-one"] {
        let d = Dir::new(&format!("strategy-{strategy}"));
        with_script(&d, GOOD8, BAD8, "if has badlib 1.1.0; then exit 1; fi");
        let r = whichdep(
            d.path(),
            &[
                "--good",
                "HEAD",
                "--strategy",
                strategy,
                "--prepare",
                "none",
                "--json",
                "--",
                "sh",
                "check.sh",
            ],
        );
        assert_eq!(r.code, 0, "{strategy}:\n{}\n{}", r.out, r.err);
        assert!(
            r.out.contains(&format!("\"strategy\": \"{strategy}\"")),
            "{}",
            r.out
        );
        assert!(r.out.contains("\"package\": \"badlib\""), "{}", r.out);
        assert!(r.out.contains("\"verdict\": \"verified\""), "{}", r.out);
    }
}

#[test]
fn uv_lock_bisection_works_end_to_end() {
    let d = Dir::new("uv-lock-e2e");
    let good = r#"version = 1

[[package]]
name = "app"
version = "0.1.0"
source = { virtual = "." }
dependencies = [{ name = "httpx" }, { name = "pydantic" }, { name = "zodiac" }]

[[package]]
name = "httpx"
version = "0.27.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "pydantic"
version = "2.8.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "zodiac"
version = "1.0.0"
source = { registry = "https://pypi.org/simple" }
"#;
    let bad = r#"version = 1

[[package]]
name = "app"
version = "0.1.0"
source = { virtual = "." }
dependencies = [{ name = "httpx" }, { name = "pydantic" }, { name = "zodiac" }]

[[package]]
name = "httpx"
version = "0.27.2"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "pydantic"
version = "2.9.2"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "zodiac"
version = "1.1.0"
source = { registry = "https://pypi.org/simple" }
"#;
    let check = r#"grep -q 'version = "2.9.2"' uv.lock && exit 1; exit 0"#;
    repo(&d, "uv.lock", good, bad, &[("check.sh", check)]);
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(r.out.contains("pydantic  2.8.0 → 2.9.2"), "{}", r.out);
    assert!(
        r.out.contains("uv lock --upgrade-package pydantic==2.8.0"),
        "{}",
        r.out
    );
}

#[test]
fn npm_package_lock_bisection_works_end_to_end() {
    let d = Dir::new("npm-lock-e2e");
    let good = r#"{
  "name": "demo",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "demo", "version": "1.0.0", "dependencies": { "alpha": "^1.0.0", "beta": "^1.0.0", "gamma": "^1.0.0" } },
    "node_modules/alpha": { "version": "1.0.0", "integrity": "sha512-a1" },
    "node_modules/beta": { "version": "1.0.0", "integrity": "sha512-b1" },
    "node_modules/gamma": { "version": "1.0.0", "integrity": "sha512-g1" }
  }
}
"#;
    let bad = r#"{
  "name": "demo",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "demo", "version": "1.0.0", "dependencies": { "alpha": "^1.0.0", "beta": "^1.0.0", "gamma": "^1.0.0" } },
    "node_modules/alpha": { "version": "1.1.0", "integrity": "sha512-a2" },
    "node_modules/beta": { "version": "1.2.0", "integrity": "sha512-b2" },
    "node_modules/gamma": { "version": "1.1.0", "integrity": "sha512-g2" }
  }
}
"#;
    let check = r#"grep -q '"1.2.0"' package-lock.json && exit 1; exit 0"#;
    repo(&d, "package-lock.json", good, bad, &[("check.sh", check)]);
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(r.out.contains("beta  1.0.0 → 1.2.0"), "{}", r.out);
    assert!(
        r.out.contains("npm install --save-exact beta@1.0.0"),
        "{}",
        r.out
    );
}

#[test]
fn pnpm_lock_yaml_bisection_works_end_to_end() {
    let d = Dir::new("pnpm-lock-e2e");
    let good = r#"lockfileVersion: '9.0'

importers:
  .:
    dependencies:
      alpha:
        specifier: ^1.0.0
        version: 1.0.0
      zod:
        specifier: ^3.22.0
        version: 3.22.4

packages:
  alpha@1.0.0:
    resolution: {integrity: sha512-a1}
  zod@3.22.4:
    resolution: {integrity: sha512-z1}

snapshots:
  alpha@1.0.0: {}
  zod@3.22.4: {}
"#;
    let bad = r#"lockfileVersion: '9.0'

importers:
  .:
    dependencies:
      alpha:
        specifier: ^1.0.0
        version: 1.1.0
      zod:
        specifier: ^3.22.0
        version: 3.23.8

packages:
  alpha@1.1.0:
    resolution: {integrity: sha512-a2}
  zod@3.23.8:
    resolution: {integrity: sha512-z2}

snapshots:
  alpha@1.1.0: {}
  zod@3.23.8: {}
"#;
    let check = r#"grep -q 'zod@3.23.8' pnpm-lock.yaml && exit 1; exit 0"#;
    repo(&d, "pnpm-lock.yaml", good, bad, &[("check.sh", check)]);
    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(r.out.contains("zod  3.22.4 → 3.23.8"), "{}", r.out);
    assert!(
        r.out.contains("pnpm add --save-exact zod@3.22.4"),
        "{}",
        r.out
    );
}

#[test]
fn platform_outputs_subcommands_and_github_summary_work_end_to_end() {
    let d = Dir::new("platform-e2e");
    with_script(&d, GOOD8, BAD8, "if has badlib 1.1.0; then exit 1; fi");

    let report_json = d.root.join("report.json");
    let comment_md = d.root.join("comment.md");
    let dashboard_html = d.root.join("dashboard.html");
    let summary_file = d.root.join("step-summary.md");

    let home = d.root.join("home");
    let out = Command::new(env!("CARGO_BIN_EXE_whichdep"))
        .args([
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--strategy",
            "adaptive",
            "--format",
            "json",
            "--out",
            report_json.to_str().unwrap(),
            "--github-summary",
            "--",
            "sh",
            "check.sh",
        ])
        .current_dir(d.path())
        .env("NO_COLOR", "1")
        .env("WHICHDEP_HOME", &home)
        .env("GITHUB_STEP_SUMMARY", &summary_file)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("::error file=requirements.txt"), "{stderr}");
    let json_text = fs::read_to_string(&report_json).unwrap();
    assert!(
        json_text.contains("\"schema\": \"whichdep-report-v2\""),
        "{json_text}"
    );
    assert!(json_text.contains("\"revert_commands\""), "{json_text}");
    let summary_text = fs::read_to_string(&summary_file).unwrap();
    assert!(summary_text.contains("badlib"), "{summary_text}");

    let fmt = whichdep(
        d.path(),
        &[
            "format",
            "--input",
            report_json.to_str().unwrap(),
            "--format",
            "github-comment",
            "--out",
            comment_md.to_str().unwrap(),
        ],
    );
    assert_eq!(fmt.code, 0, "{}\n{}", fmt.out, fmt.err);
    let comment_text = fs::read_to_string(&comment_md).unwrap();
    assert!(
        comment_text.starts_with("<!-- whichdep-report -->"),
        "{comment_text}"
    );
    assert!(comment_text.contains("badlib==1.0.0"), "{comment_text}");

    let adapters = whichdep(d.path(), &["adapters", "--json"]);
    assert_eq!(adapters.code, 0);
    for id in ["\"cargo\"", "\"uv\"", "\"pip\"", "\"npm\"", "\"pnpm\""] {
        assert!(
            adapters.out.contains(id),
            "missing adapter {id}:\n{}",
            adapters.out
        );
    }

    let hist = whichdep(
        d.path(),
        &[
            "history",
            "--html",
            "--out",
            dashboard_html.to_str().unwrap(),
        ],
    );
    assert_eq!(hist.code, 0, "{}\n{}", hist.out, hist.err);
    let html = fs::read_to_string(&dashboard_html).unwrap();
    assert!(
        html.contains("\"schema\": \"whichdep-dashboard-v1\""),
        "{html}"
    );
    assert!(html.contains("Experiment History"), "{html}");

    let clean = whichdep(d.path(), &["clean", "--build-only"]);
    assert_eq!(clean.code, 0, "{}\n{}", clean.out, clean.err);
    assert!(clean.out.contains("cleaned build caches"), "{}", clean.out);
}

#[test]
fn closure_companions_are_reported_under_single_root_cause() {
    let d = Dir::new("closure-root");
    // `app` requires newly added `helper` via pip `# via` annotation.
    // The closure pulls `helper` whenever `app` is upgraded, so `app` is the
    // single root and `helper` is a required companion, NOT a 2-package interaction.
    let good = "app==1.0\nother==1.0\n";
    let bad = "app==2.0\nhelper==1.0\n    # via app\nother==2.0\n";
    with_script(&d, good, bad, "if has app 2.0; then exit 1; fi");

    let r = whichdep(
        d.path(),
        &[
            "--good",
            "HEAD",
            "--prepare",
            "none",
            "--",
            "sh",
            "check.sh",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(
        !r.out.contains("these updates break the tests together"),
        "{}",
        r.out
    );
    assert!(
        !r.out.contains("Leaving out any single one of them"),
        "{}",
        r.out
    );
    assert!(r.out.contains("  app  1.0 → 2.0\n"), "{}", r.out);
    assert!(r.out.contains("Required companion updates:"), "{}", r.out);
    assert!(r.out.contains("    helper  1.0 (new)\n"), "{}", r.out);
    assert!(
        r.out
            .contains("To put it back to the good version:\n  app==1.0\n"),
        "{}",
        r.out
    );
}
