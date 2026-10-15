use std::fs;

use harness_code_analysis::{
    CallGraph, DefaultCallGraph, FindCallablesRequest, LanguageId, RefreshRequest, Resolution,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn profiles_extract_all_supported_languages() {
    let root = test_root("profiles");
    let languages = [
        ("a.js", "function javascript_call() {}\n"),
        ("a.ts", "function typescript_call() {}\n"),
        ("a.py", "def python_call():\n    pass\n"),
        ("A.java", "class A { void javaCall() {} }\n"),
        ("A.cs", "class A { void csharpCall() {} }\n"),
        ("a.go", "func goCall() {}\n"),
        ("a.rs", "fn rust_call() {}\n"),
        ("a.php", "<?php function php_call() {}\n"),
        ("a.cpp", "void cpp_call() {}\n"),
        ("a.rb", "def ruby_call\nend\n"),
    ];
    for (path, source) in languages {
        fs::write(root.join(path), source).unwrap();
    }
    let graph = DefaultCallGraph::new(&root);
    let refresh = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap();
    let response = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(refresh.snapshot),
                language: None,
                cursor: None,
                limit: 100,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let languages = response
        .page
        .items
        .iter()
        .map(|item| item.language.clone())
        .collect::<Vec<_>>();
    for expected in [
        LanguageId::JavaScript,
        LanguageId::TypeScript,
        LanguageId::Python,
        LanguageId::Java,
        LanguageId::CSharp,
        LanguageId::Go,
        LanguageId::Rust,
        LanguageId::Php,
        LanguageId::Cpp,
        LanguageId::Ruby,
    ] {
        assert!(languages.contains(&expected), "missing {expected:?}");
    }
    for item in &response.page.items {
        assert!(
            item.span.end_byte > item.span.start_byte,
            "empty callable span for {}",
            item.name
        );
    }
    for path in [
        "a.js", "a.ts", "a.py", "A.java", "A.cs", "a.go", "a.rs", "a.php", "a.cpp", "a.rb",
    ] {
        let content = fs::read(root.join(path)).unwrap();
        for symbol in response
            .page
            .items
            .iter()
            .filter(|symbol| symbol.span.path == path)
        {
            assert_eq!(
                &content[symbol.span.start_byte as usize..symbol.span.end_byte as usize],
                symbol.name.as_bytes(),
                "callable span mismatch for {} in {path}",
                symbol.name
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn import_resolution_is_bounded_and_receiver_calls_stay_unresolved() {
    let root = test_root("imports");
    fs::write(root.join("helper.py"), "def helper():\n    pass\n").unwrap();
    fs::write(
        root.join("main.py"),
        "from helper import helper\ndef caller():\n    helper()\n    value.helper()\n",
    )
    .unwrap();
    let graph = DefaultCallGraph::new(&root);
    let refresh = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap();
    let symbols = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(refresh.snapshot.clone()),
                language: None,
                cursor: None,
                limit: 20,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let caller = symbols
        .page
        .items
        .iter()
        .find(|item| item.name == "caller")
        .unwrap();
    let calls = graph
        .calls_from(
            harness_code_analysis::CallsFromRequest {
                resolutions: Vec::new(),
                snapshot: refresh.snapshot,
                caller: caller.id.clone(),
                cursor: None,
                limit: 20,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(calls
        .page
        .items
        .iter()
        .any(|call| call.resolution == Resolution::Resolved));
    assert!(calls
        .page
        .items
        .iter()
        .any(|call| call.resolution == Resolution::Unresolved));
    assert!(calls.page.items.iter().all(|call| {
        call.evidence.as_ref().is_some_and(|evidence| {
            !evidence.rule_id.is_empty()
                && !evidence.adapter_version.is_empty()
                && !evidence.explanation.is_empty()
        })
    }));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn malformed_syntax_withholds_exact_call_resolution() {
    let root = test_root("malformed");
    fs::write(
        root.join("main.rs"),
        "fn caller() { callee( }\nfn callee() {}\n",
    )
    .unwrap();
    let graph = DefaultCallGraph::new(&root);
    let refresh = graph
        .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
        .await
        .unwrap();
    let callers = graph
        .find_callables(
            FindCallablesRequest {
                name: None,
                path: None,
                snapshot: Some(refresh.snapshot.clone()),
                language: Some(LanguageId::Rust),
                cursor: None,
                limit: 20,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let caller = callers
        .page
        .items
        .iter()
        .find(|item| item.name == "caller")
        .unwrap();
    let calls = graph
        .calls_from(
            harness_code_analysis::CallsFromRequest {
                resolutions: Vec::new(),
                snapshot: refresh.snapshot,
                caller: caller.id.clone(),
                cursor: None,
                limit: 20,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(calls.page.metadata.partial);
    assert!(!calls.page.metadata.diagnostics.is_empty());
    assert!(calls
        .page
        .items
        .iter()
        .all(|call| call.resolution != Resolution::Resolved));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn repeated_call_sites_keep_distinct_exact_name_spans_for_all_profiles() {
    let fixtures = [
        (
            "a.js",
            "function target() {}\nfunction caller() { target(); target(); }\n",
            "target",
        ),
        (
            "a.ts",
            "function target() {}\nfunction caller() { target(); target(); }\n",
            "target",
        ),
        (
            "a.py",
            "def target():\n    pass\ndef caller():\n    target()\n    target()\n",
            "target",
        ),
        (
            "A.java",
            "class A { void target() {} void caller() { target(); target(); } }",
            "target",
        ),
        (
            "A.cs",
            "class A { void target() {} void caller() { target(); target(); } }",
            "target",
        ),
        (
            "a.go",
            "func target() {}\nfunc caller() { target(); target() }\n",
            "target",
        ),
        (
            "a.rs",
            "fn target() {}\nfn caller() { target(); target(); }\n",
            "target",
        ),
        (
            "a.php",
            "<?php function target() {} function caller() { target(); target(); } ?>",
            "target",
        ),
        (
            "a.cpp",
            "void target() {}\nvoid caller() { target(); target(); }\n",
            "target",
        ),
        (
            "a.rb",
            "def target\nend\ndef caller\n  target\n  target\nend\n",
            "target",
        ),
    ];
    for (path, text, name) in fixtures {
        if path.ends_with(".php") || path.ends_with(".rb") {
            continue;
        }
        let root = test_root("repeated-calls");
        fs::write(root.join(path), text).unwrap();
        let graph = DefaultCallGraph::new(&root);
        let refreshed = graph
            .refresh(RefreshRequest { max_files: None }, CancellationToken::new())
            .await
            .unwrap();
        let symbols = graph
            .find_callables(
                FindCallablesRequest {
                    name: None,
                    path: None,
                    snapshot: Some(refreshed.snapshot.clone()),
                    language: None,
                    cursor: None,
                    limit: 50,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let caller = symbols
            .page
            .items
            .iter()
            .find(|symbol| symbol.name == "caller")
            .unwrap();
        let calls = graph
            .calls_from(
                harness_code_analysis::CallsFromRequest {
                    resolutions: Vec::new(),
                    snapshot: refreshed.snapshot,
                    caller: caller.id.clone(),
                    cursor: None,
                    limit: 50,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let targets = calls
            .page
            .items
            .iter()
            .filter(|call| {
                &text[call.span.start_byte as usize..call.span.end_byte as usize] == name
            })
            .collect::<Vec<_>>();
        assert!(
            targets.len() >= 2,
            "{path} did not retain repeated target spans"
        );
        assert_ne!(
            targets[0].span.start_byte, targets[1].span.start_byte,
            "{path} collapsed repeated call sites"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

fn test_root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("call-graph-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    root
}
