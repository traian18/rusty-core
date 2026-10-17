mod support;

use std::sync::Arc;

use harness_repository_frameworks::AspNetAdapter;
use harness_workspace::framework_relations::{Certainty, FrameworkAdapter, RelationKind};
use support::*;

const CSPROJ: &str = "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n<PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup>\n</Project>\n";

fn adapters() -> Vec<Arc<dyn FrameworkAdapter>> {
    vec![Arc::new(AspNetAdapter)]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn controller_attributes_expand_tokens_and_join_class_and_method_templates() {
    let repo = repo(
        &[
            ("Web/Web.csproj", CSPROJ),
            ("Web/Controllers/UsersController.cs", "using Microsoft.AspNetCore.Mvc;\n\nnamespace Web.Controllers;\n\n[ApiController]\n[Route(\"api/[controller]\")]\npublic class UsersController : ControllerBase\n{\n    [HttpGet]\n    public IActionResult List() => Ok();\n\n    [HttpGet(\"{id:int}\")]\n    public IActionResult Get(int id) => Ok();\n\n    [HttpPost(\"~/api/signup\")]\n    public IActionResult Signup() => Ok();\n\n    [HttpDelete]\n    [Route(\"purge/[action]\")]\n    public IActionResult Purge() => Ok();\n\n    public IActionResult Helper() => Ok();\n}\n\npublic class Plain { [HttpGet(\"x\")] public void X() {} }\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "RouteRegisteredTo|GET /api/Users|Web/Controllers/UsersController.cs->Web/Controllers/UsersController.cs".into(),
            "RouteRegisteredTo|GET /api/Users/{id:int}|Web/Controllers/UsersController.cs->Web/Controllers/UsersController.cs".into(),
            "RouteRegisteredTo|POST /api/signup|Web/Controllers/UsersController.cs->Web/Controllers/UsersController.cs".into(),
            "RouteRegisteredTo|DELETE /api/Users/purge/Purge|Web/Controllers/UsersController.cs->Web/Controllers/UsersController.cs".into(),
        ])
    );
    let get = repo
        .relations()
        .iter()
        .find(|r| r.label == "GET /api/Users/{id:int}")
        .unwrap();
    assert_eq!(
        get.certainty,
        Certainty::Convention,
        "the [controller] token is expanded by convention"
    );
    assert_eq!(get.to_symbol.as_deref(), Some("UsersController.Get"));
    assert_eq!(get.evidence.text, "HttpGet(\"{id:int}\")");
    let signup = repo
        .relations()
        .iter()
        .find(|r| r.label == "POST /api/signup")
        .unwrap();
    assert_eq!(
        signup.certainty,
        Certainty::Convention,
        "the class route still carries the token"
    );
}

#[tokio::test]
async fn minimal_apis_follow_groups_and_resolve_handlers() {
    let repo = repo(
        &[
            ("Api/Api.csproj", CSPROJ),
            ("Api/Program.cs", "var builder = WebApplication.CreateBuilder(args);\nvar app = builder.Build();\napp.MapGet(\"/ping\", () => \"pong\");\nvar api = app.MapGroup(\"/api\");\napi.MapPost(\"/items\", Handlers.Create);\nvar nested = api.MapGroup(\"/v2\");\nnested.MapDelete(\"/items/{id}\", Remove);\nvar route = \"/x\";\napp.MapGet(route, () => 1);\nother.MapGet(\"/not-app\", () => 1);\napp.Run();\nstatic void Remove() {}\n"),
            ("Api/Handlers.cs", "public static class Handlers { public static void Create() {} }\n"),
        ],
        adapters(),
    )
    .await;
    assert_eq!(
        sorted(repo.summary()),
        sorted(vec![
            "RouteRegisteredTo|GET /ping|Api/Program.cs->Api/Program.cs".into(),
            "RouteRegisteredTo|POST /api/items|Api/Program.cs->Api/Handlers.cs".into(),
            "RouteRegisteredTo|DELETE /api/v2/items/{id}|Api/Program.cs->Api/Program.cs".into(),
        ])
    );
    assert_eq!(repo.diagnostics().len(), 1, "{:?}", repo.diagnostics());
}

#[tokio::test]
async fn registrations_are_matched_to_constructors_and_dbsets_to_tables() {
    let repo = repo(
        &[
            ("Web/Web.csproj", CSPROJ),
            ("Web/Program.cs", "var builder = WebApplication.CreateBuilder(args);\nbuilder.Services.AddScoped<IUserService, UserService>();\nbuilder.Services.AddSingleton<Clock>();\nbuilder.Services.AddTransient<IUnused, Unused>();\n"),
            ("Web/Services.cs", "public interface IUserService {}\npublic class UserService : IUserService {}\npublic class Clock {}\npublic interface IUnused {}\npublic class Unused : IUnused {}\n"),
            ("Web/UsersController.cs", "public class UsersController : ControllerBase\n{\n    public UsersController(IUserService users, Clock clock, ILogger<UsersController> log, IOther other) {}\n}\n"),
            ("Web/Data.cs", "public class AppDb : DbContext\n{\n    public DbSet<Order> Orders { get; set; }\n    public DbSet<Customer> Customers { get; set; }\n    public DbSet<Missing> Gone { get; set; }\n}\n"),
            ("Web/Models.cs", "[Table(\"customer_records\")]\npublic class Customer {}\npublic class Order {}\n"),
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
        sorted(vec![
            "ProviderInjectedInto|UserService|Web/Services.cs->Web/UsersController.cs".into(),
            "ProviderInjectedInto|Clock|Web/Services.cs->Web/UsersController.cs".into(),
        ])
    );
    let mut tables: Vec<_> = repo
        .relations()
        .iter()
        .filter(|r| r.kind == RelationKind::ModelMappedToTable)
        .map(|r| {
            (
                r.label.clone(),
                r.certainty,
                r.from.as_path().display().to_string(),
            )
        })
        .collect();
    tables.sort();
    assert_eq!(
        tables,
        [
            (
                "Orders".to_owned(),
                Certainty::Convention,
                "Web/Models.cs".to_owned()
            ),
            (
                "customer_records".to_owned(),
                Certainty::Declared,
                "Web/Models.cs".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn the_adapter_is_off_without_the_web_sdk_or_aspnet_packages() {
    let repo = repo(
        &[
            ("Lib/Lib.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><ItemGroup><PackageReference Include=\"Newtonsoft.Json\" Version=\"13\" /></ItemGroup></Project>"),
            ("Lib/A.cs", "[ApiController]\n[Route(\"x\")]\npublic class AController : ControllerBase { [HttpGet] public void G() {} }\n"),
        ],
        adapters(),
    )
    .await;
    assert!(repo.relations().is_empty());
    assert!(repo.snapshot.frameworks.applications.is_empty());
}
