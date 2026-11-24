//! Cross-platform CLI smoke tests (Linux, macOS, and Windows).

use std::fs;
use std::process::Command;

#[test]
fn cli_version_adapters_list_history_and_clean_work_on_all_platforms() {
    let root = std::env::temp_dir().join(format!("whichdep-xplat-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let project = root.join("project");
    let home = root.join("home");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&home).unwrap();

    let bin = env!("CARGO_BIN_EXE_whichdep");

    let ver = Command::new(bin).arg("--version").output().unwrap();
    assert!(ver.status.success());
    assert!(
        String::from_utf8_lossy(&ver.stdout).contains("whichdep"),
        "{}",
        String::from_utf8_lossy(&ver.stdout)
    );

    let adapters = Command::new(bin)
        .args(["adapters", "--json"])
        .env("WHICHDEP_HOME", &home)
        .output()
        .unwrap();
    assert!(adapters.status.success());
    let out = String::from_utf8_lossy(&adapters.stdout);
    for eco in ["\"cargo\"", "\"uv\"", "\"pip\"", "\"npm\"", "\"pnpm\""] {
        assert!(out.contains(eco), "missing {eco} in {out}");
    }

    // --list using plain files (no git or shell needed, works identically on Windows).
    fs::write(project.join("good.txt"), "alpha==1.0.0\nbeta==2.0.0\n").unwrap();
    fs::write(
        project.join("requirements.txt"),
        "alpha==1.0.0\nbeta==2.1.0\n",
    )
    .unwrap();
    let list = Command::new(bin)
        .args(["--good", "good.txt", "--list"])
        .current_dir(&project)
        .env("WHICHDEP_HOME", &home)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("beta  2.0.0 → 2.1.0"),
        "{}",
        String::from_utf8_lossy(&list.stdout)
    );

    let hist = Command::new(bin)
        .args(["history", "--json"])
        .env("WHICHDEP_HOME", &home)
        .output()
        .unwrap();
    assert!(hist.status.success());
    assert!(String::from_utf8_lossy(&hist.stdout).contains("whichdep-dashboard-v1"));

    let clean = Command::new(bin)
        .args(["clean", "--build-only"])
        .env("WHICHDEP_HOME", &home)
        .output()
        .unwrap();
    assert!(clean.status.success());

    let _ = fs::remove_dir_all(&root);
}
