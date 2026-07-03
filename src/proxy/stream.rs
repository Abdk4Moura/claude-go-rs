//! SSE state machine: OpenAI Chat Completions stream -> Anthropic
//! Messages stream events.
//!
//! Faithful port of `opencode-api`'s `stream.js`. Emits the full
//! event sequence: `message_start`, `content_block_start`,
//! `content_block_delta` (one or more), `content_block_stop`,
//! `message_delta` (with usage + stop_reason), `message_stop`.

use serde_json::{json, Value};

use super::translate::OpenAIStreamChunk;

#[derive(Debug, Default, Clone)]
pub struct StreamState {
    pub message_start_sent: bool,
    pub content_block_index: u64,
    pub content_block_open: bool,
    pub current_block_type: Option<String>,
    pub finished: bool,
    /// OpenAI tool_call index -> our tracking info
    pub tool_calls: std::collections::BTreeMap<u64, ToolCallTrack>,
}

#[derive(Debug, Clone)]
pub struct ToolCallTrack {
    pub id: String,
    pub name: String,
    pub anthropic_block_index: u64,
}

fn is_tool_block_open(state: &StreamState) -> bool {
    if !state.content_block_open {
        return false;
    }
    if state.current_block_type.as_deref() != Some("tool") {
        return false;
    }
    state
        .tool_calls
        .values()
        .any(|tc| tc.anthropic_block_index == state.content_block_index)
}

fn close_content_block(state: &mut StreamState) -> Option<Value> {
    if !state.content_block_open {
        return None;
    }
    state.content_block_open = false;
    state.current_block_type = None;
    Some(json!({
        "type": "content_block_stop",
        "index": state.content_block_index
    }))
}

fn map_openai_stop_reason_to_anthropic(reason: Option<&str>) -> &'static str {
    match reason {
        Some("stop") => "end_turn",
        Some("length") => "max_tokens",
        Some("tool_calls") => "tool_use",
        Some("content_filter") => "stop_sequence",
        _ => "end_turn",
    }
}

