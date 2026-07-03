//! CLI-level smoke tests for the TTY-gated no-arg path and the
//! `tui` subcommand. These spawn the binary in a controlled
//! environment with stdin/stdout piped (so no real TTY) and assert
//! on the behavior.

use std::process::{Command, Stdio};

fn binary_path() -> std::path::PathBuf {
    // assert_cmd places the test binary under target/debug; use the
    // current_exe approach via env::var. CARGO_BIN_EXE_<name> is
    // set by cargo for integration tests.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_claude-go"))
}

fn cmd() -> Command {
    let mut c = Command::new(binary_path());
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    c
}

#[test]
fn no_args_with_piped_stdout_prints_status_json_and_exits_0() {
    let out = cmd()
        .env_remove("OPENCODE_API_KEY")
        .env_remove("TERM")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "exit was {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not valid JSON: {e}\nstdout: {stdout}"));
    assert_eq!(v["version"], 1);
    assert!(v.get("enabled").is_some());
    assert!(v.get("model").is_some());
    assert!(v.get("base_url").is_some());
    assert!(v.get("path_kind").is_some());
    assert!(v.get("opencode_api_key").is_some());
}

#[test]
fn no_args_with_term_dumb_prints_status_json() {
    let out = cmd()
        .env("TERM", "dumb")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "exit was {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not valid JSON: {e}\nstdout: {stdout}"));
    assert_eq!(v["version"], 1);
}

#[test]
fn tui_subcommand_with_piped_stdout_prints_hint_and_exits_0() {
    let out = cmd()
        .arg("tui")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "exit was {:?}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no TTY detected"),
        "expected hint in stderr, got: {stderr}"
    );
}

#[test]
fn help_subcommand_works_without_tty() {
    let out = cmd()
        .arg("--help")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "exit was {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("claude-go"));
    assert!(stdout.contains("USAGE") || stdout.contains("Usage"));
}

#[test]
fn version_subcommand_works_without_tty() {
    let out = cmd()
        .arg("--version")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "exit was {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("claude-go"));
    assert!(stdout.contains("0.2.1"));
}

#[test]
fn version_subcommand_matches_long_flag() {
    // `claude-go version`, `claude-go --version`, and `claude-go -V`
    // must all produce the same `claude-go <version>` string. The
    // `version` subcommand was added in v0.2.1 to match the v0.1.1
    // bash contract; the assertion guards against drift between the
    // three output paths.
    let long = cmd().arg("--version").output().expect("spawn --version");
    let short = cmd().arg("-V").output().expect("spawn -V");
    let sub = cmd().arg("version").output().expect("spawn version");

    for (label, out) in [("long", &long), ("short", &short), ("sub", &sub)] {
        assert!(out.status.success(), "{label} exit was {:?}", out.status);
    }
    let long_s = String::from_utf8_lossy(&long.stdout).to_string();
    let short_s = String::from_utf8_lossy(&short.stdout).to_string();
    let sub_s = String::from_utf8_lossy(&sub.stdout).to_string();
    assert!(long_s.contains("0.2.1"), "long: {long_s}");
    assert_eq!(long_s, short_s, "--version and -V must match");
    assert_eq!(long_s, sub_s, "subcommand and --version must match");
}

#[test]
fn status_subcommand_works_without_tty() {
    let out = cmd()
        .arg("status")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "exit was {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("OpenCode Go is DISABLED"));
}
