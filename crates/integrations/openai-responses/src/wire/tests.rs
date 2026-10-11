use super::*;

#[test]
fn request_serializes_with_the_expected_field_names() {
    let request = OpenAiResponsesRequest {
        model: "gpt-5".to_string(),
        input: vec![ResponsesInputItem::InputMessage {
            role: "user".to_string(),
            content: vec![ResponsesInputContentPart::InputText {
                text: "hi".to_string(),
            }],
        }],
        tools: None,
        max_output_tokens: Some(4096),
        temperature: Some(0.5),
        reasoning: None,
        stream: true,
        store: false,
    };
    let json = serde_json::to_value(&request).expect("serialize");
    assert_eq!(json["model"], "gpt-5");
    assert_eq!(json["max_output_tokens"], 4096);
    assert_eq!(json["temperature"], 0.5);
    assert_eq!(json["stream"], true);
    assert_eq!(json["store"], false);
    assert_eq!(json["input"][0]["role"], "user");
    assert_eq!(json["input"][0]["content"][0]["type"], "input_text");
    assert!(
        json["input"][0].get("type").is_none(),
        "a plain user input message must not carry a type tag"
    );
    assert!(
        json.get("reasoning").is_none(),
        "reasoning must be omitted, not null, when no effort was requested"
    );
}

#[test]
fn reasoning_effort_maps_onto_the_responses_apis_own_effort_strings() {
    use harness_protocol::backend::ReasoningEffort;
    assert_eq!(
        reasoning_effort_to_responses(ReasoningEffort::Low).effort,
        "low"
    );
    assert_eq!(
        reasoning_effort_to_responses(ReasoningEffort::Medium).effort,
        "medium"
    );
    assert_eq!(
        reasoning_effort_to_responses(ReasoningEffort::High).effort,
        "high"
    );
    for (effort, wire) in [
        (ReasoningEffort::Minimal, "minimal"),
        (ReasoningEffort::XHigh, "xhigh"),
        (ReasoningEffort::Max, "max"),
        (ReasoningEffort::Ultra, "max"),
    ] {
        assert_eq!(reasoning_effort_to_responses(effort).effort, wire);
    }
}

#[test]
fn a_requested_reasoning_effort_serializes_into_the_request_body() {
    use harness_protocol::backend::ReasoningEffort;
    let request = OpenAiResponsesRequest {
        model: "gpt-5".to_string(),
        input: vec![],
        tools: None,
        max_output_tokens: None,
        temperature: None,
        reasoning: Some(reasoning_effort_to_responses(ReasoningEffort::Medium)),
        stream: true,
        store: false,
    };
    let json = serde_json::to_value(&request).expect("serialize");
    assert_eq!(json["reasoning"]["effort"], "medium");
}

#[test]
fn function_call_item_serializes_in_the_responses_shape() {
    let item = ResponsesInputItem::FunctionCall {
        kind: "function_call".to_string(),
        id: None,
        call_id: "call_123".to_string(),
        name: "get_weather".to_string(),
        arguments: "{\"city\":\"paris\"}".to_string(),
    };
    let json = serde_json::to_value(&item).expect("serialize");
    assert_eq!(json["type"], "function_call");
    assert_eq!(json["call_id"], "call_123");
    assert_eq!(json["name"], "get_weather");
    assert!(json.get("id").is_none(), "omitted id must not serialize");
}

#[test]
fn assistant_message_becomes_an_explicitly_tagged_output_message() {
    let json = serde_json::to_value(ResponsesInputItem::OutputMessage {
        kind: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![ResponsesOutputContentPart::OutputText {
            text: "hello".to_string(),
            annotations: vec![],
        }],
    })
    .expect("serialize");
    assert_eq!(json["type"], "message");
    assert_eq!(json["content"][0]["type"], "output_text");
    assert_eq!(json["content"][0]["text"], "hello");
}

fn frame(json: serde_json::Value) -> String {
    format!("data: {json}\n\n")
}

