//! Gin: `r.GET("/path", handler)` on an engine from `gin.Default()` / `gin.New()`, and groups.
use harness_workspace::{
    framework_relations::{
        FrameworkAdapter, FrameworkCapability, FrameworkInput, FrameworkOutput, RelationKind,
    },
    repository_intelligence::{DiscoveryError, Package},
};
use tokio_util::sync::CancellationToken;

use crate::go_routes::{extract, Dialect, HandlerAt};

const DIALECT: Dialect = Dialect {
    framework: "gin",
    rule: "gin-route",
    version: 1,
    marker: "gin-gonic/gin",
    constructors: &[("gin", "Default"), ("gin", "New")],
    methods: &[
        "GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD", "Any",
    ],
    handler: HandlerAt::Last,
};

pub struct GinAdapter;

impl FrameworkAdapter for GinAdapter {
    fn id(&self) -> &'static str {
        DIALECT.framework
    }

    fn version(&self) -> u32 {
        DIALECT.version
    }

    fn capability(&self) -> FrameworkCapability {
        FrameworkCapability {
            framework: "gin".into(),
            name: "Gin".into(),
            adapter_version: DIALECT.version,
            kinds: vec![RelationKind::RouteRegisteredTo],
            handled: vec![
                "GET, POST, PUT, DELETE, PATCH, OPTIONS, HEAD and Any with a literal path on an engine created by gin.Default() or gin.New(), and on groups created with .Group(\"/prefix\") (prefixes are joined)".into(),
                "handlers: a function literal, a function of the same package, or pkg.Function through an import; middleware before the last argument is ignored".into(),
            ],
            unsupported: vec![
                "r.Handle(method, path, …), routers passed as function parameters, and routes registered through helper functions".into(),
                "computed paths (reported, no relation); the lead file of an imported package stands for the package".into(),
            ],
        }
    }

    fn applicability(&self, package: &Package) -> Option<String> {
        package
            .facts
            .declared_dependencies
            .iter()
            .any(|d| d == "github.com/gin-gonic/gin")
            .then(|| "declares dependency `github.com/gin-gonic/gin`".to_owned())
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
