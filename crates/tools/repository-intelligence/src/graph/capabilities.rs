use async_trait::async_trait;
use harness_tools::CancellationToken;
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
struct NoArguments {}

/// `repo_graph.capabilities`: what is supported, what is not, and the query bounds.
pub(super) struct Capabilities(pub GraphContext);

#[async_trait]
impl Operation for Capabilities {
    fn name(&self) -> &'static str {
        "repo_graph.capabilities"
    }

    fn description(&self) -> &'static str {
        "Report supported manifest and language profiles, resolution rules, documented gaps, group kinds, framework adapters (what each reads and leaves alone, and where it applies) and query bounds."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        args::parse::<NoArguments>(arguments)?;
        let groups = self
            .0
            .graph
            .current()
            .filter(|s| s.groups.configured)
            .map(|s| {
                json!({
                    "kinds": ["declared_package", "source_root", "directory", "import_cycle"],
                    "limits": s.groups.limits,
                    "rules": s.groups.rules,
                })
            });
        let frameworks = self
            .0
            .graph
            .current()
            .filter(|s| s.frameworks.configured)
            .map(|s| {
                json!({
                    "adapters": self.0.graph.framework_capabilities(),
                    "applications": s.frameworks.applications,
                    "limits": s.frameworks.limits,
                    "diagnostics": s.frameworks.diagnostics.len(),
                })
            });
        Ok(Success::plain(json!({
            "discovery": self.0.graph.capabilities(),
            "imports": self.0.graph.import_capabilities(),
            "groups": groups,
            "frameworks": frameworks,
            "git_available": self.0.git.is_some(),
        })))
    }
}
