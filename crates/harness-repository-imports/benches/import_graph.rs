//! Reproducible import-graph timings: cold refresh, unchanged refresh, one-file edit, and bounded queries.
//!
//! Run with `cargo bench -p harness-repository-imports --bench import_graph`. The numbers are raw
//! evidence for the machine they were taken on, not a service-level promise.
use std::{fs, path::Path, sync::Arc, time::Instant};

use harness_repository_imports::{
    RustImportAdapter, RustImportResolver, TypeScriptImportAdapter, TypeScriptImportResolver,
};
use harness_workspace::{
    dependency_groups::{
        BoundaryDirection, BoundaryQuery, DefaultGroupAnalyzer, GraphDirection, GraphQuery,
        GroupId, GroupKind, OverviewQuery,
    },
    import_graph::{ImportFilter, ImportQuery, ImportSupport},
    repository_intelligence::{
        RepositoryGraph, RepositoryGraphBuilder, RepositoryGraphService, RootRelativePath,
        SnapshotId,
    },
};
use tokio_util::sync::CancellationToken;

const CRATES: usize = 40;
const MODULES_PER_CRATE: usize = 25;
const TS_PACKAGES: usize = 20;
const TS_FILES_PER_PACKAGE: usize = 25;
const QUERY_REPETITIONS: u32 = 1_000;

fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("create benchmark runtime")
        .block_on(run());
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("parent")).expect("create directory");
    fs::write(full, text).expect("write fixture");
}

