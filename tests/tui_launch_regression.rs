//! Regression test for the v0.2.1 no-args panic.
//!
//! v0.2.0 introduced an in-process proxy and wrapped the whole CLI,
//! including the TUI launch path, in `runtime.block_on(run(cli))`. The
//! TUI's `App::new` -> `maybe_load_models_for_selection` then built a
//! *second* tokio runtime and called `block_on` on it, which panics
//! with "Cannot start a runtime from within a runtime". The release
//! shipped broken in v0.2.0 and v0.2.1 because the per-version smoke
//! tests exercised only the CLI subcommand path, never the no-args TUI
//! entrypoint.
//!
//! This test launches the real binary in headless regression mode
//! (`--regression-marker <path>`, a hidden test-only flag). The binary
//! builds the same `App` the no-args path builds, writes `launched`
//! to the marker on success, and exits 0. If the nested-block_on bug
//! returns, the binary panics before writing the marker, and this test
//! fails. No PTY is required, so the test runs in plain `cargo test`.

use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_claude-go"))
}

/// Wait up to `timeout` for `path` to exist and contain `expected`.
fn wait_for_marker(path: &std::path::Path, expected: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(content) = std::fs::read_to_string(path) {
            if content.contains(expected) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn tui_no_args_path_does_not_panic() {
    let dir = TempDir::new().expect("tempdir");
    let marker = dir.path().join("launched.marker");

    let mut cmd = Command::new(binary_path());
    cmd.arg("--regression-marker").arg(&marker);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    // The regression path bypasses the OPENCODE_API_KEY check (no
    // settings write happens), but unset it anyway so the test does
    // not depend on the dev environment.
    cmd.env_remove("OPENCODE_API_KEY");
    // No real TTY; the regression path deliberately bypasses the TTY
    // gate, so TERM and the piped std streams don't matter.
    cmd.env("TERM", "dumb");

    let mut child = cmd.spawn().expect("spawn claude-go --regression-marker");
    let saw_marker = wait_for_marker(&marker, "launched", Duration::from_secs(5));

    // Drain stderr so a panic message (if the bug returns) is visible
    // in the test failure.
    let stderr = {
        let mut buf = Vec::new();
        use std::io::Read;
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    };

    // Don't wait forever; force a clean exit.
    let _ = child.wait();

    assert!(
        saw_marker,
        "no regression marker written within 5s. \
         stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "binary panicked instead of writing the marker. stderr:\n{stderr}"
    );
}