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
    fs::remove_dir_all(root).unwrap();
}

fn test_root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("call-graph-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    root
}
