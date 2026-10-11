use super::*;
use harness_protocol::backend::{ReasoningEffort, ResponseFormat};
use harness_protocol::ids::{MessageId, Timestamp};
use harness_protocol::messages::{ContentBlock, MessageRole};

#[test]
fn valid_tool_names_pass() {
    assert!(is_valid_anthropic_tool_name("web_fetch"));
    assert!(is_valid_anthropic_tool_name("agent_spawn"));
    assert!(is_valid_anthropic_tool_name("get-weather"));
    assert!(is_valid_anthropic_tool_name("ABC123_-"));
}

#[test]
fn invalid_tool_names_are_rejected() {
    assert!(!is_valid_anthropic_tool_name(""), "empty name");
    assert!(!is_valid_anthropic_tool_name("web.fetch"), "dot");
    assert!(!is_valid_anthropic_tool_name("Fetch URL"), "space");
    assert!(!is_valid_anthropic_tool_name("mcp:get_weather"), "colon");
    assert!(
        !is_valid_anthropic_tool_name(&"x".repeat(129)),
        "over 128 chars"
    );
}

#[test]
fn find_invalid_tool_names_reports_every_offender_not_just_the_first() {
    let tools = vec![
        ToolDescriptor {
            id: harness_protocol::ids::ToolId::new(),
            name: "read_file".to_string(),
            description: String::new(),
            input_schema: serde_json::json!({}),
        },
        ToolDescriptor {
            id: harness_protocol::ids::ToolId::new(),
            name: "mcp.weather.get-forecast".to_string(),
            description: String::new(),
            input_schema: serde_json::json!({}),
        },
        ToolDescriptor {
            id: harness_protocol::ids::ToolId::new(),
            name: "Fetch URL".to_string(),
            description: String::new(),
            input_schema: serde_json::json!({}),
        },
    ];
    let invalid = find_invalid_tool_names(&tools);
    assert_eq!(invalid, vec!["mcp.weather.get-forecast", "Fetch URL"]);
}

#[test]
fn no_thinking_requested_produces_no_thinking_block() {
    assert!(resolve_thinking(false, None, 8192).unwrap().is_none());
}

#[test]
fn bare_extended_thinking_uses_the_full_available_budget() {
    let thinking = resolve_thinking(true, None, 8192).unwrap().unwrap();
    assert_eq!(thinking.kind, "enabled");
    assert_eq!(thinking.budget_tokens, Some(8192 - 1024));
}

#[test]
fn an_explicit_reasoning_effort_scales_the_budget() {
    let full = 8192 - 1024;
    let low = resolve_thinking(false, Some(ReasoningEffort::Low), 8192)
        .unwrap()
        .unwrap();
    let medium = resolve_thinking(false, Some(ReasoningEffort::Medium), 8192)
        .unwrap()
        .unwrap();
    let high = resolve_thinking(false, Some(ReasoningEffort::High), 8192)
        .unwrap()
        .unwrap();
    assert_eq!(low.budget_tokens, Some(full / 4));
    assert_eq!(medium.budget_tokens, Some(full / 2));
    assert_eq!(high.budget_tokens, Some(full));
    assert!(low.budget_tokens < medium.budget_tokens);
    assert!(medium.budget_tokens < high.budget_tokens);
}

#[test]
fn reasoning_effort_alone_requests_thinking_even_without_the_extended_thinking_flag() {
    // Regression test: previously `reasoning_effort` was never read by
    // this crate at all, so a caller that only set `reasoning_effort`
    // (e.g. from a model reference's `::reasoning=` suffix, never
    // `extended_thinking`) got silently no thinking block, even though
    // the capability check let the request through.
    assert!(resolve_thinking(false, Some(ReasoningEffort::Low), 8192)
        .unwrap()
        .is_some());
}

