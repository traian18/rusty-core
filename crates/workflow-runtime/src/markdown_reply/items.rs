//! Reading list items: bullets, `ID: text` pairs and one-heading-per-item records.

use crate::markdown_reply::coerce::coerce;
use crate::markdown_reply::layout::{headings, normalize, sections};
use crate::markdown_reply::object::parse_object;
use crate::markdown_reply::schema::{body_field, is_pair, kind, properties, Kind};
use serde_json::{Map, Value};

/// `R` for requirements, `T` for tasks, `C` for criteria.
pub(super) fn id_prefix(name: &str) -> String {
    name.chars()
        .next()
        .map_or_else(|| "N".into(), |c| c.to_uppercase().to_string())
}

/// A short code such as `R1`, `T-2`, `C1.a`; never a sentence.
pub(super) fn looks_like_id(token: &str) -> bool {
    let token = token.trim();
    !token.is_empty()
        && token.len() <= 12
        && !token.contains(char::is_whitespace)
        && token.chars().any(|c| c.is_ascii_digit())
}

pub(super) fn parse_items(name: &str, items: &Value, body: &str, level: usize) -> Vec<Value> {
    if is_pair(items) && headings(body).iter().all(|(_, found, _)| *found <= level) {
        let body_name = body_field(items).unwrap_or_default();
        return bullets(body)
            .into_iter()
            .enumerate()
            .map(|(index, bullet)| {
                // `C1: text` carries its own ID; a plain bullet gets one.
                let (id, text) = bullet_id(bullet.as_str()).unwrap_or_else(|| {
                    (format!("{}{}", id_prefix(name), index + 1), bullet.clone())
                });
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
pub(super) fn split_title(title: &str) -> (Option<String>, String) {
    let mut rest = title.trim();
    for prefix in ["requirement", "criterion", "criteria", "task", "step"] {
        // `get` rather than indexing: the cut may fall inside a character
        // (`R1 — Title`).
        let after = rest
            .get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .and_then(|_| rest.get(prefix.len()..))
            .filter(|tail| tail.starts_with(' '));
        if let Some(tail) = after {
            rest = tail.trim_start();
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

pub(super) fn parse_item(schema: &Value, title: &str, section: &str, fallback_id: String) -> Value {
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

/// The ID a bullet starts with, and the text after it: `C1: text`, `C1 text`,
/// `**C14 — complete.** text`. Only the first word is looked at, so a colon
/// later in the text (`a.ts:12`) is not mistaken for the end of the ID.
pub(super) fn bullet_id(bullet: &str) -> Option<(String, String)> {
    let start = bullet.trim_start_matches(['*', '`']);
    let end = start
        .find(|c: char| c.is_whitespace() || c == ':')
        .unwrap_or(start.len());
    let id = clean_id(&start[..end]);
    if !looks_like_id(&id) {
        return None;
    }
    let text = start[end..].trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, ':' | '*' | '-' | '–' | '—')
    });
    Some((id, text.trim().to_owned()))
}

pub(super) fn clean_id(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| matches!(c, '*' | '`' | ':' | ' '))
        .to_owned()
}

pub(super) fn bullets(body: &str) -> Vec<String> {
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
