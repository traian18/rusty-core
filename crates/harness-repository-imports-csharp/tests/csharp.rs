use harness_repository_imports_csharp::{import_support, manifest_adapters, SOURCE_EXTENSIONS};
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

const CSPROJ: &str = "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup>\n  <ItemGroup>\n    <PackageReference Include=\"Newtonsoft.Json\" Version=\"13.0.3\" />\n    <PackageReference Include=\"Microsoft.EntityFrameworkCore\" Version=\"8.0.0\" />\n    <PackageReference Include=\"StyleCop.Analyzers\" Version=\"1.1\" PrivateAssets=\"all\" />\n    <ProjectReference Include=\"..\\Core\\Core.csproj\" />\n  </ItemGroup>\n</Project>\n";

#[tokio::test]
async fn csproj_names_the_package_with_references_and_the_web_sdk() {
    let (_dir, snap) = snapshot(&[
        ("Web/Web.csproj", CSPROJ),
        ("Web/Program.cs", "class Program {}\n"),
    ])
    .await;
    let package = &snap.graph.packages[0];
    assert_eq!(package.name, "Web");
    assert_eq!(
        package.facts.declared_dependencies,
        [
            "Microsoft.EntityFrameworkCore",
            "Microsoft.NET.Sdk.Web",
            "Newtonsoft.Json",
            "StyleCop.Analyzers",
            "project:Core"
        ]
    );
    assert_eq!(package.facts.dev_dependencies, ["StyleCop.Analyzers"]);
}

#[tokio::test]
async fn usings_resolve_to_declaring_files_types_stdlib_and_packages() {
    let (_dir, snap) = snapshot(&[
        ("Web/Web.csproj", CSPROJ),
        ("Web/Controllers/UserController.cs", "using System;\nusing System.Linq;\nusing static Acme.Core.Strings;\nusing Models = Acme.Core.Models;\nusing Acme.Core.Models;\nusing Acme.Core;\nusing Newtonsoft.Json;\nusing Microsoft.EntityFrameworkCore;\nusing Microsoft.AspNetCore.Mvc;\nusing Dapper;\nusing Acme.Core.Missing;\n\nnamespace Acme.Web.Controllers;\n\npublic class UserController {}\n"),
        ("Core/Models/User.cs", "namespace Acme.Core.Models\n{\n    public class User {}\n}\n"),
        ("Core/Models/Role.cs", "namespace Acme.Core.Models;\npublic enum Role { Admin }\n"),
        ("Core/Strings.cs", "namespace Acme.Core\n{\n    public static class Strings {}\n}\n"),
        ("Core/Core.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
    ])
    .await;
    let file = "Web/Controllers/UserController.cs";
    let models = edge(&snap, file, "Acme.Core.Models");
    assert!(
        matches!(&models.resolution, Resolution::Possible { candidates, .. } if candidates.len() == 2),
        "two files declare the namespace: {:?}",
        models.resolution
    );
    assert_eq!(models.rule, "csharp.namespace_declaration");
    let core = edge(&snap, file, "Acme.Core");
    assert_eq!(
        core.resolution.target_file().unwrap().as_path().to_str(),
        Some("Core/Strings.cs"),
        "one file declares it"
    );
    let strings = snap
        .imports
        .edges
        .iter()
        .find(|e| e.fact.spelling == "Acme.Core.Strings")
        .unwrap();
    assert_eq!(strings.rule, "csharp.type_declaration");
    assert!(strings.fact.scope.contains(&"static".to_owned()));
    assert_eq!(
        strings.resolution.target_file().unwrap().as_path().to_str(),
        Some("Core/Strings.cs")
    );
    let alias = snap
        .imports
        .edges
        .iter()
        .find(|e| e.fact.alias.as_deref() == Some("Models"))
        .unwrap();
    assert_eq!(alias.fact.spelling, "Acme.Core.Models");
    assert!(
        matches!(&edge(&snap, file, "System").resolution, Resolution::DeclaredExternal { package } if package == "stdlib:System")
    );
    assert!(
        matches!(&edge(&snap, file, "System.Linq").resolution, Resolution::DeclaredExternal { package } if package == "stdlib:System")
    );
    assert!(
        matches!(&edge(&snap, file, "Newtonsoft.Json").resolution, Resolution::DeclaredExternal { package } if package == "Newtonsoft.Json")
    );
    assert!(
        matches!(&edge(&snap, file, "Microsoft.EntityFrameworkCore").resolution, Resolution::DeclaredExternal { package } if package == "Microsoft.EntityFrameworkCore"),
        "a declared Microsoft package wins over the stdlib label"
    );
    assert!(
        matches!(&edge(&snap, file, "Microsoft.AspNetCore.Mvc").resolution, Resolution::DeclaredExternal { package } if package == "stdlib:Microsoft")
    );
    assert!(matches!(
        edge(&snap, file, "Dapper").resolution,
        Resolution::Unresolved {
            reason: ImportReason::UndeclaredExternal
        }
    ));
    assert!(matches!(
        edge(&snap, file, "Acme.Core.Missing").resolution,
        Resolution::Unresolved {
            reason: ImportReason::PathNotFound
        }
    ));
}

#[tokio::test]
async fn nested_and_block_namespaces_are_joined_and_global_usings_are_read() {
    let (_dir, snap) = snapshot(&[
        ("App/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>\n"),
        ("App/Globals.cs", "global using Outer.Inner;\nglobal using static Outer.Helpers;\n"),
        ("App/Inner.cs", "namespace Outer\n{\n    namespace Inner\n    {\n        class X {}\n    }\n    static class Helpers {}\n}\n"),
    ])
    .await;
    let globals = "App/Globals.cs";
    assert_eq!(
        edge(&snap, globals, "Outer.Inner")
            .resolution
            .target_file()
            .unwrap()
            .as_path()
            .to_str(),
        Some("App/Inner.cs")
    );
    assert_eq!(
        edge(&snap, globals, "Outer.Helpers").rule,
        "csharp.type_declaration"
    );
}

#[tokio::test]
async fn a_csproj_without_a_project_element_is_reported() {
    let (_dir, snap) = snapshot(&[("A/A.csproj", "not xml"), ("A/a.cs", "class A {}\n")]).await;
    assert!(snap.graph.packages.is_empty());
    assert!(snap
        .graph
        .diagnostics
        .iter()
        .any(|d| d.message.contains("no <Project>")));
}
