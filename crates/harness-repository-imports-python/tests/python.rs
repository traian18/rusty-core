use std::sync::Arc;

use harness_repository_imports_python::{import_support, manifest_adapters, SOURCE_EXTENSIONS};
use harness_workspace::{
    import_graph::{ImportEdge, ImportKind, ImportReason, ItemStatus, Resolution},
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
    let mut extensions: Vec<String> = ["rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"]
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

fn edge<'a>(
    snapshot: &'a RepositorySnapshot,
    file: &str,
    spelling: &str,
    imported: Option<&str>,
) -> &'a ImportEdge {
    snapshot
        .imports
        .edges
        .iter()
        .find(|e| {
            e.fact.importing_file.as_path().to_str() == Some(file)
                && e.fact.spelling == spelling
                && e.fact.imported_name.as_deref() == imported
        })
        .unwrap_or_else(|| {
            panic!(
                "no edge {file} {spelling} {imported:?}: {:?}",
                snapshot
                    .imports
                    .edges
                    .iter()
                    .map(|e| (&e.fact.spelling, &e.fact.imported_name))
                    .collect::<Vec<_>>()
            )
        })
}

fn target(edge: &ImportEdge) -> Option<&str> {
    edge.resolution
        .target_file()
        .and_then(|t| t.as_path().to_str())
}

const PYPROJECT: &str = "[project]\nname = \"app\"\nversion = \"0.1\"\ndependencies = [\"requests>=2\", \"PyYAML\", \"Django[argon2]>=4 ; python_version > '3.8'\"]\n[project.optional-dependencies]\ndev = [\"pytest\"]\n";

#[tokio::test]
async fn absolute_relative_and_submodule_imports_resolve_to_files() {
    let (_dir, snap) = snapshot(&[
        ("pyproject.toml", PYPROJECT),
        ("app/__init__.py", ""),
        ("app/main.py", "import app.util\nimport app.models.user as user_model\nfrom app import util, helper\nfrom app.models import User, missing_thing\nfrom . import util as sibling\nfrom .models import user\nfrom .. import outside\nfrom app.models import *\n"),
        ("app/util.py", "def run():\n    pass\n"),
        ("app/helper.py", "x = 1\n"),
        ("app/models/__init__.py", "from .user import User\nclass Base:\n    pass\n"),
        ("app/models/user.py", "class User:\n    pass\n"),
        ("outside.py", ""),
    ])
    .await;
    let main = "app/main.py";
    assert_eq!(
        target(edge(&snap, main, "app.util", None)),
        Some("app/util.py")
    );
    assert_eq!(
        target(edge(&snap, main, "app.models.user", None)),
        Some("app/models/user.py")
    );
    assert_eq!(
        target(edge(&snap, main, "app", Some("util"))),
        Some("app/util.py"),
        "a submodule wins over an attribute of the package"
    );
    assert_eq!(
        target(edge(&snap, main, "app", Some("helper"))),
        Some("app/helper.py")
    );
    let user = edge(&snap, main, "app.models", Some("User"));
    assert_eq!(target(user), Some("app/models/__init__.py"));
    assert!(
        matches!(
            user.resolution,
            Resolution::ResolvedInternal {
                item_status: ItemStatus::Found,
                ..
            }
        ) || matches!(
            user.resolution,
            Resolution::ResolvedInternal {
                item_status: ItemStatus::NotAttempted,
                ..
            }
        )
    );
    let missing = edge(&snap, main, "app.models", Some("missing_thing"));
    assert!(
        matches!(
            missing.resolution,
            Resolution::ResolvedInternal {
                item_status: ItemStatus::NotFound,
                ..
            }
        ),
        "{:?}",
        missing.resolution
    );
    assert_eq!(
        target(edge(&snap, main, ".", Some("util"))),
        Some("app/util.py")
    );
    assert_eq!(
        target(edge(&snap, main, ".models", Some("user"))),
        Some("app/models/user.py")
    );
    assert_eq!(
        edge(&snap, main, "..", Some("outside")).rule,
        "python.relative_import"
    );
    assert_eq!(
        target(edge(&snap, main, "..", Some("outside"))),
        Some("outside.py")
    );
    let star = edge(&snap, main, "app.models", Some("*"));
    assert!(star.fact.glob);
    assert_eq!(target(star), Some("app/models/__init__.py"));
    assert_eq!(
        edge(&snap, main, "app.util", None).rule,
        "python.absolute_module"
    );
}

