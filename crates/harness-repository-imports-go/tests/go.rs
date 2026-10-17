use harness_repository_imports_go::{import_support, manifest_adapters, SOURCE_EXTENSIONS};
use harness_workspace::{
    import_graph::{ImportEdge, ImportReason, Resolution},
    repository_intelligence::{
        default_manifest_adapters, RepositoryGraph, RepositoryGraphBuilder, RepositorySnapshot,
    },
};
use tokio_util::sync::CancellationToken;

async fn snapshot(files: &[(&str, &str)]) -> (tempfile::TempDir, RepositorySnapshot) {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let mut adapters = default_manifest_adapters();
    adapters.extend(manifest_adapters());
    let mut extensions: Vec<String> = ["rs", "ts", "tsx", "js", "jsx"]
        .iter()
        .map(|e| e.to_string())
        .collect();
    extensions.extend(SOURCE_EXTENSIONS.iter().map(|e| e.to_string()));
    let service = RepositoryGraphBuilder::new(std::fs::canonicalize(dir.path()).unwrap())
        .unwrap()
        .manifest_adapters(adapters)
        .source_extensions(extensions)
        .import_support(import_support())
        .unwrap()
        .build();
    let snapshot = service.refresh(CancellationToken::new()).await.unwrap();
    (dir, snapshot)
}

fn edge<'a>(snapshot: &'a RepositorySnapshot, file: &str, spelling: &str) -> &'a ImportEdge {
    snapshot
        .imports
        .edges
        .iter()
        .find(|e| {
            e.fact.importing_file.as_path().to_str() == Some(file) && e.fact.spelling == spelling
        })
        .unwrap_or_else(|| {
            panic!(
                "no edge {file} {spelling}: {:?}",
                snapshot
                    .imports
                    .edges
                    .iter()
                    .map(|e| &e.fact.spelling)
                    .collect::<Vec<_>>()
            )
        })
}

const GO_MOD: &str = "module example.com/shop\n\ngo 1.22\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.1\n\tgolang.org/x/text v0.14.0 // indirect\n)\n\nrequire github.com/stretchr/testify v1.9.0\n";

#[tokio::test]
async fn go_mod_names_the_package_and_separates_indirect_requirements() {
    let (_dir, snap) = snapshot(&[
        ("go.mod", GO_MOD),
        ("main.go", "package main\nfunc main() {}\n"),
    ])
    .await;
    let package = &snap.graph.packages[0];
    assert_eq!(package.name, "example.com/shop");
    assert_eq!(
        package.facts.declared_dependencies,
        [
            "github.com/gin-gonic/gin",
            "github.com/stretchr/testify",
            "golang.org/x/text"
        ]
    );
    assert_eq!(
        package.facts.dev_dependencies,
        ["golang.org/x/text"],
        "indirect requirements are not declared by this module's code"
    );
}

