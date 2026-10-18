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
use harness_repository_wiki::PageRenderer;
use harness_workspace::{
    dependency_groups::DefaultGroupAnalyzer,
    import_graph::ImportSupport,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder},
};
use tempfile::TempDir;

pub struct Fixture {
    pub dir: TempDir,
    pub graph: Arc<dyn RepositoryGraph>,
    pub git: Arc<dyn GitAnalysis>,
    pub findings: Arc<dyn FindingsAnalysis>,
}

pub fn write(root: &Path, relative: &str, text: &str) {
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

/// A Rust crate with a cycle (a <-> b), a dependent (c), folders, a TypeScript package with a test,
/// a Ruby file and an ignored directory, under Git history in which a.rs and b.rs change together.
pub fn write_repository(root: &Path) {
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    write(
        root,
        "src/lib.rs",
        "pub mod a;\npub mod b;\npub mod c;\npub mod tools;\n",
    );
    write(root, "src/a.rs", "use crate::b::B;\npub struct A;\n");
    write(root, "src/b.rs", "use crate::a::A;\npub struct B;\n");
    write(root, "src/c.rs", "\n\nuse crate::a::A;\n");
    write(root, "src/tools/mod.rs", "pub mod x;\n");
    write(root, "src/tools/x.rs", "pub fn x() {}\n");
    write(
        root,
        "web/package.json",
        r#"{"name":"web","types":"src/index.ts"}"#,
    );
    write(root, "web/src/index.ts", "export { x } from './x';\n");
    write(root, "web/src/x.ts", "export const x = 1;\n");
    write(
        root,
        "web/src/x.test.ts",
        "import { x } from './x';\nexport const t = x;\n",
    );
    write(root, "web/src/viewHelper.ts", "export const help = 1;\n");
    write(root, "web/src/dup/shared_thing.ts", "export const a = 1;\n");
    write(
        root,
        "web/src/other/shared_thing.ts",
        "export const b = 1;\n",
    );
    write(root, "scripts/tool.rb", "puts 1\n");
    write(root, "node_modules/dep/index.js", "module.exports = 1;\n");
}

pub fn write_history(root: &Path) {
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
        commit(&repo, 1_700_000_000 + round * 86_400);
    }
}

pub fn services(
    root: &Path,
) -> (
    Arc<dyn RepositoryGraph>,
    Arc<dyn GitAnalysis>,
    Arc<dyn FindingsAnalysis>,
) {
    let graph: Arc<dyn RepositoryGraph> = Arc::new(
        RepositoryGraphBuilder::new(std::fs::canonicalize(root).unwrap())
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
            .build(),
    );
    let git: Arc<dyn GitAnalysis> =
        Arc::new(GitAnalysisBuilder::for_repository(root).build().unwrap());
    let findings: Arc<dyn FindingsAnalysis> = Arc::new(FindingsService::new(
        graph.clone(),
        Some(git.clone()),
        Arc::new(DefaultFindingsAnalyzer::standard(FindingsConfig::default()).unwrap()),
        Arc::new(FsSourceTextReader::new(root)),
    ));
    (graph, git, findings)
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        write_repository(dir.path());
        write_history(dir.path());
        let (graph, git, findings) = services(dir.path());
        Self {
            dir,
            graph,
            git,
            findings,
        }
    }

    pub fn renderer(&self) -> PageRenderer {
        PageRenderer::new(self.graph.clone())
            .with_git(self.git.clone())
            .with_findings(self.findings.clone())
    }

    /// Builds every snapshot the renderer reads.
    pub async fn refresh(&self) {
        use harness_findings::AnalyzeRequest;
        use harness_git_analysis::RepositoryCorrelation;
        use tokio_util::sync::CancellationToken;
        let cancel = CancellationToken::new();
        let snapshot = self.graph.refresh(cancel.clone()).await.unwrap();
        self.git
            .refresh(
                RepositoryCorrelation::from_snapshot(&snapshot),
                cancel.clone(),
            )
            .await
            .unwrap();
        self.findings
            .analyze(
                AnalyzeRequest {
                    use_git: true,
                    ..AnalyzeRequest::default()
                },
                cancel,
            )
            .await
            .unwrap();
    }
}
