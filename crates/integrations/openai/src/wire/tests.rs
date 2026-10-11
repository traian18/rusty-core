use super::*;

/// M4/M4.5: proves `OpenAiRequest`'s JSON field names/shape match what
/// the Chat Completions API actually expects (`max_tokens`,
/// `temperature`, `stop` as a bare array, `model` at top level) — a
/// wire-format regression here would silently produce a request the
/// real API either ignores or rejects, which no test at the
/// `GenericModelBackend` layer (M4.1's contract suite) can catch, since
/// that layer stops at `ModelRequest`, one level above this JSON.
#[test]
fn request_serializes_with_the_expected_openai_field_names() {
    let request = OpenAiRequest {
        model: "gpt-4.1".to_string(),
        messages: vec![],
        tools: None,
        max_tokens: Some(4096),
        temperature: Some(0.5),
        stop: Some(vec!["STOP".to_string()]),
        response_format: None,
        reasoning_effort: None,
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
    };
    let json = serde_json::to_value(&request).expect("serialize OpenAiRequest");
    assert_eq!(json["model"], "gpt-4.1");
    assert_eq!(json["max_tokens"], 4096);
    assert_eq!(json["temperature"], 0.5);
    assert_eq!(json["stop"], serde_json::json!(["STOP"]));
    assert_eq!(json["stream"], true);
    // Omitted-when-None fields must actually be absent, not `null`,
    // since some OpenAI-compatible backends (this wire format is also
    // used by `openai-compatible`) reject unexpected null fields.
    let bare = OpenAiRequest {
        model: "gpt-4.1".to_string(),
        messages: vec![],
        tools: None,
        max_tokens: None,
        temperature: None,
        stop: None,
        response_format: None,
        reasoning_effort: None,
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
    };
    let bare_json = serde_json::to_value(&bare).expect("serialize bare OpenAiRequest");
    assert!(bare_json.get("max_tokens").is_none());
    assert!(bare_json.get("temperature").is_none());
    assert!(bare_json.get("stop").is_none());
}

/// `response_format` must serialize exactly as the Chat Completions API
/// spells it — a wrong shape here is accepted by serde and rejected by
/// the provider at request time.
#[test]
fn json_schema_response_format_serializes_in_the_openai_shape() {
    let schema = serde_json::json!({ "type": "object" });
    let format = OpenAiResponseFormat::from_neutral(
        &harness_protocol::backend::ResponseFormat::JsonSchema {
            name: "task_graph".into(),
            schema: schema.clone(),
            strict: true,
        },
    )
    .expect("a schema format must serialize");

    let json = serde_json::to_value(&format).expect("serialize response_format");
    assert_eq!(json["type"], "json_schema");
    assert_eq!(json["json_schema"]["name"], "task_graph");
    assert_eq!(json["json_schema"]["schema"], schema);
    assert_eq!(json["json_schema"]["strict"], true);
}

#[test]
fn json_object_response_format_serializes_as_a_bare_type_tag() {
    let format =
        OpenAiResponseFormat::from_neutral(&harness_protocol::backend::ResponseFormat::JsonObject)
            .expect("json_object must serialize");
    assert_eq!(
        serde_json::to_value(&format).expect("serialize"),
        serde_json::json!({ "type": "json_object" })
    );
}

/// `Text` maps to `None` so the field is omitted rather than sent as an
/// explicit `{"type":"text"}` — OpenAI-compatible servers vary in what
/// they accept, and text is the default everywhere.
#[test]
fn text_response_format_is_omitted_from_the_request() {
    assert!(
        OpenAiResponseFormat::from_neutral(&harness_protocol::backend::ResponseFormat::Text)
            .is_none()
    );
}

fn user_message(content: Vec<ContentBlock>) -> AgentMessage {
    AgentMessage {
        id: harness_protocol::ids::MessageId::new(),
        role: MessageRole::User,
        content,
        created_at: harness_protocol::ids::Timestamp::now(),
    }
}

