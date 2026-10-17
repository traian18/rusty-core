use async_trait::async_trait;
use harness_tools::CancellationToken;
use harness_workspace::framework_relations::{
    FrameworkCursor, FrameworkFilter, FrameworkQuery, RelationKind,
};
use harness_workspace::repository_intelligence::PackageId;
use serde::Deserialize;
use serde_json::{json, Value};

use super::GraphContext;
use crate::{
    args,
    failure::Failure,
    operation::{Operation, Success},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    snapshot: Option<String>,
    framework: Option<String>,
    kind: Option<RelationKind>,
    file: Option<String>,
    package: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

/// `repo_graph.framework_relations`: routes, injected providers, rendered components and commands.
pub(super) struct FrameworkRelations(pub GraphContext);

#[async_trait]
impl Operation for FrameworkRelations {
    fn name(&self) -> &'static str {
        "repo_graph.framework_relations"
    }

    fn description(&self) -> &'static str {
        "List framework relations: routes served by handlers, providers injected where used, components rendered by components, commands exposed to callers. Each has the framework, rule and version, certainty (declared in code or implied by convention) and the source text that justifies it. These are not imports and are never mixed with them. Paged; filter by framework, kind, file or package. Frameworks that do not apply to a package, or conventions that cannot be read statically, produce no relations."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","properties":{
            "snapshot":{"type":"string","description":"Snapshot id; defaults to the current snapshot"},
            "framework":{"type":"string","description":"Framework id, for example express or axum"},
            "kind":{"type":"string","enum":["route_registered_to","provider_injected_into","component_renders","command_exposed_as","model_mapped_to_table"]},
            "file":{"type":"string","description":"Workspace-relative file; matches relations that start or end there"},
            "package":{"type":"string","description":"Package id"},
            "cursor":{"type":"string"},
            "limit":{"type":"integer","minimum":1,"maximum":500}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: Arguments = args::parse(arguments)?;
        let snapshot = self.0.snapshot(&args.snapshot)?;
        let query = FrameworkQuery {
            snapshot: snapshot.id.clone(),
            filter: FrameworkFilter {
                framework: args.framework,
                kind: args.kind,
                file: args.file.as_deref().map(args::file).transpose()?,
                package: args.package.map(PackageId),
            },
            cursor: args::optional_cursor::<FrameworkCursor>(&args.cursor)?,
            limit: args.limit.unwrap_or(50),
        };
        let page = self.0.graph.framework_relations(&query)?;
        let result = json!({
            "relations": page.relations,
            "next_cursor": args::next_cursor(&page.next_cursor),
            "applications": snapshot.frameworks.applications,
            "diagnostics": snapshot.frameworks.diagnostics.len(),
        });
        let mut success = self.0.success(&snapshot, None, result);
        success.partial = snapshot.imports.partial || page.partial;
        Ok(success)
    }
}