#[tokio::test]
async fn imports_resolve_to_package_directories_stdlib_and_declared_modules() {
    let (_dir, snap) = snapshot(&[
        ("go.mod", GO_MOD),
        ("cmd/app/main.go", "package main\n\nimport \"fmt\"\nimport (\n\t\"net/http\"\n\t\"example.com/shop/internal/store\"\n\tmodels \"example.com/shop/pkg/models\"\n\t_ \"example.com/shop/internal/audit\"\n\t. \"example.com/shop/internal/util\"\n\t\"github.com/gin-gonic/gin\"\n\t\"github.com/gin-gonic/gin/binding\"\n\t\"github.com/unknown/lib\"\n\t\"example.com/shop/internal/missing\"\n\t\"C\"\n)\n\nfunc main() { fmt.Println() }\n"),
        ("internal/store/store.go", "package store\nfunc Open() {}\n"),
        ("internal/store/helpers.go", "package store\n"),
        ("internal/store/store_test.go", "package store\n"),
        ("internal/audit/log.go", "package audit\n"),
        ("internal/audit/doc.go", "package audit\n"),
        ("internal/util/util.go", "package util\n"),
        ("pkg/models/user.go", "package models\ntype User struct{}\nfunc New() {}\nvar secret = 1\n"),
    ])
    .await;
    let main = "cmd/app/main.go";
    let target = |spelling: &str| {
        edge(&snap, main, spelling)
            .resolution
            .target_file()
            .map(|t| t.as_path().to_str().unwrap().to_owned())
    };
    assert_eq!(
        target("example.com/shop/internal/store").as_deref(),
        Some("internal/store/store.go"),
        "the file named like the directory leads"
    );
    assert_eq!(
        target("example.com/shop/internal/audit").as_deref(),
        Some("internal/audit/doc.go"),
        "then doc.go"
    );
    assert_eq!(
        target("example.com/shop/pkg/models").as_deref(),
        Some("pkg/models/user.go"),
        "then the first file"
    );
    assert_eq!(
        edge(&snap, main, "example.com/shop/internal/store").evidence[0]
            .as_path()
            .to_str(),
        Some("go.mod")
    );
    let models = edge(&snap, main, "example.com/shop/pkg/models");
    assert_eq!(models.fact.alias.as_deref(), Some("models"));
    assert!(
        edge(&snap, main, "example.com/shop/internal/util")
            .fact
            .glob,
        "dot import"
    );
    assert_eq!(
        edge(&snap, main, "example.com/shop/internal/audit")
            .fact
            .alias
            .as_deref(),
        Some("_")
    );
    assert!(
        matches!(&edge(&snap, main, "fmt").resolution, Resolution::DeclaredExternal { package } if package == "stdlib:fmt")
    );
    assert_eq!(edge(&snap, main, "net/http").rule, "go.stdlib");
    assert!(
        matches!(&edge(&snap, main, "github.com/gin-gonic/gin").resolution, Resolution::DeclaredExternal { package } if package == "github.com/gin-gonic/gin")
    );
    assert!(
        matches!(&edge(&snap, main, "github.com/gin-gonic/gin/binding").resolution, Resolution::DeclaredExternal { package } if package == "github.com/gin-gonic/gin"),
        "subpackage of a declared module"
    );
    assert!(matches!(
        edge(&snap, main, "github.com/unknown/lib").resolution,
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal
        }
    ));
    assert!(matches!(
        edge(&snap, main, "example.com/shop/internal/missing").resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));
    assert!(
        snap.imports
            .diagnostics
            .iter()
            .any(|d| d.message.contains("cgo")),
        "{:?}",
        snap.imports.diagnostics
    );
    assert!(
        snap.imports
            .files
            .iter()
            .any(|f| f.path.as_path().to_str() == Some("internal/store/store_test.go")),
        "test files are indexed but never a package's lead file"
    );
    assert!(snap.imports.files.iter().all(|f| f.package.is_some()));
}

#[tokio::test]
async fn several_modules_resolve_against_their_own_roots_and_vendor_is_ignored() {
    let (_dir, snap) = snapshot(&[
        ("a/go.mod", "module example.com/a\n"),
        (
            "a/a.go",
            "package a\nimport \"example.com/b/lib\"\nimport \"example.com/a/inner\"\n",
        ),
        ("a/inner/inner.go", "package inner\n"),
        ("b/go.mod", "module example.com/b\n"),
        ("b/lib/lib.go", "package lib\n"),
        ("a/vendor/github.com/x/y/y.go", "package y\n"),
    ])
    .await;
    let from_a = "a/a.go";
    assert_eq!(
        edge(&snap, from_a, "example.com/b/lib")
            .resolution
            .target_file()
            .unwrap()
            .as_path()
            .to_str(),
        Some("b/lib/lib.go")
    );
    assert_eq!(
        edge(&snap, from_a, "example.com/a/inner")
            .resolution
            .target_file()
            .unwrap()
            .as_path()
            .to_str(),
        Some("a/inner/inner.go")
    );
    assert!(snap.imports.files.iter().all(|f| !f
        .path
        .as_path()
        .to_str()
        .unwrap()
        .contains("vendor")));
    let from_b = snap
        .imports
        .files
        .iter()
        .find(|f| f.path.as_path().to_str() == Some("b/lib/lib.go"))
        .unwrap();
    assert_eq!(from_b.package.as_ref().unwrap().0, "b/go.mod:example.com/b");
}

#[tokio::test]
async fn a_go_mod_without_a_module_line_is_reported() {
    let (_dir, snap) = snapshot(&[("go.mod", "go 1.22\n"), ("x.go", "package x\n")]).await;
    assert!(snap.graph.packages.is_empty());
    assert!(snap
        .graph
        .diagnostics
        .iter()
        .any(|d| d.message.contains("no module line")));
}
