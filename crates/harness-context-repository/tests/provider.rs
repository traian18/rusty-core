use std::{path::Path, sync::Arc};

use git2::{IndexAddOption, Repository, Signature, Time};
use harness_context::ContextProvider;
use harness_context_repository::{RepositoryContextConfig, RepositoryContextProvider};
use harness_git_analysis::{GitAnalysis, GitAnalysisBuilder, RepositoryCorrelation};
use harness_protocol::{
    backend::ExecutionRequest,
    ids::{MessageId, RequestId, RunId, Timestamp},
    messages::{AgentMessage, ContentBlock, MessageRole},
};
use harness_repository_imports::{RustImportAdapter, RustImportResolver};
use harness_runtime::workspace::FakeWorkspace;
use harness_workspace::{
    dependency_groups::DefaultGroupAnalyzer,
    import_graph::ImportSupport,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder},
};
use tokio_util::sync::CancellationToken;

fn write(root: &Path, relative: &str, text: &str) {
    let full = root.join(relative);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

fn commit(repo: &Repository, time: i64) {
    let mut index = repo.index().unwrap();
    index.add_all(["."], IndexAddOption::DEFAULT, None).unwrap();
    index.update_all(["."], None).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let who = Signature::new("Alice", "alice@example.com", &Time::new(time, 0)).unwrap();
    let parents = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .into_iter()
        .collect::<Vec<_>>();
    repo.commit(
        Some("HEAD"),
        &who,
        &who,
        "change",
        &tree,
        &parents.iter().collect::<Vec<_>>(),
    )
    .unwrap();
}

struct Setup {
    dir: tempfile::TempDir,
    graph: Arc<dyn RepositoryGraph>,
    git: Arc<dyn GitAnalysis>,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let root = root.as_path();
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    write(root, "src/lib.rs", "pub mod a;\npub mod b;\npub mod c;\n");
    let repo = Repository::init(root).unwrap();
    for round in 0..4 {
        write(
            root,
            "src/a.rs",
            &format!("use crate::b::B;\npub struct A;\n// {round}\n"),
        );
        write(
            root,
            "src/b.rs",
            &format!("use crate::a::A;\npub struct B;\n// {round}\n"),
        );
        write(root, "src/c.rs", "use crate::a::A;\n");
        commit(&repo, 1_700_000_000 + round * 86_400);
    }
    let graph = RepositoryGraphBuilder::new(std::fs::canonicalize(root).unwrap())
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(RustImportAdapter),
            resolver: Arc::new(RustImportResolver),
        })
        .unwrap()
        .dependency_groups(Arc::new(DefaultGroupAnalyzer::standard()))
        .build();
    Setup {
        dir,
        graph: Arc::new(graph),
        git: Arc::new(GitAnalysisBuilder::for_repository(root).build().unwrap()),
    }
}

async fn refresh_all(setup: &Setup) {
    setup.graph.refresh(CancellationToken::new()).await.unwrap();
    let correlation = RepositoryCorrelation::from_snapshot(&setup.graph.current().unwrap());
    setup
        .git
        .refresh(correlation, CancellationToken::new())
        .await
        .unwrap();
}

fn request(messages: &[(MessageRole, &str)], system: &str) -> ExecutionRequest {
    ExecutionRequest {
        request_id: RequestId::new(),
        run_id: RunId::new(),
        system_prompt: system.to_owned(),
        messages: messages
            .iter()
            .map(|(role, text)| AgentMessage {
                id: MessageId::new(),
                role: *role,
                content: vec![ContentBlock::Text {
                    text: (*text).to_owned(),
                }],
                created_at: Timestamp::now(),
            })
            .collect(),
        tools: vec![],
        extended_thinking: false,
        params: Default::default(),
    }
}

async fn assemble(provider: &RepositoryContextProvider, request: ExecutionRequest) -> String {
    provider
        .assemble(request, &FakeWorkspace::new())
        .await
        .system_prompt
}

