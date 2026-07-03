use std::path::PathBuf;
use std::sync::Arc;

use crate::error::ClaudeGoError;
use crate::models;
use crate::paths::Paths;
use crate::provider::{self, is_opencode_go_openai_model, CustomProviderEntry, Model, Provider, ProviderFormat, CUSTOM_URL_ID};
use crate::settings::{self, SettingsState, TurnOnInputs};

/// The TUI's top-level state machine. Three screens, one input
/// buffer for the "Custom URL..." entry.
pub struct App {
    pub paths: Paths,
    pub screen: Screen,
    pub providers: Vec<Provider>,
    pub provider_index: usize,
    pub model_index: usize,
    pub models: Vec<Model>,
    pub models_from_live: bool,
    pub models_loading: bool,
    pub settings: SettingsState,
    /// Port of the in-process proxy, if one is running in this
    /// process. `None` when the user is on the Anthropic path or has
    /// not yet enabled routing. Mirrors the global `PROXY` OnceCell
    /// in `cli.rs`.
    pub proxy_port: Option<u16>,
    pub input_buffer: String,
    pub input_active: bool,
    pub input_prompt: &'static str,
    pub status_message: Option<String>,
    pub status_kind: StatusKind,
    pub verify_result: Option<VerifySnapshot>,
    pub should_quit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Provider,
    Model,
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct VerifySnapshot {
    pub outcome: crate::verify::VerifyOutcome,
    pub base_url: String,
    pub model: String,
    pub http_code: u16,
}

impl App {
    pub fn new(paths: Paths) -> Self {
        let providers = provider::provider_list(&provider::load_custom_providers(
            &paths.providers_file,
        ));
        let settings = SettingsState::peek(&paths).unwrap_or_else(|_| SettingsState::peek_default().unwrap_or_else(|_| SettingsState {
            enabled: false,
            base_url: String::new(),
            model: String::new(),
            path_kind: crate::settings::PathKind::Other,
            key_in_settings: false,
            owned: false,
        }));
        let proxy_port = crate::cli::current_proxy_port();
        let provider_index = Self::initial_provider_index(&providers, &settings);
        let mut app = Self {
            paths,
            screen: Screen::Provider,
            providers,
            provider_index,
            model_index: 0,
            models: Vec::new(),
            models_from_live: false,
            models_loading: false,
            settings,
            proxy_port,
            input_buffer: String::new(),
            input_active: false,
            input_prompt: "",
            status_message: None,
            status_kind: StatusKind::Info,
            verify_result: None,
            should_quit: false,
        };
        app.maybe_load_models_for_selection();
        app
    }

    /// Try to find the provider whose base URL matches the current
    /// settings, so the cursor lands on the active row on first
    /// render.
    fn initial_provider_index(
        providers: &[Provider],
        settings: &SettingsState,
    ) -> usize {
        if !settings.enabled {
            return 0;
        }
        for (i, p) in providers.iter().enumerate() {
            if p.base_url == settings.base_url {
                return i;
            }
        }
        0
    }

    pub fn selected_provider(&self) -> Option<&Provider> {
        self.providers.get(self.provider_index)
    }

    /// Move selection to the given provider index, clamp, and reload
    /// the model list if applicable.
    pub fn select_provider(&mut self, index: usize) {
        self.provider_index = index.min(self.providers.len().saturating_sub(1));
        self.model_index = 0;
        self.models.clear();
        self.models_loading = false;
        self.maybe_load_models_for_selection();
    }

    /// Kick off (or run synchronously) the model-list load for the
    /// currently selected provider. OpenCode Go is the only provider
    /// with a live source right now; everything else uses its preset
    /// model list directly.
    pub fn maybe_load_models_for_selection(&mut self) {
        let p = self.selected_provider().cloned();
        let Some(p) = p else {
            return;
        };
        match &p.model_source {
            crate::provider::ModelSource::Any => {
                self.models.clear();
                self.models_loading = false;
            }
            crate::provider::ModelSource::Fixed(list) => {
                self.models = list.clone();
                self.models_from_live = false;
                self.models_loading = false;
            }
            crate::provider::ModelSource::Live { fallback, .. } => {
                // Try the live fetch; on failure, use the hardcoded
                // fallback. We do this synchronously here because the
                // TUI is event-loop-driven; the fetch has a 5s
                // timeout, so the user sees a brief render delay at
                // worst.
                self.models_loading = true;
                let fallback = fallback.clone();
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(_) => {
                        self.models = fallback;
                        self.models_loading = false;
                        return;
                    }
                };
                let (models, from_live) = runtime.block_on(models::opencode_go_models());
                self.models = if models.is_empty() { fallback } else { models };
                self.models_from_live = from_live;
                self.models_loading = false;
            }
        }
    }