/// Translate one OpenAI stream chunk into zero or more Anthropic SSE
/// events. The caller writes them out as `event: <type>\ndata: <json>\n\n`.
pub fn translate_chunk(chunk: &OpenAIStreamChunk, state: &mut StreamState) -> Vec<Value> {
    let mut events: Vec<Value> = Vec::new();

    if chunk.choices.is_empty() {
        return events;
    }
    let choice = &chunk.choices[0];
    let delta = match &choice.delta {
        Some(d) => d,
        None => return events,
    };

    // message_start: first time we see a chunk with a delta.
    if !state.message_start_sent {
        let prompt_tokens = chunk.usage.as_ref().and_then(|u| u.prompt_tokens).unwrap_or(0);
        let cached_tokens = chunk
            .usage
            .as_ref()
            .and_then(|u| u.prompt_tokens_details.as_ref())
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0);
        let mut usage = json!({
            "input_tokens": prompt_tokens.saturating_sub(cached_tokens),
            "output_tokens": 0
        });
        if cached_tokens > 0 {
            usage["cache_read_input_tokens"] = json!(cached_tokens);
        }
        let message = json!({
            "id": chunk.id,
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": chunk.model,
            "stop_reason": null,
            "stop_sequence": null,
            "usage": usage
        });
        events.push(json!({"type": "message_start", "message": message}));
        state.message_start_sent = true;
    }

    // reasoning_content -> thinking block
    if let Some(reasoning) = &delta.reasoning_content {
        if !reasoning.is_empty() {
            if state.content_block_open && state.current_block_type.as_deref() != Some("thinking") {
                if let Some(close) = close_content_block(state) {
                    state.content_block_index += 1;
                    events.push(close);
                }
            }
            if !state.content_block_open {
                events.push(json!({
                    "type": "content_block_start",
                    "index": state.content_block_index,
                    "content_block": {"type": "thinking", "thinking": ""}
                }));
                state.content_block_open = true;
                state.current_block_type = Some("thinking".into());
            }
            events.push(json!({
                "type": "content_block_delta",
                "index": state.content_block_index,
                "delta": {"type": "thinking_delta", "thinking": reasoning}
            }));
        }
    }

    // text content
    if let Some(content) = &delta.content {
        if !content.is_empty() {
            if state.content_block_open && state.current_block_type.as_deref() == Some("thinking") {
                if let Some(close) = close_content_block(state) {
                    state.content_block_index += 1;
                    events.push(close);
                }
            }
            if is_tool_block_open(state) {
                if let Some(close) = close_content_block(state) {
                    state.content_block_index += 1;
                    events.push(close);
                }
            }
            if !state.content_block_open {
                events.push(json!({
                    "type": "content_block_start",
                    "index": state.content_block_index,
                    "content_block": {"type": "text", "text": ""}
                }));
                state.content_block_open = true;
                state.current_block_type = Some("text".into());
            }
            events.push(json!({
                "type": "content_block_delta",
                "index": state.content_block_index,
                "delta": {"type": "text_delta", "text": content}
            }));
        }
    }

    // tool calls
    if let Some(tool_calls) = &delta.tool_calls {
        for tc in tool_calls {
            // tc.id is Option<String> (only first chunk carries it),
            // tc.function.name is Option<String> (same).
            if let (Some(id), Some(name)) = (tc.id.clone(), tc.function.name.clone()) {
                if state.content_block_open {
                    if let Some(close) = close_content_block(state) {
                        state.content_block_index += 1;
                        events.push(close);
                    }
                }
                let index = state.content_block_index;
                let slot = tc.index.unwrap_or(0);
                state.tool_calls.insert(
                    slot,
                    ToolCallTrack {
                        id: id.clone(),
                        name: name.clone(),
                        anthropic_block_index: index,
                    },
                );
                events.push(json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {}
                    }
                }));
                state.content_block_open = true;
                state.current_block_type = Some("tool".into());
            }
            if let Some(args) = &tc.function.arguments {
                let slot = tc.index.unwrap_or(0);
                if let Some(track) = state.tool_calls.get(&slot) {
                    events.push(json!({
                        "type": "content_block_delta",
                        "index": track.anthropic_block_index,
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": args
                        }
                    }));
                }
            }
        }
    }

    // finish_reason -> close blocks + message_delta + message_stop
    if let Some(finish) = &choice.finish_reason {
        if state.content_block_open {
            events.push(json!({
                "type": "content_block_stop",
                "index": state.content_block_index
            }));
            state.content_block_open = false;
            state.current_block_type = None;
        }
        let prompt_tokens = chunk.usage.as_ref().and_then(|u| u.prompt_tokens).unwrap_or(0);
        let cached_tokens = chunk
            .usage
            .as_ref()
            .and_then(|u| u.prompt_tokens_details.as_ref())
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0);
        let completion_tokens = chunk
            .usage
            .as_ref()
            .and_then(|u| u.completion_tokens)
            .unwrap_or(0);
        let mut usage = json!({
            "input_tokens": prompt_tokens.saturating_sub(cached_tokens),
            "output_tokens": completion_tokens
        });
        if cached_tokens > 0 {
            usage["cache_read_input_tokens"] = json!(cached_tokens);
        }
        events.push(json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": map_openai_stop_reason_to_anthropic(Some(finish.as_str())),
                "stop_sequence": null
            },
            "usage": usage
        }));
        events.push(json!({"type": "message_stop"}));
        state.finished = true;
    }

    events
}

