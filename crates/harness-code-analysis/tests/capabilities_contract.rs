mod support;
use support::Fixture;

use harness_code_analysis::{CallGraph, DefaultCallGraph, LanguageId};

#[test]
fn every_scoped_language_reports_versioned_tested_capabilities() {
    let graph = DefaultCallGraph::new(std::env::temp_dir());
    let capabilities = graph.capabilities();
    assert_eq!(capabilities.languages.len(), 10);
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
        let language = capabilities
            .languages
            .iter()
            .find(|l| l.language == expected)
            .unwrap_or_else(|| panic!("missing {expected:?}"));
        assert!(language.supported);
        assert!(!language.adapter_version.is_empty(), "{expected:?}");
        assert!(!language.resolver_version.is_empty(), "{expected:?}");
        let text = language.limitations.join(" ").to_lowercase();
        assert!(
            language.limitations.len() >= 3,
            "{expected:?} documents its limits"
        );
        assert!(
            text.contains("resolves"),
            "{expected:?} states the resolution subset it implements"
        );
        assert!(
            [
                "dynamic",
                "reflection",
                "macro",
                "metaprogramming",
                "variable",
                "function values",
                "unresolved"
            ]
            .iter()
            .any(|word| text.contains(word)),
            "{expected:?} declares its dynamic-call limits"
        );
    }
    assert!(capabilities.supports_partial_results && capabilities.supports_cross_file_resolution);
}

/// The capability statements are only claims about behaviour the fixtures in `lang_*.rs` verify;
/// this guards the cross-cutting promise that evidence names its rule and versions for every
/// resolved edge in a mixed-language workspace.
#[tokio::test]
async fn resolved_edges_in_a_mixed_workspace_carry_rule_and_version_evidence() {
    let f = Fixture::new(&[
        ("a.py", "def a():\n    pass\ndef b():\n    a()\n"),
        ("c.rs", "fn c() {}\nfn d() { c(); }\n"),
        ("e.js", "function e() {}\nfunction f() { e(); }\n"),
    ])
    .await;
    let mut resolved = 0;
    for caller in f.callables.clone() {
        for call in f.calls(&caller).await {
            let evidence = call.evidence.expect("every call site has evidence");
            assert!(!evidence.rule_id.is_empty() && !evidence.explanation.is_empty());
            assert!(
                evidence.adapter_version.contains('+'),
                "adapter+resolver versions"
            );
            if call.resolution == harness_code_analysis::Resolution::Resolved {
                resolved += 1;
                assert!(
                    evidence.span.is_some(),
                    "resolved edges point at source evidence"
                );
            }
        }
    }
    assert_eq!(resolved, 3);
}
