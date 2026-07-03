//! TTY detection and the "no-args" fallback path.
//!
//! Two responsibilities:
//!
//! 1. `should_launch_tui` -- gate the no-args TUI default so it only
//!    starts when there is a real terminal on both stdin and stdout and
//!    `TERM` is not `dumb`. Without this gate, running `claude-go` from
//!    a pipe, redirect, `nohup`, `cron`, `systemd`, or `make` would
//!    either crash inside `enable_raw_mode` or hang waiting for input
//!    that will never come.
//!
//! 2. `print_status_json` -- the non-interactive fallback. Prints the
//!    current `claude-go` state (enabled, model, base URL, proxy
//!    state) as one JSON object on stdout, so scripts can parse it.
//!    Exit 0 in both "enabled" and "disabled" cases; only I/O or parse
//!    errors produce a non-zero exit.

use std::io::IsTerminal;

use serde::Serialize;

use crate::paths::Paths;
use crate::settings::SettingsState;

/// True iff launching the full TUI is appropriate for the current
/// process. Both stdin and stdout must be TTYs (so we can read keys
/// and write to the alternate screen), and `TERM` must not be `dumb`
/// (so the terminal is capable of ANSI escapes). We deliberately do
/// NOT gate on `DISPLAY` or any other env var -- a TUI over SSH with
/// a proper `TERM` is a normal, supported case.
pub fn should_launch_tui() -> bool {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        return false;
    }
    if std::env::var("TERM")
        .map(|t| t == "dumb")
        .unwrap_or(false)
    {
        return false;
    }
    true
}

/// JSON shape emitted by `print_status_json`. Stable, versioned by
/// the `version` field. New optional fields are backwards-compatible.
#[derive(Debug, Serialize)]
pub struct StatusReport {
    /// Schema version of this report. Bump on any breaking field change.
    pub version: u32,
    /// True iff `~/.claude/settings.json` is currently owned by claude-go
    /// and has a complete env block.
    pub enabled: bool,
    /// Model the user is currently routed to. Empty when disabled.
    pub model: String,
    /// Anthropic base URL in settings.json. Empty when disabled.
    pub base_url: String,
    /// One of "anthropic", "openai", "other". Empty when disabled.
    pub path_kind: String,
    /// True iff the `ANTHROPIC_AUTH_TOKEN` value lives in settings.json
    /// (not just in the env).
    pub key_in_settings: bool,
    /// `OPENCODE_API_KEY` env var status: "set", "empty", or "unset".
    pub opencode_api_key: String,
    /// Path to the settings file we read.
    pub config: String,
}

const STATUS_VERSION: u32 = 1;

impl StatusReport {
    /// Build the report from current on-disk state. Does not touch
    /// the proxy. Read-only.
    pub fn from_paths(paths: &Paths) -> Self {
        let state = SettingsState::peek(paths).unwrap_or_else(|_| SettingsState::disabled_default());
        let opencode_api_key = match std::env::var("OPENCODE_API_KEY") {
            Ok(v) if !v.is_empty() => "set".into(),
            Ok(_) => "empty".into(),
            Err(_) => "unset".into(),
        };
        Self {
            version: STATUS_VERSION,
            enabled: state.enabled,
            model: state.model,
            base_url: state.base_url,
            path_kind: match state.path_kind {
                crate::settings::PathKind::Anthropic => "anthropic".into(),
                crate::settings::PathKind::OpenAI => "openai".into(),
                crate::settings::PathKind::Other => "other".into(),
            },
            key_in_settings: state.key_in_settings,
            opencode_api_key,
            config: paths.settings_file.display().to_string(),
        }
    }
}

/// Print the current state as a single JSON object to stdout, then
/// return 0. Used when `claude-go` is run with no args in a non-TTY
/// context. Scripts can `claude-go | jq '.enabled'` to branch.
pub fn print_status_json(paths: &Paths) -> i32 {
    let report = StatusReport::from_paths(paths);
    // serde_json is unlikely to fail on this struct; if it does we
    // surface a clear message to stderr.
    match serde_json::to_string_pretty(&report) {
        Ok(s) => {
            println!("{s}");
            0
        }
        Err(e) => {
            eprintln!("claude-go: failed to render status JSON: {e}");
            1
        }
    }
}

/// Hint printed to stderr when a subcommand is invoked in a context
/// that requires a TTY but does not have one. Kept here so the wording
/// is in one place.
pub const TTY_REQUIRED_HINT: &str =
    "claude-go: no TTY detected. Run 'claude-go tui' in an interactive terminal, \
or 'claude-go help' for CLI usage.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_report_disabled_when_no_settings() {
        // Build a paths under a fresh empty dir.
        let dir = std::env::temp_dir().join(format!(
            "claude-go-tty-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::resolve_under(&dir);
        let report = StatusReport::from_paths(&paths);
        assert!(!report.enabled);
        assert_eq!(report.model, "");
        assert_eq!(report.base_url, "");
        assert_eq!(report.path_kind, "other");
        assert_eq!(report.version, STATUS_VERSION);
    }
}
