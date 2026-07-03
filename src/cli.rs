use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use once_cell::sync::OnceCell;

use crate::paths::Paths;
use crate::provider::{
    self, CustomProviderEntry, ProviderFormat, CUSTOM_URL_ID,
};
use crate::proxy::ProxyHandle;
use crate::settings::{self, SettingsState, TurnOnInputs};
use crate::tty;
use crate::verify;

/// Print the version banner. Single source of truth for the
/// `claude-go <version>` string -- both `--version`/`-V` (via clap's
/// `version` attribute) and the `version` subcommand route through
/// here so the two paths are guaranteed to produce identical output.
fn print_version() {
    println!("claude-go {}", env!("CARGO_PKG_VERSION"));
}

/// Process-wide handle to the in-process translation proxy. Set by
/// `cmd_on` for OpenAI-format models; used by the TUI's status view
/// and (in principle) by future subcommands that need to forward
/// requests. The handle's server task is dropped when the process
/// exits.
pub static PROXY: OnceCell<Arc<ProxyHandle>> = OnceCell::new();

/// Returns the bound port of the in-process proxy, if one is running
/// in this process. Read-only; the TUI uses it for the status view.
pub fn current_proxy_port() -> Option<u16> {
    PROXY.get().map(|h| h.port())
}

/// Returns true iff a proxy is currently running in this process.
pub fn proxy_running() -> bool {
    PROXY.get().is_some()
}

/// Stops the in-process proxy if one is running. Returns true if a
/// proxy was actually stopped.
pub async fn stop_proxy_if_running() -> bool {
    if let Some(handle) = PROXY.get() {
        handle.stop().await;
        true
    } else {
        false
    }
}

/// claude-go: route Claude Code to any Anthropic-compatible model.
#[derive(Debug, Parser)]
#[command(
    name = "claude-go",
    version,
    about = "Route Claude Code to any Anthropic-compatible model",
    long_about = "Route Claude Code to any Anthropic-compatible model. In a TTY, runs an interactive picker. Outside a TTY (pipes, cron, nohup), no-args prints status JSON for scripting."
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Enable routing. Writes ~/.claude/settings.json and starts the in-process proxy if needed.
    On {
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        port: Option<u16>,
    },
    /// Disable routing. Strips the owned env block. If the proxy is
    /// still running in this process, stops it too.
    Off,
    /// Show the current state.
    Status,
    /// Round-trip test against the live endpoint.
    Verify,
    /// List known models.
    Models,
    /// List configured providers.
    Providers,
    /// Provider registry management.
    Provider {
        #[command(subcommand)]
        cmd: ProviderCmd,
    },
    /// Install the current binary to ~/.local/bin.
    Install,
    /// Launch the TUI explicitly. Same as running `claude-go` with
    /// no arguments in a TTY.
    Tui,
    /// Print the version of claude-go.
    Version,
}

#[derive(Debug, Subcommand)]
pub enum ProviderCmd {
    /// Add a custom provider.
    Add {
        name: String,
        #[arg(long)]
        url: String,
        #[arg(long, default_value = "anthropic")]
        format: String,
        #[arg(long, default_value = "x-api-key")]
        auth_header: String,
        #[arg(long)]
        models: Vec<String>,
    },
    /// Remove a custom provider.
    Remove { name: String },
}

