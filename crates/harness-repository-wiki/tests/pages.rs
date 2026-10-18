mod support;

use harness_repository_wiki::{PageId, PageLevel, PageStatus};
use support::*;

fn fact_texts<'a>(page: &'a harness_repository_wiki::Page, section: &str) -> Vec<&'a str> {
    page.sections
        .iter()
        .find(|s| s.key == section)
        .map(|s| s.facts.iter().map(|f| f.text.as_str()).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_file_page_lists_imports_importers_and_history_with_citations() {
    let fixture = Fixture::new();
    fixture.refresh().await;
    let page = fixture
        .renderer()
        .page(&PageId::file("src/a.rs"), None)
        .unwrap();
    assert_eq!(page.status, PageStatus::Ok, "{page:?}");
    assert_eq!(page.language.as_deref(), Some("rust"));
    assert_eq!(fact_texts(&page, "imports"), ["imports src/b.rs"]);
    let importers = &page
        .sections
        .iter()
        .find(|s| s.key == "imported_by")
        .unwrap()
        .facts;
    assert_eq!(
        importers
            .iter()
            .map(|f| f.refs[0].0.as_str())
            .collect::<Vec<_>>(),
        ["file:src/b.rs", "file:src/c.rs", "file:src/lib.rs"]
    );
    // c.rs has its import on the third line, and the citation says so.
    let c = importers
        .iter()
        .find(|f| f.refs[0].0 == "file:src/c.rs")
        .unwrap();
    assert_eq!(
        (c.citations[0].file.as_str(), c.citations[0].start_line),
        ("src/c.rs", 3)
    );
    let import = &page.sections[0].facts[0];
    assert_eq!(
        (import.citations[0].start_line, import.citations[0].end_line),
        (1, 1)
    );
    assert_eq!(import.citations[0].content_hash.len(), 64);
    assert!(page
        .sections
        .iter()
        .any(|s| s.key == "history" && s.facts.iter().any(|f| f.text.contains("4 commits"))));
    assert!(
        page.sections.iter().any(|s| s.key == "findings"),
        "the cycle is a finding on a.rs"
    );
    assert!(page.summary.contains("imported by 3 files"));
    let markdown = page.to_markdown();
    assert!(
        markdown.contains("## Imports") && markdown.contains("[src/c.rs:3-3]"),
        "{markdown}"
    );
}

#[tokio::test]
async fn pages_are_deterministic_and_only_affected_pages_change_when_a_file_changes() {
    let fixture = Fixture::new();
    fixture.refresh().await;
    let renderer = fixture.renderer();
    let digest = |id: &str| renderer.page(&PageId::file(id), None).unwrap().digest;
    assert_eq!(digest("src/a.rs"), digest("src/a.rs"));
    let (a, b, c) = (digest("src/a.rs"), digest("src/b.rs"), digest("src/c.rs"));

    write(
        fixture.dir.path(),
        "src/a.rs",
        "use crate::b::B;\npub struct A;\n// edited\n",
    );
    fixture.refresh().await;
    let renderer = fixture.renderer();
    let after = |id: &str| renderer.page(&PageId::file(id), None).unwrap().digest;
    assert_ne!(after("src/a.rs"), a, "its own content changed");
    assert_ne!(after("src/b.rs"), b, "b.rs cites a.rs");
    assert_eq!(after("src/c.rs"), c, "c.rs only cites itself");
}

#[tokio::test]
async fn a_page_is_stale_when_its_file_changed_after_the_snapshot() {
    let fixture = Fixture::new();
    fixture.refresh().await;
    write(
        fixture.dir.path(),
        "src/a.rs",
        "use crate::b::B;\npub struct A;\n// later\n",
    );
    let page = fixture
        .renderer()
        .page(&PageId::file("src/a.rs"), None)
        .unwrap();
    assert_eq!(page.status, PageStatus::Stale);
    assert!(page.gaps.iter().any(|g| g.contains("changed since")));
    assert!(
        !page.sections.is_empty(),
        "the snapshot's view is still shown"
    );
}

#[tokio::test]
async fn every_other_status_has_a_page_not_an_error() {
    let fixture = Fixture::new();
    fixture.refresh().await;
    let renderer = fixture.renderer();
    let status = |id: &str| renderer.page(&PageId::file(id), None).unwrap();
    assert_eq!(status("scripts/tool.rb").status, PageStatus::Unsupported);
    assert_eq!(status("scripts/tool.rb").language.as_deref(), Some("Ruby"));
    assert_eq!(
        status("node_modules/dep/index.js").status,
        PageStatus::NotIndexed
    );
    assert_eq!(status("src/missing.rs").status, PageStatus::NotFound);
    write(fixture.dir.path(), "src/new.rs", "pub struct N;\n");
    assert_eq!(status("src/new.rs").status, PageStatus::Stale);
    assert_eq!(
        renderer
            .page(&PageId("bogus:x".into()), None)
            .unwrap_err()
            .code(),
        "invalid_request"
    );
}

#[tokio::test]
async fn folder_package_and_repository_pages_link_the_level_below() {
    let fixture = Fixture::new();
    fixture.refresh().await;
    let renderer = fixture.renderer();

    let repo = renderer.page(&PageId::repository(), None).unwrap();
    let contents = repo.sections.iter().find(|s| s.key == "contents").unwrap();
    assert!(contents.facts.iter().all(|f| !f.refs.is_empty()));
    assert!(contents
        .facts
        .iter()
        .any(|f| f.refs[0].0.starts_with("package:")));
    assert!(repo.sections.iter().any(|s| s.key == "repository"));
    assert!(repo.summary.contains("files"));

    let src = renderer.page(&PageId::folder("dir:src"), None).unwrap();
    assert_eq!(src.level, PageLevel::Folder);
    assert_eq!(
        fact_texts(&src, "contents")
            .iter()
            .filter(|t| t.starts_with("tools"))
            .count(),
        1
    );
    assert!(src
        .sections
        .iter()
        .any(|s| s.key == "most_depended_on" && s.facts[0].text.starts_with("src/a.rs")));

    // A folder with only files lists them; one with folders and files has one loose-files entry.
    let tools = renderer
        .page(&PageId::folder("dir:src/tools"), None)
        .unwrap();
    assert_eq!(fact_texts(&tools, "contents").len(), 2);
    let loose = renderer
        .page(&PageId::folder("rest:dir:src"), None)
        .unwrap();
    assert_eq!(loose.status, PageStatus::Ok);
    assert!(loose.summary.contains("4 files"), "{}", loose.summary);

    let package = renderer
        .page(
            &PageId::new(PageLevel::Package, "declared_package:Cargo.toml:app"),
            None,
        )
        .unwrap();
    assert!(package.sections.iter().any(|s| s.key == "manifest"));
    assert_eq!(
        renderer
            .page(&PageId::folder("dir:nowhere"), None)
            .unwrap()
            .status,
        PageStatus::NotIndexed
    );
}

#[tokio::test]
async fn the_tree_walks_the_repository_one_level_at_a_time() {
    let fixture = Fixture::new();
    fixture.refresh().await;
    let renderer = fixture.renderer();
    let top = renderer.children(&PageId::repository(), None).unwrap();
    assert!(top.iter().any(|e| e.id.0.starts_with("package:")));
    let src = renderer.children(&PageId::folder("dir:src"), None).unwrap();
    assert_eq!(
        src.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
        ["tools", "files"]
    );
    let leaf = renderer
        .children(&PageId::folder("dir:src/tools"), None)
        .unwrap();
    assert!(leaf.iter().all(|e| e.level == PageLevel::File));
    assert!(renderer
        .children(&PageId::file("src/a.rs"), None)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn without_a_snapshot_there_is_an_error_to_refresh() {
    let fixture = Fixture::new();
    let error = fixture
        .renderer()
        .page(&PageId::repository(), None)
        .unwrap_err();
    assert_eq!(error.code(), "snapshot_not_found");
}
