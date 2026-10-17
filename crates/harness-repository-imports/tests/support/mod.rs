#![allow(dead_code)]
use std::{path::Path, sync::Arc};

use harness_repository_imports::{
    RustImportAdapter, RustImportResolver, TypeScriptImportAdapter, TypeScriptImportResolver,
};
use harness_workspace::{
    dependency_groups::DefaultGroupAnalyzer,
    import_graph::{Direction, ImportEdge, ImportFilter, ImportQuery, ImportSupport, Resolution},
    repository_intelligence::{
        RepositoryGraph, RepositoryGraphBuilder, RepositoryGraphService, RootRelativePath,
        SnapshotId,
    },
};
use tokio_util::sync::CancellationToken;

pub fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

pub fn service(root: &Path) -> RepositoryGraphService {
    builder(root).build()
}

/// The standard composition plus dependency groups.
pub fn grouped_service(root: &Path) -> RepositoryGraphService {
    builder(root)
        .dependency_groups(Arc::new(DefaultGroupAnalyzer::standard()))
        .build()
}

pub fn builder(root: &Path) -> RepositoryGraphBuilder {
    RepositoryGraphBuilder::new(std::fs::canonicalize(root).unwrap())
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(RustImportAdapter),
            resolver: Arc::new(RustImportResolver),
        })
        .unwrap()
        .import_support(ImportSupport {
            adapter: Arc::new(TypeScriptImportAdapter),
            resolver: Arc::new(TypeScriptImportResolver),
        })
        .unwrap()
}

pub fn path(value: &str) -> RootRelativePath {
    RootRelativePath::new(value).unwrap()
}

pub fn query(snapshot: &SnapshotId, file: &str, limit: usize) -> ImportQuery {
    ImportQuery {
        snapshot: snapshot.clone(),
        file: path(file),
        item: None,
        filter: ImportFilter::default(),
        cursor: None,
        limit,
    }
}

pub async fn refresh(graph: &RepositoryGraphService) -> SnapshotId {
    graph.refresh(CancellationToken::new()).await.unwrap().id
}

/// All outgoing edges of `file`.
pub fn outgoing(graph: &RepositoryGraphService, id: &SnapshotId, file: &str) -> Vec<ImportEdge> {
    graph.dependencies(&query(id, file, 500)).unwrap().edges
}

pub fn incoming(graph: &RepositoryGraphService, id: &SnapshotId, file: &str) -> Vec<ImportEdge> {
    graph.dependents(&query(id, file, 500)).unwrap().edges
}

pub fn edge<'a>(edges: &'a [ImportEdge], spelling: &str) -> &'a ImportEdge {
    edges
        .iter()
        .find(|e| e.fact.spelling == spelling)
        .unwrap_or_else(|| {
            panic!(
                "no edge for {spelling}; have {:?}",
                edges.iter().map(|e| &e.fact.spelling).collect::<Vec<_>>()
            )
        })
}

pub fn target(edge: &ImportEdge) -> String {
    match &edge.resolution {
        Resolution::ResolvedInternal { target_file, .. } => {
            target_file.as_path().to_string_lossy().into_owned()
        }
        other => panic!("not internal: {other:?}"),
    }
}

pub fn _direction() -> Direction {
    Direction::Dependencies
}

