use std::path::Path;

use serde_json::{Map, Value};

use crate::error::ClaudeGoError;
use crate::paths::Paths;
use crate::provider::{Provider, ProviderFormat};

/// The exact set of env keys owned by claude-go. Every write to
/// `~/.claude/settings.json` sets these (in addition to the marker),
/// and `turn_off` strips only these (gated on the marker being present).
///
/// IMPORTANT: this list must match bash v0.1.1 bit-for-bit. Adding or
/// removing a key here is a user-facing change to the settings.json
/// contract that Claude Code reads on every launch.
pub const OWNED_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "DISABLE_TELEMETRY",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
];

/// Marker written into the `env` dict. `turn_off` strips the owned keys
/// iff this marker is present and equals `"1"`. Without it, a user with
/// their own ANTHROPIC_* keys (e.g. pointing at an internal proxy) would
/// have those keys silently destroyed on `off`.
pub const OWNERSHIP_MARKER: &str = "__claude_go_owned";

/// Parsed view of `~/.claude/settings.json`. Trust the inner types once
/// you have one of these -- the boundary parse enforces the invariants
/// (env is an object if present, model/base_url/key are strings).
#[derive(Debug, Clone)]
pub struct SettingsState {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub path_kind: PathKind,
    pub key_in_settings: bool,
    /// True iff claude-go wrote this env block (marker present). `off`
    /// only strips our own keys when this is true.
    pub owned: bool,
}

/// The format the current base URL points at. Parsed from the URL
/// shape, not from a stored string. This is "parse, don't validate":
/// once we have a `PathKind`, we don't re-check the URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Anthropic,
    OpenAI,
    Other,
}

impl PathKind {
    /// Discriminate the path kind from a base URL. CRLF-safe: strips
    /// stray `\r\n` defensively to keep peek robust against editor
    /// corruption of the settings file.
    pub fn from_base_url(base_url: &str) -> Self {
        let sanitized = base_url.replace('\r', "").replace('\n', "").trim().to_string();
        if sanitized == "https://opencode.ai/zen/go" {
            Self::Anthropic
        } else if sanitized.starts_with("http://127.0.0.1:") {
            Self::OpenAI
        } else {
            Self::Other
        }
    }
}

impl SettingsState {
    /// Read `~/.claude/settings.json` and parse it into a `SettingsState`.
    /// Missing file = all-default state (not an error).
    pub fn peek(paths: &Paths) -> Result<Self, ClaudeGoError> {
        let Some(state) = read_raw(&paths.settings_file)? else {
            return Ok(Self::disabled_default());
        };
        Ok(Self::from_value(&state))
    }

    /// Convenience: read from default paths.
    pub fn peek_default() -> Result<Self, ClaudeGoError> {
        Self::peek(&Paths::resolve())
    }

    /// A default "disabled" settings state, used as the fallback when
    /// `peek` fails. Public so other modules (e.g. the TTY fallback in
    /// `tty.rs`) can construct a known-disabled state without having
    /// to fake up a `Paths`.
    pub fn disabled_default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            model: String::new(),
            path_kind: PathKind::Other,
            key_in_settings: false,
            owned: false,
        }
    }

    fn from_value(root: &Value) -> Self {
        // A non-object `env` is treated as if env were empty. This is
        // the safe default -- never assume ownership of a non-dict
        // env, so we strip nothing and never claim to be enabled.
        let env = root.get("env").and_then(|v| v.as_object());
        let Some(env) = env else {
            return Self::disabled_default();
        };

        let owned = env.get(OWNERSHIP_MARKER).and_then(Value::as_str) == Some("1");

        let raw_base = env
            .get("ANTHROPIC_BASE_URL")
            .and_then(Value::as_str)
            .unwrap_or("");
        let base_url = raw_base.replace('\r', "").replace('\n', "").trim().to_string();
        let raw_model = env
            .get("ANTHROPIC_MODEL")
            .and_then(Value::as_str)
            .unwrap_or("");
        let model = raw_model.replace('\r', "").replace('\n', "").trim().to_string();
        let key_in_settings = env.contains_key("ANTHROPIC_AUTH_TOKEN");
        let path_kind = PathKind::from_base_url(&base_url);
        let enabled = owned && !path_kind.eq(&PathKind::Other) && key_in_settings && !model.is_empty();

        Self {
            enabled,
            base_url,
            model,
            path_kind,
            key_in_settings,
            owned,
        }
    }
}

