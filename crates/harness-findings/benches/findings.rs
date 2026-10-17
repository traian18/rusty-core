//! Reproducible findings timings on a generated monorepo with Git history.
//!
//! Run with `cargo bench -p harness-findings --bench findings`. Raw evidence for the machine it
//! ran on, not a service-level promise.
use std::{fs, path::Path, sync::Arc, time::Instant};

use git2::{IndexAddOption, Repository, Signature, Time};
use harness_findings::{
    AnalyzeRequest, DefaultFindingsAnalyzer, FindingsAnalysis, FindingsConfig, FindingsListQuery,
    FindingsService, FsSourceTextReader,
};
use harness_git_analysis::{GitAnalysis, GitAnalysisBuilder, RepositoryCorrelation};
use harness_repository_imports::{
    RustImportAdapter, RustImportResolver, TypeScriptImportAdapter, TypeScriptImportResolver,
};
use harness_workspace::{
    dependency_groups::DefaultGroupAnalyzer,
    import_graph::ImportSupport,
    repository_intelligence::{RepositoryGraph, RepositoryGraphBuilder},
};
use tokio_util::sync::CancellationToken;

const CRATES: usize = 40;
const MODULES: usize = 25;
const PACKAGES: usize = 20;
const FILES_PER_PACKAGE: usize = 25;
const COMMITS: usize = 300;
const QUERIES: u32 = 2_000;

fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(run());
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("parent")).expect("directory");
    fs::write(full, text).expect("write");
}

