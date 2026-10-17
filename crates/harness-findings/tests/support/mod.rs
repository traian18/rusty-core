#![allow(dead_code)]
use std::{path::Path, sync::Arc};

use git2::{IndexAddOption, Repository, Signature, Time};
use harness_findings::{
    AnalyzeRequest, BoundaryPolicy, BoundaryRule, DefaultFindingsAnalyzer, FindingSet,
    FindingsAnalysis, FindingsConfig, FindingsService, FsSourceTextReader, Selector,
};
use harness_git_analysis::{GitAnalysis, GitAnalysisBuilder, RepositoryCorrelation};
use harness_repository_imports::{
    RustImportAdapter, RustImportResolver, TypeScriptImportAdapter, TypeScriptImportResolver,
};
use harness_workspace::{
    dependency_groups::DefaultGroupAnalyzer,
    import_graph::ImportSupport,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder, RootRelativePath},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

pub const DAY: i64 = 86_400;

pub fn write(root: &Path, relative: &str, text: &str) {
    let full = root.join(relative);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

pub fn path(value: &str) -> RootRelativePath {
    RootRelativePath::new(value).unwrap()
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

/// A monorepo with a deliberate mix of true findings and look-alikes that must not be reported.
pub fn write_monorepo(root: &Path) {
    // Rust: `engine` has a runtime cycle, an undeclared crate, an unused dependency, an orphan file.
    write(root, "engine/Cargo.toml", "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1\"\nunused_crate = \"1\"\n[dev-dependencies]\ntempfile = \"3\"\n[build-dependencies]\ncc = \"1\"\n");
    write(
        root,
        "engine/src/lib.rs",
        "pub mod a;\npub mod b;\npub mod c;\n",
    );
    write(root, "engine/src/a.rs", "use crate::b::B;\nuse serde::Serialize;\nuse missing_crate::X;\nuse Local::Variant;\npub struct A;\n");
    write(root, "engine/src/b.rs", "use crate::a::A;\npub struct B;\n");
    write(root, "engine/src/c.rs", "use crate::a::A;\n");
    write(root, "engine/src/orphan.rs", "pub fn forgotten() {}\n");
    write(root, "engine/src/bin/tool.rs", "fn main() {}\n");
    write(root, "engine/tests/it.rs", "use engine::a::A;\n");
    // Rust: `app` depends on `engine` (declared); its `ui` module breaks the configured boundary.
    write(root, "app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nengine = { path = \"../engine\" }\n");
    write(
        root,
        "app/src/main.rs",
        "mod ui;\nuse engine::a::A;\nfn main() {}\n",
    );
    write(root, "app/src/ui.rs", "use engine::b::B;\n");
    // TypeScript: `web` has cycles, an undeclared package, an undeclared sibling, unused dependencies.
    write(
        root,
        "web/package.json",
        r#"{"name":"web","types":"src/index.ts","dependencies":{"react":"1","lodash":"1"},"devDependencies":{"vitest":"1","@types/node":"1"}}"#,
    );
    write(root, "web/src/index.ts", "import React from 'react';\nimport axios from 'axios';\nimport { v } from '@acme/lib';\nexport { x } from './x';\nexport { p } from './p';\n");
    write(
        root,
        "web/src/x.ts",
        "import type { Y } from './y';\nexport const x = 1;\n",
    );
    write(
        root,
        "web/src/y.ts",
        "import { x } from './x';\nexport type Y = typeof x;\n",
    );
    write(
        root,
        "web/src/p.ts",
        "import { q } from './q';\nexport const p = 1;\n",
    );
    write(
        root,
        "web/src/q.ts",
        "import { p } from './p';\nexport const q = 1;\n",
    );
    write(root, "web/src/util.ts", "export const util = 1;\n");
    write(root, "web/src/legacy.ts", "export const legacy = 1;\n");
    write(root, "web/src/util.test.ts", "import { x } from './x';\n");
    write(root, "web/scripts/build.ts", "export {};\n");
    // TypeScript: `lib` is a sibling; `ok` declares it; `alpha` and `beta` form a cross-package cycle.
    write(
        root,
        "lib/package.json",
        r#"{"name":"@acme/lib","types":"src/index.ts"}"#,
    );
    write(root, "lib/src/index.ts", "export const v = 1;\n");
    write(
        root,
        "ok/package.json",
        r#"{"name":"ok","types":"src/index.ts","dependencies":{"@acme/lib":"*"}}"#,
    );
    write(
        root,
        "ok/src/index.ts",
        "import { v } from '@acme/lib';\nexport const w = v;\n",
    );
    write(
        root,
        "alpha/package.json",
        r#"{"name":"@acme/alpha","types":"src/index.ts","dependencies":{"@acme/beta":"*"}}"#,
    );
    write(
        root,
        "alpha/src/index.ts",
        "import { b } from '@acme/beta';\nexport const a = 1;\n",
    );
    write(
        root,
        "beta/package.json",
        r#"{"name":"@acme/beta","types":"src/index.ts","dependencies":{"@acme/alpha":"*"}}"#,
    );
    write(
        root,
        "beta/src/index.ts",
        "import { a } from '@acme/alpha';\nexport const b = 1;\n",
    );
}

/// One old commit (outside the default window) plus ten that change a.rs and util.ts together.
pub fn write_history(root: &Path) {
    let repo = Repository::init(root).unwrap();
    let start = 1_700_000_000;
    commit(&repo, start);
    for round in 0..10 {
        for file in ["engine/src/a.rs", "web/src/util.ts"] {
            let existing = std::fs::read_to_string(root.join(file)).unwrap();
            write(root, file, &format!("{existing}// change {round}\n"));
        }
        commit(&repo, start + 300 * DAY + round * DAY);
    }
}

pub fn boundary_policy() -> BoundaryPolicy {
    BoundaryPolicy {
        rules: vec![BoundaryRule {
            id: "ui-isolation".into(),
            from: Selector::PathPrefix(path("app/src/ui.rs")),
            to: Selector::Package("engine".into()),
            severity: harness_findings::Severity::High,
            reason: "The UI layer must go through the application layer, not the engine.".into(),
        }],
    }
}

pub fn test_config() -> FindingsConfig {
    FindingsConfig {
        policy: boundary_policy(),
        hotspot_min_dependents: 3,
        ..FindingsConfig::default()
    }
}

pub struct Monorepo {
    pub dir: TempDir,
    pub graph: Arc<dyn RepositoryGraph>,
    pub git: Arc<dyn GitAnalysis>,
}

pub fn graph_for(
    root: &Path,
    with_groups: bool,
    limits: Option<harness_workspace::import_graph::ImportLimits>,
) -> Arc<dyn RepositoryGraph> {
    graph_with_frameworks(root, with_groups, limits, Vec::new())
}

pub fn graph_with_frameworks(
    root: &Path,
    with_groups: bool,
    limits: Option<harness_workspace::import_graph::ImportLimits>,
    adapters: Vec<Arc<dyn harness_workspace::framework_relations::FrameworkAdapter>>,
) -> Arc<dyn RepositoryGraph> {
    let mut builder = RepositoryGraphBuilder::new(std::fs::canonicalize(root).unwrap())
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
        .unwrap();
    if let Some(limits) = limits {
        builder = builder.import_limits(limits);
    }
    if with_groups {
        builder = builder.dependency_groups(Arc::new(DefaultGroupAnalyzer::standard()));
    }
    for adapter in adapters {
        builder = builder.framework_adapter(adapter);
    }
    Arc::new(builder.build())
}

pub async fn monorepo() -> Monorepo {
    let dir = tempfile::tempdir().unwrap();
    write_monorepo(dir.path());
    write_history(dir.path());
    let graph = graph_for(dir.path(), true, None);
    let git: Arc<dyn GitAnalysis> = Arc::new(
        GitAnalysisBuilder::for_repository(dir.path())
            .build()
            .unwrap(),
    );
    graph.refresh(CancellationToken::new()).await.unwrap();
    let correlation = RepositoryCorrelation::from_snapshot(&graph.current().unwrap());
    git.refresh(correlation, CancellationToken::new())
        .await
        .unwrap();
    Monorepo { dir, graph, git }
}

pub fn service(monorepo: &Monorepo, config: FindingsConfig, with_git: bool) -> FindingsService {
    FindingsService::new(
        monorepo.graph.clone(),
        with_git.then(|| monorepo.git.clone()),
        Arc::new(DefaultFindingsAnalyzer::standard(config).unwrap()),
        Arc::new(FsSourceTextReader::new(monorepo.dir.path())),
    )
}

pub async fn analyze(service: &FindingsService) -> Arc<FindingSet> {
    service
        .analyze(
            AnalyzeRequest {
                use_git: true,
                ..AnalyzeRequest::default()
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
}

pub fn ids(set: &FindingSet) -> Vec<String> {
    set.findings.iter().map(|f| f.id.0.clone()).collect()
}