#[tokio::test]
async fn external_imports_are_stdlib_declared_or_undeclared() {
    let (_dir, snap) = snapshot(&[
        ("pyproject.toml", PYPROJECT),
        ("app/__init__.py", ""),
        ("app/net.py", "import os\nimport json.decoder\nimport requests\nimport yaml\nfrom django.db import models\nimport numpy\nimport app.nothing_here\nfrom typing import TYPE_CHECKING\nif TYPE_CHECKING:\n    from pytest import Item\n"),
    ])
    .await;
    let file = "app/net.py";
    assert!(
        matches!(&edge(&snap, file, "os", None).resolution, Resolution::DeclaredExternal { package } if package == "stdlib:os")
    );
    assert_eq!(
        edge(&snap, file, "json.decoder", None).rule,
        "python.stdlib"
    );
    assert!(
        matches!(&edge(&snap, file, "requests", None).resolution, Resolution::DeclaredExternal { package } if package == "requests")
    );
    assert!(
        matches!(&edge(&snap, file, "yaml", None).resolution, Resolution::DeclaredExternal { package } if package == "pyyaml"),
        "alias table"
    );
    assert!(
        matches!(&edge(&snap, file, "django.db", Some("models")).resolution, Resolution::DeclaredExternal { package } if package == "django")
    );
    assert!(matches!(
        edge(&snap, file, "numpy", None).resolution,
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal
        }
    ));
    assert!(
        matches!(
            edge(&snap, file, "app.nothing_here", None).resolution,
            Resolution::Unresolved {
                reason: ImportReason::PathNotFound
            }
        ),
        "first-party name, missing module"
    );
    let pytest = edge(&snap, file, "pytest", Some("Item"));
    assert_eq!(
        pytest.fact.kind,
        ImportKind::TypeOnly,
        "inside `if TYPE_CHECKING:`"
    );
    assert!(
        matches!(&pytest.resolution, Resolution::DeclaredExternal { package } if package == "pytest"),
        "dev dependency counts as declared"
    );
}