    /// Apply the current provider+model selection. Writes
    /// settings.json, starts the proxy if needed, and switches to the
    /// Status screen.
    pub fn apply_selection(&mut self) {
        let Some(p) = self.selected_provider().cloned() else {
            return;
        };
        if p.id == CUSTOM_URL_ID {
            self.begin_custom_url_input();
            return;
        }
        if !p.implemented {
            self.flash(
                format!("Provider `{}` is not yet implemented", p.display_name),
                StatusKind::Warn,
            );
            return;
        }

        // Pick the model id: from `models` if the provider has a list,
        // from the input buffer if the user just typed one for an
        // `Any` provider, or fail loud if neither.
        let model_id = if !self.models.is_empty() {
            self.models
                .get(self.model_index)
                .map(|m| m.id.clone())
                .unwrap_or_else(|| self.models[0].id.clone())
        } else if !self.input_buffer.trim().is_empty() {
            let s = self.input_buffer.trim().to_string();
            self.input_buffer.clear();
            s
        } else {
            self.flash(
                "No model selected. Type a model id or pick one from the list.".into(),
                StatusKind::Error,
            );
            return;
        };

        // The OpenCode Go provider serves both Anthropic- and
        // OpenAI-format models. Whether we need the proxy is a
        // function of the model, not the provider. For any other
        // provider we defer to the provider's own format.
        let needs_proxy = if p.id == "opencode-go" {
            is_opencode_go_openai_model(&model_id)
        } else {
            p.format == ProviderFormat::OpenAI
        };
        let format = if needs_proxy { ProviderFormat::OpenAI } else { ProviderFormat::Anthropic };

        let auth_token = std::env::var("OPENCODE_API_KEY").unwrap_or_default();
        if auth_token.is_empty() {
            self.flash(
                "OPENCODE_API_KEY is not set. Get one at https://opencode.ai/auth and re-run.".into(),
                StatusKind::Error,
            );
            return;
        }

        // If the resolved format needs the proxy, start the
        // in-process proxy now and pick a port. The TUI itself
        // becomes a long-lived holder; the user will quit to
        // shut it down (or run `claude-go off` in another terminal,
        // but that won't kill our proxy -- only this process can).
        let port = if format.needs_proxy() {
            // Idempotent: if PROXY is already set (we started one
            // earlier this session), reuse it.
            if let Some(p) = crate::cli::current_proxy_port() {
                Some(p)
            } else {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        self.flash(format!("async runtime: {e}"), StatusKind::Error);
                        return;
                    }
                };
                let upstream = crate::proxy::server::default_upstream();
                let api_key = std::env::var("OPENCODE_API_KEY").ok();
                let result = runtime.block_on(crate::proxy::start(upstream, api_key));
                match result {
                    Ok(handle) => {
                        let p = handle.port();
                        let _ = crate::cli::PROXY.set(Arc::new(handle));
                        Some(p)
                    }
                    Err(e) => {
                        self.flash(format!("Proxy start failed: {e}"), StatusKind::Error);
                        return;
                    }
                }
            }
        } else {
            None
        };

        if let Err(e) = settings::turn_on(
            &self.paths,
            &TurnOnInputs {
                provider: &p,
                model: &model_id,
                format,
                port,
                auth_token: &auth_token,
            },
        ) {
            self.flash(format!("settings.json write failed: {e}"), StatusKind::Error);
            return;
        }

        // Drop a marker file so `off` knows to stop the proxy.
        let _ = std::fs::create_dir_all(&self.paths.state_dir);
        let _ = std::fs::write(&self.paths.marker_file, b"");

        // Refresh state and switch screens.
        self.settings = SettingsState::peek(&self.paths).unwrap_or(self.settings.clone());
        self.proxy_port = crate::cli::current_proxy_port();
        self.screen = Screen::Status;
        self.flash(
            format!("Enabled: {} / {}", p.display_name, model_id),
            StatusKind::Info,
        );
    }

    /// Toggle routing on/off from the Status screen.
    pub fn toggle(&mut self) {
        if self.settings.enabled {
            self.turn_off();
        } else {
            self.apply_selection();
        }
    }

    pub fn turn_off(&mut self) {
        if let Err(e) = settings::turn_off(&self.paths) {
            self.flash(format!("turn off failed: {e}"), StatusKind::Error);
            return;
        }
        // Stop the in-process proxy if we started one. The TUI is
        // async-aware via the verify path, so we spin a small
        // runtime here to await stop().
        if self.proxy_port.is_some() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(rt) = runtime {
                rt.block_on(crate::cli::stop_proxy_if_running());
            }
        }
        let _ = std::fs::remove_file(&self.paths.marker_file);
        self.settings = SettingsState::peek(&self.paths).unwrap_or(SettingsState {
            enabled: false,
            base_url: String::new(),
            model: String::new(),
            path_kind: crate::settings::PathKind::Other,
            key_in_settings: false,
            owned: false,
        });
        self.proxy_port = None;
        self.flash("OpenCode Go routing disabled".into(), StatusKind::Info);
    }

    pub fn verify(&mut self) {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                self.flash(format!("async runtime: {e}"), StatusKind::Error);
                return;
            }
        };
        match runtime.block_on(crate::verify::verify(&self.paths)) {
            Ok(r) => {
                self.verify_result = Some(VerifySnapshot {
                    outcome: r.outcome,
                    base_url: r.base_url,
                    model: r.model,
                    http_code: r.http_code,
                });
                let kind = if r.outcome.is_ok() {
                    StatusKind::Info
                } else {
                    StatusKind::Error
                };
                self.flash(format!("{} (HTTP {})", r.outcome.message(), r.http_code), kind);
            }
            Err(e) => {
                self.flash(format!("verify: {e}"), StatusKind::Error);
            }
        }
    }

    pub fn refresh(&mut self) {
        self.settings = SettingsState::peek(&self.paths).unwrap_or(self.settings.clone());
        self.proxy_port = crate::cli::current_proxy_port();
    }

    /// Open the "Custom URL..." prompt.
    pub fn begin_custom_url_input(&mut self) {
        self.input_buffer.clear();
        self.input_prompt = "New provider URL: ";
        self.input_active = true;
    }

    /// Commit the "Custom URL..." input. Adds a custom Anthropic-format
    /// provider with the given URL.
    pub fn commit_custom_url(&mut self) {
        let url = self.input_buffer.trim().to_string();
        self.input_active = false;
        self.input_buffer.clear();
        if url.is_empty() {
            return;
        }
        // Auto-generate an id and a friendly display name from the URL.
        let id = sanitize_id(&url);
        let display = friendly_url_name(&url);
        let entry = CustomProviderEntry {
            name: display,
            base_url: url,
            format: ProviderFormat::Anthropic,
            auth_header: "x-api-key".into(),
            models: Vec::new(),
        };
        if let Err(e) = provider::add_custom_provider(&self.paths.providers_file, &id, entry) {
            self.flash(format!("add provider failed: {e}"), StatusKind::Error);
            return;
        }
        self.providers = provider::provider_list(&provider::load_custom_providers(
            &self.paths.providers_file,
        ));
        if let Some(idx) = self.providers.iter().position(|p| p.id == id) {
            self.select_provider(idx);
        }
        self.flash(format!("Added custom provider `{id}`"), StatusKind::Info);
    }

    /// Remove the currently-selected custom provider.
    pub fn remove_current_custom(&mut self) {
        let Some(p) = self.selected_provider().cloned() else {
            return;
        };
        if !p.is_custom {
            self.flash("Built-in providers cannot be removed".into(), StatusKind::Warn);
            return;
        }
        if let Err(e) = provider::remove_custom_provider(&self.paths.providers_file, &p.id) {
            self.flash(format!("remove provider failed: {e}"), StatusKind::Error);
            return;
        }
        self.providers = provider::provider_list(&provider::load_custom_providers(
            &self.paths.providers_file,
        ));
        if self.provider_index >= self.providers.len() {
            self.provider_index = self.providers.len().saturating_sub(1);
        }
        self.flash(format!("Removed custom provider `{}`", p.id), StatusKind::Info);
    }

    pub fn flash(&mut self, msg: String, kind: StatusKind) {
        self.status_message = Some(msg);
        self.status_kind = kind;
    }
}

fn sanitize_id(url: &str) -> String {
    url.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Build a short human-readable name from a URL: host + first path
/// segment if any. Falls back to the full URL if the host is empty.
fn friendly_url_name(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme);
    if host.is_empty() {
        return url.to_string();
    }
    host.to_string()
}

// Re-export for tests
pub fn _settings_disabled() -> SettingsState {
    SettingsState {
        enabled: false,
        base_url: String::new(),
        model: String::new(),
        path_kind: crate::settings::PathKind::Other,
        key_in_settings: false,
        owned: false,
    }
}

#[allow(dead_code)]
pub fn _error_chained() -> Result<(), ClaudeGoError> {
    Err(ClaudeGoError::MissingApiKey)
}

#[allow(dead_code)]
pub fn _path_helper(p: PathBuf) -> PathBuf {
    p
}
