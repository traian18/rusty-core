use super::*;
use crate::task_queue::{Criterion, Requirement};
use harness_core::behavior::ProfileRef;
use harness_core::orchestration::{
    AgentContextMode, OrchestrationNodeId, OrchestrationRunId, StructuredOutputMode,
    TaskFailurePolicy, TaskQueueConfig,
};

fn plan() -> Plan {
    let criterion = |id: &str| Criterion {
        id: id.into(),
        text: format!("{id} text"),
    };
    Plan {
        status: "ready".into(),
        summary: "two requirements".into(),
        requirements: vec![
            Requirement {
                id: "R1".into(),
                text: "chats".into(),
                criteria: vec![criterion("C1"), criterion("C2")],
            },
            Requirement {
                id: "R2".into(),
                text: "stop".into(),
                criteria: vec![criterion("C3")],
            },
        ],
        tasks: vec![task()],
    }
}

fn task() -> Task {
    Task {
        id: "T4".into(),
        requirement_ids: vec!["R1".into()],
        criterion_ids: vec!["C2".into()],
        depends_on: vec![],
        instructions: "keep the chat".into(),
        flow: None,
    }
}

fn request() -> AgentStepRequest {
    let mut request = AgentStepRequest {
        task_queue: None,
        checkpoint: None,
        run_id: OrchestrationRunId::new("run"),
        node_id: OrchestrationNodeId::new("build"),
        attempt: 1,
        instructions: String::new(),
        input: Value::Null,
        output_schema: json!({}),
        tools: Vec::new(),
        model: None,
        structured_output: StructuredOutputMode::HostValidated,
        context_mode: AgentContextMode::IsolatedChild,
        profile: None,
        feedback: Vec::new(),
        plan_revisable: false,
        subflow_depth: 0,
        run_options: Default::default(),
        previous_attempt: None,
        journal: None,
        research: Default::default(),
    };
    request.input = json!({
        "request": "Implement change 0001",
        "context": "## Design\n".to_owned() + &"decision ".repeat(5_000),
        "queue_plan": {"tasks": "T4 T5 T6 ".repeat(1_000)},
        "plan_notes": "use the existing store",
    });
    request.task_queue = Some(TaskQueueConfig {
        plan_pointer: "/queue_plan".into(),
        review_profile: ProfileRef {
            id: "review".into(),
            revision: None,
        },
        review_instructions: String::new(),
        task_planner: None,
        max_repairs: 2,
        on_task_failure: TaskFailurePolicy::Stop,
        flows: Default::default(),
    });
    request
}

fn progress() -> Progress {
    Progress {
        completed: vec![json!({
            "task_id": "T3",
            "build": {"status": "complete", "summary": "x".repeat(2_000)},
            "review": {"status": "complete", "summary": "long review", "criteria": [{"id": "C1", "evidence": "a.ts:1"}]},
        })],
        current: Some(
            json!({"task_id": "T4", "build": {"status": "needs_repair", "summary": "typecheck fails"}}),
        ),
        ..Default::default()
    }
}

#[test]
fn a_builder_with_a_step_plan_gets_only_its_task() {
    let input = builder_input(&request(), &plan(), &task(), &progress(), true);
    let fields: Vec<&str> = input
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(!fields.contains(&"context"), "{fields:?}");
    assert!(!fields.contains(&"queue_plan"), "{fields:?}");
    assert!(fields.contains(&"plan_notes"), "small bound fields stay");
    assert_eq!(
        input["requirements"],
        json!([{"id": "R1", "text": "chats", "criteria": [{"id": "C2", "text": "C2 text"}]}])
    );
    assert_eq!(input["completed_tasks"][0]["task_id"], "T3");
    assert!(
        input["completed_tasks"][0]["summary"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= DONE_SUMMARY_CHARS + 1
    );
    assert!(input["completed_tasks"][0].get("review").is_none());
    assert_eq!(
        input["previous_attempt"]["build"]["summary"],
        "typecheck fails"
    );
    assert!(
        input.to_string().len() < 3_000,
        "{}",
        input.to_string().len()
    );
}

#[test]
fn without_a_step_plan_the_builder_keeps_the_context() {
    let input = builder_input(&request(), &plan(), &task(), &progress(), false);
    assert!(input["context"].as_str().unwrap().starts_with("## Design"));
}

#[test]
fn the_planner_sees_everything_but_the_other_tasks() {
    let input = planner_input(&request(), &plan(), &task(), &progress());
    assert!(input["context"].as_str().unwrap().starts_with("## Design"));
    assert!(input.get("queue_plan").is_none());
    assert_eq!(input["requirements"].as_array().unwrap().len(), 2);
    assert!(input["completed_tasks"][0].get("review").is_none());
}
