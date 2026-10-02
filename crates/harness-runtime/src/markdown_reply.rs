//! Markdown replies for structured agent steps.
//!
//! Models are asked to answer in plain Markdown, never JSON. The step's output
//! schema still defines the data the host passes between nodes; this module
//! derives a Markdown layout from that schema (`instructions`) and reads a
//! reply back into the matching value (`parse`).
//!
//! Layout, by schema shape:
//! - top-level properties are `## name` sections;
//! - a scalar is the section text, a list of scalars is a bullet list;
//! - a list of `{id, text}` pairs is bullets written `- ID: text`;
//! - a list of other objects is one `### ID` heading per item, with the short
//!   fields as `key: value` lines, the main text as the body, and nested
//!   lists/objects under deeper headings.

use serde_json::{Map, Value};

const BODY_FIELDS: [&str; 9] = [
    "text",
    "instructions",
    "evidence",
    "summary",
    "description",
    "content",
    "message",
    "finding",
    "detail",
];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Scalar,
    List,
    Items,
    Object,
}

fn kind(schema: &Value) -> Kind {
    match schema.get("type").and_then(Value::as_str) {
        Some("array") => match schema.get("items").map(kind) {
            Some(Kind::Scalar) | None => Kind::List,
            _ => Kind::Items,
        },
        Some("object") => Kind::Object,
        Some(_) => Kind::Scalar,
        None if schema.get("properties").is_some() => Kind::Object,
        None if schema.get("items").is_some() => Kind::Items,
        None => Kind::Scalar,
    }
}

/// Whether a reply for this schema can be written as Markdown.
pub(crate) fn supports(schema: &Value) -> bool {
    kind(schema) == Kind::Object
        && schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| !properties.is_empty())
}

fn properties(schema: &Value) -> Vec<(String, &Value)> {
    let Some(map) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut ordered = Vec::new();
    for name in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some(value) = map.get(name) {
            ordered.push((name.to_owned(), value));
        }
    }
    for (name, value) in map {
        if !ordered.iter().any(|(known, _)| known == name) {
            ordered.push((name.clone(), value));
        }
    }
    ordered
}

fn enum_values(schema: &Value) -> Vec<String> {
    schema
        .get("enum")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn hint(schema: &Value) -> String {
    let values = enum_values(schema);
    if values.is_empty() {
        match schema.get("type").and_then(Value::as_str) {
            Some("integer") | Some("number") => "<number>".into(),
            Some("boolean") => "<true or false>".into(),
            _ => "<text>".into(),
        }
    } else {
        values.join(" | ")
    }
}

/// The property that carries an item's main text.
fn body_field(schema: &Value) -> Option<String> {
    let props = properties(schema);
    for wanted in BODY_FIELDS {
        if props.iter().any(|(name, value)| {
            name == wanted && kind(value) == Kind::Scalar && enum_values(value).is_empty()
        }) {
            return Some(wanted.to_owned());
        }
    }
    None
}

/// `{id, text}`: written as one bullet per item.
fn is_pair(schema: &Value) -> bool {
    let props = properties(schema);
    props.len() == 2
        && props.iter().any(|(name, _)| name == "id")
        && body_field(schema).is_some_and(|body| props.iter().any(|(name, _)| *name == body))
        && props.iter().all(|(_, value)| kind(value) == Kind::Scalar)
}

fn heading(level: usize, title: &str) -> String {
    format!("{} {}", "#".repeat(level), title)
}

// ---------------------------------------------------------------- inputs

/// A step's input as Markdown: one `## name` section per field, text as it
/// is (never escaped), lists as bullets, records as `### ID` blocks. This is
/// how earlier steps' results reach the next prompt.
pub(crate) fn render_input(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(fields) => {
            let mut out = String::new();
            render_fields(fields, 2, &mut out);
            out.trim_end().to_owned()
        }
        other => render_inline(other),
    }
}

