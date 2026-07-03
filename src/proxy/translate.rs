//! Anthropic <-> OpenAI translation for the in-process proxy.
//!
//! This is a faithful Rust port of `opencode-api`'s `translate.js` and
//! `stream.js`. The intent is wire-compat with what the Node reference
//! produced: same JSON shapes, same SSE event sequence, same tool_use /
//! thinking / image handling.
//!
//! Two entry points:
//! - `build_openai_request` -- convert an Anthropic Messages request
//!   into an OpenAI Chat Completions request.
//! - `convert_to_anthropic_response` -- convert a non-streaming OpenAI
//!   response back into Anthropic Messages shape.
//!
//! Streaming lives in `super::stream` (the SSE state machine).

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Anthropic Messages shape (what Claude Code sends) ────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicRequest {
    pub model: String,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub messages: Vec<AnthropicMessage>,
    #[serde(default)]
    pub system: Option<Value>, // string OR [{type, text}, ...]
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub top_k: Option<u64>,
    #[serde(default)]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub metadata: Option<AnthropicMetadata>,
    #[serde(default)]
    pub thinking: Option<AnthropicThinking>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMetadata {
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicThinking {
    #[serde(rename = "type")]
    pub kind: String, // "enabled" or "disabled"
    #[serde(default)]
    pub budget_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: Value, // string OR Vec<AnthropicContentBlock>
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicContentBlock {
    #[serde(rename = "type")]
    pub kind: String, // "text" | "image" | "thinking" | "tool_use" | "tool_result"
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub source: Option<AnthropicImageSource>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub content: Option<Value>, // tool_result content (string or blocks)
    #[serde(default)]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicImageSource {
    #[serde(rename = "type")]
    pub kind: String, // "base64" or "url"
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub input_schema: Value,
}

// ── OpenAI Chat Completions shape (what the upstream speaks) ─────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIRequest {
    pub model: String,
    pub messages: Vec<OpenAIMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAITool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<Value>, // string OR Vec<Value> (multimodal parts)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAITool {
    #[serde(rename = "type")]
    pub kind: String, // "function"
    pub function: OpenAIToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIToolFunction {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIToolCall {
    /// Optional: only the first chunk of a tool call carries the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u64>,
    #[serde(rename = "type", default = "default_tool_call_type")]
    pub kind: String,
    pub function: OpenAIToolCallFunction,
}

fn default_tool_call_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIToolCallFunction {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>, // stringified JSON
}

// ── Anthropic response shape ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // "message"
    pub role: String, // "assistant"
    pub content: Vec<Value>, // Vec<{type, ...}>
    pub model: String,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<Value>,
    pub usage: AnthropicUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

// ── Translation: Anthropic request -> OpenAI request ────────────────

/// Build an OpenAI Chat Completions request from an Anthropic Messages
/// request. The `upstream_model` argument is the OpenCode-Go model id
/// to send upstream (usually the same as `req.model`).
pub fn build_openai_request(req: &AnthropicRequest, upstream_model: &str) -> OpenAIRequest {
    let model = upstream_model.to_string();
    let messages = convert_to_openai_messages(req, &model);

    // The reference adds enable_thinking=true when the client asks
    // for it OR when a DeepSeek model is being used and the prior
    // turns have reasoning_content. Without that, DeepSeek errors
    // out on the first turn with "reasoning_content must be passed
    // back".
    let has_reasoning_history = messages
        .iter()
        .any(|m| m.reasoning_content.as_deref().is_some_and(|s| !s.is_empty()));
    let enable_thinking = if req
        .thinking
        .as_ref()
        .map(|t| t.kind == "enabled")
        .unwrap_or(false)
    {
        Some(true)
    } else if model.to_lowercase().contains("deepseek") && has_reasoning_history {
        Some(true)
    } else {
        None
    };

    let user = req.metadata.as_ref().and_then(|m| m.user_id.clone());

    OpenAIRequest {
        model,
        messages,
        stream: req.stream,
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        top_p: req.top_p,
        top_k: req.top_k,
        stop: req.stop_sequences.clone(),
        user,
        tools: req.tools.as_ref().map(|ts| convert_to_openai_tools(ts)),
        tool_choice: req.tool_choice.as_ref().and_then(convert_tool_choice),
        enable_thinking,
    }
}

fn convert_to_openai_messages(req: &AnthropicRequest, model: &str) -> Vec<OpenAIMessage> {
    let mut out = Vec::new();
    let system = extract_system_message(req);
    if !system.is_empty() {
        out.push(OpenAIMessage {
            role: "system".into(),
            content: Some(Value::String(system)),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }
    for msg in &req.messages {
        if msg.role == "system" {
            // Some clients put system messages inside `messages` too;
            // they're already merged into the single system message
            // above, so skip.
            if let Some(s) = msg.content.as_str() {
                // If we didn't get a top-level system, this is the
                // only one. Re-emit.
                if out.is_empty() {
                    out.push(OpenAIMessage {
                        role: "system".into(),
                        content: Some(Value::String(s.to_string())),
                        reasoning_content: None,
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                }
            }
            continue;
        }
        if msg.role == "assistant" && msg.content.is_array() {
            let blocks = msg.content.as_array().cloned().unwrap_or_default();
            let mut text_blocks: Vec<String> = Vec::new();
            let mut thinking_blocks: Vec<String> = Vec::new();
            let mut tool_calls: Vec<OpenAIToolCall> = Vec::new();
            for block_val in blocks {
                let block: AnthropicContentBlock = match serde_json::from_value(block_val) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                match block.kind.as_str() {
                    "text" => {
                        if let Some(t) = block.text {
                            if !t.is_empty() {
                                text_blocks.push(t);
                            }
                        }
                    }
                    "thinking" => {
                        if let Some(t) = block.thinking {
                            thinking_blocks.push(t);
                        }
                    }
                    "tool_use" => {
                        if let (Some(id), Some(name)) = (block.id, block.name) {
                            let args = block
                                .input
                                .as_ref()
                                .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".into()))
                                .unwrap_or_else(|| "{}".into());
                            tool_calls.push(OpenAIToolCall {
                                id: Some(id),
                                index: Some(tool_calls.len() as u64),
                                kind: "function".into(),
                                function: OpenAIToolCallFunction {
                                    name: Some(name),
                                    arguments: Some(args),
                                },
                            });
                        }
                    }
                    _ => {}
                }
            }
            let combined_thinking = thinking_blocks.join("\n\n");
            out.push(OpenAIMessage {
                role: "assistant".into(),
                content: Some(Value::String(text_blocks.join("\n\n"))),
                reasoning_content: if combined_thinking.is_empty() {
                    None
                } else {
                    Some(combined_thinking)
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: None,
                name: None,
            });
            continue;
        }
        if msg.role == "user" && msg.content.is_array() {
            let blocks = msg.content.as_array().cloned().unwrap_or_default();
            let mut tool_result_blocks: Vec<(String, Value)> = Vec::new();
            let mut other_blocks: Vec<Value> = Vec::new();
            for block_val in blocks {
                let block: AnthropicContentBlock = match serde_json::from_value(block_val.clone()) {
                    Ok(b) => b,
                    Err(_) => {
                        other_blocks.push(block_val);
                        continue;
                    }
                };
                if block.kind == "tool_result" {
                    if let Some(id) = block.tool_use_id {
                        tool_result_blocks.push((id, block.content.unwrap_or(Value::Null)));
                    }
                } else {
                    other_blocks.push(block_val);
                }
            }
            // tool_result messages come first per the protocol.
            for (id, content) in tool_result_blocks {
                out.push(OpenAIMessage {
                    role: "tool".into(),
                    content: Some(content),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some(id),
                    name: None,
                });
            }
            if !other_blocks.is_empty() {
                let converted = convert_content_blocks(&other_blocks, model);
                out.push(OpenAIMessage {
                    role: "user".into(),
                    content: Some(converted),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
            continue;
        }
        // Plain string content (most common case).
        if let Some(s) = msg.content.as_str() {
            out.push(OpenAIMessage {
                role: msg.role.clone(),
                content: Some(Value::String(s.to_string())),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
            continue;
        }
        // Array content with no tool blocks (text + image only).
        if let Some(arr) = msg.content.as_array() {
            let converted = convert_content_blocks(arr, model);
            out.push(OpenAIMessage {
                role: msg.role.clone(),
                content: Some(converted),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }
    out
}

fn extract_system_message(req: &AnthropicRequest) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(system) = &req.system {
        if let Some(s) = system.as_str() {
            parts.push(s.to_string());
        } else if let Some(arr) = system.as_array() {
            for block_val in arr {
                let Ok(block) = serde_json::from_value::<AnthropicContentBlock>(block_val.clone())
                else {
                    continue;
                };
                if block.kind == "text" {
                    if let Some(t) = block.text {
                        parts.push(t);
                    }
                }
            }
        }
    }
    for msg in &req.messages {
        if msg.role == "system" {
            if let Some(s) = msg.content.as_str() {
                parts.push(s.to_string());
            }
        }
    }
    parts.join("\n")
}

fn convert_content_blocks(blocks: &[Value], model: &str) -> Value {
    let supports_vision = !super::no_vision(model);
    let has_image = blocks.iter().any(|b| {
        b.get("type").and_then(Value::as_str) == Some("image")
    });
    let mut parts: Vec<Value> = Vec::new();
    for block_val in blocks {
        let Ok(block) = serde_json::from_value::<AnthropicContentBlock>(block_val.clone()) else {
            continue;
        };
        match block.kind.as_str() {
            "text" => {
                if let Some(t) = block.text {
                    parts.push(serde_json::json!({"type": "text", "text": t}));
                }
            }
            "thinking" => {
                let t = block.thinking.unwrap_or_default();
                parts.push(serde_json::json!({"type": "text", "text": t}));
            }
            "image" => {
                if !supports_vision {
                    parts.push(serde_json::json!({
                        "type": "text",
                        "text": "ERROR: Image input is not supported for the selected model. Please choose a vision-capable model."
                    }));
                    continue;
                }
                let source = block.source.unwrap_or(AnthropicImageSource {
                    kind: "base64".into(),
                    media_type: None,
                    data: None,
                    url: None,
                });
                if source.kind == "url" {
                    if let Some(url) = source.url {
                        parts.push(serde_json::json!({
                            "type": "image_url",
                            "image_url": {"url": url}
                        }));
                    }
                } else {
                    let media_type = source.media_type.unwrap_or_else(|| "image/jpeg".into());
                    let data = source.data.unwrap_or_default();
                    parts.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{media_type};base64,{data}")}
                    }));
                }
            }
            _ => {}
        }
    }
    if has_image {
        Value::Array(parts)
    } else {
        // For text-only, flatten to a plain string for the OpenAI
        // Chat Completions content field.
        let joined = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n");
        Value::String(joined)
    }
}

fn convert_to_openai_tools(tools: &[AnthropicTool]) -> Vec<OpenAITool> {
    tools
        .iter()
        .map(|t| OpenAITool {
            kind: "function".into(),
            function: OpenAIToolFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.input_schema.clone(),
            },
        })
        .collect()
}

fn convert_tool_choice(tc: &Value) -> Option<Value> {
    let kind = tc.get("type").and_then(Value::as_str)?;
    match kind {
        "auto" => Some(Value::String("auto".into())),
        "any" => Some(Value::String("required".into())),
        "none" => Some(Value::String("none".into())),
        "tool" => {
            let name = tc.get("name").and_then(Value::as_str)?;
            Some(serde_json::json!({
                "type": "function",
                "function": {"name": name}
            }))
        }
        _ => None,
    }
}

// ── Translation: OpenAI response -> Anthropic response ──────────────

/// Translate a non-streaming OpenAI response into an Anthropic
/// Messages response. The `request_model` is the model id from the
/// incoming Anthropic request (so the response echoes what the user
/// asked for, not what the upstream called it).
pub fn convert_to_anthropic_response(
    openai: &OpenAIChatResponse,
    request_model: &str,
) -> AnthropicResponse {
    let choice = openai.choices.first();
    let content = choice
        .and_then(|c| c.message.as_ref())
        .and_then(|m| m.content.as_ref())
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let reasoning = choice
        .and_then(|c| c.message.as_ref())
        .and_then(|m| m.reasoning_content.as_ref())
        .cloned()
        .unwrap_or_default();

    let mut content_blocks: Vec<Value> = Vec::new();
    if !reasoning.is_empty() {
        content_blocks.push(serde_json::json!({
            "type": "thinking",
            "thinking": reasoning
        }));
    }
    if !content.is_empty() {
        content_blocks.push(serde_json::json!({
            "type": "text",
            "text": content
        }));
    }
    if let Some(tool_calls) = choice
        .and_then(|c| c.message.as_ref())
        .and_then(|m| m.tool_calls.as_ref())
    {
        for tc in tool_calls {
            let input = tc
                .function
                .arguments
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or_else(|| Value::Object(Default::default()));
            content_blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": tc.id.clone().unwrap_or_default(),
                "name": tc.function.name.clone().unwrap_or_default(),
                "input": input
            }));
        }
    }

    let finish_reason = choice.and_then(|c| c.finish_reason.clone());
    let stop_reason = match finish_reason.as_deref() {
        Some("tool_calls") => Some("tool_use".into()),
        Some("length") => Some("max_tokens".into()),
        Some("stop") => Some("end_turn".into()),
        Some("content_filter") => Some("stop_sequence".into()),
        Some(other) => Some(other.to_string()),
        None => Some("end_turn".into()),
    };

    let prompt_tokens = openai.usage.as_ref().and_then(|u| u.prompt_tokens).unwrap_or(0);
    let cached_tokens = openai
        .usage
        .as_ref()
        .and_then(|u| u.prompt_tokens_details.as_ref())
        .and_then(|d| d.cached_tokens)
        .unwrap_or(0);
    let completion_tokens = openai
        .usage
        .as_ref()
        .and_then(|u| u.completion_tokens)
        .unwrap_or(0);

    let usage = AnthropicUsage {
        input_tokens: prompt_tokens.saturating_sub(cached_tokens),
        output_tokens: completion_tokens,
        cache_read_input_tokens: if cached_tokens > 0 {
            Some(cached_tokens)
        } else {
            None
        },
    };

    AnthropicResponse {
        id: openai.id.clone(),
        kind: "message".into(),
        role: "assistant".into(),
        content: content_blocks,
        model: request_model.to_string(),
        stop_reason,
        stop_sequence: None,
        usage,
    }
}

// ── OpenAI response shape (non-streaming) ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIChatResponse {
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
    pub choices: Vec<OpenAIChoice>,
    #[serde(default)]
    pub usage: Option<OpenAIUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIChoice {
    pub index: u64,
    #[serde(default)]
    pub message: Option<OpenAIResponseMessage>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub delta: Option<OpenAIDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIResponseMessage {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenAIDelta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIUsage {
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub prompt_tokens_details: Option<OpenAIPromptTokensDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIPromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: Option<u64>,
}

// ── OpenAI streaming chunk ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIStreamChunk {
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
    pub choices: Vec<OpenAIChoice>,
    #[serde(default)]
    pub usage: Option<OpenAIUsage>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(extra: Value) -> AnthropicRequest {
        let r: AnthropicRequest = serde_json::from_value(json!({
            "model": "glm-5.2",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap();
        // merge in extras
        let v: Value = serde_json::to_value(&r).unwrap();
        let merged: Value = merge(v, extra);
        serde_json::from_value(merged).unwrap()
    }

    fn merge(a: Value, b: Value) -> Value {
        match (a, b) {
            (Value::Object(mut a), Value::Object(b)) => {
                for (k, v) in b {
                    a.insert(k, v);
                }
                Value::Object(a)
            }
            (_, b) => b,
        }
    }

    #[test]
    fn simple_text_request() {
        let r = req(json!({}));
        let o = build_openai_request(&r, "glm-5.2");
        assert_eq!(o.model, "glm-5.2");
        assert_eq!(o.messages.len(), 1);
        assert_eq!(o.messages[0].role, "user");
        assert_eq!(
            o.messages[0].content.as_ref().and_then(Value::as_str),
            Some("hi")
        );
    }

    #[test]
    fn system_message_extracted_to_first() {
        let r = req(json!({
            "system": "you are a helpful bot",
            "messages": [{"role": "user", "content": "ping"}]
        }));
        let o = build_openai_request(&r, "glm-5.2");
        assert_eq!(o.messages.len(), 2);
        assert_eq!(o.messages[0].role, "system");
        assert_eq!(
            o.messages[0].content.as_ref().and_then(Value::as_str),
            Some("you are a helpful bot")
        );
    }

    #[test]
    fn system_as_array_joins_text_blocks() {
        let r = req(json!({
            "system": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ],
            "messages": [{"role": "user", "content": "x"}]
        }));
        let o = build_openai_request(&r, "glm-5.2");
        assert_eq!(
            o.messages[0].content.as_ref().and_then(Value::as_str),
            Some("first\nsecond")
        );
    }

    #[test]
    fn assistant_tool_use_becomes_tool_calls() {
        let r = req(json!({
            "messages": [
                {"role": "user", "content": "what's the weather?"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "sf"}}
                ]}
            ]
        }));
        let o = build_openai_request(&r, "glm-5.2");
        assert_eq!(o.messages.len(), 2);
        let a = &o.messages[1];
        assert_eq!(a.role, "assistant");
        let tcs = a.tool_calls.as_ref().expect("tool_calls present");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id.as_deref(), Some("toolu_1"));
        assert_eq!(tcs[0].function.name.as_deref(), Some("get_weather"));
        let args: Value = serde_json::from_str(tcs[0].function.arguments.as_deref().unwrap()).unwrap();
        assert_eq!(args["city"], "sf");
    }

    #[test]
    fn user_tool_result_becomes_tool_role_message() {
        let r = req(json!({
            "messages": [
                {"role": "user", "content": "what's the weather?"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "sf"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "72F sunny"}
                ]}
            ]
        }));
        let o = build_openai_request(&r, "glm-5.2");
        assert_eq!(o.messages.len(), 3);
        let tool_msg = &o.messages[2];
        assert_eq!(tool_msg.role, "tool");
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("toolu_1"));
        assert_eq!(
            tool_msg.content.as_ref().and_then(Value::as_str),
            Some("72F sunny")
        );
    }

    #[test]
    fn image_base64_becomes_image_url_data() {
        let r = req(json!({
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is this?"},
                {"type": "image", "source": {
                    "type": "base64", "media_type": "image/png", "data": "AAA="
                }}
            ]}]
        }));
        let o = build_openai_request(&r, "glm-5.2");
        let arr = o.messages[0]
            .content
            .as_ref()
            .and_then(Value::as_array)
            .expect("array content for image");
        assert_eq!(arr.len(), 2);
        let url = arr[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,AAA="));
    }

    #[test]
    fn image_on_non_vision_model_becomes_error_text() {
        let r = req(json!({
            "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "x"}}
            ]}]
        }));
        let o = build_openai_request(&r, "deepseek-v4-pro");
        // For non-vision model, convert_content_blocks returns an
        // array of content parts (the image block becomes a text
        // error part). Same shape as the image case, just with an
        // error message instead of a data: URL.
        let arr = o.messages[0]
            .content
            .as_ref()
            .and_then(Value::as_array)
            .expect("array content for image (even on non-vision model)");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "text");
        assert!(
            arr[0]["text"]
                .as_str()
                .unwrap()
                .contains("Image input is not supported"),
            "expected error text, got: {arr:?}"
        );
    }

    #[test]
    fn tools_translate_to_openai_functions() {
        let r = req(json!({
            "tools": [{
                "name": "get_weather",
                "description": "Get the weather",
                "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
            }]
        }));
        let o = build_openai_request(&r, "glm-5.2");
        let tools = o.tools.expect("tools present");
        assert_eq!(tools[0].kind, "function");
        assert_eq!(tools[0].function.name, "get_weather");
        assert_eq!(
            tools[0].function.parameters["properties"]["city"]["type"],
            "string"
        );
    }

    #[test]
    fn tool_choice_any_becomes_required() {
        let r = req(json!({"tool_choice": {"type": "any"}}));
        let o = build_openai_request(&r, "glm-5.2");
        assert_eq!(
            o.tool_choice.as_ref().and_then(Value::as_str),
            Some("required")
        );
    }

    #[test]
    fn tool_choice_tool_with_name_becomes_named_function() {
        let r = req(json!({
            "tool_choice": {"type": "tool", "name": "get_weather"}
        }));
        let o = build_openai_request(&r, "glm-5.2");
        assert_eq!(
            o.tool_choice.as_ref().unwrap()["function"]["name"],
            "get_weather"
        );
    }

    #[test]
    fn deepseek_with_reasoning_history_sets_enable_thinking() {
        let r = req(json!({
            "messages": [
                {"role": "user", "content": "x"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "I should think about this."},
                    {"type": "text", "text": "ok"}
                ]}
            ]
        }));
        let o = build_openai_request(&r, "deepseek-v4-pro");
        assert_eq!(o.enable_thinking, Some(true));
    }

    #[test]
    fn deepseek_without_reasoning_history_does_not_set_enable_thinking() {
        // First-turn DeepSeek call: no prior reasoning. Reference
        // intentionally skips enable_thinking to avoid the
        // "reasoning_content must be passed back" error.
        let r = req(json!({
            "messages": [{"role": "user", "content": "x"}]
        }));
        let o = build_openai_request(&r, "deepseek-v4-pro");
        assert_eq!(o.enable_thinking, None);
    }

    #[test]
    fn thinking_enabled_explicitly_sets_enable_thinking() {
        let r = req(json!({
            "thinking": {"type": "enabled", "budget_tokens": 1024},
            "messages": [{"role": "user", "content": "x"}]
        }));
        let o = build_openai_request(&r, "glm-5.2");
        assert_eq!(o.enable_thinking, Some(true));
    }

    #[test]
    fn metadata_user_id_becomes_user() {
        let r = req(json!({"metadata": {"user_id": "user-123"}}));
        let o = build_openai_request(&r, "glm-5.2");
        assert_eq!(o.user.as_deref(), Some("user-123"));
    }

    #[test]
    fn max_tokens_and_temperature_passed_through() {
        let r = req(json!({"max_tokens": 256, "temperature": 0.7}));
        let o = build_openai_request(&r, "glm-5.2");
        assert_eq!(o.max_tokens, Some(256));
        assert_eq!(o.temperature, Some(0.7));
    }

    // ── Response translation ──────────────────────────────────────

    fn openai_resp_json(content: &str, finish_reason: &str) -> Value {
        json!({
            "id": "chatcmpl-1",
            "model": "glm-5.2",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": finish_reason
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "prompt_tokens_details": {"cached_tokens": 2}
            }
        })
    }

    #[test]
    fn simple_response_translates() {
        let r: OpenAIChatResponse = serde_json::from_value(openai_resp_json("hello", "stop")).unwrap();
        let a = convert_to_anthropic_response(&r, "glm-5.2");
        assert_eq!(a.role, "assistant");
        assert_eq!(a.content.len(), 1);
        assert_eq!(a.content[0]["text"], "hello");
        assert_eq!(a.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(a.usage.input_tokens, 8);
        assert_eq!(a.usage.output_tokens, 5);
        assert_eq!(a.usage.cache_read_input_tokens, Some(2));
    }

    #[test]
    fn tool_use_response_translates() {
        let v = json!({
            "id": "chatcmpl-2",
            "model": "glm-5.2",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "toolu_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"sf\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3}
        });
        let r: OpenAIChatResponse = serde_json::from_value(v).unwrap();
        let a = convert_to_anthropic_response(&r, "glm-5.2");
        assert_eq!(a.content.len(), 1);
        assert_eq!(a.content[0]["type"], "tool_use");
        assert_eq!(a.content[0]["name"], "get_weather");
        assert_eq!(a.content[0]["input"]["city"], "sf");
        assert_eq!(a.stop_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn reasoning_content_becomes_thinking_block() {
        let v = json!({
            "id": "chatcmpl-3",
            "model": "glm-5.2",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "answer",
                    "reasoning_content": "I should reason about this"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        });
        let r: OpenAIChatResponse = serde_json::from_value(v).unwrap();
        let a = convert_to_anthropic_response(&r, "glm-5.2");
        assert_eq!(a.content.len(), 2);
        assert_eq!(a.content[0]["type"], "thinking");
        assert_eq!(a.content[0]["thinking"], "I should reason about this");
        assert_eq!(a.content[1]["type"], "text");
    }

    #[test]
    fn empty_messages_in_request() {
        let r: AnthropicRequest = serde_json::from_value(json!({
            "model": "glm-5.2",
            "max_tokens": 16,
            "messages": []
        }))
        .unwrap();
        let o = build_openai_request(&r, "glm-5.2");
        assert!(o.messages.is_empty());
    }
}
