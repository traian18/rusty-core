use harness_repository_imports_native::{import_support, manifest_adapters, source_extensions};
use harness_workspace::{
    import_graph::{ImportEdge, ImportForm, ImportReason, LanguageProfile, Resolution},
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
    extensions.extend(source_extensions().iter().map(|e| e.to_string()));
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

const CMAKE: &str = "cmake_minimum_required(VERSION 3.20)\n# a comment (with parens)\nproject(Engine LANGUAGES C CXX)\nfind_package(Boost REQUIRED COMPONENTS system)\nfind_package(ZLIB)\nadd_library(core src/core.cpp)\nadd_executable(app src/main.cpp)\ntarget_link_libraries(app PRIVATE core Boost::system pthread ${EXTRA})\ninclude_directories(include)\ntarget_include_directories(core PUBLIC\n  ${PROJECT_SOURCE_DIR}/third_party\n  ${UNKNOWN_DIR}/x\n)\n";

#[tokio::test]
async fn cmake_names_the_package_dependencies_and_include_directories() {
    let (_dir, snap) = snapshot(&[
        ("CMakeLists.txt", CMAKE),
        ("src/main.cpp", "int main() {}\n"),
    ])
    .await;
    let package = &snap.graph.packages[0];
    assert_eq!(package.name, "Engine");
    assert_eq!(
        package.facts.declared_dependencies,
        ["Boost", "ZLIB", "pthread"],
        "targets of the project and variables are not dependencies"
    );
    assert_eq!(
        package.facts.inferred_facts,
        ["include:include", "include:third_party"]
    );
    assert!(
        snap.graph
            .diagnostics
            .iter()
            .any(|d| d.message.contains("UNKNOWN_DIR")),
        "{:?}",
        snap.graph.diagnostics
    );
}

#[tokio::test]
async fn includes_resolve_relative_to_the_file_include_directories_and_the_standard_library() {
    let (_dir, snap) = snapshot(&[
        ("CMakeLists.txt", CMAKE),
        ("src/main.cpp", "#include <iostream>\n#include <stdio.h>\n#include <sys/socket.h>\n#include \"util.h\"\n#include \"engine/core.hpp\"\n#include <engine/core.hpp>\n#include <extern/lib.h>\n#include <boost/asio.hpp>\n#include <zlib.h>\n#include <fmt/format.h>\n#include \"missing.h\"\n#include MACRO_HEADER\nint main() {}\n"),
        ("src/util.h", "#pragma once\n"),
        ("include/engine/core.hpp", "#pragma once\n"),
        ("third_party/extern/lib.h", "#pragma once\n"),
        ("src/core.cpp", "#include \"../include/engine/core.hpp\"\n"),
    ])
    .await;
    let main = "src/main.cpp";
    assert_eq!(target(edge(&snap, main, "util.h")), Some("src/util.h"));
    assert_eq!(
        edge(&snap, main, "util.h").rule,
        "native.quoted_relative_to_file"
    );
    let quoted_core = snap
        .imports
        .edges
        .iter()
        .find(|e| e.fact.spelling == "engine/core.hpp" && e.fact.scope.is_empty())
        .unwrap();
    assert_eq!(target(quoted_core), Some("include/engine/core.hpp"));
    assert_eq!(quoted_core.rule, "native.include_directory");
    let angle_core = snap
        .imports
        .edges
        .iter()
        .find(|e| e.fact.spelling == "engine/core.hpp" && !e.fact.scope.is_empty())
        .unwrap();
    assert_eq!(
        target(angle_core),
        Some("include/engine/core.hpp"),
        "angle includes search the include directories"
    );
    assert_eq!(
        target(edge(&snap, main, "extern/lib.h")),
        Some("third_party/extern/lib.h")
    );
    assert!(
        matches!(&edge(&snap, main, "iostream").resolution, Resolution::DeclaredExternal { package } if package == "stdlib:iostream")
    );
    assert_eq!(edge(&snap, main, "stdio.h").rule, "native.standard_header");
    assert_eq!(
        edge(&snap, main, "sys/socket.h").rule,
        "native.standard_header"
    );
    assert!(
        matches!(&edge(&snap, main, "boost/asio.hpp").resolution, Resolution::DeclaredExternal { package } if package == "boost"),
        "find_package(Boost)"
    );
    assert!(
        matches!(&edge(&snap, main, "zlib.h").resolution, Resolution::DeclaredExternal { package } if package == "zlib")
    );
    assert!(matches!(
        edge(&snap, main, "fmt/format.h").resolution,
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal
        }
    ));
    assert!(
        matches!(
            edge(&snap, main, "missing.h").resolution,
            Resolution::Unresolved {
                reason: ImportReason::UndeclaredExternal
            }
        ),
        "a quoted include found nowhere"
    );
    let dynamic = snap
        .imports
        .edges
        .iter()
        .find(|e| !e.fact.literal_specifier)
        .unwrap();
    assert_eq!(dynamic.fact.form, ImportForm::Include);
    assert!(matches!(
        dynamic.resolution,
        Resolution::DynamicOrUnsupported {
            reason: ImportReason::NonLiteralSpecifier
        }
    ));
    assert_eq!(
        target(edge(&snap, "src/core.cpp", "../include/engine/core.hpp")),
        Some("include/engine/core.hpp")
    );
}