/// Run a CLI command. Returns the exit code (0 = success, non-zero =
/// user-facing error). This is `async` because the in-process proxy
/// needs a tokio runtime; the caller (main.rs) wraps it in
/// `Runtime::block_on`.
pub async fn run(cli: Cli) -> Result<i32> {
    let paths = Paths::resolve();
    let cmd = match cli.cmd {
        Some(c) => c,
        None => {
            // No-arg = TUI. The TTY gate already happened in main.rs.
            return run_tui(&paths);
        }
    };
    match cmd {
        Cmd::On { model, port } => cmd_on(&paths, model, port).await,
        Cmd::Off => cmd_off(&paths).await,
        Cmd::Status => cmd_status(&paths),
        Cmd::Verify => cmd_verify(&paths).await,
        Cmd::Models => cmd_models(),
        Cmd::Providers => cmd_providers(&paths),
        Cmd::Provider { cmd } => match cmd {
            ProviderCmd::Add {
                name,
                url,
                format,
                auth_header,
                models,
            } => cmd_provider_add(&paths, name, url, format, auth_header, models),
            ProviderCmd::Remove { name } => cmd_provider_remove(&paths, name),
        },
        Cmd::Install => cmd_install(&paths),
        Cmd::Tui => {
            // The explicit `tui` subcommand also requires a TTY.
            // If invoked without one, print the hint and exit 0.
            if !tty::should_launch_tui() {
                eprintln!("{}", tty::TTY_REQUIRED_HINT);
                return Ok(0);
            }
            run_tui(&paths)
        }
        Cmd::Version => {
            print_version();
            Ok(0)
        }
    }
}

fn run_tui(paths: &Paths) -> Result<i32> {
    let app = crate::tui::App::new(paths.clone());
    crate::tui::run(app)?;
    Ok(0)
}

