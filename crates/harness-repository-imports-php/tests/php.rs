use harness_repository_imports_php::{import_support, manifest_adapters, SOURCE_EXTENSIONS};
use harness_workspace::{
    import_graph::{ImportEdge, ImportForm, ImportReason, Resolution},
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

const COMPOSER: &str = r#"{
  "name": "acme/shop",
  "require": { "php": "^8.2", "ext-json": "*", "laravel/framework": "^11", "monolog/monolog": "^3", "guzzlehttp/guzzle": "^7" },
  "require-dev": { "phpunit/phpunit": "^10" },
  "autoload": { "psr-4": { "App\\": "app/" } },
  "autoload-dev": { "psr-4": { "Tests\\": ["tests/", "tests/Feature/"] } }
}"#;

#[tokio::test]
async fn composer_json_names_the_package_and_records_the_psr4_map() {
    let (_dir, snap) = snapshot(&[
        ("composer.json", COMPOSER),
        ("app/A.php", "<?php\nnamespace App;\nclass A {}\n"),
    ])
    .await;
    let package = &snap.graph.packages[0];
    assert_eq!(package.name, "acme/shop");
    assert_eq!(
        package.facts.declared_dependencies,
        [
            "guzzlehttp/guzzle",
            "laravel/framework",
            "monolog/monolog",
            "phpunit/phpunit"
        ],
        "php and ext-* are not packages"
    );
    assert_eq!(package.facts.dev_dependencies, ["phpunit/phpunit"]);
    assert_eq!(
        package.facts.inferred_facts,
        [
            "psr4:App\\=app/",
            "psr4:Tests\\=tests/",
            "psr4:Tests\\=tests/Feature/"
        ]
    );
}

#[tokio::test]
async fn use_statements_resolve_to_declaring_files_groups_aliases_and_packages() {
    let (_dir, snap) = snapshot(&[
        ("composer.json", COMPOSER),
        ("app/Http/UserController.php", "<?php\nnamespace App\\Http;\n\nuse App\\Models\\User;\nuse App\\Services\\{Mailer, Logger as Log};\nuse function App\\helper;\nuse Illuminate\\Support\\Facades\\Route;\nuse Monolog\\Logger;\nuse GuzzleHttp\\Client;\nuse Exception;\nuse Acme\\Unknown\\Thing;\nuse App\\Models\\Missing;\n\nclass UserController {}\n"),
        ("app/Models/User.php", "<?php\nnamespace App\\Models;\n\nclass User {}\n"),
        ("app/Services/Mailer.php", "<?php\nnamespace App\\Services;\n\ninterface Mailer {}\n"),
        ("app/Services/Logger.php", "<?php\nnamespace App\\Services;\n\ntrait Logger {}\n"),
    ])
    .await;
    let file = "app/Http/UserController.php";
    assert_eq!(
        edge(&snap, file, "App\\Models\\User")
            .resolution
            .target_file()
            .unwrap()
            .as_path()
            .to_str(),
        Some("app/Models/User.php")
    );
    assert_eq!(
        edge(&snap, file, "App\\Models\\User").rule,
        "php.class_by_namespace_declaration"
    );
    assert_eq!(
        edge(&snap, file, "App\\Services\\Mailer")
            .resolution
            .target_file()
            .unwrap()
            .as_path()
            .to_str(),
        Some("app/Services/Mailer.php")
    );
    let aliased = edge(&snap, file, "App\\Services\\Logger");
    assert_eq!(aliased.fact.alias.as_deref(), Some("Log"));
    assert_eq!(aliased.fact.imported_name.as_deref(), Some("Logger"));
    assert_eq!(
        aliased.resolution.target_file().unwrap().as_path().to_str(),
        Some("app/Services/Logger.php")
    );
    assert!(edge(&snap, file, "App\\helper")
        .fact
        .scope
        .contains(&"function".to_owned()));
    assert!(
        matches!(&edge(&snap, file, "Illuminate\\Support\\Facades\\Route").resolution, Resolution::DeclaredExternal { package } if package == "laravel/framework"),
        "namespace table"
    );
    assert!(
        matches!(&edge(&snap, file, "Monolog\\Logger").resolution, Resolution::DeclaredExternal { package } if package == "monolog/monolog"),
        "vendor name"
    );
    assert!(
        matches!(&edge(&snap, file, "GuzzleHttp\\Client").resolution, Resolution::DeclaredExternal { package } if package == "guzzlehttp/guzzle")
    );
    assert!(
        matches!(&edge(&snap, file, "Exception").resolution, Resolution::DeclaredExternal { package } if package == "php-builtin")
    );
    assert!(matches!(
        edge(&snap, file, "Acme\\Unknown\\Thing").resolution,
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal
        }
    ));
    assert!(matches!(
        edge(&snap, file, "App\\Models\\Missing").resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));
}

#[tokio::test]
async fn literal_requires_are_followed_and_computed_ones_are_reported() {
    let (_dir, snap) = snapshot(&[
        ("composer.json", COMPOSER),
        ("public/index.php", "<?php\nrequire __DIR__ . '/../vendor/autoload.php';\nrequire_once 'helpers.php';\ninclude dirname(__DIR__) . '/bootstrap/app.php';\nrequire 'missing.php';\n$name = 'x';\nrequire $name . '.php';\n"),
        ("public/helpers.php", "<?php\n"),
        ("bootstrap/app.php", "<?php\n"),
    ])
    .await;
    let index = "public/index.php";
    assert!(
        matches!(&edge(&snap, index, "/../vendor/autoload.php").resolution, Resolution::DeclaredExternal { package } if package == "composer-autoload")
    );
    assert_eq!(
        edge(&snap, index, "helpers.php")
            .resolution
            .target_file()
            .unwrap()
            .as_path()
            .to_str(),
        Some("public/helpers.php")
    );
    assert_eq!(
        edge(&snap, index, "../bootstrap/app.php")
            .resolution
            .target_file()
            .unwrap()
            .as_path()
            .to_str(),
        Some("bootstrap/app.php")
    );
    assert!(matches!(
        edge(&snap, index, "missing.php").resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));
    let dynamic = snap
        .imports
        .edges
        .iter()
        .find(|e| !e.fact.literal_specifier)
        .unwrap();
    assert_eq!(dynamic.fact.form, ImportForm::Require);
    assert!(matches!(
        dynamic.resolution,
        Resolution::DynamicOrUnsupported {
            reason: ImportReason::NonLiteralSpecifier
        }
    ));
}

#[tokio::test]
async fn invalid_composer_json_is_reported_and_files_still_index() {
    let (_dir, snap) = snapshot(&[
        ("composer.json", "{ nope"),
        ("a.php", "<?php\nnamespace A;\nclass X {}\n"),
    ])
    .await;
    assert!(snap.graph.packages.is_empty());
    assert!(snap
        .graph
        .diagnostics
        .iter()
        .any(|d| d.message.contains("not valid JSON")));
    assert!(snap
        .imports
        .files
        .iter()
        .any(|f| f.path.as_path().to_str() == Some("a.php")));
}
