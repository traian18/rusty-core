//! A persisted, serial task queue. Every task gets fresh builder and reviewer
//! sessions; a retry retains completed tasks, never just a prose handoff.
use crate::orchestration::{is_user_change, BasicSchemaValidator, SchemaValidator, SubflowRequest};
use crate::{
    orchestration::{AgentExecutionError, AgentStepOutput, AgentStepRequest, StepContext},
    session_agent_executor::IsolatedSessionAgentExecutor,
};
use harness_core::orchestration::{
    InputDecision, InputRequest, StructuredOutputMode, TaskFailurePolicy,
};
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
    /// The queue's named flow that does this task, instead of the builder
    /// and reviewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flow: Option<String>,
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
    /// Accepted tasks, and tasks the user chose to skip (`skipped: true`).
    completed: Vec<Value>,
    current: Option<Value>,
    #[serde(default)]
    handled_rejections: usize,
    /// What one repair task after the planned ones must fix: a final
    /// verification rejection no planned task could be matched to, or the
    /// changes the user asked for after trying the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_repair: Option<String>,
    /// How many of the user's change requests were already turned into a repair.
    #[serde(default)]
    handled_changes: usize,
    /// Repair tasks that were accepted, so the steps after this one can see
    /// what was fixed after the planned tasks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    repairs: Vec<Value>,
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

