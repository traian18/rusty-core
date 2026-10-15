use std::fs;
use std::time::Instant;

use harness_code_analysis::{CallGraph, DefaultCallGraph, FindCallablesRequest, RefreshRequest};
use tokio_util::sync::CancellationToken;

fn main() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create benchmark runtime")
        .block_on(run());
}

/// Peak resident set size of this process in KiB, where the platform reports it.
fn peak_rss_kib() -> Option<u64> {
    #[cfg(unix)]
    {
        // SAFETY: `getrusage` fills a zeroed `rusage` we own.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
            return None;
        }
        let raw = usage.ru_maxrss as u64;
        // macOS reports bytes, Linux reports KiB.
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

/// Writes `files_per_language` modules for each of five languages. Every module defines a helper
/// and a function calling its own helper and the previous module's function, so the workspace has
/// same-file and cross-file edges.
fn generate(root: &std::path::Path, files_per_language: usize) -> (usize, usize) {
    let mut count = 0;
    let mut bytes = 0;
    let mut write = |path: String, text: String| {
        bytes += text.len();
        count += 1;
        fs::write(root.join(path), text).expect("write benchmark source");
    };
    for i in 0..files_per_language {
        let prev = i.saturating_sub(1);
        write(
            format!("mod_{i}.py"),
            format!("from mod_{prev} import run_{prev}\n\ndef helper_{i}():\n    pass\n\ndef run_{i}():\n    helper_{i}()\n    run_{prev}()\n"),
        );
        write(
            format!("mod_{i}.js"),
            format!("import {{ run_{prev} }} from './mod_{prev}.js';\nfunction helper_{i}() {{}}\nexport function run_{i}() {{ helper_{i}(); run_{prev}(); }}\n"),
        );
        write(
            format!("Mod{i}.java"),
            format!("package bench;\npublic class Mod{i} {{\n  void helper() {{}}\n  void run() {{ helper(); new Mod{prev}().run(); }}\n}}\n"),
        );
        write(
            format!("mod_{i}.go"),
            format!("package bench\n\nfunc helper{i}() {{}}\n\nfunc run{i}() {{ helper{i}(); run{prev}() }}\n"),
        );
        write(
            format!("mod_{i}.rs"),
            format!("fn helper_{i}() {{}}\npub fn run_{i}() {{ helper_{i}(); }}\n"),
        );
    }
    (count, bytes)
}

async fn run() {
    let files_per_language = 200;
    let root = std::env::temp_dir().join(format!("call-graph-bench-{}", std::process::id()));
    fs::create_dir_all(&root).expect("create benchmark workspace");
    let (files, bytes) = generate(&root, files_per_language);
    let rss_before = peak_rss_kib();
    let graph = DefaultCallGraph::new(&root);
    let request = RefreshRequest {
        max_files: Some(10_000),
    };
    let start = Instant::now();
    let cold = graph
        .refresh(request.clone(), CancellationToken::new())
        .await
        .expect("cold refresh");
    let cold_time = start.elapsed();
    let start = Instant::now();
    let cached = graph
        .refresh(request.clone(), CancellationToken::new())
        .await
        .expect("cached refresh");
    let cached_time = start.elapsed();
    fs::write(root.join("mod_0.py"), "def helper_0():\n    pass\n# edit\n").expect("edit");
    let start = Instant::now();
    let edited = graph
        .refresh(request, CancellationToken::new())
        .await
        .expect("incremental refresh");
    let edit_time = start.elapsed();
    let start = Instant::now();
    let query = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(edited.snapshot.clone()),
                language: None,
                cursor: None,
                limit: 50,
            },
            CancellationToken::new(),
        )
        .await
        .expect("bounded query");
    let query_time = start.elapsed();
    let rss_after = peak_rss_kib();
    println!(
        "workspace=synthetic composition=python,javascript,java,go,rust files={files} source_bytes={bytes} \
         callables={} calls={} resolved={} unresolved={} \
         cold_refresh={cold_time:?} cold_cache_misses={} cached_refresh={cached_time:?} cached_hits={} \
         one_file_edit_refresh={edit_time:?} edit_misses={} query_50={query_time:?} query_items={} \
         peak_rss_kib_before={rss_before:?} peak_rss_kib_after={rss_after:?}",
        cold.stats.callables,
        cold.stats.calls,
        cold.stats.resolved,
        cold.stats.unresolved,
        cold.stats.cache_misses,
        cached.stats.cache_hits,
        edited.stats.cache_misses,
        query.page.items.len(),
    );
    let _ = fs::remove_dir_all(root);
}
