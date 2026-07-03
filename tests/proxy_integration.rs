//! End-to-end tests for the in-process translation proxy.
//!
//! Each test starts a real proxy bound to `127.0.0.1:0` (OS-picked
//! port) and points it at a mock upstream axum server. The proxy
//! listens on its port, the mock upstream listens on a different
//! port. We assert on the JSON shapes that come out of the proxy's
//! `/v1/messages` endpoint.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use claude_go::proxy;

/// Spawn a mock upstream that records the last request body and
/// returns the configured response. Thread-safe via Arc<Mutex>.
async fn spawn_mock_upstream(
    upstream_response: Value,
    upstream_status: u16,
) -> (String, Arc<Mutex<Option<Value>>>) {
    spawn_mock_upstream_with_stream(upstream_response, upstream_status, None).await
}

/// Like `spawn_mock_upstream` but optionally takes a separate SSE
/// response for streaming requests. When `stream_response` is Some
/// and the request body has `stream: true`, returns that as SSE
/// instead of JSON.
async fn spawn_mock_upstream_with_stream(
    upstream_response: Value,
    upstream_status: u16,
    stream_response: Option<Value>,
) -> (String, Arc<Mutex<Option<Value>>>) {
    let recorded: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route(
            "/chat/completions",
            post({
                let recorded = recorded.clone();
                let resp = upstream_response.clone();
                let sresp = stream_response.clone();
                move |body: axum::Json<Value>| {
                    let recorded = recorded.clone();
                    let resp = resp.clone();
                    let sresp = sresp.clone();
                    async move {
                        *recorded.lock().await = Some(body.0.clone());
                        if body.0.get("stream").and_then(Value::as_bool) == Some(true) {
                            if let Some(s) = sresp {
                                let body = format!("data: {s}\n\ndata: [DONE]\n\n");
                                let resp: axum::response::Response =
                                    ([(header::CONTENT_TYPE, "text/event-stream")], body)
                                        .into_response();
                                return resp;
                            }
                        }
                        (StatusCode::OK, axum::Json(resp)).into_response()
                    }
                }
            }),
        )
        .route(
            "/messages",
            post({
                let recorded = recorded.clone();
                let resp = upstream_response.clone();
                let _ = upstream_status;
                move |body: axum::Json<Value>| {
                    let recorded = recorded.clone();
                    let resp = resp.clone();
                    async move {
                        *recorded.lock().await = Some(body.0);
                        (StatusCode::OK, axum::Json(resp))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{}", addr.port()), recorded)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

#[tokio::test]
async fn proxy_translates_anthropic_request_to_openai_request() {
    let upstream_resp = json!({
        "id": "chatcmpl-1",
        "model": "glm-5.2",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "hello back"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 4, "completion_tokens": 2}
    });
    let (upstream_url, recorded) = spawn_mock_upstream(upstream_resp, 200).await;
    let handle = proxy::start(upstream_url.clone(), Some("sk-test".into()))
        .await
        .expect("proxy starts");
    let url = format!("http://127.0.0.1:{}/v1/messages", handle.port());

    let resp = client()
        .post(&url)
        .header("x-api-key", "sk-test")
        .json(&json!({
            "model": "glm-5.2",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["content"][0]["type"], "text");
    assert_eq!(body["content"][0]["text"], "hello back");
    assert_eq!(body["stop_reason"], "end_turn");

    // The upstream saw a Chat Completions request.
    let recorded = recorded.lock().await.clone().expect("upstream saw body");
    assert_eq!(recorded["model"], "glm-5.2");
    assert_eq!(recorded["messages"][0]["role"], "user");
    assert_eq!(recorded["messages"][0]["content"], "hi");

    handle.stop().await;
}

#[tokio::test]
async fn proxy_returns_400_for_missing_messages() {
    let (upstream_url, _) = spawn_mock_upstream(json!({}), 200).await;
    let handle = proxy::start(upstream_url, Some("sk-test".into()))
        .await
        .expect("proxy starts");
    let url = format!("http://127.0.0.1:{}/v1/messages", handle.port());

    let resp = client()
        .post(&url)
        .header("x-api-key", "sk-test")
        .json(&json!({"model": "glm-5.2", "max_tokens": 16}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    handle.stop().await;
}

#[tokio::test]
async fn proxy_returns_401_when_no_api_key() {
    let (upstream_url, _) = spawn_mock_upstream(json!({}), 200).await;
    // No api_key in env / state.
    let handle = proxy::start(upstream_url, None).await.expect("proxy starts");
    let url = format!("http://127.0.0.1:{}/v1/messages", handle.port());

    let resp = client()
        .post(&url)
        .json(&json!({
            "model": "glm-5.2",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["error"]["type"], "authentication_error");
    handle.stop().await;
}

#[tokio::test]
async fn proxy_translates_tool_use_request_and_response() {
    let upstream_resp = json!({
        "id": "chatcmpl-1",
        "model": "glm-5.2",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"sf\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 4, "completion_tokens": 3}
    });
    let (upstream_url, recorded) = spawn_mock_upstream(upstream_resp, 200).await;
    let handle = proxy::start(upstream_url, Some("sk-test".into()))
        .await
        .expect("proxy starts");
    let url = format!("http://127.0.0.1:{}/v1/messages", handle.port());

    let resp = client()
        .post(&url)
        .header("x-api-key", "sk-test")
        .json(&json!({
            "model": "glm-5.2",
            "max_tokens": 64,
            "tools": [{
                "name": "get_weather",
                "description": "Get weather",
                "input_schema": {"type": "object"}
            }],
            "messages": [{"role": "user", "content": "what is the weather in sf?"}]
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["content"][0]["type"], "tool_use");
    assert_eq!(body["content"][0]["name"], "get_weather");
    assert_eq!(body["content"][0]["input"]["city"], "sf");
    assert_eq!(body["stop_reason"], "tool_use");

    // Upstream got the tools translated.
    let recorded = recorded.lock().await.clone().expect("upstream saw body");
    let tools = recorded["tools"].as_array().expect("tools array");
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["function"]["name"], "get_weather");
    handle.stop().await;
}

#[tokio::test]
async fn proxy_handles_streaming_text_response() {
    // Upstream returns a single OpenAI-style SSE chunk with [DONE].
    let upstream_resp = json!({});
    let stream_resp = json!({
        "id": "chatcmpl-1",
        "model": "glm-5.2",
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": "hi there"},
            "finish_reason": null
        }]
    });
    let (upstream_url, _) =
        spawn_mock_upstream_with_stream(upstream_resp, 200, Some(stream_resp)).await;
    let handle = proxy::start(upstream_url, Some("sk-test".into()))
        .await
        .expect("proxy starts");
    let url = format!("http://127.0.0.1:{}/v1/messages", handle.port());

    let resp = client()
        .post(&url)
        .header("x-api-key", "sk-test")
        .json(&json!({
            "model": "glm-5.2",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/event-stream")
    );
    let text = resp.text().await.expect("text");
    // The stream should contain at least: message_start, content_block_start,
    // content_block_delta, content_block_stop, message_delta, message_stop.
    assert!(text.contains("event: message_start"), "no message_start in: {text}");
    assert!(text.contains("event: content_block_delta"), "no delta in: {text}");
    assert!(text.contains("event: message_stop"), "no stop in: {text}");
    assert!(text.contains("hi there"));
    handle.stop().await;
}

#[tokio::test]
async fn proxy_health_endpoint_shape() {
    let (upstream_url, _) = spawn_mock_upstream(json!({}), 200).await;
    let handle = proxy::start(upstream_url, Some("sk-test".into()))
        .await
        .expect("proxy starts");
    let url = format!("http://127.0.0.1:{}/health", handle.port());

    let resp = client().get(&url).send().await.expect("health");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "ok");
    assert!(body["opencode_go_url"].is_string());
    assert!(body["timestamp"].is_string());
    handle.stop().await;
}

#[tokio::test]
async fn proxy_translates_anthropic_passthrough_for_minimax_m27() {
    // minimax-m2.7 is "Anthropic native" so the proxy should
    // forward to /messages unchanged.
    let upstream_resp = json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "ok"}],
        "model": "minimax-m2.7",
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let (upstream_url, recorded) = spawn_mock_upstream(upstream_resp, 200).await;
    let handle = proxy::start(upstream_url, Some("sk-test".into()))
        .await
        .expect("proxy starts");
    let url = format!("http://127.0.0.1:{}/v1/messages", handle.port());

    let resp = client()
        .post(&url)
        .header("x-api-key", "sk-test")
        .json(&json!({
            "model": "minimax-m2.7",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["role"], "assistant");
    // Upstream got the raw Anthropic body (not Chat Completions).
    let recorded = recorded.lock().await.clone().expect("upstream saw body");
    assert_eq!(recorded["model"], "minimax-m2.7");
    assert!(recorded["messages"].is_array());
    handle.stop().await;
}

#[tokio::test]
async fn proxy_handles_image_content() {
    let upstream_resp = json!({
        "id": "chatcmpl-1",
        "model": "glm-5.2",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "I see a red pixel."},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 100, "completion_tokens": 5}
    });
    let (upstream_url, recorded) = spawn_mock_upstream(upstream_resp, 200).await;
    let handle = proxy::start(upstream_url, Some("sk-test".into()))
        .await
        .expect("proxy starts");
    let url = format!("http://127.0.0.1:{}/v1/messages", handle.port());

    let resp = client()
        .post(&url)
        .header("x-api-key", "sk-test")
        .json(&json!({
            "model": "glm-5.2",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is this?"},
                {"type": "image", "source": {
                    "type": "base64", "media_type": "image/png", "data": "AAAA"
                }}
            ]}]
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["content"][0]["text"], "I see a red pixel.");

    // Upstream got image_url content parts.
    let recorded = recorded.lock().await.clone().expect("upstream saw body");
    let content = recorded["messages"][0]["content"].as_array().expect("content array");
    assert!(content
        .iter()
        .any(|c| c["type"] == "image_url" && c["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,AAAA")));
    handle.stop().await;
}