#[tokio::test]
async fn profiles_follow_the_extension_and_compile_databases_stand_in_for_cmake() {
    let (_dir, snap) = snapshot(&[
        (
            "proj/compile_commands.json",
            "[{\"directory\":\"/build\",\"command\":\"cc -I/elsewhere -c a.c\",\"file\":\"a.c\"}]",
        ),
        ("db/compile_commands.json", "PLACEHOLDER"),
        ("c/main.c", "#include \"api.h\"\n#include <config.h>\n"),
        ("c/api.h", ""),
        ("cpp/widget.hpp", ""),
        ("cpp/widget.cc", "#include \"widget.hpp\"\n"),
    ])
    .await;
    let profile = |path: &str| {
        snap.imports
            .files
            .iter()
            .find(|f| f.path.as_path().to_str() == Some(path))
            .unwrap()
            .profile
    };
    assert_eq!(profile("c/main.c"), LanguageProfile::C);
    assert_eq!(profile("c/api.h"), LanguageProfile::C);
    assert_eq!(profile("cpp/widget.hpp"), LanguageProfile::Cpp);
    assert_eq!(profile("cpp/widget.cc"), LanguageProfile::Cpp);
    assert_eq!(
        target(edge(&snap, "cpp/widget.cc", "widget.hpp")),
        Some("cpp/widget.hpp")
    );
    assert!(snap
        .graph
        .diagnostics
        .iter()
        .any(|d| d.message.contains("not a valid compile database")));
}

#[tokio::test]
async fn a_compile_database_supplies_include_directories_inside_the_repository() {
    let dir_root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir_root.path()).unwrap();
    let database = format!(
        "[{{\"directory\":\"{r}/build\",\"arguments\":[\"cc\",\"-I../inc\",\"-I\",\"{r}/more\",\"-isystem\",\"/usr/include\",\"-c\",\"../src/a.c\"],\"file\":\"../src/a.c\"}}]",
        r = root.display()
    );
    for (path, text) in [
        ("build/compile_commands.json", database.as_str()),
        ("src/a.c", "#include <api.h>\n#include \"extra.h\"\n"),
        ("inc/api.h", ""),
        ("more/extra.h", ""),
    ] {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let mut adapters = default_manifest_adapters();
    adapters.extend(manifest_adapters());
    let service = RepositoryGraphBuilder::new(root.clone())
        .unwrap()
        .manifest_adapters(adapters)
        .source_extensions(source_extensions().iter().map(|e| e.to_string()).collect())
        .import_support(import_support())
        .unwrap()
        .build();
    let snap = service.refresh(CancellationToken::new()).await.unwrap();
    let package = &snap.graph.packages[0];
    assert_eq!(
        package.facts.inferred_facts,
        ["include:inc", "include:more"],
        "only directories inside the repository are kept"
    );
    // The database lives in build/, so its package does not own src/a.c; the edge cannot use its directories.
    let _ = edge(&snap, "src/a.c", "api.h");
}
