mod support;

use harness_repository_wiki::written::{
    BriefBuilder, BriefContext, WrittenKind, ACTIVE_LANDSCAPE, ARCHITECTURE, GETTING_STARTED,
    GLOSSARY, HOW_IT_WORKS, KEY_CONCEPTS, OVERVIEW,
};
use support::*;

const README: &str = "# Demo\n\nDemo turns source files into answers. It is small.\n\n## Concepts\n\n- **Answer** \u{2014} what Demo returns for a question.\n- **Store**: where answers are kept.\n- `Tool` - a helper the agents call.\n\n## Install\n\n```sh\ncargo build --release\n```\n\n## Testing\n\nRun `cargo test`.\n";

async fn builder() -> (Fixture, BriefBuilder) {
    let fixture = Fixture::new();
    write(fixture.dir.path(), "README.md", README);
    fixture.refresh().await;
    let builder = BriefBuilder::new(fixture.renderer());
    (fixture, builder)
}

fn context() -> BriefContext {
    BriefContext {
        repository_name: "demo".into(),
        ..BriefContext::default()
    }
}

#[tokio::test]
async fn targets_are_the_reading_order_then_the_modules() {
    let (_fixture, builder) = builder().await;
    let targets = builder.targets().unwrap();
    let ids: Vec<&str> = targets.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        &ids[..7],
        [
            OVERVIEW,
            GETTING_STARTED,
            KEY_CONCEPTS,
            HOW_IT_WORKS,
            ARCHITECTURE,
            ACTIVE_LANDSCAPE,
            GLOSSARY
        ]
    );
    assert!(
        targets[7..].iter().all(|t| t.kind == WrittenKind::Module),
        "{ids:?}"
    );
}

#[tokio::test]
async fn a_brief_is_deterministic_and_carries_real_code_and_the_readme_vocabulary() {
    let (_fixture, builder) = builder().await;
    let first = builder.brief(ARCHITECTURE, &context()).unwrap();
    let second = builder.brief(ARCHITECTURE, &context()).unwrap();
    assert_eq!(first.prompt, second.prompt);
    assert_eq!(first.facts_digest, second.facts_digest);
    for part in [
        "## The parts and what they own",
        "How work moves through the system",
        "# Code excerpts",
        "Demo turns source files into answers.",
        "## The parts (top-level components)",
    ] {
        assert!(first.prompt.contains(part), "missing {part}");
    }
    assert!(!first.prompt.contains("[1]"), "no footnote numbering");
    assert!(first.system.contains("Never invent"));
    assert_eq!(first.diagrams[0].key, "components");
    assert!(first.diagrams[0].svg.starts_with("<svg"));
    assert!(first.inputs.iter().any(|i| i.file == "README.md"));
}

#[tokio::test]
async fn every_target_has_a_brief_with_its_own_shape() {
    let (_fixture, builder) = builder().await;
    for target in builder.targets().unwrap() {
        let brief = builder.brief(&target.id, &context()).unwrap();
        if target.kind == WrittenKind::Glossary {
            let page = brief.prewritten.as_deref().unwrap();
            assert!(
                page.contains("| **Answer** | what Demo returns for a question. | `README.md` |"),
                "{page}"
            );
            assert_eq!(brief.input_chars, 0, "no model");
        } else {
            assert!(brief.prewritten.is_none());
            assert!(brief.input_chars > 0, "{}", target.id);
        }
    }
    let started = builder.brief(GETTING_STARTED, &context()).unwrap();
    assert!(
        started.prompt.contains("cargo build --release"),
        "setup sections verbatim"
    );
    assert!(started.prompt.contains("Never invent or tidy up a command"));
    let landscape = builder.brief(ACTIVE_LANDSCAPE, &context()).unwrap();
    assert!(
        landscape.prompt.contains("`src/a.rs`"),
        "{}",
        landscape.prompt
    );
    let concepts = builder.brief(KEY_CONCEPTS, &context()).unwrap();
    assert!(concepts.prompt.contains("## Candidate concepts"));
    let flow = builder.brief(HOW_IT_WORKS, &context()).unwrap();
    assert!(flow.prompt.contains("## The traced path"));
}

#[tokio::test]
async fn diagrams_are_deterministic_escape_labels_and_survive_cycles() {
    use harness_repository_wiki::written::{dependency_diagram, DiagramNode};
    use harness_workspace::repository_intelligence::RootRelativePath;
    let (fixture, _builder) = builder().await;
    let snapshot = fixture.renderer().current_snapshot().unwrap();
    let node = |label: &str, files: &[&str]| DiagramNode {
        label: label.into(),
        members: files
            .iter()
            .map(|f| RootRelativePath::new(f).unwrap())
            .collect(),
    };
    // a.rs and b.rs import each other: a cycle between the two nodes.
    let nodes = vec![node("a <&> \"x\"", &["src/a.rs"]), node("b", &["src/b.rs"])];
    let one = dependency_diagram(&snapshot, "k", "T", &nodes);
    let two = dependency_diagram(&snapshot, "k", "T", &nodes);
    assert_eq!(one.svg, two.svg);
    assert!(
        one.svg.contains("a &lt;&amp;&gt; &quot;x&quot;"),
        "{}",
        one.svg
    );
    assert!(!one.svg.contains("a <&>"));
    assert_eq!(one.nodes, 2);
}

#[tokio::test]
async fn an_assembled_page_names_only_what_exists_and_is_fresh_until_the_facts_move() {
    use harness_repository_wiki::written::{Author, Freshness};
    let (fixture, builder) = builder().await;
    let brief = builder.brief(OVERVIEW, &context()).unwrap();
    let author = Author {
        model: "m".into(),
        style: "comprehensive".into(),
        written_at_ms: 1,
        commit: None,
    };
    let markdown = "## What demo is
Demo keeps answers; `src/a.rs` imports `src/b.rs`, while `src/missing.rs` and `MadeUpEngine` do not exist.
@@diagram:components@@
@@diagram:nope@@";
    let page = builder.assemble(&brief, markdown, author);
    assert_eq!(page.dropped, 2, "{page:?}");
    assert!(page.markdown.contains("`src/a.rs` imports `src/b.rs`"));
    assert!(page
        .markdown
        .contains("while src/missing.rs and MadeUpEngine do not exist"));
    assert_eq!(
        page.footnotes
            .iter()
            .map(|f| match &f.reference {
                harness_repository_wiki::written::SourceRef::Lines { citation } =>
                    citation.file.as_str(),
                _ => "",
            })
            .collect::<Vec<_>>(),
        ["src/a.rs", "src/b.rs"]
    );
    assert_eq!(page.diagrams, ["components"]);
    assert!(page.needs_review, "a page this short is flagged");
    assert_eq!(builder.freshness_of(&page).unwrap(), Freshness::Fresh);
    let mut moved = page.clone();
    moved.facts_digest = "other".into();
    assert_eq!(builder.freshness_of(&moved).unwrap(), Freshness::Stale);
    for input in &mut moved.inputs {
        input.content_hash = "old".into();
    }
    assert_eq!(builder.freshness_of(&moved).unwrap(), Freshness::Expired);
    drop(fixture);
}
