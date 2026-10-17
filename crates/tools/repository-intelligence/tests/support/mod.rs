#![allow(dead_code)]
use std::{path::Path, sync::Arc};

use git2::{IndexAddOption, Repository, Signature, Time};
use harness_findings::{
    DefaultFindingsAnalyzer, FindingsAnalysis, FindingsConfig, FindingsService, FsSourceTextReader,
};
use harness_git_analysis::{GitAnalysis, GitAnalysisBuilder};
use harness_repository_imports::{
    RustImportAdapter, RustImportResolver, TypeScriptImportAdapter, TypeScriptImportResolver,
};
use harness_tool_repository_intelligence::RepositoryIntelligence;
use harness_tools::{CancellationToken, ToolInput};
use harness_workspace::{
    dependency_groups::DefaultGroupAnalyzer,
    import_graph::ImportSupport,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder},
};
use serde_json::Value;
use tempfile::TempDir;

pub struct Fixture {
    pub dir: TempDir,
    pub graph: Arc<dyn RepositoryGraph>,
    pub git: Arc<dyn GitAnalysis>,
}

pub fn write(root: &Path, relative: &str, text: &str) {
    let full = root.join(relative);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

pub fn commit(repo: &Repository, message: &str, time: i64) {
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
        message,
        &tree,
        &parents.iter().collect::<Vec<_>>(),
    )
    .unwrap();
}

/// A Rust crate with an import cycle (a <-> b), a dependent (c), and a small TypeScript package,
/// under Git history in which a.rs and b.rs change together.
pub fn write_repository(root: &Path) {
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    write(root, "src/lib.rs", "pub mod a;\npub mod b;\npub mod c;\n");
    write(root, "src/a.rs", "use crate::b::B;\npub struct A;\n");
    write(root, "src/b.rs", "use crate::a::A;\npub struct B;\n");
    write(root, "src/c.rs", "use crate::a::A;\n");
    write(
        root,
        "web/package.json",
        r#"{"name":"web","types":"src/index.ts"}"#,
    );
    write(root, "web/src/index.ts", "export { x } from './x';\n");
    write(root, "web/src/x.ts", "export const x = 1;\n");
}

pub fn write_history(root: &Path) {
    let repo = Repository::init(root).unwrap();
    let mut time = 1_700_000_000;
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
        time += 86_400;
        commit(&repo, "change a and b", time);
    }
}

pub fn services(root: &Path) -> (Arc<dyn RepositoryGraph>, Arc<dyn GitAnalysis>) {
    let graph = RepositoryGraphBuilder::new(std::fs::canonicalize(root).unwrap())
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(RustImportAdapter),
            resolver: Arc::new(RustImportResolver),
        })
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(TypeScriptImportAdapter),
            resolver: Arc::new(TypeScriptImportResolver),
        })
        .unwrap()
        .dependency_groups(Arc::new(DefaultGroupAnalyzer::standard()))
        .build();
    let git = GitAnalysisBuilder::for_repository(root).build().unwrap();
    (Arc::new(graph), Arc::new(git))
}

pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    write_repository(dir.path());
    write_history(dir.path());
    let (graph, git) = services(dir.path());
    Fixture { dir, graph, git }
}

impl Fixture {
    pub fn all(&self) -> RepositoryIntelligence {
        RepositoryIntelligence::new()
            .with_graph(self.graph.clone())
            .with_git(self.git.clone())
    }

    pub fn graph_only(&self) -> RepositoryIntelligence {
        RepositoryIntelligence::new().with_graph(self.graph.clone())
    }
}

/// Runs one tool and returns its output and error flag.
pub async fn call(
    intelligence: &RepositoryIntelligence,
    tool: &str,
    arguments: Value,
) -> (Value, bool) {
    call_with(intelligence, tool, arguments, CancellationToken::new()).await
}

pub async fn call_with(
    intelligence: &RepositoryIntelligence,
    tool: &str,
    arguments: Value,
    cancel: CancellationToken,
) -> (Value, bool) {
    let executor = intelligence
        .executor_for(tool)
        .unwrap_or_else(|| panic!("{tool} is not enabled"));
    let result = executor
        .execute(ToolInput { arguments }, cancel)
        .await
        .unwrap();
    (result.output, result.is_error)
}

pub fn error_code(output: &Value) -> &str {
    output["error"]["code"].as_str().unwrap_or("<no error>")
}

impl Fixture {
    /// Graph, Git and findings all enabled.
    pub fn everything(&self) -> RepositoryIntelligence {
        self.all().with_findings(self.findings())
    }

    pub fn findings(&self) -> Arc<dyn FindingsAnalysis> {
        Arc::new(FindingsService::new(
            self.graph.clone(),
            Some(self.git.clone()),
            Arc::new(DefaultFindingsAnalyzer::standard(FindingsConfig::default()).unwrap()),
            Arc::new(FsSourceTextReader::new(self.dir.path())),
        ))
    }
}