fn render_inline(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Array(items) => items
            .iter()
            .map(render_inline)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

fn render_fields(fields: &Map<String, Value>, level: usize, out: &mut String) {
    for (name, value) in fields {
        if value.is_null() {
            continue;
        }
        out.push_str(&heading(level, name));
        out.push('\n');
        render_body(value, level, out);
        out.push_str("\n\n");
    }
}

fn render_body(value: &Value, level: usize, out: &mut String) {
    match value {
        Value::Array(items) if items.iter().all(is_scalar) => {
            for item in items {
                out.push_str(&format!("- {}\n", render_inline(item)));
            }
        }
        Value::Array(items)
            if items.iter().all(|item| {
                item.as_object().is_some_and(|record| {
                    record.len() == 2
                        && record.contains_key("id")
                        && record.values().all(|value| value.is_string())
                })
            }) =>
        {
            // `{id, text}` pairs read best as `- ID: text`.
            for item in items {
                let record = item.as_object().unwrap();
                let text = record
                    .iter()
                    .find(|(name, _)| *name != "id")
                    .map_or("", |(_, value)| value.as_str().unwrap_or(""));
                out.push_str(&format!("- {}: {text}\n", render_inline(&record["id"])));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                match item {
                    Value::Object(record) => {
                        let title = record
                            .get("id")
                            .or_else(|| record.get("task_id"))
                            .map_or_else(|| (index + 1).to_string(), render_inline);
                        out.push_str(&heading(level + 1, &title));
                        out.push('\n');
                        render_record(record, level + 1, out);
                    }
                    other => out.push_str(&format!("- {}\n", render_inline(other))),
                }
            }
        }
        Value::Object(fields) => render_fields(fields, level + 1, out),
        other => out.push_str(&render_inline(other)),
    }
}

/// Short fields first as `key: value`, then text and nested data under
/// deeper headings.
fn render_record(record: &Map<String, Value>, level: usize, out: &mut String) {
    let short = |value: &Value| match value {
        Value::String(text) => !text.contains('\n') && text.len() <= 80,
        Value::Array(items) => items.iter().all(is_scalar),
        Value::Null => false,
        _ => true,
    };
    for (name, value) in record {
        if name != "id" && name != "task_id" && short(value) && !value.is_null() {
            out.push_str(&format!("{name}: {}\n", render_inline(value)));
        }
    }
    for (name, value) in record {
        if name == "id" || name == "task_id" || short(value) || value.is_null() {
            continue;
        }
        out.push_str(&heading(level + 1, name));
        out.push('\n');
        render_body(value, level + 1, out);
        out.push('\n');
    }
}

// ---------------------------------------------------------------- template

/// The reply-format section appended to a step's prompt.
pub(crate) fn instructions(schema: &Value) -> String {
    let mut template = String::new();
    template_object(schema, 2, &mut template);
    format!(
        "\n\nWrite your final message in plain Markdown, not JSON. Use exactly these section headings, in this order, and put nothing before the first heading. Replace each <...> with your content; repeat an item's block once per item. IDs are optional: the host numbers items for you.\n\n{}",
        template.trim_end()
    )
}

fn template_object(schema: &Value, level: usize, out: &mut String) {
    for (name, property) in properties(schema) {
        out.push_str(&heading(level, &name));
        out.push('\n');
        match kind(property) {
            Kind::Scalar => out.push_str(&hint(property)),
            Kind::List => out.push_str("- <item>\n- <item>"),
            Kind::Items => template_items(property, level, out),
            Kind::Object => template_object(property, level + 1, out),
        }
        out.push_str("\n\n");
    }
}

fn template_items(property: &Value, level: usize, out: &mut String) {
    let Some(items) = property.get("items") else {
        return;
    };
    if is_pair(items) {
        out.push_str("- <text>\n- <text>");
        return;
    }
    out.push_str(&heading(level + 1, "<short name>"));
    out.push('\n');
    let body = body_field(items);
    for (name, field) in properties(items) {
        if name == "id" || Some(&name) == body.as_ref() {
            continue;
        }
        match kind(field) {
            Kind::Scalar => out.push_str(&format!("{name}: {}\n", hint(field))),
            Kind::List => out.push_str(&format!("{name}: <comma-separated>\n")),
            Kind::Items => {
                out.push_str(&heading(level + 2, &name));
                out.push('\n');
                template_items(field, level + 2, out);
                out.push('\n');
            }
            Kind::Object => {
                out.push_str(&heading(level + 2, &name));
                out.push('\n');
                template_object(field, level + 3, out);
            }
        }
    }
    if body.is_some() {
        out.push_str("<text>\n");
    }
}

// ------------------------------------------------------------------ parsing

/// `# `-style heading: (level, title).
fn heading_of(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = trimmed[level..].strip_prefix(' ')?;
    Some((level, rest.trim()))
}

/// Heading lines outside code fences: (line index, level, title).
fn headings(text: &str) -> Vec<(usize, usize, String)> {
    let mut fenced = false;
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            if let Some((level, title)) = heading_of(line) {
                found.push((index, level, title.to_owned()));
            }
        }
    }
    found
}

