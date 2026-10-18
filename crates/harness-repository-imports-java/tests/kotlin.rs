use harness_repository_imports_java::{import_support, manifest_adapters, SOURCE_EXTENSIONS};
use harness_workspace::{
    import_graph::{ImportEdge, ImportReason, LanguageProfile, Resolution},
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
    let service = RepositoryGraphBuilder::new(std::fs::canonicalize(dir.path()).unwrap())
        .unwrap()
        .manifest_adapters(adapters)
        .source_extensions(SOURCE_EXTENSIONS.iter().map(|e| e.to_string()).collect())
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
        .unwrap_or_else(|| panic!("no edge {file} {spelling}"))
}

fn target(edge: &ImportEdge) -> Option<&str> {
    edge.resolution
        .target_file()
        .and_then(|t| t.as_path().to_str())
}

const GRADLE: &str = "plugins { kotlin(\"jvm\") version \"2.0.0\" }\ndependencies {\n    implementation(\"org.jetbrains.kotlinx:kotlinx-coroutines-core:1.8.0\")\n    implementation(\"io.ktor:ktor-server-core:2.3.0\")\n    ksp(\"com.google.dagger:dagger-compiler:2.51\")\n}\n";
const BASE: &str = "src/main/kotlin/com/acme";

fn files() -> Vec<(String, &'static str)> {
    vec![
        ("build.gradle.kts".into(), GRADLE),
        ("settings.gradle.kts".into(), "rootProject.name = \"shop\"\n"),
        (format!("{BASE}/app/Service.kt"), "package com.acme.app\n\nimport com.acme.model.User\nimport com.acme.model.*\nimport com.acme.util.format\nimport com.acme.util.Strings as S\nimport com.acme.legacy.LegacyClient\nimport kotlin.collections.List\nimport java.time.Instant\nimport kotlinx.coroutines.launch\nimport io.ktor.server.application.Application\nimport org.slf4j.Logger\nimport com.acme.model.Missing\n\nclass Service(private val user: User) {\n    fun run(): String {\n        return format(user.name)\n    }\n}\n"),
        (format!("{BASE}/model/User.kt"), "package com.acme.model\n\ndata class User(val name: String)\n\nprivate fun hidden() = 1\n"),
        (format!("{BASE}/model/Role.kt"), "package com.acme.model\n\nenum class Role {\n    ADMIN\n}\n"),
        (format!("{BASE}/util/Format.kt"), "package com.acme.util\n\nfun format(value: String): String = value.trim()\n\nobject Strings {\n    fun join() = \"\"\n}\n\ntypealias Text = String\n"),
        ("src/main/java/com/acme/legacy/LegacyClient.java".into(), "package com.acme.legacy;\n\nimport com.acme.model.User;\n\npublic class LegacyClient {\n    public User load() { return null; }\n}\n"),
    ]
}

#[tokio::test]
async fn kotlin_files_get_the_kotlin_profile_and_their_top_level_declarations() {
    let owned = files();
    let borrowed: Vec<(&str, &str)> = owned.iter().map(|(p, t)| (p.as_str(), *t)).collect();
    let (_dir, snap) = snapshot(&borrowed).await;
    let profiles: Vec<(String, LanguageProfile)> = snap
        .imports
        .edges
        .iter()
        .map(|e| {
            (
                e.fact.importing_file.as_path().display().to_string(),
                e.fact.profile,
            )
        })
        .collect();
    assert!(profiles
        .iter()
        .any(|(p, profile)| p.ends_with("Service.kt") && *profile == LanguageProfile::Kotlin));
    assert!(profiles
        .iter()
        .any(|(p, profile)| p.ends_with("LegacyClient.java") && *profile == LanguageProfile::Java));
    assert!(
        !profiles.iter().any(|(p, _)| p.ends_with(".gradle.kts")),
        "Gradle scripts are not sources"
    );
    let package = &snap.graph.packages[0];
    assert_eq!(package.name, "shop");
    assert_eq!(
        package.facts.declared_dependencies,
        [
            "com.google.dagger:dagger-compiler",
            "io.ktor:ktor-server-core",
            "org.jetbrains.kotlinx:kotlinx-coroutines-core"
        ]
    );
    assert_eq!(
        package.facts.dev_dependencies,
        ["com.google.dagger:dagger-compiler"]
    );
}

#[tokio::test]
async fn kotlin_imports_resolve_to_kotlin_and_java_files_and_java_imports_resolve_to_kotlin() {
    let owned = files();
    let borrowed: Vec<(&str, &str)> = owned.iter().map(|(p, t)| (p.as_str(), *t)).collect();
    let (_dir, snap) = snapshot(&borrowed).await;
    let file = format!("{BASE}/app/Service.kt");
    let file = file.as_str();
    let user = edge(&snap, file, "com.acme.model.User");
    assert_eq!(target(user), Some("src/main/kotlin/com/acme/model/User.kt"));
    assert_eq!(user.rule, "java.type_by_package_declaration");
    let wildcard = snap
        .imports
        .edges
        .iter()
        .find(|e| e.fact.glob && e.fact.spelling == "com.acme.model")
        .unwrap();
    match &wildcard.resolution {
        Resolution::Possible { candidates, .. } => assert_eq!(candidates.len(), 2),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        target(edge(&snap, file, "com.acme.util.format")),
        Some("src/main/kotlin/com/acme/util/Format.kt"),
        "a top-level function is importable"
    );
    let aliased = edge(&snap, file, "com.acme.util.Strings");
    assert_eq!(aliased.fact.alias.as_deref(), Some("S"));
    assert_eq!(
        target(aliased),
        Some("src/main/kotlin/com/acme/util/Format.kt")
    );
    assert_eq!(
        target(edge(&snap, file, "com.acme.legacy.LegacyClient")),
        Some("src/main/java/com/acme/legacy/LegacyClient.java"),
        "Kotlin imports a Java class"
    );
    assert_eq!(
        target(edge(
            &snap,
            "src/main/java/com/acme/legacy/LegacyClient.java",
            "com.acme.model.User"
        )),
        Some("src/main/kotlin/com/acme/model/User.kt"),
        "Java imports a Kotlin class"
    );
    let stdlib = edge(&snap, file, "kotlin.collections.List");
    assert_eq!(stdlib.rule, "kotlin.stdlib");
    assert!(
        matches!(&stdlib.resolution, Resolution::DeclaredExternal { package } if package == "stdlib:kotlin.collections")
    );
    assert!(
        matches!(&edge(&snap, file, "java.time.Instant").resolution, Resolution::DeclaredExternal { package } if package == "stdlib:java.time")
    );
    assert!(
        matches!(&edge(&snap, file, "kotlinx.coroutines.launch").resolution, Resolution::DeclaredExternal { package } if package == "org.jetbrains.kotlinx:kotlinx-coroutines-core"),
        "kotlinx maps to the org.jetbrains.kotlinx group"
    );
    assert!(
        matches!(&edge(&snap, file, "io.ktor.server.application.Application").resolution, Resolution::DeclaredExternal { package } if package == "io.ktor:ktor-server-core")
    );
    assert!(matches!(
        edge(&snap, file, "org.slf4j.Logger").resolution,
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal
        }
    ));
    assert!(matches!(
        edge(&snap, file, "com.acme.model.Missing").resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));
}
