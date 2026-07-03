//! In-process translation proxy.
//!
//! Wraps the axum server (`server.rs`), the request/response
//! translation (`translate.rs`), and the SSE state machine (`stream.rs`)
//! behind a single `ProxyHandle` that the rest of the app can hold
//! and stop.
//!
//! Lifecycle: `start` binds to `127.0.0.1:0` (OS picks a free port),
//! spawns the server as a `tokio::task`, and returns a handle. The
//! task lives as long as the runtime does. `stop` is best-effort:
//! waits up to 2s for in-flight requests, then drops the receiver
//! (which causes axum to return `Connection closed` and exit).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::Notify;

pub mod server;
pub mod stream;
pub mod translate;

/// Models that speak Anthropic Messages natively and are forwarded
/// without translation. From `opencode-api`'s `config.js`.
const ANTHROPIC_NATIVE: &[&str] = &["minimax-m2.7", "minimax-m2.5"];

/// Models that do NOT support image input. From `opencode-api`'s
/// `config.js`.
const NO_VISION_SET: &[&str] = &["deepseek-v4-pro", "deepseek-v4-flash"];

pub fn is_anthropic_native(model: &str) -> bool {
    ANTHROPIC_NATIVE.contains(&model)
}

pub fn no_vision(model: &str) -> bool {
    NO_VISION_SET.contains(&model)
}

/// Handle to a running proxy. The handle is `Send + Sync` so it can
/// be parked in a `OnceCell` or carried across CLI subcommands.
#[derive(Clone)]
pub struct ProxyHandle {
    pub port: u16,
    /// Wakes the server task so it can shut down gracefully.
    shutdown: Arc<Notify>,
    /// The server's abort handle. `JoinHandle` is not `Clone`, but
    /// `AbortHandle` is, and that's all we need to cancel the task.
    abort: Option<tokio::task::AbortHandle>,
}

impl ProxyHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Best-effort stop. Sends the shutdown signal, waits up to 2s
    /// for the server task to drain, then aborts. Idempotent. Takes
    /// `&self` so callers holding `Arc<ProxyHandle>` can stop without
    /// moving out of the Arc.
    pub async fn stop(&self) {
        self.shutdown.notify_waiters();
        // The graceful path: yield repeatedly to let axum's
        // with_graceful_shutdown future exit. We don't need to await
        // the JoinHandle -- abort() below will reap it either way.
        // A 2s deadline is plenty for an idle proxy.
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Some(abort) = &self.abort {
            abort.abort();
        }
    }
}

/// Start the proxy. `upstream_base` is the OpenAI/Anthropic endpoint
/// root; `api_key` is the bearer to forward. Returns a handle that
/// can be `.stop()`-ed.
pub async fn start(upstream_base: String, api_key: Option<String>) -> Result<ProxyHandle, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("bind 127.0.0.1:0: {e}"))?;
    let local: SocketAddr = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?;
    let port = local.port();

    let state = server::ProxyState::new(upstream_base, api_key);
    let router = server::router(state);
    let shutdown = Arc::new(Notify::new());

    let shutdown_signal = shutdown.clone();
    let join = tokio::spawn(async move {
        let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
            shutdown_signal.notified().await;
        });
        if let Err(e) = serve.await {
            eprintln!("claude-go: proxy server error: {e}");
        }
    });
    let abort = join.abort_handle();

    // Give the server a beat to actually start accepting. The
    // axum::serve future returns the first time it's polled, which
    // is essentially instantaneous, but a too-fast caller can race
    // the first connection. Yield once to the runtime.
    tokio::task::yield_now().await;

    Ok(ProxyHandle {
        port,
        shutdown,
        abort: Some(abort),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Spin up a real proxy in the current tokio runtime and return
    /// the handle plus the local URL base.
    pub async fn start_for_test() -> ProxyHandle {
        start(
            "http://127.0.0.1:1".into(), // unused for the no-upstream test
            Some("sk-test".into()),
        )
        .await
        .expect("proxy should start")
    }

    #[tokio::test]
    async fn handle_port_reflects_bound_port() {
        let h = start_for_test().await;
        assert!(h.port() > 0);
        h.stop().await;
    }

    #[tokio::test]
    async fn health_endpoint_responds_ok() {
        let h = start_for_test().await;
        let url = format!("http://127.0.0.1:{}/health", h.port());
        let resp = reqwest::get(&url).await.expect("health request");
        assert!(resp.status().is_success());
        let v: serde_json::Value = resp.json().await.expect("health json");
        assert_eq!(v["status"], "ok");
        h.stop().await;
    }

    #[tokio::test]
    async fn messages_without_messages_array_returns_400() {
        let h = start_for_test().await;
        let url = format!("http://127.0.0.1:{}/v1/messages", h.port());
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .json(&json!({"model": "glm-5.2", "max_tokens": 16}))
            .send()
            .await
            .expect("messages request");
        assert_eq!(resp.status(), 400);
        let v: serde_json::Value = resp.json().await.expect("error json");
        assert_eq!(v["error"]["type"], "invalid_request_error");
        h.stop().await;
    }

    #[tokio::test]
    async fn messages_without_api_key_returns_401() {
        // Re-start with api_key = None.
        let h = start("http://127.0.0.1:1".into(), None).await.unwrap();
        let url = format!("http://127.0.0.1:{}/v1/messages", h.port());
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .json(&json!({"model": "glm-5.2", "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .expect("messages request");
        assert_eq!(resp.status(), 401);
        let v: serde_json::Value = resp.json().await.expect("error json");
        assert_eq!(v["error"]["type"], "authentication_error");
        h.stop().await;
    }
}