#[tokio::test]
async fn dynamic_imports_and_src_layouts_and_ambiguity_are_handled_explicitly() {
    let (_dir, snap) = snapshot(&[
        ("svc/pyproject.toml", "[project]\nname = \"svc\"\n"),
        ("svc/src/svcpkg/__init__.py", ""),
        ("svc/src/svcpkg/core.py", "import importlib\nmod = importlib.import_module('svcpkg.plugins')\nname = 'x'\nother = importlib.import_module(name)\nimport svcpkg.plugins\n"),
        ("svc/src/svcpkg/plugins.py", ""),
        ("one/pyproject.toml", "[project]\nname = \"one\"\n"),
        ("one/common.py", ""),
        ("two/pyproject.toml", "[project]\nname = \"two\"\n"),
        ("two/common.py", ""),
        ("three/pyproject.toml", "[project]\nname = \"three\"\n"),
        ("three/user.py", "import common\n"),
    ])
    .await;
    let core = "svc/src/svcpkg/core.py";
    let static_import = snap
        .imports
        .edges
        .iter()
        .find(|e| {
            e.fact.importing_file.as_path().to_str() == Some(core)
                && e.fact.spelling == "svcpkg.plugins"
                && e.fact.form == harness_workspace::import_graph::ImportForm::Import
        })
        .unwrap();
    assert_eq!(
        target(static_import),
        Some("svc/src/svcpkg/plugins.py"),
        "src layout"
    );
    let dynamic: Vec<_> = snap
        .imports
        .edges
        .iter()
        .filter(|e| e.fact.form == harness_workspace::import_graph::ImportForm::DynamicImport)
        .collect();
    assert_eq!(dynamic.len(), 2);
    assert!(dynamic
        .iter()
        .any(|e| e.fact.literal_specifier && target(e) == Some("svc/src/svcpkg/plugins.py")));
    assert!(dynamic.iter().any(|e| !e.fact.literal_specifier
        && matches!(
            e.resolution,
            Resolution::DynamicOrUnsupported {
                reason: ImportReason::NonLiteralSpecifier
            }
        )));
    let ambiguous = edge(&snap, "three/user.py", "common", None);
    match &ambiguous.resolution {
        Resolution::Possible { candidates, .. } => {
            let names: Vec<_> = candidates
                .iter()
                .map(|c| c.as_path().to_str().unwrap())
                .collect();
            assert_eq!(names, ["one/common.py", "two/common.py"]);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn project_files_become_packages_with_declared_and_dev_dependencies() {
    let (_dir, snap) = snapshot(&[
        ("pyproject.toml", PYPROJECT),
        ("poetry/pyproject.toml", "[tool.poetry]\nname = \"poet\"\n[tool.poetry.dependencies]\npython = \"^3.11\"\nflask = \"*\"\n[tool.poetry.group.dev.dependencies]\nblack = \"*\"\n"),
        ("cfg/setup.cfg", "[metadata]\nname = cfgpkg\n[options]\ninstall_requires =\n    click>=8\n    rich\n[options.extras_require]\ntest = pytest\n"),
        ("plain/requirements.txt", "# comment\nflask==2.0\n-r other.txt\nrequests[security]>=2\ngit+https://x/y.git#egg=thing\n"),
        ("plain/requirements-dev.txt", "pytest\n"),
        ("pipenv/Pipfile", "[packages]\nFlask = \"*\"\n[dev-packages]\npytest = \"*\"\n"),
        ("tools/pyproject.toml", "[tool.ruff]\nline-length = 100\n"),
        ("tools/requirements.txt", "ruff\n"),
    ])
    .await;
    let package = |name: &str| {
        snap.graph
            .packages
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "no package {name}: {:?}",
                    snap.graph
                        .packages
                        .iter()
                        .map(|p| &p.name)
                        .collect::<Vec<_>>()
                )
            })
    };
    let app = package("app");
    assert_eq!(
        app.facts.declared_dependencies,
        ["django", "pyyaml", "requests", "pytest"]
    );
    assert_eq!(app.facts.dev_dependencies, ["pytest"]);
    let poet = package("poet");
    assert_eq!(poet.facts.declared_dependencies, ["flask", "black"]);
    assert_eq!(poet.facts.dev_dependencies, ["black"]);
    let cfg = package("cfgpkg");
    assert_eq!(cfg.facts.declared_dependencies, ["click", "rich", "pytest"]);
    assert_eq!(cfg.facts.dev_dependencies, ["pytest"]);
    let plain = package("plain");
    assert_eq!(
        plain.facts.declared_dependencies,
        ["flask", "requests", "thing"]
    );
    assert_eq!(
        package("pipenv").facts.declared_dependencies,
        ["flask", "pytest"]
    );
    // A pyproject with only tool configuration is not a package; the requirements file beside it is.
    assert!(snap
        .graph
        .packages
        .iter()
        .all(|p| p.manifest.as_path().to_str() != Some("tools/pyproject.toml")));
    assert!(snap
        .graph
        .packages
        .iter()
        .any(|p| p.manifest.as_path().to_str() == Some("tools/requirements.txt")));
    assert!(
        !snap
            .graph
            .packages
            .iter()
            .any(|p| p.manifest.as_path().to_str() == Some("plain/requirements-dev.txt")),
        "dev requirements beside the main file add no package"
    );
}

#[tokio::test]
async fn files_belong_to_the_nearest_python_package_and_malformed_files_are_reported() {
    let (_dir, snap) = snapshot(&[
        ("pyproject.toml", "[project]\nname = \"root\"\n"),
        ("sub/pyproject.toml", "[project\nname = broken"),
        ("sub/mod.py", "import os\n"),
        ("top.py", "import os\n"),
        ("bad.py", "def (:\n"),
    ])
    .await;
    let file = |path: &str| {
        snap.imports
            .files
            .iter()
            .find(|f| f.path.as_path().to_str() == Some(path))
            .unwrap()
    };
    assert_eq!(
        file("top.py").package.as_ref().map(|p| p.0.as_str()),
        Some("pyproject.toml:root")
    );
    assert_eq!(
        file("sub/mod.py").package.as_ref().map(|p| p.0.as_str()),
        Some("pyproject.toml:root"),
        "a malformed manifest names no package"
    );
    assert!(snap
        .graph
        .diagnostics
        .iter()
        .any(|d| d.message.contains("not valid TOML")));
    assert!(
        snap.imports
            .diagnostics
            .iter()
            .any(|d| d.message.contains("syntax errors")),
        "{:?}",
        snap.imports.diagnostics
    );
    let capability = &Arc::new(import_support()).resolver.unsupported();
    assert!(capability.iter().any(|u| u.contains("sys.path")));
}