#[tokio::test]
async fn nothing_is_added_before_a_snapshot_exists_and_the_provider_never_refreshes() {
    let setup = setup();
    let provider = RepositoryContextProvider::new(setup.graph.clone());
    let prompt = assemble(
        &provider,
        request(&[(MessageRole::User, "look at src/a.rs")], "base"),
    )
    .await;
    assert_eq!(prompt, "base");
    assert!(
        setup.graph.current().is_none(),
        "assembling context must not trigger a scan"
    );
    assert!(setup.git.current().is_none());
}

#[tokio::test]
async fn adds_an_overview_and_facts_for_files_named_in_the_latest_message() {
    let setup = setup();
    refresh_all(&setup).await;
    let provider = RepositoryContextProvider::new(setup.graph.clone()).with_git(setup.git.clone());
    let prompt = assemble(
        &provider,
        request(
            &[
                (MessageRole::User, "earlier we talked about src/c.rs"),
                (MessageRole::Assistant, "ok"),
                (
                    MessageRole::User,
                    "What depends on `./src/a.rs`? Also src/missing.rs.",
                ),
            ],
            "Be careful.",
        ),
    )
    .await;
    assert!(
        prompt.starts_with("Be careful.\n\nRepository facts"),
        "{prompt}"
    );
    assert!(
        prompt.contains("1 packages") || prompt.contains("1 package"),
        "{prompt}"
    );
    assert!(prompt.contains("1 import cycle"), "{prompt}");
    assert!(
        prompt.contains("- src/a.rs: 1 imports, 3 dependents"),
        "{prompt}"
    );
    assert!(prompt.contains("DeclaredPackage app"), "{prompt}");
    assert!(prompt.contains("history: 4 commits"), "{prompt}");
    assert!(prompt.contains("often changed with src/b.rs"), "{prompt}");
    assert!(prompt.contains("correlation, not dependency"));
    assert!(
        !prompt.contains("- src/c.rs"),
        "only the latest message selects files"
    );
    assert!(
        !prompt.contains("src/missing.rs"),
        "unindexed names are ignored"
    );
}

#[tokio::test]
async fn without_git_there_is_no_history_clause_and_the_block_is_deterministic() {
    let setup = setup();
    refresh_all(&setup).await;
    let provider = RepositoryContextProvider::new(setup.graph.clone());
    let messages = [(MessageRole::User, "explain src/b.rs and src/a.rs")];
    let first = assemble(&provider, request(&messages, "")).await;
    let second = assemble(&provider, request(&messages, "")).await;
    assert_eq!(first, second);
    assert!(!first.contains("history:"));
    // Files appear in the order they were mentioned.
    assert!(first.find("- src/b.rs").unwrap() < first.find("- src/a.rs").unwrap());
    drop(setup.dir);
}

#[tokio::test]
async fn the_block_respects_the_file_and_character_budgets() {
    let setup = setup();
    refresh_all(&setup).await;
    let text = "src/a.rs src/b.rs src/c.rs src/lib.rs";
    let few =
        RepositoryContextProvider::new(setup.graph.clone()).with_config(RepositoryContextConfig {
            max_files: 2,
            ..RepositoryContextConfig::default()
        });
    let prompt = assemble(&few, request(&[(MessageRole::User, text)], "")).await;
    assert_eq!(prompt.matches("\n- ").count(), 2);

    let tight =
        RepositoryContextProvider::new(setup.graph.clone()).with_config(RepositoryContextConfig {
            max_chars: 160,
            max_files: 3,
        });
    let prompt = assemble(&tight, request(&[(MessageRole::User, text)], "")).await;
    assert!(
        prompt.chars().count() <= 160,
        "{} chars",
        prompt.chars().count()
    );
    assert!(prompt.starts_with("Repository facts"));
    assert!(
        prompt
            .lines()
            .all(|line| line.starts_with("Repository") || line.starts_with("- ")),
        "no line is cut in half"
    );

    // A budget too small for even the header leaves the request untouched.
    let none =
        RepositoryContextProvider::new(setup.graph.clone()).with_config(RepositoryContextConfig {
            max_chars: 10,
            max_files: 3,
        });
    assert_eq!(
        assemble(&none, request(&[(MessageRole::User, text)], "base")).await,
        "base"
    );
}