/// M4: a text-only message must keep serializing `content` as a bare
/// string (the pre-image-support shape) rather than always paying for
/// the more verbose typed-parts array form.
#[test]
fn a_text_only_message_serializes_content_as_a_plain_string() {
    let message = user_message(vec![ContentBlock::Text {
        text: "hello".into(),
    }]);
    let openai = agent_message_to_openai(&message, &HashMap::new());
    let json = serde_json::to_value(&openai[0]).expect("serialize OpenAiMessage");
    assert_eq!(json["content"], "hello");
}

/// M4: an image content block must convert into a real
/// `image_url`/`data:` part, not be silently dropped — matching
/// Anthropic's existing image pass-through, previously OpenAI-specific
/// wire support for it did not exist even though the client already
/// advertised `images: true` in its capabilities.
#[test]
fn an_image_block_becomes_a_real_image_url_part_not_silently_dropped() {
    let message = user_message(vec![
        ContentBlock::Text {
            text: "what is this?".into(),
        },
        ContentBlock::Image {
            mime_type: "image/png".into(),
            data: vec![1, 2, 3],
        },
    ]);
    let openai = agent_message_to_openai(&message, &HashMap::new());
    let json = serde_json::to_value(&openai[0]).expect("serialize OpenAiMessage");
    let parts = json["content"]
        .as_array()
        .expect("multimodal content must serialize as an array");
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "what is this?");
    assert_eq!(parts[1]["type"], "image_url");
    let url = parts[1]["image_url"]["url"]
        .as_str()
        .expect("image_url.url must be a string");
    assert!(
        url.starts_with("data:image/png;base64,"),
        "unexpected image_url shape: {url}"
    );
    assert!(url.ends_with(&base64::engine::general_purpose::STANDARD.encode([1, 2, 3])));
}

fn assistant_message(content: Vec<ContentBlock>) -> AgentMessage {
    AgentMessage {
        role: MessageRole::Assistant,
        ..user_message(content)
    }
}

/// A transcript can carry an assistant message with no text and no tool
/// calls (a turn that only produced reasoning, recorded by an older
/// core). Its wire form, `{"role":"assistant"}`, is rejected by Cohere
/// behind OpenRouter with "must have non-empty content or tool calls",
/// so it must be omitted rather than sent.
#[test]
fn a_content_less_assistant_message_is_omitted_from_the_wire() {
    let messages = [
        user_message(vec![ContentBlock::Text { text: "hi".into() }]),
        assistant_message(Vec::new()),
        assistant_message(vec![ContentBlock::Text {
            text: String::new(),
        }]),
        user_message(vec![ContentBlock::Text {
            text: "again".into(),
        }]),
    ];
    let openai = convert_messages_with_tool_ids(&messages, &HashMap::new());
    let roles: Vec<&str> = openai.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["user", "user"]);
}

/// The guard must not touch assistant messages that do say something:
/// tool-call-only turns keep their (content-less) message, since the
/// tool calls are what the following `tool` messages answer.
#[test]
fn a_tool_call_only_assistant_message_is_still_sent() {
    let call = harness_protocol::tools::ToolCall {
        id: harness_protocol::ids::ToolCallId::new(),
        name: "search".into(),
        arguments: serde_json::json!({"q": "x"}),
    };
    let message = assistant_message(vec![ContentBlock::ToolUse { call }]);
    let openai = agent_message_to_openai(&message, &HashMap::new());
    assert_eq!(openai.len(), 1);
    let json = serde_json::to_value(&openai[0]).expect("serialize OpenAiMessage");
    assert!(json.get("content").is_none());
    assert_eq!(json["tool_calls"][0]["function"]["name"], "search");
}

const FIXTURE: &str = "data: {\"id\":\"gen-fixture\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n\
data: {\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n\
data: {\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"model\":\"gpt-4o\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n\
data: [DONE]\n\n";

#[test]
fn incremental_parser_handles_single_byte_chunks() {
    let mut parser = OpenAiSseParser::new();
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
        .any(|e| matches!(e, ModelEvent::TextDelta { delta } if delta == "hi")));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelEvent::UsageUpdate { .. }))
            .count(),
        1
    );
    assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
    assert_eq!(result.stop_reason, "stop");
    assert_eq!(parser.response_id(), Some("gen-fixture"));
    assert_eq!(result.usage.input_tokens.value(), Some(2));
    assert_eq!(result.usage.output_tokens.value(), Some(1));
    assert_eq!(result.usage.total_tokens.value(), Some(3));
}

