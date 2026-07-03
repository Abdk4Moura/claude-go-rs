//! axum-based HTTP server for the in-process translation proxy.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::stream::{translate_chunk, StreamState};
use super::translate::{
    build_openai_request, convert_to_anthropic_response, AnthropicRequest, OpenAIChatResponse,
};

/// Default upstream. Matches the Node reference.
const DEFAULT_UPSTREAM_BASE: &str = "https://opencode.ai/zen/go/v1";

#[derive(Clone)]
pub struct ProxyState {
    pub upstream_base: String,
    pub api_key: Option<String>,
    pub http: reqwest::Client,
}

impl ProxyState {
    pub fn new(upstream_base: String, api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client builds");
        Self {
            upstream_base,
            api_key,
            http,
        }
    }
}

pub fn router(state: ProxyState) -> Router {
    Router::new()
        .route("/v1/messages", post(handle_messages))
        .route("/health", get(handle_health))
        .with_state(Arc::new(state))
}

async fn handle_health(State(state): State<Arc<ProxyState>>) -> impl IntoResponse {
    axum::Json(json!({
        "status": "ok",
        "opencode_go_url": state.upstream_base,
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}

async fn handle_messages(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    body: axum::Json<Value>,
) -> Response {
    let req: AnthropicRequest = match serde_json::from_value(body.0) {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, "invalid_request_error", format!("invalid request body: {e}")),
    };

    if req.messages.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "messages is required".to_string(),
        );
    }

    // Resolve API key: header > env.
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(str::to_string)
        })
        .or_else(|| state.api_key.clone());
    let Some(api_key) = api_key else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "API key required. Set x-api-key header or OPENCODE_API_KEY env var.".to_string(),
        );
    };

    let model = req.model.clone();
    let is_anthropic = super::is_anthropic_native(&model);
    let stream = req.stream.unwrap_or(false);

    if is_anthropic {
        // Pass-through to upstream /messages. The provider speaks
        // Anthropic natively, so we forward the body unchanged.
        let url = format!(
            "{}/messages",
            state.upstream_base.trim_end_matches('/')
        );
        if stream {
            match forward_anthropic_stream(&state.http, &url, &api_key, &req).await {
                Ok(resp) => resp,
                Err(e) => error_response(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    format!("upstream error: {e}"),
                ),
            }
        } else {
            match forward_anthropic(&state.http, &url, &api_key, &req).await {
                Ok(resp) => resp,
                Err(e) => error_response(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    format!("upstream error: {e}"),
                ),
            }
        }
    } else {
        // OpenAI-format: translate request, forward, translate response.
        let openai_req = build_openai_request(&req, &model);
        let url = format!(
            "{}/chat/completions",
            state.upstream_base.trim_end_matches('/')
        );
        if stream {
            match forward_openai_stream(&state.http, &url, &api_key, &openai_req, &req).await {
                Ok(resp) => resp,
                Err(e) => error_response(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    format!("upstream error: {e}"),
                ),
            }
        } else {
            match forward_openai(&state.http, &url, &api_key, &openai_req, &req).await {
                Ok(resp) => resp,
                Err(e) => error_response(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    format!("upstream error: {e}"),
                ),
            }
        }
    }
}

async fn forward_anthropic(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    req: &AnthropicRequest,
) -> Result<Response, String> {
    let resp = http
        .post(url)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {api_key}"))
        .header("x-api-key", api_key)
        .header("x-anthropic-version", "2023-06-01")
        .json(req)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Ok(translate_error_response(status, &bytes));
    }
    let mut out = Response::builder().status(status);
    out = out.header(header::CONTENT_TYPE, "application/json");
    Ok(out.body(Body::from(bytes)).unwrap())
}

