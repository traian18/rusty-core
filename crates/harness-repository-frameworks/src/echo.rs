//! Echo: `e.GET("/path", handler)` on an instance from `echo.New()`, and groups.
use harness_workspace::{
    framework_relations::{
        FrameworkAdapter, FrameworkCapability, FrameworkInput, FrameworkOutput, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package},
};
use tokio_util::sync::CancellationToken;

use crate::go_routes::{extract, Dialect, HandlerAt};

const DIALECT: Dialect = Dialect {
    framework: "echo",
    rule: "echo-route",
    version: 1,
    marker: "labstack/echo",
    constructors: &[("echo", "New")],
    methods: &[
        "GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD", "Any",
    ],
    handler: HandlerAt::Second,
};

pub struct EchoAdapter;

impl FrameworkAdapter for EchoAdapter {
    fn id(&self) -> &'static str {
        DIALECT.framework
    }

    fn version(&self) -> u32 {
        DIALECT.version
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: "echo".into(),
            name: "Echo".into(),
            adapter_version: DIALECT.version,
            kinds: vec![RelationKind::RouteRegisteredTo],
            handled: vec![
                "GET, POST, PUT, DELETE, PATCH, OPTIONS, HEAD and Any with a literal path on an instance created by echo.New(), and on groups created with .Group(\"/prefix\") (prefixes are joined)".into(),
                "handlers: a function literal, a function of the same package, or pkg.Function through an import; the handler is the argument after the path, middleware follows".into(),
            ],
            unsupported: vec![
                "e.Add(method, path, …), routers passed as function parameters, and routes registered through helper functions".into(),
                "computed paths (reported, no relation); the lead file of an imported package stands for the package".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        package
            .facts
            .declared_dependencies
            .iter()
            .find(|d| d.starts_with("github.com/labstack/echo"))
            .map(|d| format!("declares dependency `{d}`"))
    }

    fn extract(
        &self,
        package: &Package,
        input: &FrameworkInput<'_>,
        cancel: &CancellationToken,
        output: &mut FrameworkOutput,
    ) -> Result<(), DiscoveryError> {
        extract(&DIALECT, package, input, cancel, output)
    }
}
