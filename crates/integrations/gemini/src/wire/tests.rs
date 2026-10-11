use super::*;

/// M4/M4.5: proves `GeminiGenerationConfig`'s JSON field names match the
/// `generateContent` API's `camelCase` expectations
/// (`maxOutputTokens`/`stopSequences`, via `#[serde(rename_all = ...)]`
/// wherever that's declared on the struct) — same rationale as the
/// equivalent OpenAI wire-shape test: the M4.1 contract suite proves
/// `ModelRequest` carries the right values up to this boundary, not that
/// this boundary serializes them the way the real API expects.
#[test]
fn generation_config_serializes_with_the_expected_gemini_field_names() {
    let config = GeminiGenerationConfig {
        max_output_tokens: Some(2048),
        temperature: Some(0.7),
        stop_sequences: Some(vec!["STOP".to_string()]),
        response_mime_type: None,
        response_schema: None,
    };
    let json = serde_json::to_value(&config).expect("serialize GeminiGenerationConfig");
    assert_eq!(json["maxOutputTokens"], 2048);
    assert_eq!(json["temperature"], 0.7);
    assert_eq!(json["stopSequences"], serde_json::json!(["STOP"]));

    let bare = GeminiGenerationConfig {
        max_output_tokens: None,
        temperature: None,
        stop_sequences: None,
        response_mime_type: None,
        response_schema: None,
    };
    let bare_json = serde_json::to_value(&bare).expect("serialize bare GeminiGenerationConfig");
    assert!(bare_json.get("maxOutputTokens").is_none());
    assert!(bare_json.get("temperature").is_none());
    assert!(bare_json.get("stopSequences").is_none());
}

/// Gemini needs *both* the JSON mime type and the schema — a schema
/// alone is silently ignored by the API, which would look like the
/// feature simply not working.
#[test]
fn json_schema_sets_both_mime_type_and_schema() {
    let mut config = GeminiGenerationConfig {
        max_output_tokens: None,
        temperature: None,
        stop_sequences: None,
        response_mime_type: None,
        response_schema: None,
    };
    config.apply_response_format(Some(
        &harness_protocol::backend::ResponseFormat::JsonSchema {
            name: "task_graph".into(),
            schema: serde_json::json!({ "type": "object" }),
            strict: true,
        },
    ));

    let json = serde_json::to_value(&config).expect("serialize");
    assert_eq!(json["responseMimeType"], "application/json");
    assert_eq!(
        json["responseSchema"],
        serde_json::json!({ "type": "object" })
    );
}

#[test]
fn json_object_sets_the_mime_type_without_a_schema() {
    let mut config = GeminiGenerationConfig {
        max_output_tokens: None,
        temperature: None,
        stop_sequences: None,
        response_mime_type: None,
        response_schema: None,
    };
    config.apply_response_format(Some(&harness_protocol::backend::ResponseFormat::JsonObject));

    let json = serde_json::to_value(&config).expect("serialize");
    assert_eq!(json["responseMimeType"], "application/json");
    assert!(json.get("responseSchema").is_none());
}