async fn forward_anthropic_stream(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    req: &AnthropicRequest,
) -> Result<Response, String> {
    let mut body = serde_json::to_value(req).map_err(|e| e.to_string())?;
    if let Some(obj) = body.as_object_mut() {
        obj.insert("stream".into(), json!(true));
    }
    let resp = http
        .post(url)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {api_key}"))
        .header("x-api-key", api_key)
        .header("x-anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        return Ok(translate_error_response(status, &bytes));
    }
    // Pipe bytes through as SSE. The upstream already speaks
    // Anthropic SSE, so this is a byte-for-byte copy.
    let stream = resp.bytes_stream();
    let (tx, rx) = mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        let mut s = stream;
        while let Some(chunk) = s.next().await {
            match chunk {
                Ok(b) => {
                    if tx.send(Ok(b)).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let body = Body::from_stream(ReceiverStream::new(rx));
    Ok(Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .unwrap())
}

async fn forward_openai(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    openai_req: &super::translate::OpenAIRequest,
    original: &AnthropicRequest,
) -> Result<Response, String> {
    let resp = http
        .post(url)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {api_key}"))
        .json(openai_req)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Ok(translate_error_response(status, &bytes));
    }
    let openai: OpenAIChatResponse =
        serde_json::from_slice(&bytes).map_err(|e| format!("bad upstream json: {e}"))?;
    let anth = convert_to_anthropic_response(&openai, &original.model);
    let body = serde_json::to_vec(&anth).map_err(|e| e.to_string())?;
    Ok(Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap())
}

async fn forward_openai_stream(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    openai_req: &super::translate::OpenAIRequest,
    _original: &AnthropicRequest,
) -> Result<Response, String> {
    let mut body = serde_json::to_value(openai_req).map_err(|e| e.to_string())?;
    if let Some(obj) = body.as_object_mut() {
        obj.insert("stream".into(), json!(true));
    }
    let resp = http
        .post(url)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        return Ok(translate_error_response(status, &bytes));
    }
    let upstream = resp.bytes_stream();
    let (tx, rx) = mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        let mut s = upstream;
        let mut state = StreamState::default();
        let mut buffer = String::new();
        let mut pending_done = false;
        while let Some(chunk) = s.next().await {
            let Ok(bytes) = chunk else { break };
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(idx) = buffer.find('\n') {
                let line: String = buffer.drain(..=idx).collect();
                let line = line.trim_end_matches('\n').to_string();
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if !trimmed.starts_with("data: ") {
                    continue;
                }
                let data = &trimmed[6..];
                if data == "[DONE]" {
                    if !state.finished {
                        for evt in super::stream::finalize_on_stream_end(&state) {
                            let k = evt["type"].as_str().unwrap_or("event").to_string();
                            send_sse(&tx, &k, evt);
                        }
                    }
                    pending_done = true;
                    continue;
                }
                let parsed: super::translate::OpenAIStreamChunk = match serde_json::from_str(data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                for evt in translate_chunk(&parsed, &mut state) {
                    let k = evt["type"].as_str().unwrap_or("event").to_string();
                    send_sse(&tx, &k, evt);
                }
            }
        }
        // Stream ended without [DONE].
        if !pending_done {
            for evt in super::stream::finalize_on_stream_end(&state) {
                let k = evt["type"].as_str().unwrap_or("event").to_string();
                send_sse(&tx, &k, evt);
            }
        }
        drop(tx);
    });
    let body = Body::from_stream(ReceiverStream::new(rx));
    Ok(Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .unwrap())
}

fn send_sse(tx: &mpsc::Sender<Result<bytes::Bytes, std::io::Error>>, event: &str, data: Value) {
    let payload = match serde_json::to_string(&data) {
        Ok(s) => s,
        Err(_) => return,
    };
    let s = format!("event: {event}\ndata: {payload}\n\n");
    // Use blocking send via `try_send` (since we're not in an async
    // context here, and the channel has plenty of buffer space).
    // If the receiver is gone (e.g. client disconnected), just drop
    // the message; the task will exit on its own.
    let _ = tx.try_send(Ok(bytes::Bytes::from(s)));
}

fn error_response(status: StatusCode, kind: &str, message: String) -> Response {
    let body = json!({
        "error": {"type": kind, "message": message}
    });
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn translate_error_response(status: StatusCode, body: &[u8]) -> Response {
    let body_str = String::from_utf8_lossy(body).to_string();
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let mut message = body_str.clone();
    let mut kind = "api_error".to_string();
    if let Some(v) = parsed {
        if let Some(err) = v.get("error") {
            if let Some(m) = err.get("message").and_then(Value::as_str) {
                message = m.to_string();
            }
            if let Some(t) = err.get("type").and_then(Value::as_str) {
                kind = translate_error_type(t).to_string();
            } else if let Some(t) = err.get("code").and_then(Value::as_str) {
                kind = translate_error_type(t).to_string();
            }
        }
    }
    let payload = json!({
        "error": {"type": kind, "message": message}
    });
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap()
}

fn translate_error_type(openai_kind: &str) -> &'static str {
    match openai_kind {
        "invalid_request_error" => "invalid_request_error",
        "authentication_error" => "authentication_error",
        "permission_error" => "permission_error",
        "not_found" => "not_found",
        "rate_limit_error" | "rate_limit" => "rate_limit_error",
        "insufficient_quota" => "permission_error",
        "context_length_exceeded" => "invalid_request_error",
        _ => "api_error",
    }
}

pub fn default_upstream() -> String {
    std::env::var("OPENCODE_GO_BASE_URL").unwrap_or_else(|_| DEFAULT_UPSTREAM_BASE.to_string())
}
