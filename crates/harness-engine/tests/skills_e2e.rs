//! Integration test: `SessionBuilder::skills(...)` reaches the backend —
//! the catalog lands in the system prompt, the instruction bodies do *not*,
//! the skill tools are advertised, and all of it composes with a
//! caller-supplied context provider rather than clobbering it.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_context::{ContextProvider, StaticSystemPromptProvider};
use harness_engine::{Harness, SkillsConfig};
use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_protocol::ids::BackendId;
use harness_protocol::usage::{Cost, ModelUsage};
use harness_runtime::traits::{ExecutionBackend, SimpleToolRegistry};
use tempfile::TempDir;

/// Records the exact `ExecutionRequest` the backend received, after every
/// decorator (tool advertising, context assembly) has run.
struct RecordingBackend {
    seen: Mutex<Vec<ExecutionRequest>>,
}

#[async_trait]
impl ExecutionBackend for RecordingBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "recording".to_string(),
            description: "test double".to_string(),
            capabilities: BackendCapabilities::default(),
        }
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }

    async fn execute(
        &self,
        request: ExecutionRequest,
        _sink: broadcast::Sender<ExecutionEvent>,
        _cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        let request_id = request.request_id;
        self.seen.lock().unwrap().push(request);
        Ok(ExecutionResult {
            request_id,
            usage: ModelUsage::default(),
            cost: Cost::default(),
            finish_reason: "end_turn".to_string(),
        })
    }
}

const DESCRIPTION: &str = "Generate a formatted PDF report from CSV data.";
const INSTRUCTIONS: &str = "Step one: read the CSV. Step two: render the template.";

async fn write_skill(root: &Path, name: &str) {
    let dir = root.join(name);
    tokio::fs::create_dir_all(&dir).await.expect("create dir");
    tokio::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {DESCRIPTION}\n---\n{INSTRUCTIONS}\n"),
    )
    .await
    .expect("write SKILL.md");
}

fn skills_config(root: &Path) -> SkillsConfig {
    SkillsConfig {
        workspace_root: None,
        include_user_dir: false,
        extra_roots: vec![root.to_path_buf()],
    }
}

/// Drives one prompt through a session and returns what the backend saw.
async fn run_and_capture(
    backend: Arc<RecordingBackend>,
    build: impl FnOnce(harness_engine::SessionBuilder) -> harness_engine::SessionBuilder,
) -> ExecutionRequest {
    let builder = Harness::new()
        .session()
        .backend(backend.clone())
        .tools(Arc::new(SimpleToolRegistry::new()));

    let handle = build(builder)
        .start()
        .await
        .expect("SessionBuilder::start() should succeed");

    handle
        .send("hello from test")
        .await
        .expect("SessionHandle::send() should succeed");

    for _ in 0..50 {
        if !backend.seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let seen = backend.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "backend should have received one request");
    seen[0].clone()
}

#[tokio::test]
async fn the_catalog_reaches_the_prompt_and_the_tools_are_advertised() {
    let temp = TempDir::new().expect("tempdir");
    write_skill(temp.path(), "pdf-report").await;

    let backend = Arc::new(RecordingBackend {
        seen: Mutex::new(Vec::new()),
    });
    let request = run_and_capture(backend, |builder| {
        builder.skills(skills_config(temp.path()))
    })
    .await;

    assert!(
        request.system_prompt.contains("pdf-report"),
        "skill name missing from prompt: {}",
        request.system_prompt
    );
    assert!(
        request.system_prompt.contains(DESCRIPTION),
        "skill description missing from prompt: {}",
        request.system_prompt
    );

    let advertised: Vec<&str> = request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert!(
        advertised.contains(&"skill.load") && advertised.contains(&"skill.read"),
        "skill tools were not advertised: {advertised:?}"
    );
}

/// The assertion that pins progressive disclosure. If this fails, skills
/// have quietly become "paste every instruction body into every request",
/// which is the whole thing the design exists to avoid.
#[tokio::test]
async fn instruction_bodies_never_reach_the_prompt() {
    let temp = TempDir::new().expect("tempdir");
    write_skill(temp.path(), "pdf-report").await;

    let backend = Arc::new(RecordingBackend {
        seen: Mutex::new(Vec::new()),
    });
    let request = run_and_capture(backend, |builder| {
        builder.skills(skills_config(temp.path()))
    })
    .await;

    assert!(
        !request.system_prompt.contains(INSTRUCTIONS),
        "instruction body leaked into the system prompt: {}",
        request.system_prompt
    );
}

/// `.skills()` installs a context provider, and so does
/// `.context_provider()`. Both must survive — this is what
/// `ChainedContextProvider` is doing in `start()`.
#[tokio::test]
async fn skills_compose_with_a_caller_supplied_context_provider() {
    let temp = TempDir::new().expect("tempdir");
    write_skill(temp.path(), "pdf-report").await;

    let backend = Arc::new(RecordingBackend {
        seen: Mutex::new(Vec::new()),
    });
    let caller: Arc<dyn ContextProvider> =
        Arc::new(StaticSystemPromptProvider::new("project instructions"));

    let request = run_and_capture(backend, |builder| {
        builder
            .skills(skills_config(temp.path()))
            .context_provider(caller)
    })
    .await;

    assert!(
        request.system_prompt.contains("project instructions"),
        "caller's provider was dropped: {}",
        request.system_prompt
    );
    assert!(
        request.system_prompt.contains(DESCRIPTION),
        "skills provider was dropped: {}",
        request.system_prompt
    );
}

/// Without `.skills()`, nothing changes at all — no prompt text, no tools.
#[tokio::test]
async fn a_session_without_skills_is_unaffected() {
    let backend = Arc::new(RecordingBackend {
        seen: Mutex::new(Vec::new()),
    });
    let request = run_and_capture(backend, |builder| builder).await;

    assert!(
        request.system_prompt.is_empty(),
        "{}",
        request.system_prompt
    );
    assert!(request.tools.is_empty(), "{:?}", request.tools);
}

/// A malformed `SKILL.md` must not take the session down with it.
#[tokio::test]
async fn a_broken_skill_does_not_prevent_the_session_from_starting() {
    let temp = TempDir::new().expect("tempdir");
    write_skill(temp.path(), "pdf-report").await;
    let broken = temp.path().join("broken");
    tokio::fs::create_dir_all(&broken).await.expect("mkdir");
    tokio::fs::write(broken.join("SKILL.md"), "no frontmatter at all\n")
        .await
        .expect("write");

    let backend = Arc::new(RecordingBackend {
        seen: Mutex::new(Vec::new()),
    });
    let request = run_and_capture(backend, |builder| {
        builder.skills(skills_config(temp.path()))
    })
    .await;

    assert!(
        request.system_prompt.contains("pdf-report"),
        "the healthy skill should still have loaded: {}",
        request.system_prompt
    );
}
