use async_trait::async_trait;
use harness_tools::CancellationToken;
use harness_workspace::import_graph::{
    ImportCursor, ImportFilter, ImportKind, ImportQuery, ResolutionKind,
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
    file: String,
    item: Option<String>,
    import_kind: Option<ImportKind>,
    #[serde(default)]
    resolutions: Vec<ResolutionKind>,
    other_package: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

/// `repo_graph.dependencies` and `repo_graph.dependents`: one file's import edges.
pub(super) struct Edges {
    context: GraphContext,
    dependents: bool,
}

impl Edges {
    pub fn dependencies(context: GraphContext) -> Self {
        Self {
            context,
            dependents: false,
        }
    }

    pub fn dependents(context: GraphContext) -> Self {
        Self {
            context,
            dependents: true,
        }
    }
}

#[async_trait]
impl Operation for Edges {
    fn name(&self) -> &'static str {
        if self.dependents {
            "repo_graph.dependents"
        } else {
            "repo_graph.dependencies"
        }
    }

    fn description(&self) -> &'static str {
        if self.dependents {
            "List the import sites that target a file (optionally one item of it), each with its resolution, rule, evidence and byte span. Paged."
        } else {
            "List a file's import sites in source order, each resolved to an internal file, a declared external package, or an explicit unresolved/dynamic reason, with rule, evidence and byte span. Paged."
        }
    }

    fn schema(&self) -> Value {
        let mut properties = json!({
            "snapshot":{"type":"string","description":"Snapshot id; defaults to the current snapshot"},
            "file":{"type":"string","description":"Workspace-relative file"},
            "import_kind":{"type":"string","enum":["runtime","type_only"]},
            "resolutions":{"type":"array","items":{"type":"string","enum":["resolved_internal","declared_external","possible","unresolved","dynamic_or_unsupported"]}},
            "other_package":{"type":"string","description":"Package id on the far side of the edge"},
            "cursor":{"type":"string"},
            "limit":{"type":"integer","minimum":1,"maximum":500}});
        if self.dependents {
            properties["item"] =
                json!({"type":"string","description":"Only imports of this item of the file"});
        }
        json!({"type":"object","required":["file"],"properties":properties,"additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: Arguments = args::parse(arguments)?;
        if args.item.is_some() && !self.dependents {
            return Err(Failure::invalid(
                "item is only valid for repo_graph.dependents",
            ));
        }
        let snapshot = self.context.snapshot(&args.snapshot)?;
        let query = ImportQuery {
            snapshot: snapshot.id.clone(),
            file: args::file(&args.file)?,
            item: args.item,
            filter: ImportFilter {
                kind: args.import_kind,
                resolution: args.resolutions,
                staticness: None,
                other_package: args.other_package.map(PackageId),
            },
            cursor: args::optional_cursor::<ImportCursor>(&args.cursor)?,
            limit: args.limit.unwrap_or(50),
        };
        let page = if self.dependents {
            self.context.graph.dependents(&query)?
        } else {
            self.context.graph.dependencies(&query)?
        };
        let result = json!({
            "file": args.file,
            "edges": page.edges,
            "next_cursor": args::next_cursor(&page.next_cursor),
        });
        let mut success = self.context.success(&snapshot, None, result);
        success.partial = page.partial_coverage;
        Ok(success)
    }
}