fn normalize(name: &str) -> String {
    name.trim()
        .trim_matches(|c| matches!(c, '*' | '`' | ':' | '#' | ' '))
        .to_lowercase()
        .replace([' ', '-'], "_")
}

/// Sections opened by headings of exactly `level`: (title, body).
fn sections(text: &str, level: usize) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let marks: Vec<_> = headings(text)
        .into_iter()
        .filter(|(_, found, _)| *found == level)
        .collect();
    marks
        .iter()
        .enumerate()
        .map(|(position, (start, _, title))| {
            let end = marks.get(position + 1).map_or(lines.len(), |next| next.0);
            (title.clone(), lines[start + 1..end].join("\n"))
        })
        .collect()
}

pub(crate) fn parse(schema: &Value, text: &str) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("the reply was empty".into());
    }
    // A bare `status` / `summary` line counts as a section heading.
    let mut value = parse_object(schema, &promote_bare_names(schema, text));
    if let Value::Object(map) = &mut value {
        fill_missing(schema, map, text);
    }
    repair(schema, &mut value);
    Ok(value)
}

/// Models do not always format a reply the same way, and the work is done
/// either way: what the reply does not state is inferred from its text rather
/// than failing the step on form.
fn fill_missing(schema: &Value, map: &mut Map<String, Value>, text: &str) {
    for (name, property) in properties(schema) {
        if map.contains_key(&name) || !is_required(schema, &name) {
            continue;
        }
        let filled = match kind(property) {
            Kind::List | Kind::Items => Some(Value::Array(Vec::new())),
            Kind::Scalar => {
                let values = enum_values(property);
                if !values.is_empty() {
                    // The reply names its status somewhere, else it is the
                    // success value (the first one listed).
                    let found = text
                        .lines()
                        .map(|line| normalize(line.trim_start_matches(['-', '*', ' '])))
                        .find_map(|line| {
                            let line = line.strip_prefix(&format!("{name}_")).unwrap_or(&line);
                            values.iter().find(|value| normalize(value) == line)
                        });
                    values
                        .first()
                        .map(|first| Value::String(found.unwrap_or(first).clone()))
                } else if BODY_FIELDS.contains(&name.as_str()) {
                    Some(Value::String(text.to_owned()))
                } else if name == "verdict" {
                    // Stated by the check results when the reply does not.
                    let results = map.get("criteria").and_then(Value::as_array);
                    let passing = results.is_some_and(|items| {
                        !items.is_empty() && items.iter().all(|item| item["status"] == "pass")
                    });
                    Some(Value::String(if passing {
                        "pass".into()
                    } else {
                        "fail: the reply did not report every criterion as passing".into()
                    }))
                } else {
                    None
                }
            }
            Kind::Object => None,
        };
        if let Some(filled) = filled {
            map.insert(name, filled);
        }
    }
}

