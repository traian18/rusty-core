use std::collections::HashSet;

use serde_json::{json, Value};

use super::plan;

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
/// The plans composed from a change folder (spec.md and tasks.md) are the plans this queue runs.
#[test]
fn the_plans_of_a_change_folder_are_accepted() {
    use rusty_specs::{plan::queue_plans, MemoryStore};
    let store = MemoryStore::default();
    store.insert(
        "specs/changes/0001-a/spec.md",
        "# A\n\n## Requirements\n### R1: One\n- C1: WHEN a THEN b\n- C2: WHEN c THEN d\n### R2: Two\n- C3: WHEN e THEN f\n",
    );
    let tasks = [
        "- [x] **T1** Model. R1 (C1)\n  Add it.\n- [ ] **T2** Sidebar. R1 (C2) after T1\n- [ ] **T3** Delete. R2 (C3)\n",
        "- [x] **T1** Model. R1 (C1, C2)\n- [x] **T2** Delete. R2 (C3)\n",
    ];
    for text in tasks {
        store.insert("specs/changes/0001-a/tasks.md", text);
        let plans = queue_plans(&store, "0001-a").unwrap();
        let full = plan(&plans.full).unwrap();
        let remaining = plan(&plans.remaining).unwrap();
        assert_eq!(full.tasks.len(), plans.total_tasks);
        assert_eq!(remaining.tasks.len(), plans.remaining_tasks.max(1));
        // Nothing already done is planned again: every criterion of the queue's plan is covered by its tasks.
        let covered: Vec<&String> = remaining
            .tasks
            .iter()
            .flat_map(|t| &t.criterion_ids)
            .collect();
        for criterion in remaining.requirements.iter().flat_map(|r| &r.criteria) {
            assert!(
                covered.contains(&&criterion.id),
                "{} is not covered",
                criterion.id
            );
        }
    }
}