async fn cmd_on(paths: &Paths, model: Option<String>, port: Option<u16>) -> Result<i32> {
    let auth = std::env::var("OPENCODE_API_KEY").unwrap_or_default();
    if auth.is_empty() {
        eprintln!("error: OPENCODE_API_KEY is not set. Get one at https://opencode.ai/auth and re-run.");
        return Ok(1);
    }

    let model = model.unwrap_or_else(|| "minimax-m3".into());
    let is_openai_format_model = is_known_openai_model(&model);

    // For OpenAI-format models, start the in-process proxy. The port
    // is OS-assigned (we ignore the `--port` arg for OpenAI-format,
    // because there's nothing to coordinate with a sibling process
    // anymore; the port lives only as long as this process does).
    let proxy_port = if is_openai_format_model {
        if port.is_some() {
            eprintln!("note: --port is ignored for OpenAI-format models; the in-process proxy picks its own port");
        }
        let upstream = crate::proxy::server::default_upstream();
        let api_key = std::env::var("OPENCODE_API_KEY").ok();
        let handle = crate::proxy::start(upstream, api_key)
            .await
            .map_err(crate::error::ClaudeGoError::ProxyBind)?;
        let p = handle.port();
        // Stash globally so `off` and the TUI can find it.
        let _ = PROXY.set(Arc::new(handle));
        Some(p)
    } else {
        if let Some(p) = port {
            if !((4141..=4242).contains(&p)) {
                eprintln!("error: invalid port: {p}");
                return Ok(1);
            }
        }
        None
    };

    // Build a synthetic "opencode-go" provider to feed into
    // `settings::turn_on`, since `on` is hardcoded to that provider
    // in the bash contract.
    let provider = provider::built_in_presets()
        .into_iter()
        .find(|p| p.id == "opencode-go")
        .map(|mut p| {
            if is_openai_format_model {
                p.format = ProviderFormat::OpenAI;
            }
            p
        })
        .ok_or_else(|| anyhow::anyhow!("opencode-go preset not found"))?;

    settings::turn_on(
        paths,
        &TurnOnInputs {
            provider: &provider,
            model: &model,
            format: if is_openai_format_model {
                ProviderFormat::OpenAI
            } else {
                ProviderFormat::Anthropic
            },
            port: proxy_port,
            auth_token: &auth,
        },
    )
    .context("write settings.json")?;

    println!("OpenCode Go routing enabled");
    println!("  Path:     {}", provider.format.as_str());
    println!("  Model:    {model}");
    if let Some(p) = proxy_port {
        println!("  Proxy:    http://127.0.0.1:{p} (in-process)");
        println!();
        println!("The proxy runs in this process. To stop it, run 'claude-go off'");
        println!("in this terminal, or send Ctrl-C / SIGINT to this process.");
        println!();
        println!("Claude Code is now pointed at http://127.0.0.1:{p}/v1/messages.");
        println!("You can launch it from another terminal:");
        println!("  claude");
    } else {
        println!("  Base:     {}", provider.base_url);
    }
    println!("  Auth:     OPENCODE_API_KEY env var");
    println!("  Config:   {}", paths.settings_file.display());

    // For OpenAI-format models, keep the proxy alive by parking
    // until SIGINT. The TUI's `verify` and Claude Code's outgoing
    // requests need the proxy to be live.
    if is_openai_format_model {
        wait_for_shutdown_signal().await;
        if let Some(handle) = PROXY.get() {
            handle.stop().await;
        }
        println!();
        println!("Proxy stopped. Claude Code will now fail to reach http://127.0.0.1:{}/v1/messages until you re-enable routing.", proxy_port.unwrap_or(0));
    }
    Ok(0)
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => {
                // Fall back to Ctrl-C handler.
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                sigint.recv().await;
                return;
            }
        };
        tokio::select! {
            _ = sigint.recv() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn cmd_off(paths: &Paths) -> Result<i32> {
    let was_enabled = SettingsState::peek(paths).map(|s| s.enabled).unwrap_or(false);
    settings::turn_off(paths).context("strip settings.json")?;
    if let Some(handle) = PROXY.get() {
        handle.stop().await;
        println!("proxy stopped");
    }
    if was_enabled {
        println!("OpenCode Go routing disabled. Claude Code will use the default Anthropic endpoint.");
    } else {
        println!("OpenCode Go routing was already disabled");
    }
    Ok(0)
}

fn cmd_status(paths: &Paths) -> Result<i32> {
    let s = SettingsState::peek(paths).context("read settings")?;
    if s.enabled {
        println!("OpenCode Go is ENABLED");
        println!("  State:    {}", path_kind_str(s.path_kind));
        println!("  Model:    {}", s.model);
        println!("  Base:     {}", s.base_url);
        println!(
            "  Auth:     {}",
            if std::env::var("OPENCODE_API_KEY").is_ok() {
                "OPENCODE_API_KEY env var".to_string()
            } else if s.key_in_settings {
                "ANTHROPIC_AUTH_TOKEN (from settings.json)".to_string()
            } else {
                "not set (verify will fail)".to_string()
            }
        );
        if matches!(s.path_kind, settings::PathKind::OpenAI) {
            match PROXY.get() {
                Some(handle) => {
                    println!("  Proxy:    running on http://127.0.0.1:{} (in-process)", handle.port());
                }
                None => {
                    println!("  Proxy:    EXPECTED BUT NOT RUNNING -- run: claude-go on");
                }
            }
        }
        println!("  Config:   {}", paths.settings_file.display());
    } else {
        println!("OpenCode Go is DISABLED");
        println!("  Endpoint: default Anthropic (api.anthropic.com)");
        println!("  Config:   {}", paths.settings_file.display());
    }
    Ok(0)
}

async fn cmd_verify(paths: &Paths) -> Result<i32> {
    match verify::verify(paths).await {
        Ok(r) => {
            println!("{}", r.outcome.message());
            println!("  Model:    {}", r.model);
            println!("  Base:     {}", r.base_url);
            println!("  HTTP:     {}", r.http_code);
            Ok(if r.outcome.is_ok() { 0 } else { 1 })
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(1)
        }
    }
}

fn cmd_models() -> Result<i32> {
    let preset = provider::built_in_presets()
        .into_iter()
        .find(|p| p.id == "opencode-go")
        .ok_or_else(|| anyhow::anyhow!("opencode-go preset not found"))?;
    let models = match &preset.model_source {
        provider::ModelSource::Live { fallback, .. } => fallback.clone(),
        _ => unreachable!(),
    };
    println!("{:<22} {:<10} DESCRIPTION", "MODEL", "PATH");
    println!("{:<22} {:<10} -----------", "-----", "----");
    for m in &models {
        let path = if m.description.contains("proxy") {
            "openai"
        } else {
            "anthropic"
        };
        println!("{:<22} {:<10} {}", m.id, path, m.description);
    }
    println!();
    println!("default model: minimax-m3 (anthropic path)");
    Ok(0)
}

fn cmd_providers(paths: &Paths) -> Result<i32> {
    let custom = provider::load_custom_providers(&paths.providers_file);
    let list = provider::provider_list(&custom);
    println!("{:<22} {:<10} {:<10} {}", "ID", "FORMAT", "AUTH", "BASE URL");
    println!("{:<22} {:<10} {:<10} {}", "--", "------", "----", "--------");
    for p in &list {
        println!(
            "{:<22} {:<10} {:<10} {}",
            p.id,
            p.format.as_str(),
            p.auth_header,
            if p.base_url.is_empty() {
                "(none)"
            } else {
                p.base_url.as_str()
            }
        );
    }
    Ok(0)
}

fn cmd_provider_add(
    paths: &Paths,
    name: String,
    url: String,
    format: String,
    auth_header: String,
    models: Vec<String>,
) -> Result<i32> {
    if name == CUSTOM_URL_ID {
        eprintln!("error: '{name}' is a reserved id");
        return Ok(1);
    }
    let format = match format.as_str() {
        "anthropic" => ProviderFormat::Anthropic,
        "openai" => ProviderFormat::OpenAI,
        other => {
            eprintln!("error: unknown format '{other}' (use 'anthropic' or 'openai')");
            return Ok(1);
        }
    };
    let entry = CustomProviderEntry {
        name: name.clone(),
        base_url: url,
        format,
        auth_header,
        models,
    };
    provider::add_custom_provider(&paths.providers_file, &name, entry)
        .context("write providers.json")?;
    println!("Added provider '{name}'");
    Ok(0)
}

fn cmd_provider_remove(paths: &Paths, name: String) -> Result<i32> {
    provider::remove_custom_provider(&paths.providers_file, &name)
        .context("write providers.json")?;
    println!("Removed provider '{name}'");
    Ok(0)
}

fn cmd_install(paths: &Paths) -> Result<i32> {
    let src = current_exe_path().context("locate current binary")?;
    let target = &paths.install_path;
    if same_file(&src, target).unwrap_or(false) {
        println!("Already installed at {}", target.display());
        return Ok(0);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).context("create ~/.local/bin")?;
    }
    std::fs::copy(&src, target).context("copy binary")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(target)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(target, perms)?;
    }
    println!("Installed: {}", target.display());
    if let Ok(which) = which("claude-go") {
        println!("Run: {which} --help");
    }
    Ok(0)
}

fn current_exe_path() -> Result<PathBuf> {
    std::env::current_exe().context("get current_exe")
}

fn same_file(a: &PathBuf, b: &PathBuf) -> std::io::Result<bool> {
    if !a.exists() || !b.exists() {
        return Ok(false);
    }
    let ma = std::fs::metadata(a)?;
    let mb = std::fs::metadata(b)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
    }
    #[cfg(not(unix))]
    {
        Ok(false)
    }
}

fn which(name: &str) -> Result<String> {
    let path = std::env::var_os("PATH").context("PATH not set")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Ok(cand.display().to_string());
        }
    }
    anyhow::bail!("'{name}' not found on PATH");
}

fn is_known_openai_model(model: &str) -> bool {
    provider::is_opencode_go_openai_model(model)
}

fn path_kind_str(k: settings::PathKind) -> &'static str {
    match k {
        settings::PathKind::Anthropic => "anthropic",
        settings::PathKind::OpenAI => "openai (via in-process proxy)",
        settings::PathKind::Other => "(none)",
    }
}
