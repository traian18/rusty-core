//! The task plan the queue runs, composed from `spec.md` and `tasks.md`. The queue builds what is
//! left (unticked tasks); verification checks the whole spec.
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    layout::Layout,
    model::{SpecDoc, Task},
    parse,
    store::{SpecError, SpecStore},
};

/// The id of the task added when every planned task is already ticked.
pub const FINAL_CHECK_ID: &str = "final-check";

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuePlans {
    /// Every requirement, criterion and task: what verification checks.
    pub full: Value,
    /// Only the unticked tasks and the criteria they cover: what the queue builds.
    pub remaining: Value,
    pub total_tasks: usize,
    pub remaining_tasks: usize,
}

pub fn queue_plans(store: &dyn SpecStore, dir: &str) -> Result<QueuePlans, SpecError> {
    let base = Layout::of(store).change(dir);
    let spec_text = store
        .read(&format!("{base}/spec.md"))
        .ok_or_else(|| SpecError::new("spec.md is missing; run Specify first"))?;
    let tasks_text = store
        .read(&format!("{base}/tasks.md"))
        .ok_or_else(|| SpecError::new("tasks.md is missing; run Design & tasks first"))?;
    let spec = parse::spec::parse(&spec_text);
    let tasks = parse::tasks::parse(&tasks_text).tasks;
    if spec.requirements.is_empty() {
        return Err(SpecError::new("spec.md has no requirements (### R1: …)"));
    }
    if tasks.is_empty() {
        return Err(SpecError::new("tasks.md has no tasks (- [ ] **T1** …)"));
    }
    let remaining: Vec<&Task> = tasks.iter().filter(|t| !t.done).collect();
    let all: Vec<&Task> = tasks.iter().collect();
    let title = spec.title.clone().unwrap_or_else(|| dir.to_owned());
    Ok(QueuePlans {
        full: plan_value(&spec, &all, &format!("All tasks of {title}."), None),
        remaining: if remaining.is_empty() {
            final_check(&spec, &title)
        } else {
            plan_value(
                &spec,
                &remaining,
                &format!("The tasks of {title} that are not ticked yet."),
                Some(&remaining),
            )
        },
        total_tasks: tasks.len(),
        remaining_tasks: remaining.len(),
    })
}

/// A plan of `tasks`. With `only`, requirements keep just the criteria those tasks cover and
/// dependencies on tasks outside it are dropped, so nothing already done is planned again.
fn plan_value(spec: &SpecDoc, tasks: &[&Task], summary: &str, only: Option<&[&Task]>) -> Value {
    let covered: Vec<&str> = tasks
        .iter()
        .flat_map(|t| t.criteria.iter().map(String::as_str))
        .collect();
    let named_requirements: Vec<&str> = tasks
        .iter()
        .flat_map(|t| t.requirements.iter().map(String::as_str))
        .collect();
    let requirements: Vec<Value> = spec
        .requirements
        .iter()
        .filter_map(|r| {
            let criteria: Vec<Value> = r
                .criteria
                .iter()
                .filter(|c| {
                    only.is_none()
                        || covered.contains(&c.id.as_str())
                        || (r.criteria.iter().all(|c| !covered.contains(&c.id.as_str()))
                            && named_requirements.contains(&r.id.as_str()))
                })
                .map(|c| json!({"id": c.id, "text": c.text}))
                .collect();
            let included = only.is_none()
                || !criteria.is_empty()
                || named_requirements.contains(&r.id.as_str());
            included.then(|| json!({"id": r.id, "text": r.title, "criteria": criteria}))
        })
        .collect();
    let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    let tasks: Vec<Value> = tasks
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "requirement_ids": t.requirements,
                "criterion_ids": t.criteria,
                "depends_on": t.after.iter().filter(|a| only.is_none() || ids.contains(&a.as_str())).collect::<Vec<_>>(),
                "instructions": instructions(t),
            })
        })
        .collect();
    json!({"status": "ready", "requirements": requirements, "tasks": tasks, "summary": summary})
}

fn instructions(task: &Task) -> String {
    if task.instructions.trim().is_empty() {
        task.title.clone()
    } else {
        format!("{}\n{}", task.title, task.instructions)
    }
}

