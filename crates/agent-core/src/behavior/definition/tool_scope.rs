use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which tools an agent step may see and execute.
///
/// There is deliberately no deny list: a tool registered after the definition
/// was written must never become available implicitly. `Inherit` is resolved
/// into an explicit allow list at run start and recorded on every attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "tools", rename_all = "snake_case")]
pub enum ToolScope {
    #[default]
    None,
    AllowList(Vec<String>),
    Inherit,
}