/// Gemini's `responseSchema` accepts a restricted subset; a schema
/// carrying `$schema`/`additionalProperties`/`$defs` (which every
/// `schemars`-generated schema does) fails the whole request with a 400.
#[test]
fn schema_keywords_gemini_rejects_are_stripped_recursively() {
    let mut config = GeminiGenerationConfig {
        max_output_tokens: None,
        temperature: None,
        stop_sequences: None,
        response_mime_type: None,
        response_schema: None,
    };
    config.apply_response_format(Some(
        &harness_protocol::backend::ResponseFormat::JsonSchema {
            name: "n".into(),
            schema: serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "nested": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": { "a": { "type": "string" } }
                    }
                },
                "required": ["nested"]
            }),
            strict: false,
        },
    ));

    let schema = config.response_schema.expect("a schema must be set");
    assert!(
        schema.get("$schema").is_none(),
        "top-level $schema survived"
    );
    assert!(
        schema.get("additionalProperties").is_none(),
        "top-level additionalProperties survived"
    );
    assert!(
        schema["properties"]["nested"]
            .get("additionalProperties")
            .is_none(),
        "nested additionalProperties survived: {schema}"
    );
    // The structural constraints must survive — stripping is targeted,
    // not a blanket flattening.
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], serde_json::json!(["nested"]));
    assert_eq!(
        schema["properties"]["nested"]["properties"]["a"]["type"],
        "string"
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

/// M4: an image content block must convert into a real `inlineData`
/// part, not be silently dropped — matching Anthropic's existing image
/// pass-through, previously Gemini-specific wire support for it did not
/// exist even though the client already advertised `images: true` in
/// its capabilities.
#[test]
fn an_image_block_becomes_a_real_inline_data_part_not_silently_dropped() {
    let message = user_message(vec![
        ContentBlock::Text {
            text: "what is this?".into(),
        },
        ContentBlock::Image {
            mime_type: "image/png".into(),
            data: vec![1, 2, 3],
        },
    ]);
    let contents = convert_messages(std::slice::from_ref(&message));
    assert_eq!(contents.len(), 1);
    let json = serde_json::to_value(&contents[0]).expect("serialize GeminiContent");
    let parts = json["parts"].as_array().expect("parts must be an array");
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["text"], "what is this?");
    assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
    assert_eq!(
        parts[1]["inlineData"]["data"],
        base64::engine::general_purpose::STANDARD.encode([1, 2, 3])
    );
}

const FIXTURE: &str = "\
data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hello, \"}]}}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":1,\"totalTokenCount\":11}}\n\n\
data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"world!\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":5,\"totalTokenCount\":15}}\n\n";

#[test]
fn incremental_parser_handles_single_byte_chunks() {
    let mut parser = GeminiSseParser::new("gemini-1.5-pro".to_string());
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

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TextDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello, world!");
    assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
    assert_eq!(result.stop_reason, "STOP");
    assert_eq!(result.usage.input_tokens.value(), Some(10));
    assert_eq!(result.usage.output_tokens.value(), Some(5));
    assert_eq!(result.usage.total_tokens.value(), Some(15));
}

#[test]
fn parser_rejects_a_stream_without_finish_reason() {
    let mut parser = GeminiSseParser::new("gemini-1.5-pro".to_string());
    parser
            .push_chunk(b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hi\"}]}}]}\n\n")
            .expect("chunk parses");
    assert!(matches!(parser.finish(), Err(ModelError::Protocol { .. })));
}

#[test]
fn function_call_arrives_whole_and_completes_immediately() {
    let mut parser = GeminiSseParser::new("gemini-1.5-pro".to_string());
    let chunk = b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"get_weather\",\"args\":{\"city\":\"paris\"}}}]},\"finishReason\":\"STOP\"}]}\n\n";
    let events = parser.push_chunk(chunk).expect("valid chunk");
    let completed = events
        .iter()
        .find_map(|e| match e {
            ModelEvent::ToolCallCompleted { name, input, .. } => {
                Some((name.clone(), input.clone()))
            }
            _ => None,
        })
        .expect("a ToolCallCompleted event on the very first chunk");
    assert_eq!(completed.0, "get_weather");
    assert_eq!(completed.1, serde_json::json!({ "city": "paris" }));
}

#[test]
fn multiple_parts_in_one_chunk_each_produce_an_event() {
    let mut parser = GeminiSseParser::new("gemini-1.5-pro".to_string());
    let chunk = b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"checking...\"},{\"functionCall\":{\"name\":\"get_weather\",\"args\":{}}}]},\"finishReason\":\"STOP\"}]}\n\n";
    let events = parser.push_chunk(chunk).expect("valid chunk");
    assert!(events
        .iter()
        .any(|e| matches!(e, ModelEvent::TextDelta { delta } if delta == "checking...")));
    assert!(events
        .iter()
        .any(|e| matches!(e, ModelEvent::ToolCallCompleted { .. })));
}
