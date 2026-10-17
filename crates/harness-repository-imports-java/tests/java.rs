use harness_repository_imports_java::{import_support, manifest_adapters, SOURCE_EXTENSIONS};
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

fn target(edge: &ImportEdge) -> Option<&str> {
    edge.resolution
        .target_file()
        .and_then(|t| t.as_path().to_str())
}

const POM: &str = "<?xml version=\"1.0\"?>\n<project>\n  <!-- <artifactId>commented</artifactId> -->\n  <parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId></parent>\n  <groupId>com.acme</groupId>\n  <artifactId>shop</artifactId>\n  <dependencies>\n    <dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId></dependency>\n    <dependency><groupId>com.google.guava</groupId><artifactId>guava</artifactId><version>33</version></dependency>\n    <dependency><groupId>org.junit.jupiter</groupId><artifactId>junit-jupiter</artifactId><scope>test</scope></dependency>\n  </dependencies>\n</project>\n";

#[tokio::test]
async fn pom_xml_names_the_package_and_marks_test_scope_as_development() {
    let (_dir, snap) = snapshot(&[
        ("pom.xml", POM),
        (
            "src/main/java/com/acme/App.java",
            "package com.acme;\nclass App {}\n",
        ),
    ])
    .await;
    let package = &snap.graph.packages[0];
    assert_eq!(package.name, "shop");
    assert_eq!(
        package.facts.declared_dependencies,
        [
            "com.google.guava:guava",
            "org.junit.jupiter:junit-jupiter",
            "org.springframework.boot:spring-boot-starter-web"
        ]
    );
    assert_eq!(
        package.facts.dev_dependencies,
        ["org.junit.jupiter:junit-jupiter"]
    );
}

#[tokio::test]
async fn imports_resolve_by_package_declaration_wildcard_nested_static_stdlib_and_dependencies() {
    let (_dir, snap) = snapshot(&[
        ("pom.xml", POM),
        ("src/main/java/com/acme/web/UserController.java", "package com.acme.web;\n\nimport com.acme.model.User;\nimport com.acme.model.*;\nimport com.acme.model.User.Builder;\nimport com.acme.util.Strings;\nimport static com.acme.util.Strings.join;\nimport java.util.List;\nimport org.springframework.web.bind.annotation.GetMapping;\nimport com.google.common.collect.ImmutableList;\nimport org.apache.commons.lang3.StringUtils;\nimport com.acme.model.Missing;\n\npublic class UserController {}\n"),
        ("src/main/java/com/acme/model/User.java", "package com.acme.model;\npublic class User { public static class Builder {} }\n"),
        ("src/main/java/com/acme/model/Role.java", "package com.acme.model;\npublic enum Role { ADMIN }\n"),
        ("src/main/java/com/acme/util/Strings.java", "package com.acme.util;\npublic final class Strings { public static String join() { return \"\"; } }\n"),
    ])
    .await;
    let file = "src/main/java/com/acme/web/UserController.java";
    assert_eq!(
        target(edge(&snap, file, "com.acme.model.User")),
        Some("src/main/java/com/acme/model/User.java")
    );
    assert_eq!(
        edge(&snap, file, "com.acme.model.User").rule,
        "java.type_by_package_declaration"
    );
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
    assert_eq!(wildcard.rule, "java.package_wildcard");
    assert_eq!(
        edge(&snap, file, "com.acme.model.User.Builder").rule,
        "java.nested_type"
    );
    assert_eq!(
        target(edge(&snap, file, "com.acme.model.User.Builder")),
        Some("src/main/java/com/acme/model/User.java")
    );
    let static_import = snap
        .imports
        .edges
        .iter()
        .find(|e| e.fact.imported_name.as_deref() == Some("join"))
        .unwrap();
    assert_eq!(static_import.fact.spelling, "com.acme.util.Strings");
    assert!(static_import.fact.scope.contains(&"static".to_owned()));
    assert_eq!(
        target(static_import),
        Some("src/main/java/com/acme/util/Strings.java")
    );
    assert!(
        matches!(&edge(&snap, file, "java.util.List").resolution, Resolution::DeclaredExternal { package } if package == "stdlib:java.util")
    );
    assert!(
        matches!(&edge(&snap, file, "org.springframework.web.bind.annotation.GetMapping").resolution, Resolution::DeclaredExternal { package } if package == "org.springframework.boot:spring-boot-starter-web"),
        "group id prefix"
    );
    assert!(
        matches!(&edge(&snap, file, "com.google.common.collect.ImmutableList").resolution, Resolution::DeclaredExternal { package } if package == "com.google.guava:guava"),
        "first two segments shared"
    );
    assert!(matches!(
        edge(&snap, file, "org.apache.commons.lang3.StringUtils").resolution,
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

#[tokio::test]
async fn gradle_files_become_packages_with_configurations_and_a_pom_wins() {
    let (_dir, snap) = snapshot(&[
        ("app/build.gradle", "plugins { id 'java' }\ndependencies {\n    implementation 'org.slf4j:slf4j-api:2.0.9'\n    api(\"com.google.guava:guava:33.0\")\n    implementation project(':core')\n    testImplementation 'org.junit.jupiter:junit-jupiter:5.10.0'\n    annotationProcessor 'org.projectlombok:lombok:1.18'\n    implementation platform('org.springframework.boot:spring-boot-dependencies:3')\n}\n"),
        ("app/settings.gradle", "rootProject.name = 'my-app'\n"),
        ("kts/build.gradle.kts", "dependencies {\n    implementation(\"io.ktor:ktor-server-core:2.3.0\")\n    testImplementation(\"junit:junit:4.13\")\n}\n"),
        ("both/pom.xml", "<project><artifactId>both</artifactId></project>"),
        ("both/build.gradle", "dependencies { implementation 'x:y:1' }\n"),
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
    let app = package("my-app");
    assert_eq!(
        app.facts.declared_dependencies,
        [
            "com.google.guava:guava",
            "org.junit.jupiter:junit-jupiter",
            "org.projectlombok:lombok",
            "org.slf4j:slf4j-api",
            "project:core"
        ]
    );
    assert_eq!(
        app.facts.dev_dependencies,
        [
            "org.junit.jupiter:junit-jupiter",
            "org.projectlombok:lombok"
        ]
    );
    assert_eq!(
        package("kts").facts.declared_dependencies,
        ["io.ktor:ktor-server-core", "junit:junit"]
    );
    assert_eq!(
        snap.graph
            .packages
            .iter()
            .filter(|p| p.manifest.directory().to_str() == Some("both"))
            .count(),
        1,
        "the pom is the project"
    );
}

#[tokio::test]
async fn malformed_poms_and_types_in_files_without_a_package_are_reported_not_guessed() {
    let (_dir, snap) = snapshot(&[
        ("pom.xml", "<project><name>no artifact</name></project>"),
        ("Loose.java", "import java.util.List;\nclass Loose {}\n"),
        ("src/Other.java", "import Loose;\nclass Other {}\n"),
    ])
    .await;
    assert!(snap.graph.packages.is_empty());
    assert!(snap
        .graph
        .diagnostics
        .iter()
        .any(|d| d.message.contains("no artifactId")));
    // A type in the default package cannot be imported by name, so the import stays unresolved.
    let other = edge(&snap, "src/Other.java", "Loose");
    assert!(
        matches!(other.resolution, Resolution::Unresolved { .. }),
        "{:?}",
        other.resolution
    );
}