/// An empty text a schema calls non-empty is not worth failing a step for:
/// it takes the item's ID (or a plain marker) instead.
fn repair(schema: &Value, value: &mut Value) {
    match value {
        Value::String(text) => {
            // A status the schema does not list (`Done`) is read as the
            // success value, as a missing one is.
            let values = enum_values(schema);
            if !values.is_empty() && !values.contains(text) {
                // An unclear check result is "unverified", never a pass.
                *text = if values.iter().any(|value| value == "unverified") {
                    "unverified".into()
                } else {
                    values[0].clone()
                };
            }
            if text.trim().is_empty()
                && schema.get("minLength").and_then(Value::as_u64).unwrap_or(0) > 0
            {
                *text = "(not stated)".into();
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = schema.get("items") {
                for item in items {
                    repair(item_schema, item);
                }
            }
        }
        Value::Object(map) => {
            let id = map.get("id").and_then(Value::as_str).map(str::to_owned);
            for (name, property) in properties(schema) {
                if let Some(field) = map.get_mut(&name) {
                    if let (Value::String(text), Some(id)) = (&mut *field, &id) {
                        if text.trim().is_empty()
                            && property
                                .get("minLength")
                                .and_then(Value::as_u64)
                                .unwrap_or(0)
                                > 0
                        {
                            *text = id.clone();
                        }
                    }
                    repair(property, field);
                }
            }
        }
        _ => {}
    }
}

fn is_required(schema: &Value, name: &str) -> bool {
    schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| required.iter().any(|item| item.as_str() == Some(name)))
}

/// `status` / `**Summary:**` alone on a line becomes `## status`; a short
/// `status: complete` line for an enum property too.
fn promote_bare_names(schema: &Value, text: &str) -> String {
    let props = properties(schema);
    let mut fenced = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let key = normalize(line);
        let plain = !line.trim_start().starts_with('#');
        if !fenced && plain && props.iter().any(|(name, _)| *name == key) {
            out.push(format!("## {key}"));
            continue;
        }
        if !fenced && plain {
            if let Some((name, value)) = line.split_once(':') {
                let name = normalize(name.trim_start_matches(['-', '#', ' ']));
                let inline = props.iter().any(|(prop, property)| {
                    *prop == name
                        && kind(property) == Kind::Scalar
                        && !enum_values(property).is_empty()
                }) && !value.trim().is_empty();
                if inline {
                    out.push(format!("## {name}\n{}", value.trim()));
                    continue;
                }
            }
        }
        out.push(line.to_owned());
    }
    out.join("\n")
}

fn parse_object(schema: &Value, text: &str) -> Value {
    let props = properties(schema);
    let matching = |title: &str| props.iter().any(|(name, _)| *name == normalize(title));
    // Models do not always honour the exact level; use the shallowest heading
    // that names one of the expected properties.
    let level = headings(text)
        .into_iter()
        .filter(|(_, _, title)| matching(title))
        .map(|(_, level, _)| level)
        .min();
    let mut out = Map::new();
    let Some(level) = level else {
        return Value::Object(out);
    };
    for (title, body) in sections(text, level) {
        let key = normalize(&title);
        if let Some((name, property)) = props.iter().find(|(name, _)| *name == key) {
            if let Some(value) = parse_value(name, property, &body, level) {
                out.insert(name.clone(), value);
            }
        }
    }
    Value::Object(out)
}

fn parse_value(name: &str, schema: &Value, body: &str, level: usize) -> Option<Value> {
    match kind(schema) {
        Kind::Scalar => Some(coerce(schema, body.trim())),
        Kind::List => Some(Value::Array(
            bullets(body)
                .into_iter()
                .map(|item| coerce(schema.get("items").unwrap_or(&Value::Null), &item))
                .collect(),
        )),
        Kind::Items => {
            let items = schema.get("items")?;
            Some(Value::Array(parse_items(name, items, body, level)))
        }
        Kind::Object => {
            let value = parse_object(schema, body);
            value
                .as_object()
                .is_some_and(|map| !map.is_empty())
                .then_some(value)
        }
    }
}