/// Chained crates and packages (like the import benchmark) plus a few cycles and undeclared imports.
fn generate(root: &Path) -> usize {
    let mut files = 0;
    let mut put = |path: String, text: String| {
        files += 1;
        write(root, &path, &text);
    };
    for c in 0..CRATES {
        let dependency = if c > 0 {
            format!("c{}= {{ path = \"../c{}\" }}\n", c - 1, c - 1)
        } else {
            String::new()
        };
        put(
            format!("crates/c{c}/Cargo.toml"),
            format!(
                "[package]\nname = \"c{c}\"\nversion = \"0.1.0\"\n[dependencies]\n{dependency}"
            ),
        );
        let mods = (0..MODULES)
            .map(|m| format!("pub mod m{m};\n"))
            .collect::<String>();
        put(format!("crates/c{c}/src/lib.rs"), mods);
        for m in 0..MODULES {
            let prev = m.saturating_sub(1);
            // Every fifth module closes a two-file cycle with its neighbour.
            let back = if m % 5 == 1 {
                format!("use crate::m{}::Item{};\n", m + 1, m + 1)
            } else {
                String::new()
            };
            let forward = if m % 5 == 2 {
                format!("use crate::m{}::Item{};\n", m - 1, m - 1)
            } else {
                String::new()
            };
            put(format!("crates/c{c}/src/m{m}.rs"), format!("use crate::m{prev}::Item{prev};\n{back}{forward}use undeclared_{c}::Thing;\npub struct Item{m};\n"));
        }
    }
    for p in 0..PACKAGES {
        put(
            format!("packages/p{p}/package.json"),
            format!(r#"{{"name":"@bench/p{p}","types":"src/index.ts"}}"#),
        );
        put(
            format!("packages/p{p}/src/index.ts"),
            (0..FILES_PER_PACKAGE)
                .map(|f| format!("export * from './f{f}';\n"))
                .collect(),
        );
        for f in 0..FILES_PER_PACKAGE {
            let prev = f.saturating_sub(1);
            let cross = if p > 0 {
                format!("import {{ v0 }} from '@bench/p{}';\n", p - 1)
            } else {
                String::new()
            };
            put(
                format!("packages/p{p}/src/f{f}.ts"),
                format!(
                    "import {{ v{prev} }} from './f{prev}';\n{cross}export const v{f} = {f};\n"
                ),
            );
        }
    }
    files
}

fn commit(repo: &Repository, number: usize) {
    for offset in 0..3 {
        let file = (number * 7 + offset) % MODULES;
        let path = repo
            .workdir()
            .expect("workdir")
            .join(format!("crates/c{}/src/m{file}.rs", number % CRATES));
        let existing = fs::read_to_string(&path).expect("read");
        fs::write(&path, format!("{existing}// change {number}\n")).expect("write");
    }
    let mut index = repo.index().expect("index");
    index
        .add_all(["."], IndexAddOption::DEFAULT, None)
        .expect("stage");
    index.write().expect("write index");
    let tree = repo
        .find_tree(index.write_tree().expect("tree"))
        .expect("find tree");
    let who = Signature::new(
        "Bench",
        "bench@example.com",
        &Time::new(1_700_000_000 + number as i64 * 600, 0),
    )
    .expect("signature");
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
    .expect("commit");
}

async fn run() {
    let root = std::env::temp_dir().join(format!("findings-bench-{}", std::process::id()));
    fs::create_dir_all(&root).expect("workspace");
    let files = generate(&root);
    let repo = Repository::init(&root).expect("init");
    for number in 0..COMMITS {
        commit(&repo, number);
    }
    let graph: Arc<dyn RepositoryGraph> = Arc::new(
        RepositoryGraphBuilder::new(fs::canonicalize(&root).expect("root"))
            .expect("builder")
            .import_support(ImportSupport {
                adapter: Arc::new(RustImportAdapter),
                resolver: Arc::new(RustImportResolver),
            })
            .expect("rust")
            .import_support(ImportSupport {
                adapter: Arc::new(TypeScriptImportAdapter),
                resolver: Arc::new(TypeScriptImportResolver),
            })
            .expect("typescript")
            .dependency_groups(Arc::new(DefaultGroupAnalyzer::standard()))
            .build(),
    );
    let git: Arc<dyn GitAnalysis> = Arc::new(
        GitAnalysisBuilder::for_repository(&root)
            .build()
            .expect("git"),
    );
    graph
        .refresh(CancellationToken::new())
        .await
        .expect("refresh");
    let correlation = RepositoryCorrelation::from_snapshot(&graph.current().expect("snapshot"));
    git.refresh(correlation, CancellationToken::new())
        .await
        .expect("git refresh");

    let service = FindingsService::new(
        graph.clone(),
        Some(git),
        Arc::new(DefaultFindingsAnalyzer::standard(FindingsConfig::default()).expect("config")),
        Arc::new(FsSourceTextReader::new(&root)),
    );
    let request = || AnalyzeRequest {
        use_git: true,
        ..AnalyzeRequest::default()
    };
    let timed = Instant::now();
    let set = service
        .analyze(request(), CancellationToken::new())
        .await
        .expect("analysis");
    let cold = timed.elapsed();
    let timed = Instant::now();
    service
        .analyze(request(), CancellationToken::new())
        .await
        .expect("repeat");
    let repeat = timed.elapsed();

    let query = FindingsListQuery {
        set: set.id.clone(),
        rules: vec![],
        kind: None,
        min_severity: None,
        min_confidence: None,
        package: None,
        file: None,
        include_suppressed: false,
        cursor: None,
        limit: 50,
    };
    let timed = Instant::now();
    for _ in 0..QUERIES {
        service.list(&query).expect("list");
    }
    let list_time = timed.elapsed() / QUERIES;

    println!(
        "environment: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("fixture: {files} files ({CRATES} crates x {MODULES} modules, {PACKAGES} packages x {FILES_PER_PACKAGE} files), {COMMITS} commits");
    println!(
        "limits: max {} findings per rule, {} evidence items",
        FindingsConfig::default().max_findings_per_rule,
        FindingsConfig::default().max_evidence_per_finding
    );
    println!(
        "findings: {} ranked, {} suppressed; by rule {:?}",
        set.summary.total, set.summary.suppressed, set.summary.by_rule
    );
    println!(
        "truncated rules: {:?}; skipped: {}",
        set.summary.truncated_rules,
        set.summary.skipped_rules.len()
    );
    println!("analysis (cold):   {cold:?}");
    println!("analysis (repeat): {repeat:?}");
    println!("list query:        {list_time:?} per call (limit 50, mean of {QUERIES} calls)");
    let _ = fs::remove_dir_all(&root);
}
