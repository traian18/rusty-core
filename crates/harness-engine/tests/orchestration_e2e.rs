//! End-to-end coverage for the orchestration layer through the public engine
//! API: `HarnessBuilder::orchestration` → `SessionHandle::start_orchestration`
//! → isolated agent session → verified, schema-checked, durable result.

use std::sync::Arc;

use harness_core::orchestration::{
    default_orchestration_definition, DefinitionRef, OrchestrationDefinitionId,
    OrchestrationOutcome, OrchestrationRunId, OrchestrationStatus,
};
use harness_engine::{Harness, HarnessError, OrchestrationConfig, OrchestrationRequest};
use harness_protocol::backend::{ExecutionEvent, ExecutionResult};
use harness_protocol::ids::RequestId;
use harness_protocol::usage::{Cost, ModelUsage};
use harness_runtime::orchestration::{FileOrchestrationStore, OrchestrationStore};
use harness_runtime::testing::{FakeBackend, FakeToolRegistry};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

fn report() -> Value {
    json!({ "summary": "Added reset flow", "status": "completed", "artifacts": [], "claimsToVerify": [] })
}

/// A backend whose every request answers with the execution report.
fn reporting_backend() -> Arc<FakeBackend> {
    let request_id = RequestId::new();
    let result = ExecutionResult {
        request_id,
        usage: ModelUsage::default(),
        cost: Cost::default(),
        finish_reason: "end_turn".into(),
    };
    Arc::new(
        FakeBackend::new()
            .with_events(vec![
                ExecutionEvent::TextDelta {
                    request_id,
                    delta: report().to_string(),
                },
                ExecutionEvent::Completed {
                    request_id,
                    result: result.clone(),
                },
            ])
            .with_result(result),
    )
}

async fn harness_with(config: OrchestrationConfig) -> Harness {
    Harness::builder()
        .orchestration(config)
        .build()
        .await
        .expect("harness builds")
}

async fn session(harness: &Harness) -> harness_engine::SessionHandle {
    harness
        .session()
        .backend(reporting_backend())
        .tools(Arc::new(FakeToolRegistry::new()))
        .start()
        .await
        .expect("session starts")
}

#[tokio::test]
async fn default_workflow_completes_and_is_durable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileOrchestrationStore::new(dir.path()));
    let harness = harness_with(OrchestrationConfig::default().with_store(store.clone())).await;
    let session = session(&harness).await;

    let run = session
        .start_orchestration(OrchestrationRequest::default_workflow(
            "reset-1",
            "Add password reset support",
        ))
        .await
        .expect("run starts");
    let output = run.wait().await.expect("run finishes");

    assert_eq!(
        output.result.status,
        OrchestrationOutcome::Completed,
        "{:?}",
        output.result
    );
    assert_eq!(output.result.output, Some(report()));
    assert_eq!(output.state.status, OrchestrationStatus::Completed);

    let run_id = OrchestrationRunId::from("reset-1");
    let snapshot = store
        .load_snapshot(&run_id)
        .await
        .unwrap()
        .expect("persisted");
    assert_eq!(
        snapshot.result.unwrap().status,
        OrchestrationOutcome::Completed
    );
    assert_eq!(store.load_events(&run_id).await.unwrap(), output.events);

    // A finished run is not resumable.
    let error = session
        .resume_orchestration(
            run_id,
            DefinitionRef::LatestPublished(OrchestrationDefinitionId::from("rusty.default")),
        )
        .await
        .err()
        .expect("finished runs cannot resume");
    assert!(matches!(error, HarnessError::OrchestrationRuntime(_)));

    // The direct session is untouched and still usable.
    session
        .send("hello")
        .await
        .expect("direct prompts still work");
}

