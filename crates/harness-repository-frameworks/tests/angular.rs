mod support;

use std::sync::Arc;

use harness_repository_frameworks::AngularAdapter;
use harness_workspace::framework_relations::{FrameworkAdapter, RelationKind};
use support::*;

const PACKAGE: &str =
    r#"{"name":"app","dependencies":{"@angular/core":"17","@angular/router":"17"}}"#;

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(AngularAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn templates_render_components_by_selector_inline_and_by_url() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/app.component.ts", "import { Component } from '@angular/core';\n@Component({\n  selector: 'app-root',\n  template: `<app-header></app-header><main><app-card /></main><div app-card></div><app-root></app-root>`,\n})\nexport class AppComponent {}\n"),
            ("src/header.component.ts", "import { Component } from '@angular/core';\n@Component({ selector: 'app-header', templateUrl: './header.component.html' })\nexport class HeaderComponent {}\n"),
            ("src/header.component.html", "<nav><app-card></app-card><app-card></app-card></nav>\n"),
            ("src/card.component.ts", "import { Component } from '@angular/core';\n@Component({ selector: 'app-card', template: '<p>card</p>' })\nexport class CardComponent {}\n"),
        ],
        adapters(),
    )
    .await;
    let renders = sorted(
        repo.summary()
            .into_iter()
            .filter(|s| s.starts_with("ComponentRenders"))
            .collect(),
    );
    assert_eq!(
        renders,
        [
            "ComponentRenders|<app-card>|src/app.component.ts->src/card.component.ts",
            "ComponentRenders|<app-card>|src/header.component.ts->src/card.component.ts",
            "ComponentRenders|<app-header>|src/app.component.ts->src/header.component.ts",
        ],
        "attribute uses, self-use and repeated tags give nothing extra"
    );
    let from_html = repo
        .relations()
        .iter()
        .find(|r| r.from.as_path().ends_with("header.component.ts"))
        .unwrap();
    assert_eq!(
        from_html.evidence.file.as_path().to_str(),
        Some("src/header.component.html")
    );
    assert_eq!(from_html.evidence.text, "<app-card>");
    assert_eq!(from_html.from_symbol.as_deref(), Some("HeaderComponent"));
    assert_eq!(from_html.to_symbol.as_deref(), Some("CardComponent"));
}

#[tokio::test]
async fn router_entries_join_children_and_follow_lazy_components() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/app.routes.ts", "import { Routes } from '@angular/router';\nimport { HomeComponent } from './home.component';\nimport { UsersComponent } from './users.component';\nconst base = 'dyn';\nexport const routes: Routes = [\n  { path: '', component: HomeComponent },\n  { path: 'users', component: UsersComponent, children: [\n    { path: ':id', loadComponent: () => import('./user.component').then((m) => m.UserComponent) },\n  ] },\n  { path: base, component: HomeComponent },\n  { path: 'admin', loadChildren: () => import('./admin') },\n  { path: 'redirect', redirectTo: '' },\n];\n"),
            ("src/home.component.ts", "export class HomeComponent {}\n"),
            ("src/users.component.ts", "export class UsersComponent {}\n"),
            ("src/user.component.ts", "export class UserComponent {}\n"),
        ],
        adapters(),
    )
    .await;
    let routes = sorted(
        repo.summary()
            .into_iter()
            .filter(|s| s.starts_with("RouteRegisteredTo"))
            .collect(),
    );
    let expected = sorted(vec![
        "RouteRegisteredTo|ROUTE /|src/app.routes.ts->src/home.component.ts".to_owned(),
        "RouteRegisteredTo|ROUTE /users/:id|src/app.routes.ts->src/user.component.ts".to_owned(),
        "RouteRegisteredTo|ROUTE /users|src/app.routes.ts->src/users.component.ts".to_owned(),
    ]);
    assert_eq!(routes, expected);
    let diagnostics = repo.diagnostics();
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("computed path `base`")),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("lazy-loads children")),
        "{diagnostics:?}"
    );
}

#[tokio::test]
async fn services_are_injected_through_constructors_and_inject() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/data.service.ts", "import { Injectable } from '@angular/core';\n@Injectable({ providedIn: 'root' })\nexport class DataService {}\n"),
            ("src/log.service.ts", "import { Injectable } from '@angular/core';\n@Injectable()\nexport class LogService {}\n"),
            ("src/list.component.ts", "import { Component, inject } from '@angular/core';\nimport { DataService } from './data.service';\nimport { LogService } from './log.service';\n@Component({ selector: 'app-list', template: '' })\nexport class ListComponent {\n  private data = inject(DataService);\n  private token = inject(getToken());\n  constructor(private log: LogService) {}\n}\n"),
        ],
        adapters(),
    )
    .await;
    let injections = sorted(
        repo.summary()
            .into_iter()
            .filter(|s| s.starts_with("ProviderInjectedInto"))
            .collect(),
    );
    assert_eq!(
        injections,
        [
            "ProviderInjectedInto|DataService|src/data.service.ts->src/list.component.ts",
            "ProviderInjectedInto|LogService|src/log.service.ts->src/list.component.ts",
        ]
    );
    assert!(
        repo.diagnostics()
            .iter()
            .any(|d| d.contains("explicit or computed token")),
        "{:?}",
        repo.diagnostics()
    );
    assert!(repo
        .relations()
        .iter()
        .all(|r| r.kind != RelationKind::ComponentRenders));
}

#[tokio::test]
async fn duplicate_selectors_are_reported_and_the_adapter_is_off_without_angular() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/a.component.ts", "@Component({ selector: 'app-dup', template: '<app-dup></app-dup>' })\nexport class A {}\n"),
            ("src/b.component.ts", "@Component({ selector: 'app-dup', template: '' })\nexport class B {}\n"),
            ("src/c.component.ts", "@Component({ selector: 'app-c', template: '<app-dup></app-dup>' })\nexport class C {}\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty(), "{:?}", repo.summary());
    assert!(repo
        .diagnostics()
        .iter()
        .any(|d| d.contains("declared by 2 components")));

    let off = repo_off().await;
    assert!(off.relations().is_empty());
}

async fn repo_off() -> Repo {
    repo(
        &[
            ("package.json", r#"{"name":"x","dependencies":{"react":"18"}}"#),
            ("src/a.component.ts", "@Component({ selector: 'app-a', template: '<app-b></app-b>' })\nexport class A {}\n"),
            ("src/b.component.ts", "@Component({ selector: 'app-b', template: '' })\nexport class B {}\n"),
        ],
        adapters(),
    )
    .await
}