#[test]
fn low_effort_budget_never_drops_below_the_1024_floor_on_a_small_max_tokens() {
    // full_budget = 2048 - 1024 = 1024; full_budget / 4 = 256, which the
    // `.max(1024)` floor must bring back up to 1024, clamped to not
    // exceed full_budget by the trailing `.min(full_budget)`.
    let low = resolve_thinking(false, Some(ReasoningEffort::Low), 2048)
        .unwrap()
        .unwrap();
    assert_eq!(low.budget_tokens, Some(1024));
}

#[test]
fn adaptive_only_models_are_recognized_in_anthropic_and_gateway_spellings() {
    for model in [
        "claude-opus-4-7",
        "claude-opus-4.7",
        "claude-opus-4-8-20260101",
        "claude-opus-5-5",
        "claude-sonnet-5.5",
        "claude-fable-5-1",
        "claude-mythos-preview",
    ] {
        assert!(requires_adaptive_thinking(model), "{model}");
    }
    for model in [
        "claude-opus-4-6",
        "claude-opus-4.1",
        "claude-sonnet-4-5-20250929",
        "claude-sonnet-4.5",
        "claude-haiku-4.5",
        "claude-sonnet-4-20250514",
        "claude-3-7-sonnet-latest",
        "gpt-5",
    ] {
        assert!(!requires_adaptive_thinking(model), "{model}");
    }
}

#[test]
fn adaptive_thinking_sends_no_budget_and_maps_the_effort() {
    let (thinking, output_config) =
        resolve_adaptive_thinking(false, Some(ReasoningEffort::Medium)).unwrap();
    let thinking = serde_json::to_value(thinking).unwrap();
    assert_eq!(thinking, serde_json::json!({"type": "adaptive"}));
    assert_eq!(output_config.unwrap().effort, "medium");

    let (_, output_config) = resolve_adaptive_thinking(true, None).unwrap();
    assert!(output_config.is_none());
    assert!(resolve_adaptive_thinking(false, None).is_none());
}

#[test]
fn recognizes_the_adaptive_thinking_rejection_relayed_by_copilot() {
    let body = r#"{"error":{"message":"HTTP 400 Bad Request: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"\\\"thinking.type.enabled\\\" is not supported for this model. Use \\\"thinking.type.adaptive\\\" and \\\"output_config.effort\\\" to control thinking behavior.\"}}"}}"#;
    assert!(is_adaptive_thinking_rejection(body));
    assert!(!is_adaptive_thinking_rejection(
        "max_tokens: 128000 > 64000, which is the maximum allowed"
    ));
}

#[test]
fn thinking_requires_max_tokens_at_least_2048() {
    assert!(matches!(
        resolve_thinking(true, None, 2047),
        Err(ModelError::InvalidRequest { .. })
    ));
    assert!(matches!(
        resolve_thinking(false, Some(ReasoningEffort::Low), 1024),
        Err(ModelError::InvalidRequest { .. })
    ));
}

/// An assistant message with no content blocks (a reasoning-only turn
/// recorded by an older core) must be omitted: the Messages API rejects
/// empty `content`, and there is nothing in it to send.
#[test]
fn a_content_less_assistant_message_is_omitted_from_the_wire() {
    let message = |role: MessageRole, content: Vec<ContentBlock>| AgentMessage {
        id: MessageId::new(),
        role,
        content,
        created_at: Timestamp::now(),
    };
    let messages = [
        message(
            MessageRole::User,
            vec![ContentBlock::Text { text: "hi".into() }],
        ),
        message(MessageRole::Assistant, Vec::new()),
        message(
            MessageRole::Assistant,
            vec![ContentBlock::Text {
                text: String::new(),
            }],
        ),
        message(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "again".into(),
            }],
        ),
    ];
    let converted = convert_messages(&messages);
    assert_eq!(converted.len(), 2);
    assert!(converted
        .iter()
        .all(|message| matches!(message.role, AnthropicRole::User)));
}

