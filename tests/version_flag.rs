//! `--version` / `-V` / `--help` of the binaries: printed to stdout, exit 0,
//! and nothing else starts (no window, no lock, no log file).

use std::process::Command;

fn run(exe: &str, arg: &str) -> (bool, String) {
    let out = Command::new(exe).arg(arg).output().expect("run the binary");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_owned(),
    )
}

fn check(exe: &str, name: &str) {
    let want = format!("{name} {}", env!("CARGO_PKG_VERSION"));
    for flag in ["--version", "-V"] {
        let (ok, out) = run(exe, flag);
        assert!(ok, "{name} {flag} failed");
        assert_eq!(out, want);
    }
    let (ok, out) = run(exe, "--help");
    assert!(ok, "{name} --help failed");
    assert!(out.contains("--version") && out.contains("usage:"), "{out}");
}

#[test]
fn ferrocord_prints_its_version() {
    check(env!("CARGO_BIN_EXE_ferrocord"), "ferrocord");
}

#[cfg(feature = "video")]
#[test]
fn player_prints_its_version() {
    check(env!("CARGO_BIN_EXE_ferrocord-player"), "ferrocord-player");
}

#[cfg(feature = "web-login")]
#[test]
fn login_helper_prints_its_version() {
    check(env!("CARGO_BIN_EXE_ferrocord-login"), "ferrocord-login");
}
