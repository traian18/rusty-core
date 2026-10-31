//! Repairing a planner's plan before it is checked: numbering, loose references, orphan criteria.
//!
//! IDs are the host's bookkeeping, not the model's: missing or repeated ones are numbered,
//! references are matched loosely and dropped when they point at nothing, and a criterion no
//! task names goes to the task that owns its requirement (or the last task). What is left is a
//! real planning gap.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::task_queue::ids::{id_key, Numbering};

pub(super) fn canonicalize(value: &Value) -> Value {
    let mut value = value.clone();
    let owners = number_requirements(&mut value);
    number_tasks(&mut value, &owners);
    cover_orphans(&mut value, &owners);
    value
}

/// Which requirement owns each criterion, in plan order.
struct Owners(Vec<(String, String)>);

impl Owners {
    fn criterion_ids(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|(criterion, _)| criterion.clone())
            .collect()
    }

    fn requirement_ids(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for (_, requirement) in &self.0 {
            if !seen.contains(requirement) {
                seen.push(requirement.clone());
            }
        }
        seen
    }

    fn requirement_of(&self, criterion: &str) -> Option<&String> {
        self.0
            .iter()
            .find(|(known, _)| known == criterion)
            .map(|(_, requirement)| requirement)
    }

    fn criteria_of(&self, requirements: &[String]) -> Vec<String> {
        self.0
            .iter()
            .filter(|(_, requirement)| requirements.contains(requirement))
            .map(|(criterion, _)| criterion.clone())
            .collect()
    }
}

fn text(value: &Value, name: &str) -> String {
    value[name].as_str().unwrap_or("").trim().to_owned()
}

fn names(value: &Value, name: &str) -> Vec<String> {
    value[name]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn find(known: &[String], wanted: &str) -> Option<String> {
    known
        .iter()
        .find(|id| id_key(id) == id_key(wanted))
        .cloned()
}

fn number_requirements(value: &mut Value) -> Owners {
    let mut requirement_ids = Numbering::new("R");
    let mut criterion_ids = Numbering::new("C");
    let mut owners = Vec::new();
    for requirement in value["requirements"].as_array_mut().into_iter().flatten() {
        let id = requirement_ids.claim(text(requirement, "id"));
        requirement["id"] = Value::String(id.clone());
        let criteria = number_criteria(requirement, &mut criterion_ids, &id, &mut owners);
        requirement["criteria"] = Value::Array(criteria);
    }
    Owners(owners)
}

/// The requirement's criteria with ids; a requirement without any gets its own text as one.
fn number_criteria(
    requirement: &Value,
    ids: &mut Numbering,
    owner: &str,
    owners: &mut Vec<(String, String)>,
) -> Vec<Value> {
    let mut criteria: Vec<Value> = requirement["criteria"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|criterion| !text(criterion, "text").is_empty())
        .collect();
    if criteria.is_empty() {
        criteria.push(json!({"id": "", "text": text(requirement, "text")}));
    }
    for criterion in &mut criteria {
        let id = ids.claim(text(criterion, "id"));
        criterion["id"] = Value::String(id.clone());
        owners.push((id, owner.to_owned()));
    }
    criteria
}

fn number_tasks(value: &mut Value, owners: &Owners) {
    let criterion_ids = owners.criterion_ids();
    let requirement_ids = owners.requirement_ids();
    let mut task_numbers = Numbering::new("T");
    let mut task_ids: Vec<String> = Vec::new();
    for task in value["tasks"].as_array_mut().into_iter().flatten() {
        let id = task_numbers.claim(text(task, "id"));
        let (covers, needs) = coverage(task, owners, &criterion_ids, &requirement_ids);
        let depends: Vec<String> = names(task, "depends_on")
            .iter()
            .filter_map(|wanted| find(&task_ids, wanted))
            .collect();
        task["id"] = Value::String(id.clone());
        task["criterion_ids"] = json!(covers);
        task["requirement_ids"] = json!(needs);
        task["depends_on"] = json!(depends);
        task_ids.push(id);
    }
}

/// The criteria and requirements a task names, matched loosely against the real ones.
fn coverage(
    task: &Value,
    owners: &Owners,
    criterion_ids: &[String],
    requirement_ids: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut covers: Vec<String> = names(task, "criterion_ids")
        .iter()
        .filter_map(|wanted| find(criterion_ids, wanted))
        .collect();
    let mut needs: Vec<String> = names(task, "requirement_ids")
        .iter()
        .filter_map(|wanted| find(requirement_ids, wanted))
        .collect();
    if covers.is_empty() {
        // Naming only a requirement covers its criteria.
        covers = owners.criteria_of(&needs);
    }
    for criterion in &covers {
        if let Some(requirement) = owners.requirement_of(criterion) {
            if !needs.contains(requirement) {
                needs.push(requirement.clone());
            }
        }
    }
    (covers, needs)
}

/// Gives every criterion no task names to the last task of its requirement.
fn cover_orphans(value: &mut Value, owners: &Owners) {
    let Some(tasks) = value["tasks"]
        .as_array_mut()
        .filter(|tasks| !tasks.is_empty())
    else {
        return;
    };
    let covered: HashSet<String> = tasks
        .iter()
        .flat_map(|task| names(task, "criterion_ids"))
        .collect();
    for (criterion, requirement) in &owners.0 {
        if covered.contains(criterion) {
            continue;
        }
        let at = tasks
            .iter()
            .rposition(|task| names(task, "requirement_ids").contains(requirement))
            .unwrap_or(tasks.len() - 1);
        for (field, id) in [
            ("criterion_ids", criterion),
            ("requirement_ids", requirement),
        ] {
            add_once(&mut tasks[at][field], id);
        }
    }
}

fn add_once(list: &mut Value, id: &str) {
    if let Some(list) = list.as_array_mut() {
        if !list.iter().any(|known| known.as_str() == Some(id)) {
            list.push(Value::String(id.to_owned()));
        }
    }
}