/// Every task is ticked: one task that confirms the spec against the code and fixes what is missing.
fn final_check(spec: &SpecDoc, title: &str) -> Value {
    let all: Vec<&Task> = Vec::new();
    let mut plan = plan_value(
        spec,
        &all,
        &format!("Every task of {title} is ticked; confirm the whole spec."),
        None,
    );
    let criteria: Vec<&str> = spec
        .requirements
        .iter()
        .flat_map(|r| r.criteria.iter().map(|c| c.id.as_str()))
        .collect();
    let requirements: Vec<&str> = spec.requirements.iter().map(|r| r.id.as_str()).collect();
    plan["tasks"] = json!([{
        "id": FINAL_CHECK_ID,
        "requirement_ids": requirements,
        "criterion_ids": criteria,
        "depends_on": [],
        "instructions": "Every planned task is already ticked. Read the code and the tests and confirm that each criterion of the spec holds. Fix whatever is missing or wrong and add the tests it needs; change nothing else.",
    }]);
    plan["requirements"] = Value::Array(
        spec.requirements.iter().map(|r| json!({"id": r.id, "text": r.title, "criteria": r.criteria.iter().map(|c| json!({"id": c.id, "text": c.text})).collect::<Vec<_>>()})).collect(),
    );
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    const SPEC: &str = "# Albums\n\n## Requirements\n### R1: Create\n- C1: WHEN a THEN b\n- C2: WHEN c THEN d\n### R2: Delete\n- C3: WHEN e THEN f\n";

    fn store(tasks: &str) -> MemoryStore {
        let store = MemoryStore::default();
        store.insert("specs/changes/0001-a/spec.md", SPEC);
        store.insert("specs/changes/0001-a/tasks.md", tasks);
        store
    }
    fn ids(value: &Value, key: &str) -> Vec<String> {
        value[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_owned())
            .collect()
    }

    const TASKS: &str = "- [x] **T1** Model. R1 (C1)\n  Add Album.\n- [ ] **T2** Rename. R1 (C2) after T1\n- [ ] **T3** Delete. R2 (C3)\n";

    #[test]
    fn the_full_plan_holds_everything_and_the_queue_plan_only_what_is_left() {
        let plans = queue_plans(&store(TASKS), "0001-a").unwrap();
        assert_eq!((plans.total_tasks, plans.remaining_tasks), (3, 2));
        assert_eq!(ids(&plans.full, "tasks"), ["T1", "T2", "T3"]);
        assert_eq!(ids(&plans.remaining, "tasks"), ["T2", "T3"]);
        let remaining_criteria: Vec<String> = plans.remaining["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|r| ids(r, "criteria"))
            .collect();
        assert_eq!(remaining_criteria, ["C2", "C3"]);
        assert_eq!(plans.remaining["tasks"][0]["depends_on"], json!([]));
        assert_eq!(plans.full["tasks"][1]["depends_on"], json!(["T1"]));
        assert_eq!(plans.full["status"], "ready");
    }

    #[test]
    fn a_task_carries_its_title_and_instructions() {
        let plans = queue_plans(&store(TASKS), "0001-a").unwrap();
        assert_eq!(plans.full["tasks"][0]["instructions"], "Model\nAdd Album.");
        assert_eq!(plans.full["tasks"][1]["instructions"], "Rename");
    }

    #[test]
    fn when_everything_is_ticked_one_final_check_covers_the_whole_spec() {
        let plans = queue_plans(
            &store("- [x] **T1** A. R1 (C1, C2)\n- [x] **T2** B. R2 (C3)\n"),
            "0001-a",
        )
        .unwrap();
        assert_eq!(plans.remaining_tasks, 0);
        assert_eq!(ids(&plans.remaining, "tasks"), [FINAL_CHECK_ID]);
        assert_eq!(
            plans.remaining["tasks"][0]["criterion_ids"],
            json!(["C1", "C2", "C3"])
        );
        assert_eq!(ids(&plans.remaining, "requirements"), ["R1", "R2"]);
    }

    #[test]
    fn missing_or_empty_documents_say_what_to_run() {
        let store = MemoryStore::default();
        assert!(queue_plans(&store, "0001-a")
            .unwrap_err()
            .message
            .contains("Specify"));
        store.insert("specs/changes/0001-a/spec.md", SPEC);
        assert!(queue_plans(&store, "0001-a")
            .unwrap_err()
            .message
            .contains("Design & tasks"));
        store.insert("specs/changes/0001-a/tasks.md", "# Tasks\n");
        assert!(queue_plans(&store, "0001-a")
            .unwrap_err()
            .message
            .contains("no tasks"));
    }
}
