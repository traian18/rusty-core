use async_trait::async_trait;
use harness_tools::CancellationToken;
use harness_workspace::{
    dependency_groups::{GroupKind, OverviewQuery, Truncation},
    import_graph::{ImportFilter, ImportKind},
};
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
    kind: GroupKind,
    node_limit: Option<usize>,
    edge_limit: Option<usize>,
    import_kind: Option<ImportKind>,
}

/// `repo_graph.overview`: groups of one kind and the imports between them.
pub(super) struct Overview(pub GraphContext);

#[async_trait]
impl Operation for Overview {
    fn name(&self) -> &'static str {
        "repo_graph.overview"
    }

    fn description(&self) -> &'static str {
        "Package/module overview: the groups of one kind (declared_package, source_root, directory, import_cycle) with import counts between them, split into runtime and type-only. Start here, then drill into files."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","required":["kind"],"properties":{
            "snapshot":{"type":"string","description":"Snapshot id; defaults to the current snapshot"},
            "kind":{"type":"string","enum":["declared_package","source_root","directory","import_cycle"]},
            "node_limit":{"type":"integer","minimum":1,"maximum":500},
            "edge_limit":{"type":"integer","minimum":1,"maximum":5000},
            "import_kind":{"type":"string","enum":["runtime","type_only"]}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: Arguments = args::parse(arguments)?;
        let snapshot = self.0.snapshot(&args.snapshot)?;
        let overview = self.0.graph.group_overview(&OverviewQuery {
            snapshot: snapshot.id.clone(),
            kind: args.kind,
            node_limit: args.node_limit.unwrap_or(100),
            edge_limit: args.edge_limit.unwrap_or(500),
            filter: ImportFilter {
                kind: args.import_kind,
                ..ImportFilter::default()
            },
        })?;
        let truncation = truncation_name(overview.truncation);
        Ok(self.0.success(&snapshot, truncation, json!(overview)))
    }
}

pub(crate) fn truncation_name(truncation: Truncation) -> Option<&'static str> {
    match truncation {
        Truncation::None => None,
        Truncation::DepthLimit => Some("depth_limit"),
        Truncation::NodeLimit => Some("node_limit"),
        Truncation::EdgeLimit => Some("edge_limit"),
    }
}
