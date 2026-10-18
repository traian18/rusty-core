//! Measures retrieval on recorded runs. Run by hand:
//! `REPLAY_ROOT=<workspace> cargo test -p harness-repository-wiki --test replay_trajectories -- --ignored --nocapture`
//!
//! For every recorded workflow run in `<root>/.rusty/observability/trajectories`, the request is the
//! prompt and the files the run read or edited are the answer key. The trajectories stay on the machine and
//! are never committed. The snapshot is the current one, so files that no longer exist are ignored.
mod support;

use std::{collections::BTreeSet, path::Path, sync::Arc};

use harness_repository_imports::{
    RustImportAdapter, RustImportResolver, TypeScriptImportAdapter, TypeScriptImportResolver,
};
use harness_repository_wiki::{
    replay::{replay_all, ReplayCase},
    TaskKind,
};
use harness_workspace::{
    dependency_groups::DefaultGroupAnalyzer,
    import_graph::ImportSupport,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

fn cases(directory: &Path, indexed: &BTreeSet<String>, root: &str) -> Vec<ReplayCase> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl")
            || path.file_name().and_then(|n| n.to_str()) == Some("index.jsonl")
        {
            continue;
        }
        let mut prompt = None;
        let mut task = TaskKind::General;
        let mut used = BTreeSet::new();
        for line in std::fs::read_to_string(&path).unwrap_or_default().lines() {
            let Ok(row) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match row["source"].as_str() {
                Some("Workflow") if prompt.is_none() => {
                    prompt = row["payload"]["input"]["request"]
                        .as_str()
                        .map(str::to_owned);
                    task = row["payload"]["workflow"]
                        .as_str()
                        .map(TaskKind::from_profile)
                        .unwrap_or_default();
                }
                Some("ToolCallRequested") => {
                    let call = &row["payload"]["ToolCallRequested"]["call"];
                    if matches!(
                        call["name"].as_str(),
                        Some("read_file" | "edit_file" | "write_file")
                    ) {
                        if let Some(file) = call["arguments"]["path"].as_str() {
                            let file = file
                                .trim_start_matches("./")
                                .trim_start_matches(root)
                                .trim_start_matches('/');
                            if indexed.contains(file) {
                                used.insert(file.to_owned());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(prompt) = prompt.filter(|_| !used.is_empty()) {
            out.push(ReplayCase {
                prompt,
                used: used.into_iter().collect(),
                task,
                ..Default::default()
            });
        }
    }
    out
}

#[tokio::test]
#[ignore = "measures recorded runs on this machine; run by hand"]
async fn compare_retrieval_configurations_on_recorded_workflow_runs() {
    let root = std::env::var("REPLAY_ROOT")
        .expect("set REPLAY_ROOT to the workspace the trajectories were recorded in");
    let directory = std::env::var("REPLAY_DIR")
        .unwrap_or_else(|_| format!("{root}/.rusty/observability/trajectories"));
    let graph: Arc<dyn RepositoryGraph> = Arc::new(
        RepositoryGraphBuilder::new(std::fs::canonicalize(&root).unwrap())
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
    graph.refresh(CancellationToken::new()).await.unwrap();
    let snapshot = graph.current().unwrap();
    let indexed: BTreeSet<String> = snapshot
        .imports
        .files
        .iter()
        .map(|f| f.path.as_path().display().to_string())
        .collect();
    let cases = cases(Path::new(&directory), &indexed, &root);
    println!(
        "{} indexed files, {} recorded runs with a prompt and used files",
        indexed.len(),
        cases.len()
    );
    if std::env::var("REPLAY_VERBOSE").is_ok() {
        use harness_repository_wiki::{Centrality, ContextPlanner, PlanInput, Tier};
        let planner = ContextPlanner::new(&snapshot, Arc::new(Centrality::new(&snapshot)));
        for case in &cases {
            let plan = planner.plan(&PlanInput {
                message: &case.prompt,
                focus: &case.focus,
                task: case.task,
            });
            let high: Vec<_> = plan
                .items
                .iter()
                .filter(|i| i.tier == Tier::High)
                .map(|i| format!("{} ({})", i.page, i.reason))
                .collect();
            if !high.is_empty() {
                println!(
                    "PROMPT: {}\n  excerpts: {high:?}\n  used: {:?}",
                    case.prompt
                        .chars()
                        .take(160)
                        .collect::<String>()
                        .replace('\n', " "),
                    case.used.iter().take(8).collect::<Vec<_>>()
                );
            }
        }
    }
    println!(
        "{:<18} {:>9} {:>9} {:>9} {:>10} {:>10} {:>10} {:>9} {:>9}",
        "configuration",
        "abstained",
        "injected",
        "hit rate",
        "precision",
        "recall(ex)",
        "recall(any)",
        "excerpts",
        "harmful"
    );
    for (config, m) in replay_all(&snapshot, &cases) {
        println!(
            "{:<18} {:>9} {:>9} {:>9.3} {:>10.3} {:>10.3} {:>10.3} {:>9} {:>9}",
            format!("{config:?}"),
            m.abstained,
            m.injected_runs,
            m.hit_rate(),
            m.precision(),
            m.recall_excerpt(),
            m.recall_any(),
            m.excerpts,
            m.harmful
        );
        assert!(m.precision().is_finite());
    }
    let _ = support::write; // the shared fixture module is only used by the other test files
}