/// Rust crates chained by path dependencies plus npm packages chained by imports.
fn generate(root: &Path) -> (usize, usize) {
    let (mut files, mut bytes) = (0, 0);
    let mut put = |path: String, text: String| {
        bytes += text.len();
        files += 1;
        write(root, &path, &text);
    };
    put(
        "Cargo.toml".into(),
        format!(
            "[workspace]\nmembers = [{}]\n",
            (0..CRATES)
                .map(|c| format!("\"crates/c{c}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
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
        let mods = (0..MODULES_PER_CRATE)
            .map(|m| format!("pub mod m{m};\n"))
            .collect::<String>();
        let reexport = if c > 0 {
            format!("pub use c{}::m0::Item0;\n", c - 1)
        } else {
            String::new()
        };
        put(
            format!("crates/c{c}/src/lib.rs"),
            format!("{mods}{reexport}"),
        );
        for m in 0..MODULES_PER_CRATE {
            let prev = m.saturating_sub(1);
            put(
                format!("crates/c{c}/src/m{m}.rs"),
                format!("use crate::m{prev}::Item{prev};\nuse std::fmt;\npub struct Item{m};\n"),
            );
        }
    }
    put(
        "package.json".into(),
        r#"{"name":"root","workspaces":["packages/*"]}"#.into(),
    );
    for p in 0..TS_PACKAGES {
        put(
            format!("packages/p{p}/package.json"),
            format!(r#"{{"name":"@bench/p{p}","types":"src/index.ts"}}"#),
        );
        let exports = (0..TS_FILES_PER_PACKAGE)
            .map(|f| format!("export * from './f{f}';\n"))
            .collect::<String>();
        put(format!("packages/p{p}/src/index.ts"), exports);
        for f in 0..TS_FILES_PER_PACKAGE {
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
    (files, bytes)
}

fn service(root: &Path) -> RepositoryGraphService {
    builder(root).build()
}

fn builder(root: &Path) -> RepositoryGraphBuilder {
    RepositoryGraphBuilder::new(fs::canonicalize(root).expect("canonical root"))
        .expect("builder")
        .import_support(ImportSupport {
            adapter: Arc::new(RustImportAdapter),
            resolver: Arc::new(RustImportResolver),
        })
        .expect("rust support")
        .import_support(ImportSupport {
            adapter: Arc::new(TypeScriptImportAdapter),
            resolver: Arc::new(TypeScriptImportResolver),
        })
        .expect("typescript support")
}

fn query(snapshot: &SnapshotId, file: &str) -> ImportQuery {
    ImportQuery {
        snapshot: snapshot.clone(),
        file: RootRelativePath::new(file).expect("path"),
        item: None,
        filter: ImportFilter::default(),
        cursor: None,
        limit: 50,
    }
}

/// Peak resident set size in KiB where the platform reports it.
fn peak_rss_kib() -> Option<u64> {
    #[cfg(unix)]
    {
        // SAFETY: `getrusage` fills a zeroed `rusage` we own.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
            return None;
        }
        let raw = usage.ru_maxrss as u64;
        Some(if cfg!(target_os = "macos") {
            raw / 1024
        } else {
            raw
        })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

async fn run() {
    let root = std::env::temp_dir().join(format!("import-graph-bench-{}", std::process::id()));
    fs::create_dir_all(&root).expect("create workspace");
    let (files, bytes) = generate(&root);
    let graph = service(&root);

    let timed = Instant::now();
    let cold = graph
        .refresh(CancellationToken::new())
        .await
        .expect("cold refresh");
    let cold_time = timed.elapsed();
    let metrics = graph
        .last_refresh_metrics()
        .and_then(|m| m.import_details)
        .expect("metrics");

    let timed = Instant::now();
    graph
        .refresh(CancellationToken::new())
        .await
        .expect("unchanged refresh");
    let unchanged_time = timed.elapsed();

    write(
        &root,
        "crates/c3/src/m5.rs",
        "use crate::m4::Item4;\npub struct Item5;\npub struct Extra;\n",
    );
    let timed = Instant::now();
    graph
        .refresh(CancellationToken::new())
        .await
        .expect("edit refresh");
    let edit_time = timed.elapsed();
    let edit_metrics = graph
        .last_refresh_metrics()
        .and_then(|m| m.import_details)
        .expect("metrics");

    let current = graph.current().expect("snapshot").id.clone();
    let dependents = query(&current, "crates/c3/src/m0.rs");
    let dependencies = query(&current, "crates/c3/src/m10.rs");
    let timed = Instant::now();
    for _ in 0..QUERY_REPETITIONS {
        graph.dependents(&dependents).expect("dependents");
        graph.dependencies(&dependencies).expect("dependencies");
    }
    let query_time = timed.elapsed() / (2 * QUERY_REPETITIONS);

    println!(
        "environment: {} {}, rustc {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        option_env!("RUSTC_VERSION").unwrap_or("see `rustc -V`")
    );
    println!("fixture: {files} files, {bytes} bytes ({CRATES} crates x {MODULES_PER_CRATE} modules, {TS_PACKAGES} packages x {TS_FILES_PER_PACKAGE} files)");
    println!("limits: {:?}", cold.imports.limits);
    println!(
        "indexed: {} files, {} imports ({} internal, {} external, {} unresolved)",
        metrics.files,
        metrics.facts,
        metrics.resolved_internal,
        metrics.declared_external,
        metrics.unresolved
    );
    println!("cold refresh:       {cold_time:?} (extraction {:?}, resolution {:?}, cache {} hits / {} misses)", metrics.durations.extraction, metrics.durations.resolution, metrics.cache.hits, metrics.cache.misses);
    println!("unchanged refresh:  {unchanged_time:?}");
    println!(
        "one-file edit:      {edit_time:?} (cache {} hits / {} misses)",
        edit_metrics.cache.hits, edit_metrics.cache.misses
    );
    println!(
        "bounded query:      {query_time:?} per call (limit 50, mean of {} calls)",
        2 * QUERY_REPETITIONS
    );
    group_benchmarks(&root).await;
    match peak_rss_kib() {
        Some(kib) => println!("peak RSS:           {kib} KiB (whole process)"),
        None => println!("peak RSS:           unavailable on this platform"),
    }
    let _ = fs::remove_dir_all(&root);
}

/// Group analysis and the group-level queries on the same fixture.
async fn group_benchmarks(root: &Path) {
    let graph = builder(root)
        .dependency_groups(Arc::new(DefaultGroupAnalyzer::standard()))
        .build();
    let timed = Instant::now();
    graph
        .refresh(CancellationToken::new())
        .await
        .expect("grouped refresh");
    let cold = timed.elapsed();
    let metrics = graph.last_refresh_metrics().expect("metrics");
    let snapshot = graph.current().expect("snapshot");
    let id = snapshot.id.clone();
    let package = snapshot
        .groups
        .groups
        .iter()
        .find(|g| g.kind == GroupKind::DeclaredPackage && g.label == "c20")
        .map(|g| g.id.clone())
        .unwrap_or_else(|| GroupId(String::new()));

    let overview = OverviewQuery {
        snapshot: id.clone(),
        kind: GroupKind::DeclaredPackage,
        node_limit: 500,
        edge_limit: 5_000,
        filter: ImportFilter::default(),
    };
    let projection = GraphQuery {
        snapshot: id.clone(),
        roots: vec![RootRelativePath::new("crates/c20/src/m10.rs").expect("path")],
        direction: GraphDirection::Both,
        depth: 4,
        node_limit: 500,
        edge_limit: 5_000,
        filter: ImportFilter::default(),
    };
    let boundary = BoundaryQuery {
        snapshot: id,
        group: package,
        direction: BoundaryDirection::Outgoing,
        filter: ImportFilter::default(),
        cursor: None,
        limit: 50,
    };
    let repetitions = 200u32;
    let timed = Instant::now();
    for _ in 0..repetitions {
        graph.group_overview(&overview).expect("overview");
    }
    let overview_time = timed.elapsed() / repetitions;
    let timed = Instant::now();
    let mut nodes = 0;
    for _ in 0..repetitions {
        nodes = graph.graph(&projection).expect("projection").nodes.len();
    }
    let projection_time = timed.elapsed() / repetitions;
    let timed = Instant::now();
    for _ in 0..repetitions {
        graph.boundary_edges(&boundary).expect("boundary");
    }
    let boundary_time = timed.elapsed() / repetitions;
    println!(
        "groups:             {} groups ({} packages, {} source roots, {} cycles)",
        snapshot.groups.groups.len(),
        count(&snapshot, GroupKind::DeclaredPackage),
        count(&snapshot, GroupKind::SourceRoot),
        count(&snapshot, GroupKind::ImportCycle)
    );
    println!(
        "cold refresh+groups:{cold:?} (group analysis {:?})",
        metrics.groups
    );
    println!("package overview:   {overview_time:?} per call (limits 500 nodes / 5000 edges, mean of {repetitions} calls)");
    println!("graph projection:   {projection_time:?} per call (depth 4, both directions, {nodes} nodes)");
    println!("boundary edges:     {boundary_time:?} per call (limit 50)");
}

fn count(
    snapshot: &harness_workspace::repository_intelligence::RepositorySnapshot,
    kind: GroupKind,
) -> usize {
    snapshot
        .groups
        .groups
        .iter()
        .filter(|g| g.kind == kind)
        .count()
}