#[test]
fn text_delta_and_completed_produce_the_expected_event_sequence() {
    let mut parser = OpenAiResponsesSseParser::new();
    let mut events = Vec::new();
    events.extend(
        parser
            .push_chunk(
                frame(serde_json::json!({
                    "type": "response.output_text.delta",
                    "output_index": 0,
                    "delta": "hi"
                }))
                .as_bytes(),
            )
            .expect("valid frame"),
    );
    events.extend(
        parser
            .push_chunk(
                frame(serde_json::json!({
                    "type": "response.completed",
                    "response": {
                        "model": "gpt-5",
                        "status": "completed",
                        "usage": {
                            "input_tokens": 2,
                            "output_tokens": 1,
                            "total_tokens": 3
                        }
                    }
                }))
                .as_bytes(),
            )
            .expect("valid frame"),
    );
    let (terminal, result) = parser.finish().expect("terminal event seen");
    events.extend(terminal);

    assert!(events
        .iter()
        .any(|e| matches!(e, ModelEvent::TextDelta { delta } if delta == "hi")));
    assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
    assert_eq!(result.stop_reason, "stop");
    assert_eq!(result.usage.input_tokens.value(), Some(2));
    assert_eq!(result.usage.output_tokens.value(), Some(1));
    assert_eq!(result.usage.total_tokens.value(), Some(3));
}

#[test]
fn parser_rejects_a_stream_without_a_terminal_event() {
    let mut parser = OpenAiResponsesSseParser::new();
    parser
        .push_chunk(
            frame(serde_json::json!({
                "type": "response.output_text.delta",
                "output_index": 0,
                "delta": "hi"
            }))
            .as_bytes(),
        )
        .expect("valid frame");
    let error = parser
        .finish()
        .expect_err("must reject a stream missing a terminal event");
    assert!(matches!(error, ModelError::StreamInterrupted { .. }));
    assert!(
        error.is_retryable(),
        "a dropped connection, not malformed data, should be retried"
    );
}

#[test]
fn function_call_streams_started_delta_completed_and_marks_tool_calls_stop_reason() {
    let mut parser = OpenAiResponsesSseParser::new();
    let mut events = Vec::new();
    events.extend(
        parser
            .push_chunk(
                frame(serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {
                        "type": "function_call",
                        "call_id": "call_abc",
                        "name": "get_weather",
                        "arguments": ""
                    }
                }))
                .as_bytes(),
            )
            .expect("valid frame"),
    );
    events.extend(
        parser
            .push_chunk(
                frame(serde_json::json!({
                    "type": "response.function_call_arguments.delta",
                    "output_index": 0,
                    "delta": "{\"city\":\"paris\"}"
                }))
                .as_bytes(),
            )
            .expect("valid frame"),
    );
    events.extend(
        parser
            .push_chunk(
                frame(serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "type": "function_call",
                        "call_id": "call_abc",
                        "name": "get_weather",
                        "arguments": "{\"city\":\"paris\"}"
                    }
                }))
                .as_bytes(),
            )
            .expect("valid frame"),
    );
    events.extend(
        parser
            .push_chunk(
                frame(serde_json::json!({
                    "type": "response.completed",
                    "response": { "status": "completed" }
                }))
                .as_bytes(),
            )
            .expect("valid frame"),
    );
    let (terminal, result) = parser.finish().expect("terminal event seen");
    events.extend(terminal);

    assert!(events
        .iter()
        .any(|e| matches!(e, ModelEvent::ToolCallStarted { name, .. } if name == "get_weather")));
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
    assert_eq!(result.stop_reason, "tool_calls");
}

#[test]
fn response_failed_surfaces_as_backend_error() {
    let mut parser = OpenAiResponsesSseParser::new();
    let result = parser.push_chunk(
        frame(serde_json::json!({
            "type": "response.failed",
            "response": {
                "error": { "code": "server_error", "message": "boom" }
            }
        }))
        .as_bytes(),
    );
    assert!(matches!(
        result,
        Err(ModelError::BackendError { code, .. }) if code == "server_error"
    ));
}

#[test]
fn nested_stream_error_surfaces_its_code_and_message() {
    let mut parser = OpenAiResponsesSseParser::new();
    let result = parser.push_chunk(
        frame(serde_json::json!({
            "type": "error",
            "error": {
                "type": "service_unavailable_error",
                "code": "server_is_overloaded",
                "message": "Our servers are currently overloaded."
            },
            "sequence_number": 2
        }))
        .as_bytes(),
    );
    let Err(error @ ModelError::BackendError { .. }) = result else {
        panic!("expected backend error, got {result:?}");
    };
    let ModelError::BackendError { code, message } = &error else {
        unreachable!()
    };
    assert_eq!(code, "server_is_overloaded");
    assert_eq!(message, "Our servers are currently overloaded.");
    assert!(error.is_retryable());
}