/// Inputs to `turn_on`. Built up by the CLI subcommand before calling.
#[derive(Debug, Clone)]
pub struct TurnOnInputs<'a> {
    pub provider: &'a Provider,
    pub model: &'a str,
    /// Resolved wire format for the chosen model+provider pair. For
    /// OpenCode Go the format is per-model (some are Anthropic, some
    /// are OpenAI), so the caller must compute it before calling.
    /// For other providers this matches `provider.format`.
    pub format: ProviderFormat,
    /// Local port to point ANTHROPIC_BASE_URL at. Required for
    /// OpenAI-format (where the base URL is
    /// `http://127.0.0.1:<port>`). Ignored for Anthropic-format.
    pub port: Option<u16>,
    /// Auth value. Resolved by the caller (OPENCODE_API_KEY env, or
    /// whatever the user configured for the provider).
    pub auth_token: &'a str,
}

/// Write the owned env block into `~/.claude/settings.json`. Preserves
/// any other keys the user has in `env` (e.g. custom env vars,
/// permissions, theme, plugins -- all on the `root`).
pub fn turn_on(paths: &Paths, input: &TurnOnInputs<'_>) -> Result<(), ClaudeGoError> {
    let base_url = if input.format == ProviderFormat::OpenAI {
        let port = input
            .port
            .ok_or_else(|| ClaudeGoError::InvalidPort(0))?;
        format!("http://127.0.0.1:{port}")
    } else {
        input.provider.base_url.clone()
    };

    let mut root = read_raw(&paths.settings_file)?.unwrap_or_else(|| {
        let mut m = Map::new();
        m.insert("env".into(), Value::Object(Map::new()));
        Value::Object(m)
    });

    // Refuse to merge into a non-dict env. The user must fix their
    // settings.json by hand.
    let env_value = root
        .get("env")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let Value::Object(mut env) = env_value else {
        return Err(ClaudeGoError::EnvNotObject {
            found: type_name_of(&env_value).to_string(),
        });
    };

    for k in OWNED_ENV_KEYS {
        env.insert(
            (*k).to_string(),
            Value::String(env_value_for(k, &base_url, input.model, input.auth_token)),
        );
    }
    env.insert(OWNERSHIP_MARKER.to_string(), Value::String("1".into()));

    // Set the env back. If the user had no env at all, we keep the
    // top-level `env` key present (Claude Code expects an object).
    if let Value::Object(ref mut root_map) = root {
        root_map.insert("env".into(), Value::Object(env));
    } else {
        // Root is not an object -- bail loudly per the bash contract.
        return Err(ClaudeGoError::EnvNotObject {
            found: type_name_of(&root).to_string(),
        });
    }

    write_atomic(&paths.settings_file, &root)
}

/// Strip the owned env block iff the marker is present. If the
/// settings file does not exist, or the env block lacks the marker,
/// this is a no-op (idempotent). Never touches user-owned env vars.
pub fn turn_off(paths: &Paths) -> Result<(), ClaudeGoError> {
    let Some(root) = read_raw(&paths.settings_file)? else {
        return Ok(());
    };
    let mut root = root;

    let env_value = match root.get("env") {
        Some(v) => v.clone(),
        None => return Ok(()),
    };
    let Value::Object(mut env) = env_value else {
        // Non-dict env: treat as if env were absent. Never claim
        // ownership of a non-dict env.
        return Ok(());
    };
    if env.get(OWNERSHIP_MARKER).and_then(Value::as_str) != Some("1") {
        return Ok(());
    }
    for k in OWNED_ENV_KEYS {
        env.remove(*k);
    }
    env.remove(OWNERSHIP_MARKER);

    let Value::Object(ref mut root_map) = root else {
        return Ok(());
    };
    if env.is_empty() {
        root_map.remove("env");
    } else {
        root_map.insert("env".into(), Value::Object(env));
    }
    write_atomic(&paths.settings_file, &root)
}

fn env_value_for(key: &str, base_url: &str, model: &str, auth: &str) -> String {
    match key {
        "ANTHROPIC_BASE_URL" => base_url.to_string(),
        "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY" => auth.to_string(),
        "ANTHROPIC_MODEL"
        | "ANTHROPIC_DEFAULT_HAIKU_MODEL"
        | "ANTHROPIC_DEFAULT_SONNET_MODEL"
        | "ANTHROPIC_DEFAULT_OPUS_MODEL" => model.to_string(),
        "DISABLE_TELEMETRY" | "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC" => "1".into(),
        _ => unreachable!("OWNED_ENV_KEYS contains unknown key: {key}"),
    }
}