pub fn rust_workspace(dir: &std::path::Path) {
    write(
        dir,
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"util\"]\n",
    );
    write(
        dir,
        "util/Cargo.toml",
        "[package]\nname = \"util\"\nversion = \"0.1.0\"\n",
    );
    write(
        dir,
        "util/src/lib.rs",
        "pub mod shapes;\npub mod nested;\npub use shapes::Circle;\npub use nested::deep::*;\npub fn helper() {}\n",
    );
    write(
        dir,
        "util/src/shapes.rs",
        "pub struct Circle;\npub struct Square;\n",
    );
    write(dir, "util/src/nested/mod.rs", "pub mod deep;\n");
    write(
        dir,
        "util/src/nested/deep.rs",
        "pub fn f() {}\npub fn g() {}\n",
    );
    write(
        dir,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nutil = { path = \"../util\" }\nserde = \"1\"\n",
    );
    write(
        dir,
        "app/src/main.rs",
        "mod config;\nmod missing;\n#[path = \"other/odd.rs\"]\nmod odd;\nuse util::Circle;\nuse util::shapes::{Square, Triangle};\nuse util::nested::deep::f;\nuse util::g;\nuse util::nofn;\nuse serde::Serialize;\nuse std::fmt;\nuse unknown::X;\nuse crate::config::Settings;\nuse config::*;\nextern crate serde;\nfn main() { use self::config::Settings as S; }\n",
    );
    write(
        dir,
        "app/src/config.rs",
        "pub struct Settings;\nuse super::odd;\nuse crate::missing_item::Y;\n",
    );
    write(dir, "app/src/other/odd.rs", "pub fn odd() {}\n");
    write(dir, "app/tests/it.rs", "mod common;\nuse util::helper;\n");
    write(dir, "app/tests/common/mod.rs", "pub fn setup() {}\n");
    write(dir, "util/tests/own.rs", "use util::helper;\n");
    write(
        dir,
        "app/src/stray.rs",
        "use std::io;\nuse crate::config;\n",
    );
}

pub fn ts_workspace(dir: &std::path::Path) {
    write(
        dir,
        "package.json",
        r#"{"name":"root","workspaces":["packages/*"]}"#,
    );
    write(
        dir,
        "packages/core/package.json",
        r#"{"name":"@acme/core","types":"src/index.ts","dependencies":{"lodash":"1"}}"#,
    );
    write(
        dir,
        "packages/core/src/index.ts",
        "export { Button } from './button';\nexport * from './util';\nexport type { Props } from './types';\n",
    );
    write(
        dir,
        "packages/core/src/button.ts",
        "export class Button {}\n",
    );
    write(
        dir,
        "packages/core/src/util.ts",
        "export function slug() {}\n",
    );
    write(
        dir,
        "packages/core/src/types.ts",
        "export interface Props {}\n",
    );
    write(
        dir,
        "packages/app/package.json",
        r#"{"name":"app","dependencies":{"@acme/core":"*","lodash":"1","react":"1"},"devDependencies":{"@types/node":"1"}}"#,
    );
    write(
        dir,
        "packages/app/src/main.ts",
        r#"import { Button, slug, Props, Missing } from '@acme/core';
import type { Props as P2 } from '@acme/core';
import def, * as ns from './local';
import './side-effect';
import { x } from './local.js';
import lodash from 'lodash';
import React from 'react';
import fs from 'node:fs';
import x2 from 'nope';
import a from '@/alias';
import logo from './logo.svg';
import { b, type C } from './dir';
import sub from '@acme/core/src/util';
import u from './util';
const r = require('./local');
const d = import('./lazy');
const v = import(someVar);
export * from './local';
import { slug as slugged } from '@lib/util';
import { Button as UiButton } from '@acme/ui';
import missing from '@missing/thing';
"#,
    );
    write(
        dir,
        "tsconfig.base.json",
        r#"{
  // shared path aliases
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@acme/ui": ["packages/core/src/index.ts"],
      "@lib/*": ["packages/core/src/*"], /* wildcard */
      "@missing/*": ["packages/none/*"],
    },
  },
}"#,
    );
    write(
        dir,
        "packages/app/tsconfig.json",
        r#"{"extends": "../../tsconfig.base"}"#,
    );
    write(dir, "packages/lib2/package.json", r#"{"name":"lib2"}"#);
    write(
        dir,
        "packages/lib2/tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":"src"}}"#,
    );
    write(
        dir,
        "packages/lib2/src/main.ts",
        "import helper from 'shared/helper';\n",
    );
    write(
        dir,
        "packages/lib2/src/shared/helper.ts",
        "export default 1;\n",
    );
    write(
        dir,
        "packages/app/src/local.ts",
        "export default function () {}\nexport const x = 1;\n",
    );
    write(
        dir,
        "packages/app/src/dir/index.ts",
        "export const b = 1;\nexport type C = string;\n",
    );
    write(dir, "packages/app/src/lazy.ts", "export {};\n");
    write(
        dir,
        "packages/app/src/comp.tsx",
        "import { x } from './local';\nexport const C = () => <div/>;\n",
    );
    write(
        dir,
        "packages/app/src/legacy.js",
        "const l = require('./local');\nmodule.exports = l;\n",
    );
}

/// Nested packages, a Rust and a TypeScript runtime cycle, a cycle that only exists through
/// type-only imports, a boundary-crossing import, and files no manifest owns.
pub fn groups_workspace(dir: &Path) {
    write(dir, "Cargo.toml", "[workspace]\nmembers = [\"outer\"]\n");
    write(
        dir,
        "outer/Cargo.toml",
        "[package]\nname = \"outer\"\nversion = \"0.1.0\"\n[dependencies]\nnested = { path = \"nested\" }\n",
    );
    write(
        dir,
        "outer/src/lib.rs",
        "pub mod a;\npub mod b;\npub mod c;\n",
    );
    write(dir, "outer/src/a.rs", "use crate::b::B;\npub struct A;\n");
    write(dir, "outer/src/b.rs", "use crate::a::A;\npub struct B;\n");
    write(dir, "outer/src/c.rs", "use crate::a::A;\nuse nested::n;\n");
    write(dir, "outer/tests/it.rs", "use outer::a::A;\n");
    write(
        dir,
        "outer/nested/Cargo.toml",
        "[package]\nname = \"nested\"\nversion = \"0.1.0\"\n",
    );
    write(dir, "outer/nested/src/lib.rs", "pub fn n() {}\n");
    write(
        dir,
        "web/package.json",
        r#"{"name":"web","types":"src/index.ts"}"#,
    );
    write(
        dir,
        "web/src/index.ts",
        "export { x } from './x';\nexport { p } from './p';\n",
    );
    write(
        dir,
        "web/src/x.ts",
        "import type { Y } from './y';\nexport const x = 1;\n",
    );
    write(
        dir,
        "web/src/y.ts",
        "import { x } from './x';\nexport type Y = typeof x;\n",
    );
    write(
        dir,
        "web/src/p.ts",
        "import { q } from './q';\nexport const p = 1;\n",
    );
    write(
        dir,
        "web/src/q.ts",
        "import { p } from './p';\nexport const q = 1;\n",
    );
    write(
        dir,
        "loose/a.ts",
        "import { b } from './b';\nexport const a = b;\n",
    );
    write(dir, "loose/b.ts", "export const b = 1;\n");
    write(dir, "loose/deep/c.ts", "import { a } from '../a';\n");
}
