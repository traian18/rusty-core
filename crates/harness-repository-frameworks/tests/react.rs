mod support;

use std::sync::Arc;

use harness_repository_frameworks::ReactAdapter;
use harness_workspace::framework_relations::{FrameworkAdapter, RelationKind};
use support::*;

const PACKAGE: &str = r#"{"name":"web","dependencies":{"react":"18","react-router-dom":"6"}}"#;

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(ReactAdapter)]
}

#[tokio::test]
async fn components_point_at_the_components_they_render_through_imports() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/App.tsx", "import React from 'react';\nimport { Header } from './Header';\nimport Footer from './Footer';\nimport * as Ui from './ui';\nimport { Link } from 'react-router-dom';\nexport function App() {\n  return (\n    <div>\n      <Header title=\"x\" />\n      <Link to=\"/\">home</Link>\n      <Ui.Card><Ui.Card /></Ui.Card>\n      <Footer />\n      <Header />\n      <Local />\n    </div>\n  );\n}\nconst Local = () => <span>local</span>;\n"),
            ("src/Header.tsx", "export function Header(props: { title?: string }) { return <h1>{props.title}</h1>; }\n"),
            ("src/Footer.tsx", "export default function Footer() { return <footer /> }\n"),
            ("src/ui.tsx", "export function Card(props: any) { return <section>{props.children}</section>; }\n"),
            ("src/main.tsx", "import { App } from './App';\nimport { createRoot } from 'react-dom/client';\ncreateRoot(document.body).render(<App />);\n"),
        ],
        adapters(),
    )
    .await;
    let mut summary = repo.summary();
    summary.sort();
    assert_eq!(
        summary,
        [
            "ComponentRenders|<App />|src/main.tsx->src/App.tsx",
            "ComponentRenders|<Footer />|src/App.tsx->src/Footer.tsx",
            "ComponentRenders|<Header />|src/App.tsx->src/Header.tsx",
            "ComponentRenders|<Local />|src/App.tsx->src/App.tsx",
            "ComponentRenders|<Ui />|src/App.tsx->src/ui.tsx",
        ],
        "html tags, external components and repeated uses give no extra relations"
    );
    let header = repo
        .relations()
        .iter()
        .find(|r| r.label == "<Header />")
        .unwrap();
    assert_eq!(header.kind, RelationKind::ComponentRenders);
    assert_eq!(header.from_symbol.as_deref(), Some("App"));
    assert_eq!(header.to_symbol.as_deref(), Some("Header"));
    assert_eq!(header.evidence.text, "<Header title=\"x\" />");
    let entry = repo
        .relations()
        .iter()
        .find(|r| r.label == "<App />")
        .unwrap();
    assert_eq!(
        entry.from_symbol, None,
        "JSX outside any component belongs to the file"
    );
}

#[tokio::test]
async fn look_alikes_are_not_relations() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/Page.tsx", "import { Thing } from './thing';\nexport function Page() {\n  const text = '<Thing />';\n  // <Thing />\n  return <div className=\"x\">{text}</div>;\n}\nexport function Recursive() { return <Recursive />; }\n"),
            ("src/thing.ts", "export const Thing = 1;\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty(), "{:?}", repo.summary());
}

#[tokio::test]
async fn the_adapter_is_off_without_react() {
    let repo = repo(
        &[
            ("package.json", r#"{"name":"w","dependencies":{"vue":"3"}}"#),
            (
                "src/a.tsx",
                "import { B } from './b';\nexport function A() { return <B /> }\n",
            ),
            ("src/b.tsx", "export function B() { return <i /> }\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());
}