fn type_name_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn read_raw(path: &Path) -> Result<Option<Value>, ClaudeGoError> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let v: Value = serde_json::from_slice(&bytes)?;
    Ok(Some(v))
}

fn write_atomic(path: &Path, value: &Value) -> Result<(), ClaudeGoError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    // Temp file MUST be in the same directory so `persist` is a rename,
    // not a cross-filesystem copy (which would not be atomic).
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    use std::io::Write;
    let mut f = tmp.as_file();
    f.write_all(&bytes)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ModelSource, Provider, ProviderFormat};
    use std::path::PathBuf;

    fn test_paths(dir: &Path) -> Paths {
        Paths::resolve_under(dir)
    }

    fn make_provider() -> Provider {
        Provider {
            id: "test".into(),
            display_name: "Test".into(),
            base_url: "https://example.test".into(),
            format: ProviderFormat::Anthropic,
            auth_header: "x-api-key".into(),
            model_source: ModelSource::Any,
            implemented: true,
            is_custom: false,
        }
    }

    fn fresh_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("claude-go-test-{name}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn owned_marker_and_keys_are_stable() {
        // This is a contract test -- if it ever changes, downstream
        // Claude Code installations will see a different env block.
        assert_eq!(OWNED_ENV_KEYS.len(), 9);
        assert!(OWNED_ENV_KEYS.contains(&"ANTHROPIC_BASE_URL"));
        assert!(OWNED_ENV_KEYS.contains(&"ANTHROPIC_DEFAULT_HAIKU_MODEL"));
        assert!(OWNED_ENV_KEYS.contains(&"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"));
        assert_eq!(OWNERSHIP_MARKER, "__claude_go_owned");
    }

    #[test]
    fn turn_on_then_off_round_trips() {
        let dir = fresh_dir("round-trip");
        let paths = test_paths(&dir);
        let mut provider = make_provider();
        // Use a base URL that `peek` recognizes as a valid (opencode-go-style)
        // path so the round-trip "enabled" check holds.
        provider.base_url = "https://opencode.ai/zen/go".into();

        turn_on(
            &paths,
            &TurnOnInputs {
                provider: &provider,
                model: "minimax-m3",
                format: ProviderFormat::Anthropic,
                port: None,
                auth_token: "sk-test",
            },
        )
        .unwrap();

        // Read raw file and check the env block is right.
        let raw = std::fs::read_to_string(&paths.settings_file).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let env = v.get("env").and_then(|o| o.as_object()).unwrap();
        assert_eq!(env.get("ANTHROPIC_BASE_URL").unwrap(), "https://opencode.ai/zen/go");
        assert_eq!(env.get("ANTHROPIC_MODEL").unwrap(), "minimax-m3");
        assert_eq!(env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").unwrap(), "minimax-m3");
        assert_eq!(env.get("ANTHROPIC_AUTH_TOKEN").unwrap(), "sk-test");
        assert_eq!(env.get(OWNERSHIP_MARKER).unwrap(), "1");
        assert_eq!(env.get("DISABLE_TELEMETRY").unwrap(), "1");

        // State should report enabled.
        let state = SettingsState::peek(&paths).unwrap();
        assert!(state.enabled);
        assert_eq!(state.model, "minimax-m3");
        assert_eq!(state.path_kind, PathKind::Anthropic);

        // Now turn off.
        turn_off(&paths).unwrap();
        let raw2 = std::fs::read_to_string(&paths.settings_file).unwrap();
        let v2: Value = serde_json::from_str(&raw2).unwrap();
        let env2 = v2.get("env").and_then(|o| o.as_object());
        // env is removed entirely because no other env vars exist.
        assert!(env2.is_none() || env2.unwrap().is_empty());

        let state2 = SettingsState::peek(&paths).unwrap();
        assert!(!state2.enabled);
    }

    #[test]
    fn turn_off_without_marker_does_not_strip() {
        let dir = fresh_dir("no-marker");
        let paths = test_paths(&dir);
        // Hand-craft a user-owned settings.json with their own ANTHROPIC_*
        // keys, no marker.
        let user_value = serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://internal.proxy.corp",
                "ANTHROPIC_AUTH_TOKEN": "user-key",
                "ANTHROPIC_MODEL": "claude-internal"
            }
        });
        std::fs::create_dir_all(paths.settings_file.parent().unwrap()).unwrap();
        std::fs::write(
            &paths.settings_file,
            serde_json::to_vec_pretty(&user_value).unwrap(),
        )
        .unwrap();

        turn_off(&paths).unwrap();

        let raw = std::fs::read_to_string(&paths.settings_file).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let env = v.get("env").and_then(|o| o.as_object()).unwrap();
        // User's keys must still be there.
        assert_eq!(env.get("ANTHROPIC_BASE_URL").unwrap(), "https://internal.proxy.corp");
        assert_eq!(env.get("ANTHROPIC_AUTH_TOKEN").unwrap(), "user-key");
        assert_eq!(env.get("ANTHROPIC_MODEL").unwrap(), "claude-internal");
    }

    #[test]
    fn turn_on_preserves_user_env_vars() {
        let dir = fresh_dir("preserve");
        let paths = test_paths(&dir);
        let initial = serde_json::json!({
            "permissions": {"defaultMode": "auto"},
            "env": {
                "MY_CUSTOM_VAR": "leave-me-alone"
            }
        });
        std::fs::create_dir_all(paths.settings_file.parent().unwrap()).unwrap();
        std::fs::write(
            &paths.settings_file,
            serde_json::to_vec_pretty(&initial).unwrap(),
        )
        .unwrap();
        let provider = make_provider();
        turn_on(
            &paths,
            &TurnOnInputs {
                provider: &provider,
                model: "minimax-m3",
                format: ProviderFormat::Anthropic,
                port: None,
                auth_token: "sk-test",
            },
        )
        .unwrap();

        let raw = std::fs::read_to_string(&paths.settings_file).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let env = v.get("env").and_then(|o| o.as_object()).unwrap();
        assert_eq!(env.get("MY_CUSTOM_VAR").unwrap(), "leave-me-alone");
        assert_eq!(env.get(OWNERSHIP_MARKER).unwrap(), "1");
        // top-level keys are still there
        assert!(v.get("permissions").is_some());
    }

    #[test]
    fn openai_format_uses_localhost_base_url() {
        let dir = fresh_dir("openai-base-url");
        let paths = test_paths(&dir);
        let mut provider = make_provider();
        provider.format = ProviderFormat::OpenAI;
        provider.base_url = "https://upstream.invalid".into();

        turn_on(
            &paths,
            &TurnOnInputs {
                provider: &provider,
                model: "glm-5.2",
                format: ProviderFormat::OpenAI,
                port: Some(4188),
                auth_token: "sk-test",
            },
        )
        .unwrap();

        let raw = std::fs::read_to_string(&paths.settings_file).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let env = v.get("env").and_then(|o| o.as_object()).unwrap();
        assert_eq!(env.get("ANTHROPIC_BASE_URL").unwrap(), "http://127.0.0.1:4188");
    }

    #[test]
    fn peek_strips_crlf() {
        // Editor corruption / CRLF safety: a stray \r in the base URL
        // must not make `peek` claim `enabled = false` when the URL
        // would otherwise be opencode-go.
        let v: Value = serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://opencode.ai/zen/go\r",
                "ANTHROPIC_AUTH_TOKEN": "sk-x",
                "ANTHROPIC_MODEL": "minimax-m3",
                "__claude_go_owned": "1"
            }
        });
        let state = SettingsState::from_value(&v);
        assert!(state.enabled);
        assert_eq!(state.path_kind, PathKind::Anthropic);
    }

    #[test]
    fn env_not_object_errors() {
        let dir = fresh_dir("env-not-object");
        let paths = test_paths(&dir);
        let bad = serde_json::json!({"env": "not-an-object"});
        std::fs::create_dir_all(paths.settings_file.parent().unwrap()).unwrap();
        std::fs::write(
            &paths.settings_file,
            serde_json::to_vec_pretty(&bad).unwrap(),
        )
        .unwrap();
        let provider = make_provider();
        let err = turn_on(
            &paths,
            &TurnOnInputs {
                provider: &provider,
                model: "m",
                format: ProviderFormat::Anthropic,
                port: None,
                auth_token: "k",
            },
        )
        .unwrap_err();
        match err {
            ClaudeGoError::EnvNotObject { .. } => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
