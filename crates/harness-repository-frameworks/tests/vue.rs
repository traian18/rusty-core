mod support;

use std::sync::Arc;

use harness_repository_frameworks::VueAdapter;
use harness_workspace::framework_relations::{Certainty, FrameworkAdapter};
use support::*;

const PACKAGE: &str = r#"{"name":"web","dependencies":{"vue":"3","vue-router":"4"}}"#;

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(VueAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn single_file_components_render_the_components_they_import_and_use() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/App.vue", "<script setup lang=\"ts\">\nimport TheHeader from './components/TheHeader.vue';\nimport { ref } from 'vue';\nimport UserCard from '@/components/UserCard.vue';\nimport Unused from './components/Unused.vue';\nimport Odd from '~/odd.vue';\nconst n = ref(0);\n</script>\n\n<template>\n  <div>\n    <TheHeader />\n    <user-card v-if=\"n\"><template #footer><p>x</p></template></user-card>\n    <UserCard />\n    <router-view />\n  </div>\n</template>\n\n<style scoped>\n.a { color: red }\n</style>\n"),
            ("src/components/TheHeader.vue", "<template><h1>hi</h1></template>\n"),
            ("src/components/UserCard.vue", "<template><section /></template>\n"),
            ("src/components/Unused.vue", "<template><i /></template>\n"),
        ],
        adapters(),
    )
    .await;
    let renders = sorted(repo.summary());
    assert_eq!(
        renders,
        [
            "ComponentRenders|<TheHeader />|src/App.vue->src/components/TheHeader.vue",
            "ComponentRenders|<user-card />|src/App.vue->src/components/UserCard.vue",
        ],
        "a repeated component, an unused import and html elements give nothing extra"
    );
    let card = repo
        .relations()
        .iter()
        .find(|r| r.label == "<user-card />")
        .unwrap();
    assert_eq!(
        card.certainty,
        Certainty::Convention,
        "@/ is a convention, not written in the code"
    );
    assert_eq!(card.to_symbol.as_deref(), Some("UserCard"));
    assert!(card.evidence.text.starts_with("<user-card v-if"));
    let header = repo
        .relations()
        .iter()
        .find(|r| r.label == "<TheHeader />")
        .unwrap();
    assert_eq!(header.certainty, Certainty::Declared);
    assert!(
        repo.diagnostics().iter().any(|d| d.contains("~/odd.vue")),
        "{:?}",
        repo.diagnostics()
    );
}

#[tokio::test]
async fn router_entries_name_vue_components_directly_or_lazily() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/router.ts", "import { createRouter } from 'vue-router';\nimport Home from './views/Home.vue';\nimport About from './views/About';\nconst path = 'x';\nexport const routes = [\n  { path: '/', component: Home },\n  { path: '/about', component: About },\n  { path: '/users', component: () => import('./views/Users.vue'), children: [\n    { path: ':id', component: () => import('./views/User.vue') },\n  ] },\n  { path, component: Home },\n];\n"),
            ("src/views/Home.vue", "<template><div /></template>\n"),
            ("src/views/About.ts", "export default {};\n"),
            ("src/views/Users.vue", "<template><div /></template>\n"),
            ("src/views/User.vue", "<template><div /></template>\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "RouteRegisteredTo|ROUTE /|src/router.ts->src/views/Home.vue".into(),
            "RouteRegisteredTo|ROUTE /about|src/router.ts->src/views/About.ts".into(),
            "RouteRegisteredTo|ROUTE /users|src/router.ts->src/views/Users.vue".into(),
            "RouteRegisteredTo|ROUTE /users/:id|src/router.ts->src/views/User.vue".into(),
        ])
    );
    assert!(
        repo.diagnostics()
            .iter()
            .any(|d| d.contains("computed path `path`")),
        "{:?}",
        repo.diagnostics()
    );
}

#[tokio::test]
async fn files_without_vue_router_are_not_route_tables_and_the_adapter_is_off_without_vue() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            (
                "src/table.ts",
                "export const rows = [{ path: '/a', component: 'x' }];\n",
            ),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());

    let off = repo_without_vue().await;
    assert!(off.relations().is_empty());
}

async fn repo_without_vue() -> Repo {
    repo(
        &[
            (
                "package.json",
                r#"{"name":"x","dependencies":{"react":"18"}}"#,
            ),
            (
                "src/App.vue",
                "<script setup>\nimport B from './B.vue';\n</script>\n<template><B /></template>\n",
            ),
            ("src/B.vue", "<template><i /></template>\n"),
        ],
        adapters(),
    )
    .await
}
