//! The instructions that tell a model which Markdown layout to answer in.

use crate::markdown_reply::layout::heading;
use crate::markdown_reply::schema::{body_field, hint, is_pair, kind, properties, Kind};
use serde_json::Value;

/// The reply-format section appended to a step's prompt.
pub(crate) fn instructions(schema: &Value) -> String {
    let mut template = String::new();
    template_object(schema, 2, &mut template);
    format!(
        "\n\nWrite your final message in plain Markdown, not JSON. Use exactly these section headings, in this order, and put nothing before the first heading. Replace each <...> with your content; repeat an item's block once per item. IDs are optional: the host numbers items for you.\n\n{}",
        template.trim_end()
    )
}

pub(super) fn template_object(schema: &Value, level: usize, out: &mut String) {
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

pub(super) fn template_items(property: &Value, level: usize, out: &mut String) {
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
