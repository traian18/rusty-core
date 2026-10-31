use serde_json::json;

use super::{result_schema, review_complete};
use crate::task_queue::model::Task;
use crate::task_queue::plan::plan;

fn example() -> serde_json::Value {
    json!({"status":"ready","requirements":[{"id":"R1","text":"change","criteria":[{"id":"C1","text":"observable behavior"}]}],"tasks":[{"id":"T1","requirement_ids":["R1"],"criterion_ids":["C1"],"depends_on":[],"instructions":"implement"}],"summary":"plan"})
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
fn a_markdown_review_of_criteria_not_starting_at_c1_is_accepted() {
    // A real reviewer reply (rusty-ide change 0001, task T3): its bullets
    // cite `file:line`, and their positions (C1, C2, C3) were once taken
    // for the IDs, so the queue rejected an approved task again and again.
    let task = Task {
        id: "T3".into(),
        requirement_ids: vec!["R1".into(), "R6".into()],
        criterion_ids: vec!["C2".into(), "C14".into(), "C15".into()],
        depends_on: vec!["T2".into()],
        flow: None,
        instructions: "Scope session persistence by chat".into(),
    };
    let reply = "## status\ncomplete\n\n## summary\nT3 meets its acceptance criteria. Persistence operations use the selected `ChatTurnContext`; the regression test verifies that a delayed save for chat A does not replace chat B’s visible metadata, and that switching back restores A’s metadata. The saved-chat format remains limited to conversation data and supported metadata, without runtime-only state.\n\n## criteria\n- **C2 — complete.** `useChatSession.ts:19–23, 37–55` initializes session values from the supplied context and guards delayed-save UI updates so they apply only while that context is selected. `useAgentTab.ts:47, 55` passes the selected context into the hook. `useChatSession.test.tsx:66–105` checks separate queues and metadata for A and B, asserts B remains selected after A’s delayed save resolves, and verifies A’s metadata when selected again.\n- **C14 — complete.** `useChatSession.test.tsx:52–64` restores a legacy conversation with messages and no run-related or optional metadata, and checks that restore succeeds.\n- **C15 — complete.** `agentChatPersistence.ts:5–19` preserves the existing fallbacks: missing modified files become an empty list, missing workflow is undefined, and missing flow switching is false. `useChatSession.test.tsx:52–64` checks the restore fallbacks, including absent checkpoint and summary. `agentChatPersistence.test.ts:43–54` confirms saved messages and metadata while excluding runtime-only fields.";
    let reviewed = crate::markdown_reply::parse(&result_schema(true), reply).unwrap();
    let ids: Vec<_> = reviewed["criteria"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["C2", "C14", "C15"]);
    assert!(review_complete(&task, &reviewed));
}