/// Emit the final closing events when the upstream stream ends without
/// sending `finish_reason`. Returns the events to write; if
/// `message_start` was never sent, an error event is emitted.
pub fn finalize_on_stream_end(state: &StreamState) -> Vec<Value> {
    let mut events: Vec<Value> = Vec::new();
    if state.message_start_sent && !state.finished {
        if state.content_block_open {
            events.push(json!({
                "type": "content_block_stop",
                "index": state.content_block_index
            }));
        }
        events.push(json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "end_turn",
                "stop_sequence": null
            },
            "usage": {"input_tokens": 0, "output_tokens": 0}
        }));
        events.push(json!({"type": "message_stop"}));
    } else if !state.message_start_sent {
        if state.content_block_open {
            events.push(json!({
                "type": "content_block_stop",
                "index": state.content_block_index
            }));
        }
        events.push(json!({
            "type": "error",
            "error": {
                "type": "api_error",
                "message": "Upstream returned empty stream"
            }
        }));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(d: Value) -> OpenAIStreamChunk {
        serde_json::from_value(d).unwrap()
    }

    #[test]
    fn message_start_then_text() {
        let mut s = StreamState::default();
        let c = chunk(json!({
            "id": "chatcmpl-1",
            "model": "glm-5.2",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "hi"}}]
        }));
        let ev = translate_chunk(&c, &mut s);
        let kinds: Vec<&str> = ev.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["message_start", "content_block_start", "content_block_delta"]);
        let delta_text = ev[2]["delta"]["text"].as_str().unwrap();
        assert_eq!(delta_text, "hi");
    }

    #[test]
    fn text_then_finish_emits_full_sequence() {
        let mut s = StreamState::default();
        let _ = translate_chunk(
            &chunk(json!({
                "id": "x", "model": "glm-5.2",
                "choices": [{"index": 0, "delta": {"content": "hello"}}]
            })),
            &mut s,
        );
        let ev = translate_chunk(
            &chunk(json!({
                "id": "x", "model": "glm-5.2",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            })),
            &mut s,
        );
        let kinds: Vec<&str> = ev.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["content_block_stop", "message_delta", "message_stop"]);
        assert_eq!(ev[1]["delta"]["stop_reason"], "end_turn");
        assert!(s.finished);
    }

    #[test]
    fn reasoning_then_text_closes_thinking_block() {
        let mut s = StreamState::default();
        let _ = translate_chunk(
            &chunk(json!({
                "id": "x", "model": "glm-5.2",
                "choices": [{"index": 0, "delta": {"reasoning_content": "I think."}}]
            })),
            &mut s,
        );
        let ev = translate_chunk(
            &chunk(json!({
                "id": "x", "model": "glm-5.2",
                "choices": [{"index": 0, "delta": {"content": "answer"}}]
            })),
            &mut s,
        );
        let kinds: Vec<&str> = ev.iter().map(|e| e["type"].as_str().unwrap()).collect();
        // First close the thinking block, then open text, then delta.
        assert_eq!(kinds, vec!["content_block_stop", "content_block_start", "content_block_delta"]);
    }

    #[test]
    fn tool_call_arguments_arrive_in_two_deltas() {
        let mut s = StreamState::default();
        // First delta: opens tool_use block.
        let _ = translate_chunk(
            &chunk(json!({
                "id": "x", "model": "glm-5.2",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0, "id": "call_1", "type": "function",
                    "function": {"name": "get_weather", "arguments": ""}
                }]}}]
            })),
            &mut s,
        );
        // Second delta: argument fragment.
        let ev = translate_chunk(
            &chunk(json!({
                "id": "x", "model": "glm-5.2",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0,
                    "function": {"arguments": "{\"city\":\"sf\"}"}
                }]}}]
            })),
            &mut s,
        );
        assert_eq!(ev[0]["type"], "content_block_delta");
        assert_eq!(ev[0]["delta"]["type"], "input_json_delta");
        assert_eq!(ev[0]["delta"]["partial_json"], "{\"city\":\"sf\"}");
    }

    #[test]
    fn tool_call_finish_reason_becomes_tool_use() {
        let mut s = StreamState::default();
        let _ = translate_chunk(
            &chunk(json!({
                "id": "x", "model": "glm-5.2",
                "choices": [{"index": 0, "delta": {"tool_calls": [{
                    "index": 0, "id": "call_1", "type": "function",
                    "function": {"name": "f", "arguments": "{}"}
                }]}}]
            })),
            &mut s,
        );
        let ev = translate_chunk(
            &chunk(json!({
                "id": "x", "model": "glm-5.2",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
            })),
            &mut s,
        );
        assert_eq!(ev[1]["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn empty_stream_emits_error() {
        let s = StreamState::default();
        let ev = finalize_on_stream_end(&s);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["type"], "error");
    }

    #[test]
    fn stream_without_finish_emits_end_turn_close() {
        let mut s = StreamState::default();
        let _ = translate_chunk(
            &chunk(json!({
                "id": "x", "model": "glm-5.2",
                "choices": [{"index": 0, "delta": {"content": "hi"}}]
            })),
            &mut s,
        );
        let ev = finalize_on_stream_end(&s);
        let kinds: Vec<&str> = ev.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["content_block_stop", "message_delta", "message_stop"]);
        assert_eq!(ev[1]["delta"]["stop_reason"], "end_turn");
    }
}
