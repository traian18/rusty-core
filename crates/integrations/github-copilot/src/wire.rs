//! Sanitizing parser for Copilot CLI JSON output.

pub fn assistant_text(value: &serde_json::Value) -> Option<String> {
    ["content", "message", "result", "text"]
        .iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .or_else(|| {
            value.get("data")
                .and_then(|data| ["content", "message", "text"].iter().find_map(|key| data.get(*key).and_then(serde_json::Value::as_str)))
                .map(str::to_owned)
        })
}

pub fn safe_error(value: &serde_json::Value) -> Option<String> {
    let kind = value.get("type").and_then(serde_json::Value::as_str).unwrap_or_default();
    if !kind.contains("error") && value.get("error").is_none() { return None; }
    value.get("error").and_then(|error| error.as_str().or_else(|| error.get("message").and_then(serde_json::Value::as_str)))
        .or_else(|| value.get("message").and_then(serde_json::Value::as_str))
        .map(|message| message.chars().take(500).collect())
}

pub fn parse_output(bytes: &[u8]) -> Result<String, String> {
    let source = String::from_utf8_lossy(bytes);
    let values = source.lines().filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    let values = if values.is_empty() {
        serde_json::from_slice::<serde_json::Value>(bytes).ok().into_iter().collect()
    } else {
        values
    };
    let mut text = String::new();
    for value in values {
        if let Some(error) = safe_error(&value) { return Err(error); }
        if let Some(delta) = assistant_text(&value) { text.push_str(&delta); }
    }
    if text.is_empty() { Err("Copilot CLI response did not contain assistant text".into()) } else { Ok(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_fixture_shapes() {
        assert_eq!(assistant_text(&serde_json::json!({"content":"hello"})).as_deref(), Some("hello"));
        assert_eq!(assistant_text(&serde_json::json!({"data":{"message":"nested"}})).as_deref(), Some("nested"));
    }

    #[test]
    fn parses_jsonl_fixture() {
        let fixture = br#"{"type":"message","content":"hello "}
{"type":"message","content":"world"}"#;
        assert_eq!(parse_output(fixture).expect("fixture"), "hello world");
    }

    #[test]
    fn sanitizes_error_length() {
        let value = serde_json::json!({"type":"error","message":"x".repeat(800)});
        assert_eq!(safe_error(&value).expect("error").len(), 500);
    }
}
