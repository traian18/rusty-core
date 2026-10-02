//! A persisted, serial task queue. Every task gets fresh builder and reviewer
//! sessions; a retry retains completed tasks, never just a prose handoff.
use crate::orchestration::{BasicSchemaValidator, SchemaValidator};
use crate::{
    orchestration::{AgentExecutionError, AgentStepOutput, AgentStepRequest, StepContext},
    session_agent_executor::IsolatedSessionAgentExecutor,
};
use harness_core::orchestration::StructuredOutputMode;
use harness_protocol::{ids::AgentId, usage::AgentUsageMetrics};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Criterion {
    id: String,
    text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Requirement {
    id: String,
    text: String,
    criteria: Vec<Criterion>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Task {
    id: String,
    requirement_ids: Vec<String>,
    criterion_ids: Vec<String>,
    depends_on: Vec<String>,
    instructions: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Plan {
    status: String,
    requirements: Vec<Requirement>,
    tasks: Vec<Task>,
    summary: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Progress {
    plan: Option<Plan>,
    completed: Vec<Value>,
    current: Option<Value>,
    #[serde(default)]
    handled_rejections: usize,
}

fn invalid(message: impl Into<String>) -> AgentExecutionError {
    AgentExecutionError::new("invalid_task_plan", message)
}
/// IDs are the host's bookkeeping, not the model's: missing or repeated ones
/// are numbered, references are matched loosely and dropped when they point at
/// nothing, and a criterion no task names goes to the task that owns its
/// requirement (or the last task). What is left is a real planning gap.
fn canonicalize(value: &Value) -> Value {
    let key = |id: &str| {
        id.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let text = |v: &Value, name: &str| v[name].as_str().unwrap_or("").trim().to_owned();
    let unique = |wanted: String, prefix: &str, used: &mut HashSet<String>| {
        let mut id = wanted;
        let mut n = used.len();
        while id.is_empty() || used.contains(&key(&id)) {
            n += 1;
            id = format!("{prefix}{n}");
        }
        used.insert(key(&id));
        id
    };
    let mut value = value.clone();
    let mut used_requirements = HashSet::new();
    let mut used_criteria = HashSet::new();
    // criterion id -> requirement id, in plan order
    let mut owner: Vec<(String, String)> = Vec::new();
    if let Some(requirements) = value["requirements"].as_array_mut() {
        for requirement in requirements.iter_mut() {
            let id = unique(text(requirement, "id"), "R", &mut used_requirements);
            requirement["id"] = Value::String(id.clone());
            let criteria = requirement["criteria"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut criteria: Vec<Value> = criteria
                .into_iter()
                .filter(|c| !text(c, "text").is_empty())
                .collect();
            if criteria.is_empty() {
                criteria.push(json!({"id": "", "text": text(requirement, "text")}));
            }
            for criterion in &mut criteria {
                let cid = unique(text(criterion, "id"), "C", &mut used_criteria);
                criterion["id"] = Value::String(cid.clone());
                owner.push((cid, id.clone()));
            }
            requirement["criteria"] = Value::Array(criteria);
        }
    }
    let requirement_ids: Vec<String> = {
        let mut seen = Vec::new();
        for (_, r) in &owner {
            if !seen.contains(r) {
                seen.push(r.clone());
            }
        }
        seen
    };
    let find =
        |known: &[String], wanted: &str| known.iter().find(|id| key(id) == key(wanted)).cloned();
    let criterion_ids: Vec<String> = owner.iter().map(|(c, _)| c.clone()).collect();
    let mut used_tasks = HashSet::new();
    let mut task_ids: Vec<String> = Vec::new();
    if let Some(tasks) = value["tasks"].as_array_mut() {
        for task in tasks.iter_mut() {
            let id = unique(text(task, "id"), "T", &mut used_tasks);
            let names = |name: &str| -> Vec<String> {
                task[name]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            };
            let mut covers: Vec<String> = names("criterion_ids")
                .iter()
                .filter_map(|c| find(&criterion_ids, c))
                .collect();
            let mut needs: Vec<String> = names("requirement_ids")
                .iter()
                .filter_map(|r| find(&requirement_ids, r))
                .collect();
            if covers.is_empty() {
                // Naming only a requirement covers its criteria.
                covers = owner
                    .iter()
                    .filter(|(_, r)| needs.contains(r))
                    .map(|(c, _)| c.clone())
                    .collect();
            }
            for c in &covers {
                if let Some((_, r)) = owner.iter().find(|(known, _)| known == c) {
                    if !needs.contains(r) {
                        needs.push(r.clone());
                    }
                }
            }
            let depends: Vec<String> = names("depends_on")
                .iter()
                .filter_map(|d| find(&task_ids, d))
                .collect();
            task["id"] = Value::String(id.clone());
            task["criterion_ids"] = json!(covers);
            task["requirement_ids"] = json!(needs);
            task["depends_on"] = json!(depends);
            task_ids.push(id);
        }
        let covered: HashSet<String> = tasks
            .iter()
            .flat_map(|t| t["criterion_ids"].as_array().cloned().unwrap_or_default())
            .filter_map(|c| c.as_str().map(str::to_owned))
            .collect();
        for (criterion, requirement) in &owner {
            if covered.contains(criterion) || tasks.is_empty() {
                continue;
            }
            let at = tasks
                .iter()
                .rposition(|t| {
                    t["requirement_ids"]
                        .as_array()
                        .is_some_and(|r| r.iter().any(|r| r.as_str() == Some(requirement)))
                })
                .unwrap_or(tasks.len() - 1);
            for (field, id) in [
                ("criterion_ids", criterion),
                ("requirement_ids", requirement),
            ] {
                let list = tasks[at][field].as_array_mut().unwrap();
                if !list.iter().any(|v| v.as_str() == Some(id)) {
                    list.push(Value::String(id.clone()));
                }
            }
        }
    }
    value
}

fn plan(value: &Value) -> Result<Plan, AgentExecutionError> {
    let value = canonicalize(value);
    let plan: Plan = serde_json::from_value(value).map_err(|e| invalid(e.to_string()))?;
    if plan.status != "ready"
        || plan.requirements.is_empty()
        || plan.tasks.is_empty()
        || plan.tasks.len() > 128
    {
        return Err(invalid("plan needs requirements and 1–128 bounded tasks"));
    }
    let mut requirements = HashSet::new();
    let mut criteria = HashMap::new();
    for r in &plan.requirements {
        if r.id.trim().is_empty()
            || r.text.trim().is_empty()
            || !requirements.insert(r.id.as_str())
            || r.criteria.is_empty()
        {
            return Err(invalid(
                "requirements must have unique IDs, text and criteria",
            ));
        }
        for c in &r.criteria {
            if c.id.trim().is_empty()
                || c.text.trim().is_empty()
                || criteria.insert(c.id.as_str(), r.id.as_str()).is_some()
            {
                return Err(invalid("criteria need unique IDs and observable text"));
            }
        }
    }
    let mut tasks = HashSet::new();
    let mut covered = HashSet::new();
    for t in &plan.tasks {
        if t.id.trim().is_empty()
            || t.instructions.trim().is_empty()
            || tasks.contains(t.id.as_str())
            || t.criterion_ids.is_empty()
            || t.requirement_ids.is_empty()
        {
            return Err(invalid("tasks need unique IDs, instructions and coverage"));
        }
        if t.depends_on.iter().any(|d| !tasks.contains(d.as_str())) {
            return Err(invalid(format!(
                "{} depends on a missing, later or cyclic task",
                t.id
            )));
        }
        if t.requirement_ids
            .iter()
            .any(|r| !requirements.contains(r.as_str()))
        {
            return Err(invalid("task references unknown requirement"));
        }
        for c in &t.criterion_ids {
            let owner = criteria
                .get(c.as_str())
                .ok_or_else(|| invalid("task references unknown criterion"))?;
            if !t.requirement_ids.iter().any(|r| r == owner) {
                return Err(invalid("criterion is not owned by a task requirement"));
            }
            covered.insert(c.as_str());
        }
        tasks.insert(t.id.as_str());
    }
    if covered.len() != criteria.len() {
        return Err(invalid("plan leaves acceptance criteria without a task"));
    }
    Ok(plan)
}

fn result_schema(review: bool) -> Value {
    let mut schema = json!({"type":"object","additionalProperties":false,"required":["status","summary"],"properties":{
        "status":{"type":"string","enum":["complete","needs_repair","blocked_environment","blocked_user","checkpoint"]},"summary":{"type":"string","minLength":1}
    }});
    if review {
        schema["required"]
            .as_array_mut()
            .unwrap()
            .push(json!("criteria"));
        schema["properties"]["criteria"] = json!({"type":"array","items":{"type":"object","additionalProperties":false,"required":["id","evidence"],"properties":{"id":{"type":"string"},"evidence":{"type":"string","minLength":1}}}});
    }
    schema
}

fn review_complete(task: &Task, result: &Value) -> bool {
    if result["status"] != "complete" {
        return false;
    }
    let Some(items) = result["criteria"].as_array() else {
        return false;
    };
    let expected: Vec<&str> = task.criterion_ids.iter().map(String::as_str).collect();
    crate::orchestration::steps::match_results(&expected, items, |c| c["id"].as_str())
        .iter()
        .all(|found| {
            found.is_some_and(|c| c["evidence"].as_str().is_some_and(|s| !s.trim().is_empty()))
        })
}

impl IsolatedSessionAgentExecutor {
    pub(crate) async fn execute_tasks(
        &self,
        request: AgentStepRequest,
        context: &mut StepContext,
        usage: &mut HashMap<AgentId, AgentUsageMetrics>,
    ) -> Result<AgentStepOutput, AgentExecutionError> {
        let queue = request.task_queue.as_ref().expect("task queue");
        let source = request
            .input
            .pointer(&queue.plan_pointer)
            .ok_or_else(|| invalid("task plan is missing"))?;
        let plan = plan(source)?;
        let mut progress: Progress = match request.checkpoint.clone() {
            Some(value) => serde_json::from_value(value)
                .map_err(|e| invalid(format!("invalid checkpoint: {e}")))?,
            None => Progress {
                plan: Some(plan.clone()),
                ..Default::default()
            },
        };
        if progress.plan.as_ref() != Some(&plan) || progress.completed.len() > plan.tasks.len() {
            return Err(invalid("checkpoint does not match the task plan"));
        }
        for (task, done) in plan.tasks.iter().zip(&progress.completed) {
            if done["task_id"] != task.id || !review_complete(task, &done["review"]) {
                return Err(invalid(
                    "checkpoint contains unverified or out-of-order tasks",
                ));
            }
        }
        // A downstream acceptance rejection may invalidate any previously accepted
        // task. Re-inspect the queue instead of blindly declaring it complete.
        let rejections: Vec<_> = request
            .feedback
            .iter()
            .filter(|f| f.code == "verification_failed")
            .collect();
        if let Some(rejection) = rejections
            .last()
            .filter(|_| rejections.len() > progress.handled_rejections)
        {
            progress.handled_rejections = rejections.len();
            let results = rejection
                .message
                .split_once("<criteria>")
                .and_then(|(_, v)| v.split_once("</criteria>"))
                .and_then(|(json, _)| serde_json::from_str::<Vec<Value>>(json).ok());
            let failed: HashSet<String> = results
                .unwrap_or_default()
                .into_iter()
                .filter(|r| r["status"] != "pass")
                .filter_map(|r| r["id"].as_str().map(str::to_owned))
                .collect();
            let reopen = plan
                .tasks
                .iter()
                .position(|t| t.criterion_ids.iter().any(|id| failed.contains(id)))
                .unwrap_or(0);
            progress.completed.truncate(reopen);
            progress.current = Some(json!({"rejection":rejection.message}));
        }
        context
            .checkpoint(serde_json::to_value(&progress).unwrap())
            .await?;
        for task in plan.tasks.iter().skip(progress.completed.len()) {
            let mut accepted = false;
            for repair in 0..=queue.max_repairs {
                if context.cancellation.is_cancelled() {
                    return Err(AgentExecutionError::new(
                        "cancelled",
                        "task queue cancelled",
                    ));
                }
                let mut child = request.clone();
                child.task_queue = None;
                child.checkpoint = None;
                child.structured_output = StructuredOutputMode::HostValidated;
                child.output_schema = result_schema(false);
                child.input = json!({"request":request.input.get("request"),"context":request.input.get("context"),"requirements":plan.requirements,"task":task,"completed_tasks":progress.completed,"previous_attempt":progress.current});
                // Preserve preparation outputs (diagnosis, architecture, baseline,
                // audit) in addition to the task-specific contract.
                if let Some(fields) = request.input.as_object() {
                    for (name, value) in fields {
                        child
                            .input
                            .as_object_mut()
                            .unwrap()
                            .entry(name.clone())
                            .or_insert_with(|| value.clone());
                    }
                }
                child.instructions = format!("{}\nImplement only workflow_input.task. Preserve completed tasks. Return status complete only when every assigned criterion is implemented. Commands really run on this machine: when a check fails or a tool is missing, read the output and try the sensible alternatives before giving up. Never leave the implementation undone because a command fails: make every change the task needs first. If the command still cannot run, quote it and its output, end the summary with a short \"Please test\" request giving the exact command for the user to run and what to report back, and use complete. Use blocked_user only when you cannot do the task without something the user must provide. For unfinished work use checkpoint, with precise remaining work in summary. Never invent evidence.", request.instructions);
                let built = self
                    .execute_once(child.clone(), context, usage)
                    .await?
                    .value;
                BasicSchemaValidator
                    .validate(&child.output_schema, &built)
                    .map_err(|e| AgentExecutionError::new("invalid_task_result", e.to_string()))?;
                progress.current = Some(json!({"task_id":task.id,"build":built,"repair":repair}));
                context
                    .checkpoint(serde_json::to_value(&progress).unwrap())
                    .await?;
                let status = built["status"].as_str().unwrap_or("invalid");
                if status == "checkpoint" || status == "needs_repair" {
                    continue;
                }
                // A check that cannot run here (no network, a missing tool) does
                // not stop the work: the reviewer judges the code that was
                // written, and the final validation reports what did not run.
                if status != "complete" && status != "blocked_environment" {
                    return Err(AgentExecutionError::new(
                        format!("task_{status}"),
                        format!(
                            "Task {}: {}. Completed tasks are checkpointed.",
                            task.id, built["summary"]
                        ),
                    ));
                }
                child.profile = Some(queue.review_profile.clone());
                child.instructions = queue.review_instructions.clone();
                child.output_schema = result_schema(true);
                child.input["build"] = built;
                // Reviewer never gets a mutating tool, even if a user supplies a
                // permissive profile. Checks run in the dedicated validation step.
                child.tools.retain(|t| {
                    matches!(
                        t.as_str(),
                        "read_file"
                            | "search_codebase"
                            | "list_files"
                            | "project_info"
                            | "report_progress"
                            | "ask_user_question"
                            | "open_document"
                            | "read_workflow_context"
                    )
                });
                let reviewed = self
                    .execute_once(child.clone(), context, usage)
                    .await?
                    .value;
                BasicSchemaValidator
                    .validate(&child.output_schema, &reviewed)
                    .map_err(|e| AgentExecutionError::new("invalid_task_review", e.to_string()))?;
                progress.current.as_mut().unwrap()["review"] = reviewed.clone();
                if review_complete(task, &reviewed) {
                    progress.completed.push(json!({"task_id":task.id,"build":progress.current.as_ref().unwrap()["build"],"review":reviewed}));
                    progress.current = None;
                    accepted = true;
                }
                context
                    .checkpoint(serde_json::to_value(&progress).unwrap())
                    .await?;
                if accepted {
                    break;
                }
                if matches!(
                    reviewed["status"].as_str(),
                    Some("blocked_environment" | "blocked_user" | "checkpoint")
                ) {
                    return Err(AgentExecutionError::new(
                        "task_blocked",
                        format!("Task {}: {}", task.id, reviewed["summary"]),
                    ));
                }
            }
            if !accepted {
                return Err(AgentExecutionError::new(
                    "task_repairs_exhausted",
                    format!(
                        "Task {} needs further repair; completed tasks are checkpointed",
                        task.id
                    ),
                ));
            }
        }
        Ok(AgentStepOutput {
            value: json!({"status":"implemented","summary":format!("Implemented and inspected {} tasks. Final validation remains required.",plan.tasks.len()),"tasks":progress.completed}),
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_plan_without_ids_or_with_loose_references_is_numbered_and_covered() {
        let loose = json!({"status":"ready","summary":"s",
            "requirements":[{"id":"","text":"one","criteria":[{"id":"","text":"a"},{"id":"","text":"b"}]},
                            {"id":"R1","text":"two","criteria":[]}],
            "tasks":[{"id":"","requirement_ids":[],"criterion_ids":["c-1","nope"],"depends_on":["t9"],"instructions":"do a"},
                     {"id":"T1","requirement_ids":["r 2"],"criterion_ids":[],"depends_on":["T1"],"instructions":"do two"}]});
        let plan = super::plan(&loose).expect("loose plans are numbered and covered");
        assert_eq!(plan.requirements[0].id, "R1");
        assert_eq!(plan.requirements[1].id, "R2", "a repeated ID is renumbered");
        assert_eq!(
            plan.requirements[1].criteria.len(),
            1,
            "a requirement without criteria gets one"
        );
        assert_eq!(plan.tasks[0].id, "T1");
        assert_eq!(plan.tasks[1].id, "T2");
        assert!(
            plan.tasks[0].depends_on.is_empty(),
            "unknown dependencies are dropped"
        );
        let covered: HashSet<_> = plan
            .tasks
            .iter()
            .flat_map(|t| t.criterion_ids.iter())
            .collect();
        assert_eq!(
            covered.len(),
            3,
            "criterion b went to the task owning its requirement"
        );
    }

    use super::*;
    fn example() -> Value {
        json!({"status":"ready","requirements":[{"id":"R1","text":"change","criteria":[{"id":"C1","text":"observable behavior"}]}],"tasks":[{"id":"T1","requirement_ids":["R1"],"criterion_ids":["C1"],"depends_on":[],"instructions":"implement"}],"summary":"plan"})
    }
    #[test]
    fn repairs_loose_references_and_still_rejects_an_empty_plan() {
        assert!(plan(&example()).is_ok());
        // An unknown criterion, a self-dependency and a forgotten criterion are
        // formatting slips: repaired, with every criterion left covered.
        let mut p = example();
        p["tasks"][0]["criterion_ids"] = json!(["unknown"]);
        p["tasks"][0]["depends_on"] = json!(["T1"]);
        p["requirements"][0]["criteria"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"C2","text":"forgotten"}));
        let repaired = plan(&p).unwrap();
        assert!(repaired.tasks[0].depends_on.is_empty());
        assert_eq!(repaired.tasks[0].criterion_ids.len(), 2);
        // No tasks, or nothing to build, is a real planning gap.
        let mut p = example();
        p["tasks"] = json!([]);
        assert!(plan(&p).is_err());
        let mut p = example();
        p["requirements"] = json!([]);
        assert!(plan(&p).is_err());
    }
    #[test]
    fn review_must_cover_each_criterion_with_evidence() {
        let task = plan(&example()).unwrap().tasks.remove(0);
        assert!(!review_complete(
            &task,
            &json!({"status":"complete","criteria":[]})
        ));
        assert!(!review_complete(
            &task,
            &json!({"status":"complete","criteria":[{"id":"C1","evidence":""}]})
        ));
        assert!(review_complete(
            &task,
            &json!({"status":"complete","criteria":[{"id":"C1","evidence":"src/a.rs:12 implements the behavior"}]})
        ));
    }
}
