mod support;

use std::sync::Arc;

use harness_repository_frameworks::NestjsAdapter;
use harness_workspace::framework_relations::{Certainty, FrameworkAdapter, RelationKind};
use support::*;

const PACKAGE: &str =
    r#"{"name":"api","dependencies":{"@nestjs/common":"10","@nestjs/core":"10"}}"#;

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(NestjsAdapter)]
}

#[tokio::test]
async fn controller_routes_join_prefix_and_method_paths() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/users.controller.ts", "import { Controller, Get, Post, Delete, Put } from '@nestjs/common';\n@Controller('users')\nexport class UsersController {\n  @Get()\n  list() {}\n  @Get(':id')\n  find() {}\n  @Post('/bulk/')\n  bulk() {}\n  @Delete({ path: 'x' } as any)\n  weird() {}\n  helper() {}\n}\n@Controller({ path: 'orders' })\nclass OrdersController {\n  @Put(':id')\n  update() {}\n}\n@Controller()\nclass RootController {\n  @Get('health')\n  health() {}\n}\n"),
        ],
        adapters(),
    )
    .await;
    let routes: Vec<_> = repo
        .relations()
        .iter()
        .filter(|r| r.kind == RelationKind::RouteRegisteredTo)
        .map(|r| (r.label.as_str(), r.to_symbol.clone().unwrap(), r.certainty))
        .collect();
    assert_eq!(
        routes,
        [
            (
                "GET /users",
                "UsersController.list".to_owned(),
                Certainty::Declared
            ),
            (
                "GET /users/:id",
                "UsersController.find".to_owned(),
                Certainty::Declared
            ),
            (
                "POST /users/bulk",
                "UsersController.bulk".to_owned(),
                Certainty::Declared
            ),
            (
                "PUT /orders/:id",
                "OrdersController.update".to_owned(),
                Certainty::Declared
            ),
            (
                "GET /health",
                "RootController.health".to_owned(),
                Certainty::Declared
            ),
        ]
    );
    let first = &repo.relations()[0];
    assert_eq!(first.evidence.text, "@Get()");
    assert_eq!(first.rule.id, "nestjs-controller-route");
    assert!(
        repo.diagnostics()
            .iter()
            .any(|d| d.contains("computed @Delete path")),
        "{:?}",
        repo.diagnostics()
    );
}

#[tokio::test]
async fn constructor_injection_points_from_the_provider_to_its_consumer() {
    let repo = repo(
        &[
            ("package.json", PACKAGE),
            ("src/users.service.ts", "import { Injectable } from '@nestjs/common';\n@Injectable()\nexport class UsersService {}\n"),
            ("src/plain.ts", "export class PlainThing {}\n"),
            ("src/users.controller.ts", "import { Controller, Inject } from '@nestjs/common';\nimport { UsersService } from './users.service';\nimport { PlainThing } from './plain';\nimport { ConfigService } from '@nestjs/config';\n@Controller('users')\nexport class UsersController {\n  constructor(\n    private readonly users: UsersService,\n    private readonly plain: PlainThing,\n    private readonly config: ConfigService,\n    @Inject('TOKEN') private readonly token: string,\n  ) {}\n}\n"),
            ("src/audit.service.ts", "import { Injectable } from '@nestjs/common';\nimport { UsersService } from './users.service';\n@Injectable()\nexport class AuditService {\n  constructor(private users: UsersService) {}\n}\n"),
        ],
        adapters(),
    )
    .await;
    let injections: Vec<String> = repo
        .summary()
        .into_iter()
        .filter(|s| s.starts_with("ProviderInjectedInto"))
        .collect();
    assert_eq!(
        injections,
        [
            "ProviderInjectedInto|UsersService|src/users.service.ts->src/audit.service.ts",
            "ProviderInjectedInto|UsersService|src/users.service.ts->src/users.controller.ts",
        ]
    );
    let relation = repo
        .relations()
        .iter()
        .find(|r| r.kind == RelationKind::ProviderInjectedInto)
        .unwrap();
    assert_eq!(relation.from_symbol.as_deref(), Some("UsersService"));
    assert!(relation.evidence.text.contains("users: UsersService"));
    let diagnostics = repo.diagnostics();
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("explicit or computed token")),
        "{diagnostics:?}"
    );
}

#[tokio::test]
async fn the_adapter_is_off_without_nest_and_ignores_unrelated_decorators() {
    let off = repo(
        &[
            (
                "package.json",
                r#"{"name":"x","dependencies":{"express":"4"}}"#,
            ),
            (
                "src/a.ts",
                "@Controller('users')\nexport class A { @Get() list() {} }\n",
            ),
        ],
        adapters(),
    )
    .await;
    assert!(off.relations().is_empty());

    let unrelated = repo(
        &[
            ("package.json", PACKAGE),
            ("src/a.ts", "@Component('x')\nexport class A { @Input() name: string; @Get2() list() {} }\nexport class NotDecorated { @Get() maybe() {} }\n"),
        ],
        adapters(),
    )
    .await;
    assert!(
        unrelated.relations().is_empty(),
        "{:?}",
        unrelated.summary()
    );
}
