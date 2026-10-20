use harness_code_health::{Category, Severity};

use crate::{
    doc::yaml::Node_,
    file::InfraFile,
    rule::{code, finding, rule},
};

const TASK_LISTS: [&str; 4] = ["tasks", "pre_tasks", "post_tasks", "handlers"];
const BLOCKS: [&str; 3] = ["block", "rescue", "always"];
const KEYWORDS: [&str; 24] = [
    "name",
    "when",
    "become",
    "become_user",
    "register",
    "tags",
    "vars",
    "loop",
    "with_items",
    "notify",
    "ignore_errors",
    "changed_when",
    "failed_when",
    "delegate_to",
    "environment",
    "args",
    "until",
    "retries",
    "delay",
    "no_log",
    "check_mode",
    "diff",
    "async",
    "poll",
];

fn collect<'a>(list: &'a Node_, out: &mut Vec<&'a Node_>) {
    for task in list.items() {
        let mut nested = false;
        for key in BLOCKS {
            if let Some(inner) = task.get(key) {
                nested = true;
                collect(inner, out);
            }
        }
        if !nested {
            out.push(task);
        }
    }
}

/// Every task of a playbook or a role's task file.
fn tasks<'a>(file: &'a InfraFile<'_>) -> Vec<&'a Node_> {
    let mut out = Vec::new();
    let Some(yaml) = &file.yaml else {
        return out;
    };
    for document in &yaml.documents {
        let plays = document.items().iter().any(|i| i.get("hosts").is_some());
        if plays {
            for play in document.items() {
                for key in TASK_LISTS {
                    if let Some(list) = play.get(key) {
                        collect(list, &mut out);
                    }
                }
            }
        } else {
            collect(document, &mut out);
        }
    }
    out
}

/// The module a task calls: its first key that is not a task keyword.
fn module(task: &Node_) -> Option<&str> {
    task.entries()
        .iter()
        .map(|e| e.key.as_str())
        .find(|k| !KEYWORDS.contains(k) && !k.starts_with("with_"))
}

rule!(
    UnnamedTask,
    "ans_unnamed_task",
    Category::Infrastructure,
    "A task has no name, so run output and failures say nothing about what was being done.",
    [Ansible],
    |this, file, _config| {
        tasks(file)
            .into_iter()
            .filter(|t| t.entry("name").is_none() && module(t).is_some())
            .map(|t| {
                let module = module(t).unwrap_or("");
                finding(
                    this,
                    file,
                    Severity::Low,
                    (t.line, t.line),
                    module,
                    format!("A {} task has no `name`.", code(module)),
                )
            })
            .collect()
    }
);

rule!(
    NonIdempotent,
    "ans_non_idempotent",
    Category::Infrastructure,
    "A shell or command task reports changed on every run because nothing says when it is done.",
    [Ansible],
    |this, file, _config| {
        tasks(file)
            .into_iter()
            .filter(|t| matches!(module(t), Some("shell" | "command" | "raw" | "ansible.builtin.shell" | "ansible.builtin.command")))
            .filter(|t| {
                let args_has = |key: &str| t.get("args").is_some_and(|a| a.entry(key).is_some());
                t.entry("changed_when").is_none()
                    && t.entry("when").is_none()
                    && !args_has("creates")
                    && !args_has("removes")
            })
            .map(|t| {
                let module = module(t).unwrap_or("");
                finding(
                    this,
                    file,
                    Severity::Low,
                    (t.line, t.line),
                    module,
                    format!("A {} task has no `creates`, `removes`, `when` or `changed_when`, so it always reports changed; use a module or say when it is done.", code(module)),
                )
            })
            .collect()
    }
);