#[tokio::test]
async fn registered_definitions_run_by_exact_revision() {
    let harness = harness_with(OrchestrationConfig::default()).await;
    let mut custom = default_orchestration_definition();
    custom.id = OrchestrationDefinitionId::from("team.review");
    custom.name = "Review".into();
    harness
        .orchestration()
        .expect("configured")
        .register(custom)
        .expect("valid definition registers");

    let output = session(&harness)
        .await
        .run_orchestration(
            OrchestrationRequest {
                run_id: OrchestrationRunId::from("review-1"),
                definition: DefinitionRef::Exact {
                    id: OrchestrationDefinitionId::from("team.review"),
                    revision: 1,
                },
                input: json!({ "request": "Review the change" }),
            },
            CancellationToken::new(),
        )
        .await
        .expect("run finishes");
    assert_eq!(output.result.definition_id.as_str(), "team.review");
    assert_eq!(output.result.status, OrchestrationOutcome::Completed);
}

#[tokio::test]
async fn invalid_definitions_are_rejected_at_registration() {
    let harness = harness_with(OrchestrationConfig::default()).await;
    let mut broken = default_orchestration_definition();
    broken.id = OrchestrationDefinitionId::from("broken");
    broken.edges.clear();
    let error = harness
        .orchestration()
        .unwrap()
        .register(broken)
        .unwrap_err();
    assert!(
        matches!(error, HarnessError::OrchestrationDefinition(message) if message.contains("missing_success_route") || message.contains("on_success"))
    );
}

#[tokio::test]
async fn orchestration_is_opt_in() {
    let harness = Harness::builder().build().await.unwrap();
    let error = session(&harness)
        .await
        .start_orchestration(OrchestrationRequest::default_workflow("r", "x"))
        .await
        .err()
        .expect("not configured");
    assert!(matches!(error, HarnessError::OrchestrationNotConfigured));
}

/// What an editor does: a workflow document from disk whose step names a
/// draft workspace profile, run on a session that allows drafts.
#[tokio::test]
async fn json_workflows_run_steps_under_draft_workspace_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join(".rusty/profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("careful.json"),
        json!({ "schema_version": 1, "id": "careful", "revision": 1, "name": "Careful",
                "status": "draft", "instructions": { "text": "Be careful." } })
        .to_string(),
    )
    .unwrap();

    let mut document = serde_json::to_value(default_orchestration_definition()).unwrap();
    document["id"] = json!("editor.flow");
    document["status"] = json!("draft");
    for node in document["nodes"].as_array_mut().unwrap() {
        if node["type"] == "agent" {
            node["config"]["profile"] = json!({ "id": "careful" });
        }
    }
    let config = OrchestrationConfig::default().allow_drafts(true);
    let (id, revision) = config.register_json(document).expect("valid document");
    assert_eq!((id.as_str(), revision), ("editor.flow", 1));
    let harness = harness_with(config).await;

    let start = |allow: bool| {
        harness
            .session()
            .backend(reporting_backend())
            .tools(Arc::new(FakeToolRegistry::new()))
            .workspace_profiles(dir.path())
            .allow_draft_profiles(allow)
            .start()
    };

    let strict = start(false).await.expect("session starts");
    let error = strict
        .start_orchestration(OrchestrationRequest::exact(
            "r0",
            &id,
            revision,
            json!({ "request": "go" }),
        ))
        .await
        .err()
        .expect("draft profiles do not resolve by default");
    assert!(
        matches!(error, HarnessError::OrchestrationDefinition(_)),
        "{error}"
    );

    let session = start(true).await.expect("session starts");
    let run = session
        .start_orchestration(OrchestrationRequest::exact(
            "r1",
            &id,
            revision,
            json!({ "request": "go" }),
        ))
        .await
        .expect("run starts");
    let output = run.wait().await.expect("run finishes");
    assert_eq!(
        output.result.status,
        OrchestrationOutcome::Completed,
        "{:?}",
        output.result
    );

    assert!(matches!(
        OrchestrationConfig::default().register_json(json!({ "id": "broken" })),
        Err(HarnessError::OrchestrationDefinition(_))
    ));
}