/// A planner sometimes writes a task's instructions on its `flow:` line. A
/// flow value that is not an offered name and contains whitespace is prose:
/// it becomes the instructions when those are empty, and is dropped otherwise.
fn unmix_flows(value: &Value, offered: &dyn Fn(&str) -> bool) -> Value {
    let mut value = value.clone();
    for task in value["tasks"].as_array_mut().into_iter().flatten() {
        let Some(flow) = task["flow"].as_str().map(|f| f.trim().to_owned()) else {
            continue;
        };
        if flow.is_empty() || offered(&flow) || !flow.contains(char::is_whitespace) {
            continue;
        }
        if task["instructions"]
            .as_str()
            .map_or(true, |i| i.trim().is_empty())
        {
            task["instructions"] = Value::String(flow);
        }
        if let Some(fields) = task.as_object_mut() {
            fields.remove("flow");
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
        if t.instructions.trim().is_empty() {
            return Err(invalid(format!(
                "task {} has empty instructions: write them as the task's body, not on the flow line",
                t.id
            )));
        }
        if t.id.trim().is_empty()
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

/// Why a task could not be completed, as the user is told.
enum TaskOutcome {
    Accepted,
    Failed(String),
}

/// What happens after a task failed.
enum AfterFailure {
    Retry(Option<String>),
    Skip,
    RevisePlan(String),
}

/// Loose id comparison (`C-1`, `c1` and `C1` are one), as in [`canonicalize`].
fn id_key(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The criteria a downstream verification found unmet: the `- ID [status]`
/// lines of its "Focus on these unmet criteria" section (criteria left for
/// manual checks are listed elsewhere and are not reopened), or an older
/// `<criteria>[...]</criteria>` JSON list.
fn unmet_criteria(message: &str) -> HashSet<String> {
    if let Some(results) = message
        .split_once("<criteria>")
        .and_then(|(_, v)| v.split_once("</criteria>"))
        .and_then(|(json, _)| serde_json::from_str::<Vec<Value>>(json).ok())
    {
        return results
            .into_iter()
            .filter(|r| r["status"] != "pass")
            .filter_map(|r| r["id"].as_str().map(id_key))
            .collect();
    }
    let Some((_, section)) = message.split_once("Focus on these unmet criteria:") else {
        return HashSet::new();
    };
    section
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.trim().strip_prefix("- "))
        .filter_map(|line| line.split_once(" [").map(|(id, _)| id_key(id)))
        .collect()
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
        let plan = plan(&unmix_flows(source, &|name| queue.flows.contains_key(name)))?;
        for task in &plan.tasks {
            if let Some(name) = task
                .flow
                .as_deref()
                .filter(|name| !queue.flows.contains_key(*name))
            {
                let offered: Vec<&str> = queue.flows.keys().map(String::as_str).collect();
                return Err(invalid(format!(
                    "task {} names the flow {name:?}, which this step does not offer (it offers: {})",
                    task.id,
                    if offered.is_empty() { "none".into() } else { offered.join(", ") }
                )));
            }
        }
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
            let settled = done["skipped"] == true
                || done["flow"].is_string()
                || review_complete(task, &done["review"]);
            if done["task_id"] != task.id || !settled {
                return Err(invalid(
                    "checkpoint contains unverified or out-of-order tasks",
                ));
            }
        }
        // A downstream acceptance rejection may invalidate any previously accepted
        // task. Re-inspect the queue instead of blindly declaring it complete:
        // reopen from the first task owning an unmet criterion, or, when none
        // can be matched, keep the accepted tasks and run one repair task.
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
            let failed = unmet_criteria(&rejection.message);
            let reopen = plan.tasks.iter().position(|t| {
                t.criterion_ids
                    .iter()
                    .any(|id| failed.contains(&id_key(id)))
            });
            match reopen {
                Some(at) => {
                    progress.completed.truncate(at);
                    progress.current = Some(json!({"rejection":rejection.message}));
                }
                None => {
                    progress.pending_repair = Some(format!(
                        "Fix what the final verification of the completed tasks found:\n{}",
                        rejection.message
                    ));
                    progress.current = None;
                }
            }
        }
        // The user tried the result and asked for changes: the accepted
        // tasks stay, and one repair task makes the changes after them.
        let changes: Vec<_> = request
            .feedback
            .iter()
            .filter(|f| is_user_change(&f.code))
            .collect();
        if let Some(change) = changes
            .last()
            .filter(|_| changes.len() > progress.handled_changes)
        {
            progress.handled_changes = changes.len();
            let asked = format!(
                "The user tried the result and asked for these changes; make them and keep everything else as it is:\n{}",
                change.message
            );
            progress.pending_repair = Some(match progress.pending_repair.take() {
                Some(earlier) => format!("{earlier}\n\n{asked}"),
                None => asked,
            });
        }
        context
            .checkpoint(serde_json::to_value(&progress).unwrap())
            .await?;
        let mut questions = 0;
        let mut index = progress.completed.len();
        let repair = progress.pending_repair.clone().map(|message| Task {
            id: "final-repair".into(),
            requirement_ids: Vec::new(),
            criterion_ids: Vec::new(),
            depends_on: Vec::new(),
            flow: None,
            instructions: message,
        });
        loop {
            // Past the planned tasks only the repair task can still be due.
            let planned = index < plan.tasks.len();
            let task = match plan.tasks.get(index) {
                Some(task) => task,
                None => match &repair {
                    Some(task) if progress.pending_repair.is_some() => task,
                    _ => break,
                },
            };
            let mut guidance: Option<String> = None;
            loop {
                let reason = match self
                    .run_task(
                        &request,
                        &plan,
                        task,
                        &mut progress,
                        context,
                        usage,
                        guidance.as_deref(),
                        planned,
                    )
                    .await?
                {
                    TaskOutcome::Accepted => break,
                    TaskOutcome::Failed(reason) => reason,
                };
                questions += 1;
                match self
                    .after_failure(&request, task, &reason, questions, context)
                    .await?
                {
                    AfterFailure::Retry(notes) => guidance = notes,
                    AfterFailure::Skip => {
                        if !planned {
                            progress.pending_repair = None;
                        } else {
                            progress
                                .completed
                                .push(json!({"task_id":task.id,"skipped":true,"reason":reason}));
                        }
                        progress.current = None;
                        context
                            .checkpoint(serde_json::to_value(&progress).unwrap())
                            .await?;
                        break;
                    }
                    AfterFailure::RevisePlan(changes) => {
                        return Err(AgentExecutionError::retryable(
                            crate::orchestration::CHANGES_REQUESTED,
                            changes,
                            harness_core::orchestration::RetryReason::ChangesRequested,
                        ));
                    }
                }
            }
            if !planned {
                // The repair task ran (or was skipped): the queue is done.
                progress.pending_repair = None;
                context
                    .checkpoint(serde_json::to_value(&progress).unwrap())
                    .await?;
                break;
            }
            index += 1;
        }
        let skipped = progress
            .completed
            .iter()
            .filter(|done| done["skipped"] == true)
            .count();
        let mut summary = if skipped == 0 {
            format!("Implemented and inspected {} tasks.", plan.tasks.len())
        } else {
            format!(
                "Implemented and inspected {} of {} tasks; {skipped} skipped at the user's request.",
                plan.tasks.len() - skipped,
                plan.tasks.len()
            )
        };
        if !progress.repairs.is_empty() {
            summary.push_str(&format!(
                " {} repair task(s) ran after them; see repairs.",
                progress.repairs.len()
            ));
        }
        summary.push_str(" Final validation remains required.");
        let mut value =
            json!({"status":"implemented","summary":summary,"tasks":progress.completed});
        if !progress.repairs.is_empty() {
            value["repairs"] = Value::Array(progress.repairs.clone());
        }
        Ok(AgentStepOutput { value })
    }

    /// Builds and reviews one task, repairing up to `max_repairs` times, and
    /// records it as completed when it is a planned task (`record`). A task
    /// that cannot be completed is reported, not raised: what happens next is
    /// the queue's failure policy.
    #[allow(clippy::too_many_arguments)]
    async fn run_task(
        &self,
        request: &AgentStepRequest,
        plan: &Plan,
        task: &Task,
        progress: &mut Progress,
        context: &mut StepContext,
        usage: &mut HashMap<AgentId, AgentUsageMetrics>,
        guidance: Option<&str>,
        record: bool,
    ) -> Result<TaskOutcome, AgentExecutionError> {
        let queue = request.task_queue.as_ref().expect("task queue");
        if let Some(target) = task.flow.as_deref().and_then(|name| queue.flows.get(name)) {
            return self
                .run_task_flow(request, plan, task, target, progress, context, record)
                .await;
        }
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
            // The user's change requests are the repair task's instructions,
            // not something every task and reviewer should act on.
            child.feedback.retain(|f| !is_user_change(&f.code));
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
            if let Some(guidance) = guidance {
                // The user's own words, given when they chose to try again.
                child.instructions.push_str(&format!(
                    "\n\nThe user asked you to try this task again with this guidance:\n{guidance}"
                ));
            }
            let built = self
                .execute_once(child.clone(), context, usage)
                .await?
                .value;
            BasicSchemaValidator
                .validate(&child.output_schema, &built)
                .map_err(|e| AgentExecutionError::new("invalid_task_result", e.to_string()))?;
            progress.current = Some(json!({"task_id":task.id,"build":built,"repair":repair}));
            context
                .checkpoint(serde_json::to_value(&*progress).unwrap())
                .await?;
            let status = built["status"].as_str().unwrap_or("invalid");
            if status == "checkpoint" || status == "needs_repair" {
                continue;
            }
            // A check that cannot run here (no network, a missing tool) does
            // not stop the work: the reviewer judges the code that was
            // written, and the final validation reports what did not run.
            if status != "complete" && status != "blocked_environment" {
                let why = if status == "blocked_user" {
                    "it needs something from you"
                } else {
                    "the builder could not finish it"
                };
                return Ok(TaskOutcome::Failed(format!(
                    "Task {} stopped because {why}: {}",
                    task.id,
                    built["summary"].as_str().unwrap_or("no summary")
                )));
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
            let accepted = review_complete(task, &reviewed);
            if accepted && record {
                progress.completed.push(json!({"task_id":task.id,"build":progress.current.as_ref().unwrap()["build"],"review":reviewed}));
            } else if accepted {
                progress.repairs.push(json!({"task_id":task.id,"instructions":task.instructions,"build":progress.current.as_ref().unwrap()["build"],"review":reviewed}));
            }
            if accepted {
                progress.current = None;
            }
            context
                .checkpoint(serde_json::to_value(&*progress).unwrap())
                .await?;
            if accepted {
                return Ok(TaskOutcome::Accepted);
            }
            if matches!(
                reviewed["status"].as_str(),
                Some("blocked_environment" | "blocked_user" | "checkpoint")
            ) {
                return Ok(TaskOutcome::Failed(format!(
                    "The review of task {} could not finish: {}",
                    task.id,
                    reviewed["summary"].as_str().unwrap_or("no summary")
                )));
            }
        }
        Ok(TaskOutcome::Failed(format!(
            "Task {} still needs repair after {} attempts; completed tasks are checkpointed",
            task.id,
            queue.max_repairs + 1
        )))
    }

    /// Does `task` by running one of the queue's flows. The flow judges its
    /// own result, so there is no separate review; a flow that fails fails
    /// the task, which the failure policy then handles.
    #[allow(clippy::too_many_arguments)]
    async fn run_task_flow(
        &self,
        request: &AgentStepRequest,
        plan: &Plan,
        task: &Task,
        target: &harness_core::orchestration::SubflowTarget,
        progress: &mut Progress,
        context: &mut StepContext,
        record: bool,
    ) -> Result<TaskOutcome, AgentExecutionError> {
        let subflows = self.subflows.as_ref().ok_or_else(|| {
            AgentExecutionError::new(
                "subflows_unavailable",
                "this host cannot run flows inside flows",
            )
        })?;
        let mut input = json!({
            "request": request.input.get("request"),
            "context": request.input.get("context"),
            "requirements": plan.requirements,
            "task": task,
            "completed_tasks": progress.completed,
        });
        if let Some(fields) = request.input.as_object() {
            for (name, value) in fields {
                input
                    .as_object_mut()
                    .unwrap()
                    .entry(name.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        let result = subflows
            .execute(
                SubflowRequest {
                    run_id: request.run_id.clone(),
                    node_id: harness_core::orchestration::OrchestrationNodeId::new(format!(
                        "{}.{}",
                        request.node_id, task.id
                    )),
                    attempt: request.attempt,
                    target: target.clone(),
                    input,
                    depth: request.subflow_depth,
                    options: request.run_options.clone(),
                },
                context,
            )
            .await;
        match result {
            Ok(output) => {
                if record {
                    progress.completed.push(json!({
                        "task_id": task.id,
                        "flow": task.flow,
                        "result": output,
                    }));
                }
                progress.current = None;
                context
                    .checkpoint(serde_json::to_value(&*progress).unwrap())
                    .await?;
                Ok(TaskOutcome::Accepted)
            }
            // Stopping the run is not a failed task.
            Err(error) if error.code == "cancelled" => Err(error),
            Err(error) => Ok(TaskOutcome::Failed(format!(
                "Task {} could not be done by the flow {}: {}",
                task.id,
                task.flow.as_deref().unwrap_or(""),
                error.message
            ))),
        }
    }

    /// The queue's failure policy: stop, skip, or ask the user what to do.
    async fn after_failure(
        &self,
        request: &AgentStepRequest,
        task: &Task,
        reason: &str,
        question: usize,
        context: &mut StepContext,
    ) -> Result<AfterFailure, AgentExecutionError> {
        let queue = request.task_queue.as_ref().expect("task queue");
        match queue.on_task_failure {
            TaskFailurePolicy::Stop => {
                return Err(AgentExecutionError::new("task_failed", reason));
            }
            TaskFailurePolicy::Skip => return Ok(AfterFailure::Skip),
            TaskFailurePolicy::Ask => {}
        }
        let decision = |id: &str, label: &str, requires_text: bool| InputDecision {
            id: id.into(),
            label: label.into(),
            requires_text,
        };
        let mut decisions = vec![
            decision("retry", "Try again", false),
            decision("skip", "Skip this task", false),
        ];
        if request.plan_revisable {
            decisions.push(decision("revise_plan", "Revise the plan", true));
        }
        decisions.push(decision("stop", "Stop", false));
        let response = context
            .ask(InputRequest {
                id: format!(
                    "{}:{}:{}:{question}",
                    request.node_id, request.attempt, task.id
                ),
                kind: "task_failure".into(),
                prompt: format!(
                    "Task {} could not be completed. Try it again (your notes guide the next try), skip it, {}or stop the workflow.",
                    task.id,
                    if request.plan_revisable { "revise the plan, " } else { "" }
                ),
                subject: json!({"task": format!("{}: {}", task.id, task.instructions), "problem": reason}),
                decisions,
            })
            .await?;
        let notes = response.text.filter(|text| !text.trim().is_empty());
        match response.decision.as_str() {
            "retry" => Ok(AfterFailure::Retry(notes)),
            "skip" => Ok(AfterFailure::Skip),
            "revise_plan" => Ok(AfterFailure::RevisePlan(notes.unwrap_or_default())),
            _ => Err(AgentExecutionError::new(
                "task_stopped",
                format!("{reason}. Stopped at your request; completed tasks are checkpointed."),
            )),
        }
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

    #[test]
    fn unmet_criteria_come_from_the_repair_focus_and_skip_manual_checks() {
        let message = "Verification did not pass: criteria:/check/criteria: not met: C-2 [fail]\n\nFocus on these unmet criteria:\n- C-2 [fail]: Button does nothing.\n- c3 [unverified]: no test\n\nLeft for the user to check by hand; do not try to fix these:\n- C4 [manual]: needs a phone\n\nReview summary: close";
        let unmet = unmet_criteria(message);
        assert_eq!(
            unmet,
            ["c2".to_string(), "c3".to_string()].into_iter().collect()
        );
        let legacy = r#"rejected <criteria>[{"id":"C1","status":"fail"},{"id":"C2","status":"pass"}]</criteria>"#;
        assert_eq!(
            unmet_criteria(legacy),
            ["c1".to_string()].into_iter().collect()
        );
        assert!(unmet_criteria("no criteria here").is_empty());
    }
}