/// `R` for requirements, `T` for tasks, `C` for criteria.
fn id_prefix(name: &str) -> String {
    name.chars()
        .next()
        .map_or_else(|| "N".into(), |c| c.to_uppercase().to_string())
}

/// A short code such as `R1`, `T-2`, `C1.a`; never a sentence.
fn looks_like_id(token: &str) -> bool {
    let token = token.trim();
    !token.is_empty()
        && token.len() <= 12
        && !token.contains(char::is_whitespace)
        && token.chars().any(|c| c.is_ascii_digit())
}

fn parse_items(name: &str, items: &Value, body: &str, level: usize) -> Vec<Value> {
    if is_pair(items) && headings(body).iter().all(|(_, found, _)| *found <= level) {
        let body_name = body_field(items).unwrap_or_default();
        return bullets(body)
            .into_iter()
            .enumerate()
            .map(|(index, bullet)| {
                // `C1: text` carries its own ID; a plain bullet gets one.
                let (id, text) = bullet
                    .split_once(':')
                    .or_else(|| bullet.split_once(char::is_whitespace))
                    .filter(|(id, _)| looks_like_id(&clean_id(id)))
                    .map_or_else(
                        || (format!("{}{}", id_prefix(name), index + 1), bullet.clone()),
                        |(id, text)| (clean_id(id), text.trim().to_owned()),
                    );
                let mut map = Map::new();
                map.insert("id".into(), Value::String(id));
                map.insert(body_name.clone(), Value::String(text));
                Value::Object(map)
            })
            .collect();
    }
    let item_level = headings(body)
        .into_iter()
        .map(|(_, found, _)| found)
        .filter(|found| *found > level)
        .min();
    let Some(item_level) = item_level else {
        return Vec::new();
    };
    sections(body, item_level)
        .into_iter()
        .enumerate()
        .map(|(index, (title, section))| {
            parse_item(
                items,
                &title,
                &section,
                format!("{}{}", id_prefix(name), index + 1),
            )
        })
        .collect()
}

/// `R1`, `R1: Show it`, `Requirement R1 - Show it`: the ID, and any words
/// after it.
fn split_title(title: &str) -> (Option<String>, String) {
    let mut rest = title.trim();
    for prefix in ["requirement", "criterion", "criteria", "task", "step"] {
        if rest.len() > prefix.len()
            && rest[..prefix.len()].eq_ignore_ascii_case(prefix)
            && rest[prefix.len()..].starts_with(' ')
        {
            rest = rest[prefix.len()..].trim_start();
            break;
        }
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || c == ':')
        .unwrap_or(rest.len());
    let id = clean_id(&rest[..end]);
    if !looks_like_id(&id) {
        // Not an ID: the whole heading is the item's words.
        return (None, title.trim().to_owned());
    }
    let words = rest[end..].trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, ':' | '.' | '-' | '–' | '—' | '*' | '`')
    });
    (Some(id), words.trim().to_owned())
}

