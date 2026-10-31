//! Planners sometimes write a task's instructions on its `flow:` line.

use serde_json::Value;

/// A flow value that is not an offered name and contains whitespace is prose: it becomes the
/// task's instructions when those are empty, and is dropped otherwise.
pub(in crate::task_queue) fn unmix_flows(value: &Value, offered: &dyn Fn(&str) -> bool) -> Value {
    let mut value = value.clone();
    for task in value["tasks"].as_array_mut().into_iter().flatten() {
        let Some(flow) = task["flow"].as_str().map(|flow| flow.trim().to_owned()) else {
            continue;
        };
        if flow.is_empty() || offered(&flow) || !flow.contains(char::is_whitespace) {
            continue;
        }
        if task["instructions"]
            .as_str()
            .is_none_or(|instructions| instructions.trim().is_empty())
        {
            task["instructions"] = Value::String(flow);
        }
        if let Some(fields) = task.as_object_mut() {
            fields.remove("flow");
        }
    }
    value
}