#[test]
fn usage_on_final_choice_chunk_is_emitted_before_completion() {
    let mut parser = OpenAiSseParser::new();
    let stream = b"data: {\"id\":\"gen-gemini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":20,\"total_tokens\":120}}\n\ndata: [DONE]\n\n";
    let streamed = parser.push_chunk(stream).unwrap();
    assert!(streamed
        .iter()
        .any(|event| matches!(event, ModelEvent::TextDelta { delta } if delta == "answer")));
    assert!(parser.has_usage());
    let (terminal, result) = parser.finish().unwrap();
    assert_eq!(result.usage.total_tokens.value(), Some(120));
    assert!(
        matches!(terminal.first(), Some(ModelEvent::UsageUpdate { usage }) if usage.total_tokens.value() == Some(120))
    );
    assert!(matches!(
        terminal.last(),
        Some(ModelEvent::Completed { .. })
    ));
}

#[test]
fn parser_rejects_a_stream_without_done() {
    let mut parser = OpenAiSseParser::new();
    parser
        .push_chunk(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n")
        .expect("chunk parses");
    let error = parser
        .finish()
        .expect_err("must reject a stream missing [DONE]");
    assert!(matches!(error, ModelError::StreamInterrupted { .. }));
    assert!(
        error.is_retryable(),
        "a dropped connection, not malformed data, should be retried"
    );
}

#[test]
fn tool_call_arguments_accumulate_across_chunks_and_complete_at_finish() {
    let mut parser = OpenAiSseParser::new();
    let chunks = [
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_abc\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"paris\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ];
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(parser.push_chunk(chunk.as_bytes()).expect("valid chunk"));
    }
    let (terminal, _) = parser.finish().expect("complete fixture");
    events.extend(terminal);

    let completed = events
        .iter()
        .find_map(|e| match e {
            ModelEvent::ToolCallCompleted { name, input, .. } => {
                Some((name.clone(), input.clone()))
            }
            _ => None,
        })
        .expect("a ToolCallCompleted event");
    assert_eq!(completed.0, "get_weather");
    assert_eq!(completed.1, serde_json::json!({ "city": "paris" }));
}

#[test]
fn reasoning_effort_maps_onto_the_expected_strings() {
    use harness_protocol::backend::ReasoningEffort;
    assert_eq!(reasoning_effort_to_openai(ReasoningEffort::Low), "low");
    assert_eq!(
        reasoning_effort_to_openai(ReasoningEffort::Medium),
        "medium"
    );
    assert_eq!(reasoning_effort_to_openai(ReasoningEffort::High), "high");
    assert_eq!(reasoning_effort_to_openai(ReasoningEffort::XHigh), "xhigh");
    assert_eq!(reasoning_effort_to_openai(ReasoningEffort::Max), "max");
    assert_eq!(reasoning_effort_to_openai(ReasoningEffort::Ultra), "max");
}

#[test]
fn a_requested_reasoning_effort_serializes_into_the_request_body() {
    let request = OpenAiRequest {
        model: "big-pickle".to_string(),
        messages: vec![],
        tools: None,
        max_tokens: None,
        temperature: None,
        stop: None,
        response_format: None,
        reasoning_effort: Some("medium".to_string()),
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
    };
    let json = serde_json::to_value(&request).expect("serialize");
    assert_eq!(json["reasoning_effort"], "medium");
}

#[test]
fn reasoning_content_delta_produces_a_reasoning_event_not_a_text_event() {
    let mut parser = OpenAiSseParser::new();
    let events = parser
            .push_chunk(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"thinking...\"}}]}\n\n")
            .expect("valid chunk");
    assert!(matches!(
        events.as_slice(),
        [ModelEvent::ReasoningDelta { delta }] if delta == "thinking..."
    ));
}

#[test]
fn reasoning_field_spelling_is_also_accepted() {
    let mut parser = OpenAiSseParser::new();
    let events = parser
        .push_chunk(
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"pondering\"}}]}\n\n",
        )
        .expect("valid chunk");
    assert!(matches!(
        events.as_slice(),
        [ModelEvent::ReasoningDelta { delta }] if delta == "pondering"
    ));
}