/// M4: an image content block must reach the wire as a real base64
/// `image` block — not be silently dropped, which was the pre-M4
/// behavior (`ContentBlock::Image { .. } => None`).
#[test]
fn image_content_block_becomes_a_real_anthropic_image_block() {
    let message = AgentMessage {
        id: MessageId::new(),
        role: MessageRole::User,
        content: vec![ContentBlock::Image {
            mime_type: "image/png".to_string(),
            data: vec![1, 2, 3, 4],
        }],
        created_at: Timestamp::now(),
    };

    let converted = agent_message_to_anthropic(&message);
    assert_eq!(
        converted.content.len(),
        1,
        "image block must not be dropped"
    );
    match &converted.content[0] {
        AnthropicContentBlock::Image { source } => {
            assert_eq!(source.kind, "base64");
            assert_eq!(source.media_type, "image/png");
            assert_eq!(
                source.data,
                base64::engine::general_purpose::STANDARD.encode([1, 2, 3, 4])
            );
        }
        other => panic!("expected an Image block, got {other:?}"),
    }
}

const FIXTURE: &str = r#"event: message_start
data: {"message":{"model":"claude-sonnet-4-20250513","usage":{"input_tokens":2,"output_tokens":0}}}

event: content_block_delta
data: {"index":0,"delta":{"type":"text_delta","text":"hi"}}

event: message_delta
data: {"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}

event: message_stop
data: {"type":"message_stop"}

"#;

#[test]
fn incremental_parser_handles_single_byte_chunks() {
    let mut parser = AnthropicSseParser::new();
    let mut events = Vec::new();
    for byte in FIXTURE.as_bytes() {
        events.extend(
            parser
                .push_chunk(std::slice::from_ref(byte))
                .expect("valid chunk"),
        );
    }
    let (terminal, result) = parser.finish().expect("complete fixture");
    events.extend(terminal);

    assert!(events
        .iter()
        .any(|event| { matches!(event, ModelEvent::TextDelta { delta } if delta == "hi") }));
    assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
    assert_eq!(result.usage.input_tokens.value(), Some(2));
    assert_eq!(result.usage.output_tokens.value(), Some(1));
    assert_eq!(result.usage.total_tokens.value(), Some(3));
}

#[test]
fn parser_rejects_a_stream_without_message_stop() {
    let mut parser = AnthropicSseParser::new();
    parser
        .push_chunk(b"event: ping\ndata: {}\n\n")
        .expect("ping parses");
    let error = parser
        .finish()
        .expect_err("must reject a stream missing message_stop");
    assert!(matches!(error, ModelError::StreamInterrupted { .. }));
    assert!(
        error.is_retryable(),
        "a dropped connection, not malformed data, should be retried"
    );
}

#[test]
fn parser_accepts_message_stop_with_empty_data() {
    let mut parser = AnthropicSseParser::new();
    parser
        .push_chunk(b"event: message_stop\n\n")
        .expect("message_stop parses");
    let (events, _) = parser.finish().expect("must accept empty message_stop");
    assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
}

#[test]
fn parser_accepts_data_done_sentinel() {
    let mut parser = AnthropicSseParser::new();
    parser
        .push_chunk(b"data: [DONE]\n\n")
        .expect("[DONE] parses");
    let (events, _) = parser.finish().expect("must accept [DONE]");
    assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
}

#[test]
fn parser_accepts_data_only_message_stop() {
    let mut parser = AnthropicSseParser::new();
    parser
        .push_chunk(b"data: {\"type\":\"message_stop\"}\n\n")
        .expect("data-only message_stop parses");
    let (events, _) = parser.finish().expect("must accept data-only message_stop");
    assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
}

#[test]
fn parser_handles_non_streaming_json_message_fallback() {
    let json_body = serde_json::json!({
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            { "type": "text", "text": "Hello from OpenCode Haiku" }
        ],
        "model": "claude-3-5-haiku-20241022",
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 12,
            "output_tokens": 6
        }
    });
    let mut parser = AnthropicSseParser::new();
    parser
        .push_chunk(json_body.to_string().as_bytes())
        .expect("json body chunked");
    let (events, result) = parser.finish().expect("finish parses non-streaming json");
    assert!(events.iter().any(
        |e| matches!(e, ModelEvent::TextDelta { delta } if delta == "Hello from OpenCode Haiku")
    ));
    assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
    assert_eq!(result.usage.input_tokens.value(), Some(12));
    assert_eq!(result.usage.output_tokens.value(), Some(6));
}