fn parse_item(schema: &Value, title: &str, section: &str, fallback_id: String) -> Value {
    let props = properties(schema);
    let body_name = body_field(schema);
    let mut out = Map::new();
    let (id, title_words) = split_title(title);
    let id = id.unwrap_or(fallback_id);
    if props.iter().any(|(name, _)| name == "id") {
        out.insert("id".into(), Value::String(id.clone()));
    }
    // Everything before the first nested heading is `key: value` lines and
    // the item's own text.
    let first_heading = headings(section).first().map(|found| found.0);
    let lines: Vec<&str> = section.lines().collect();
    let split = first_heading.unwrap_or(lines.len());
    let mut text = Vec::new();
    for line in &lines[..split] {
        let field = line.split_once(':').and_then(|(key, value)| {
            let key = normalize(key.trim_start_matches(['-', '*', ' ']));
            props
                .iter()
                .find(|(name, property)| {
                    *name == key
                        && Some(name) != body_name.as_ref()
                        && matches!(kind(property), Kind::Scalar | Kind::List)
                })
                .map(|(name, property)| (name.clone(), *property, value.trim()))
        });
        match field {
            Some((name, property, value)) => {
                let parsed = if kind(property) == Kind::List {
                    Value::Array(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|part| !part.is_empty())
                            .map(|part| coerce(property.get("items").unwrap_or(&Value::Null), part))
                            .collect(),
                    )
                } else {
                    coerce(property, value)
                };
                out.insert(name, parsed);
            }
            None => text.push(*line),
        }
    }
    if let Some(name) = body_name {
        let mut body = text.join("\n").trim().to_owned();
        // The words may be in the heading instead (`### R1: Show it`).
        if body.is_empty() {
            body = title_words;
        }
        if body.is_empty() {
            body = id;
        }
        out.insert(name, Value::String(body));
    }
    if first_heading.is_some() {
        let nested = lines[split..].join("\n");
        if let Value::Object(map) = parse_object(schema, &nested) {
            out.extend(map);
        }
    }
    // Lists the model left out of an item are empty, not missing.
    for (name, property) in &props {
        if !out.contains_key(name) && matches!(kind(property), Kind::List | Kind::Items) {
            out.insert(name.clone(), Value::Array(Vec::new()));
        }
    }
    Value::Object(out)
}

fn clean_id(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| matches!(c, '*' | '`' | ':' | ' '))
        .to_owned()
}

fn bullets(body: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let mut plain = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let marker = ["- ", "* ", "• "]
            .iter()
            .find_map(|marker| trimmed.strip_prefix(marker))
            .or_else(|| {
                let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
                (digits > 0)
                    .then(|| {
                        trimmed[digits..]
                            .strip_prefix(". ")
                            .or_else(|| trimmed[digits..].strip_prefix(") "))
                    })
                    .flatten()
            });
        match marker {
            Some(text) => items.push(text.trim().to_owned()),
            None => {
                plain.push(trimmed.to_owned());
                if let Some(last) = items.last_mut() {
                    last.push('\n');
                    last.push_str(trimmed);
                }
            }
        }
    }
    if items.is_empty() {
        plain
    } else {
        items
    }
}