// -----------------------------------------------------------------
// Structured output (emulated — Anthropic has no `response_format`)
// -----------------------------------------------------------------

#[test]
fn json_schema_becomes_a_forced_single_purpose_tool() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "title": { "type": "string" } },
        "required": ["title"],
    });
    let (tool, choice) = structured_output_tool(&ResponseFormat::JsonSchema {
        name: "task_graph".into(),
        schema: schema.clone(),
        strict: true,
    })
    .expect("a schema format must produce a tool");

    assert_eq!(tool.name, STRUCTURED_OUTPUT_TOOL);
    // The caller's schema is the tool's input schema verbatim — that is
    // the whole mechanism by which the response conforms.
    assert_eq!(tool.input_schema, schema);
    assert!(
        tool.description.contains("task_graph"),
        "the schema name should reach the model as a hint: {}",
        tool.description
    );
    assert_eq!(
        choice,
        AnthropicToolChoice::Tool {
            name: STRUCTURED_OUTPUT_TOOL.into()
        },
        "the tool must be forced, not merely offered"
    );
}

#[test]
fn text_format_needs_no_emulation() {
    assert!(structured_output_tool(&ResponseFormat::Text).is_none());
}

#[test]
fn json_object_constrains_only_to_an_object() {
    let (tool, _) =
        structured_output_tool(&ResponseFormat::JsonObject).expect("must produce a tool");
    assert_eq!(tool.input_schema, serde_json::json!({ "type": "object" }));
}

/// The important one: the synthetic tool's input must reach the caller as
/// assistant **text**. If it leaked through as a tool call, the runtime
/// would try to execute a tool that was never registered, and a caller
/// that asked for JSON would receive empty text.
#[test]
fn structured_output_tool_input_streams_back_as_text_not_a_tool_call() {
    let mut parser = AnthropicSseParser::new();
    let mut events = Vec::new();

    for chunk in [
            format!(
                "event: content_block_start\ndata: {{\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"{STRUCTURED_OUTPUT_TOOL}\",\"input\":{{}}}}}}\n\n"
            ),
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"title\\\":\"}}\n\n".to_string(),
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"hi\\\"}\"}}\n\n".to_string(),
            "event: content_block_stop\ndata: {\"index\":0}\n\n".to_string(),
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n".to_string(),
            "event: message_stop\ndata: {}\n\n".to_string(),
        ] {
            events.extend(parser.push_chunk(chunk.as_bytes()).expect("chunk parses"));
        }

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::TextDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, r#"{"title":"hi"}"#,
        "the tool input should reassemble as the response text"
    );

    assert!(
        !events.iter().any(|event| matches!(
            event,
            ModelEvent::ToolCallStarted { .. }
                | ModelEvent::ToolCallDelta { .. }
                | ModelEvent::ToolCallCompleted { .. }
        )),
        "the synthetic tool must not surface as a tool call: {events:?}"
    );
}

/// A *real* tool call in the same stream must keep behaving normally —
/// the suppression is keyed on the synthetic tool's name, not on
/// "any tool call while a response format is set".
#[test]
fn a_real_tool_call_is_unaffected_by_the_structured_output_path() {
    let mut parser = AnthropicSseParser::new();
    let mut events = Vec::new();

    for chunk in [
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"fs.read\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        ] {
            events.extend(parser.push_chunk(chunk.as_bytes()).expect("chunk parses"));
        }

    assert!(
        events.iter().any(
            |event| matches!(event, ModelEvent::ToolCallStarted { name, .. } if name == "fs.read")
        ),
        "a real tool call must still start: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ModelEvent::ToolCallCompleted { .. })),
        "a real tool call must still complete: {events:?}"
    );
}