fn coerce(schema: &Value, text: &str) -> Value {
    match schema.get("type").and_then(Value::as_str) {
        Some("integer") | Some("number") => {
            let number = text.trim().trim_matches('`');
            if let Ok(value) = number.parse::<i64>() {
                return Value::from(value);
            }
            if let Ok(value) = number.parse::<f64>() {
                return Value::from(value);
            }
        }
        Some("boolean") => {
            return Value::Bool(matches!(
                normalize(text).as_str(),
                "true" | "yes" | "y" | "1"
            ))
        }
        _ => {}
    }
    let values = enum_values(schema);
    if !values.is_empty() {
        let plain = normalize(text);
        if let Some(found) = values.iter().find(|value| normalize(value) == plain) {
            return Value::String(found.clone());
        }
        let first = plain
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .find(|word| !word.is_empty())
            .unwrap_or("");
        if let Some(found) = values.iter().find(|value| normalize(value) == first) {
            return Value::String(found.clone());
        }
        // `failed`, `passed`, `completed`.
        if let Some(found) = values
            .iter()
            .find(|value| !first.is_empty() && first.starts_with(&normalize(value)))
        {
            return Value::String(found.clone());
        }
    }
    Value::String(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plan_schema() -> Value {
        json!({"type":"object","required":["status","requirements","tasks","summary"],"properties":{
            "status":{"type":"string","enum":["ready","blocked","incomplete"]},
            "requirements":{"type":"array","items":{"type":"object","required":["id","text","criteria"],"properties":{
                "id":{"type":"string"},"text":{"type":"string"},
                "criteria":{"type":"array","items":{"type":"object","required":["id","text"],"properties":{"id":{"type":"string"},"text":{"type":"string"}}}}}}},
            "tasks":{"type":"array","items":{"type":"object","required":["id","requirement_ids","criterion_ids","depends_on","instructions"],"properties":{
                "id":{"type":"string"},
                "requirement_ids":{"type":"array","items":{"type":"string"}},
                "criterion_ids":{"type":"array","items":{"type":"string"}},
                "depends_on":{"type":"array","items":{"type":"string"}},
                "instructions":{"type":"string"}}}},
            "summary":{"type":"string"}}})
    }

    #[test]
    fn a_plan_is_read_from_markdown() {
        let reply = "## status\nReady.\n\n## requirements\n### R1\nShow the badge\n#### criteria\n- C1: badge visible\n- C2: badge hidden when empty\n\n## tasks\n### T1\nrequirement_ids: R1\ncriterion_ids: C1, C2\ndepends_on:\nEdit `badge.ts`.\nThen test it.\n\n## summary\nOne task.";
        let value = parse(&plan_schema(), reply).unwrap();
        assert_eq!(
            value,
            json!({
                "status":"ready",
                "requirements":[{"id":"R1","text":"Show the badge","criteria":[
                    {"id":"C1","text":"badge visible"},{"id":"C2","text":"badge hidden when empty"}]}],
                "tasks":[{"id":"T1","requirement_ids":["R1"],"criterion_ids":["C1","C2"],"depends_on":[],
                    "instructions":"Edit `badge.ts`.\nThen test it."}],
                "summary":"One task."
            })
        );
    }

    #[test]
    fn verdicts_and_criteria_are_read_from_markdown() {
        let schema = json!({"type":"object","required":["verdict","criteria","summary"],"properties":{
            "verdict":{"type":"string"},
            "criteria":{"type":"array","items":{"type":"object","required":["id","status","evidence"],"properties":{
                "id":{"type":"string"},"status":{"type":"string","enum":["pass","fail","unverified"]},"evidence":{"type":"string"}}}},
            "summary":{"type":"string"}}});
        let reply = "# Verdict\nfail: src/a.rs:3 misses the case\n# Criteria\n## C1\nstatus: **Fail**\nsrc/a.rs:3 has no branch.\n# Summary\nNot done.";
        let value = parse(&schema, reply).unwrap();
        assert_eq!(value["verdict"], "fail: src/a.rs:3 misses the case");
        assert_eq!(
            value["criteria"],
            json!([{"id":"C1","status":"fail","evidence":"src/a.rs:3 has no branch."}])
        );
    }

    #[test]
    fn inputs_are_rendered_without_json_escaping() {
        let text = render_input(&json!({
            "request": "do \"it\"",
            "research": "line one\nline two",
            "plan": {"status":"ready","tasks":[{"id":"T1","depends_on":[],"instructions":"Edit a.rs\nthen b.rs"}]}
        }));
        assert!(text.contains("## research\nline one\nline two"));
        assert!(text.contains("do \"it\""));
        assert!(text.contains("### T1\ndepends_on: \n"));
        assert!(!text.contains("#### criteria") || text.contains("- C1:"));
        assert!(text.contains("#### instructions\nEdit a.rs\nthen b.rs"));
        assert!(!text.contains("\\n") && !text.contains('{'));
    }

    #[test]
    fn ids_are_optional_and_numbered_by_the_host() {
        let value = parse(&plan_schema(), "## requirements\n### Show the badge\n#### criteria\n- badge visible\n- C7: hidden when empty\n### Hide it\nbody\n## tasks\n### Edit badge.ts\nEdit it.").unwrap();
        assert_eq!(value["requirements"][0]["id"], "R1");
        assert_eq!(value["requirements"][0]["text"], "Show the badge");
        assert_eq!(value["requirements"][0]["criteria"][0]["id"], "C1");
        assert_eq!(value["requirements"][0]["criteria"][1]["id"], "C7");
        assert_eq!(value["requirements"][1]["id"], "R2");
        assert_eq!(value["tasks"][0]["id"], "T1");
        assert_eq!(value["tasks"][0]["instructions"], "Edit it.");
    }

    #[test]
    fn an_unclear_check_result_is_never_a_pass() {
        let schema = json!({"type":"object","required":["verdict","criteria"],"properties":{
            "verdict":{"type":"string"},
            "criteria":{"type":"array","items":{"type":"object","required":["id","status","evidence"],"properties":{
                "id":{"type":"string"},"status":{"type":"string","enum":["pass","fail","unverified"]},"evidence":{"type":"string"}}}}}});
        let value = parse(
            &schema,
            "## criteria\n### C1\nstatus: FAILED\nbroken\n### C2\nstatus: looks ok\nfine",
        )
        .unwrap();
        assert_eq!(value["criteria"][0]["status"], "fail");
        assert_eq!(value["criteria"][1]["status"], "unverified");
        assert!(value["verdict"].as_str().unwrap().starts_with("fail"));
        let value = parse(&schema, "## criteria\n### C1\nstatus: Passed\nsrc/a.rs:1").unwrap();
        assert_eq!(value["verdict"], "pass");
    }

    #[test]
    fn words_in_the_heading_count_as_the_item_text() {
        let schema = json!({"type":"object","required":["requirements"],"properties":{
            "requirements":{"type":"array","items":{"type":"object","required":["id","text"],"properties":{
                "id":{"type":"string"},"text":{"type":"string","minLength":1}}}}}});
        let reply =
            "## requirements\n### R1: Show the badge\n### Requirement R2 - Hide it\n### R3\n";
        let value = parse(&schema, reply).unwrap();
        assert_eq!(
            value["requirements"],
            json!([{"id":"R1","text":"Show the badge"},{"id":"R2","text":"Hide it"},{"id":"R3","text":"R3"}])
        );
    }

    #[test]
    fn a_reply_is_forgiven_a_missing_status_and_bare_section_names() {
        let schema = json!({"type":"object","required":["status","summary"],"properties":{
            "status":{"type":"string","enum":["complete","blocked"]},"summary":{"type":"string"}}});
        // No status section at all: the summary is kept, the status inferred.
        let value = parse(&schema, "## summary\nAudited everything.").unwrap();
        assert_eq!(
            value,
            json!({"status":"complete","summary":"Audited everything."})
        );
        // Section names without any `#`.
        let value = parse(&schema, "status\nblocked\nsummary\nNo access.").unwrap();
        assert_eq!(value, json!({"status":"blocked","summary":"No access."}));
        // Plain prose becomes the summary.
        let value = parse(&schema, "I looked at it and all is fine.").unwrap();
        assert_eq!(value["summary"], "I looked at it and all is fine.");
        assert_eq!(value["status"], "complete");
        // The status is found when only stated in passing.
        let value = parse(&schema, "## summary\nStopped.\n\nStatus: blocked").unwrap();
        assert_eq!(value["status"], "blocked");
    }

    #[test]
    fn headings_inside_code_fences_are_text() {
        let schema = json!({"type":"object","properties":{"summary":{"type":"string"}}});
        let value = parse(&schema, "## summary\n```\n## status\n```").unwrap();
        assert_eq!(value["summary"], "```\n## status\n```");
    }

    #[test]
    fn the_template_names_every_section() {
        let text = instructions(&plan_schema());
        for expected in [
            "## status",
            "## requirements",
            "### <short name>",
            "- <text>",
            "## tasks",
            "## summary",
        ] {
            assert!(text.contains(expected), "{expected} missing from {text}");
        }
        assert!(!text.contains("JSON Schema"));
    }
}
