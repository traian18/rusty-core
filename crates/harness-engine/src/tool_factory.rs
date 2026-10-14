use std::sync::Arc;

use harness_code_analysis::CallGraph;
use harness_tool_code_analysis::tool;
use harness_tool_filesystem::{EditTool, ReadTool, SearchTool};
use harness_tool_git::{GitDiffTool, GitLogTool, GitShowTool, GitStatusTool};
use harness_tool_shell::ExecTool;
use harness_tool_web::FetchTool;

#[allow(dead_code)]
pub(crate) fn build_executor_for(
    descriptor: &harness_protocol::tools::ToolDescriptor,
    workspace: Arc<dyn harness_runtime::traits::Workspace>,
) -> Arc<dyn harness_runtime::traits::ToolExecutor> {
    build_executor_for_with_graph(descriptor, workspace, None)
}

pub(crate) fn build_executor_for_with_graph(
    descriptor: &harness_protocol::tools::ToolDescriptor,
    workspace: Arc<dyn harness_runtime::traits::Workspace>,
    graph: Option<Arc<dyn CallGraph>>,
) -> Arc<dyn harness_runtime::traits::ToolExecutor> {
    match descriptor.name.as_str() {
        "fs.read" => Arc::new(ReadTool::new(workspace)),
        "fs.edit" => Arc::new(EditTool::new(workspace)),
        "workspace.search" => Arc::new(SearchTool::new(workspace)),
        "shell.exec" => Arc::new(ExecTool::new()),
        // Git tools resolve the repo directly from the workspace root
        // via git2::Repository::discover — they don't go through the
        // Workspace trait's virtual filesystem access.
        "git.status" => Arc::new(GitStatusTool::new(workspace.root().to_path_buf())),
        "git.diff" => Arc::new(GitDiffTool::new(workspace.root().to_path_buf())),
        "git.log" => Arc::new(GitLogTool::new(workspace.root().to_path_buf())),
        "git.show" => Arc::new(GitShowTool::new(workspace.root().to_path_buf())),
        "web_fetch" => Arc::new(FetchTool::new()),
        "code_graph.refresh"
        | "code_graph.capabilities"
        | "code_graph.find_callables"
        | "code_graph.calls"
        | "code_graph.traverse" => graph.map_or_else(
            || {
                let tool_desc = harness_tools::ToolDescriptor {
                    id: harness_tools::ToolId::new(&descriptor.name),
                    name: descriptor.name.clone(),
                    description: descriptor.description.clone(),
                    input_schema: descriptor.input_schema.clone(),
                };
                Arc::new(harness_tools::UnknownTool {
                    descriptor: tool_desc,
                }) as Arc<dyn harness_runtime::traits::ToolExecutor>
            },
            |graph| {
                tool(
                    graph,
                    match descriptor.name.as_str() {
                        "code_graph.refresh" => "code_graph.refresh",
                        "code_graph.capabilities" => "code_graph.capabilities",
                        "code_graph.find_callables" => "code_graph.find_callables",
                        "code_graph.calls" => "code_graph.calls",
                        _ => "code_graph.traverse",
                    },
                )
            },
        ),
        _ => {
            let tool_desc = harness_tools::ToolDescriptor {
                id: harness_tools::ToolId::new(&descriptor.name),
                name: descriptor.name.clone(),
                description: descriptor.description.clone(),
                input_schema: descriptor.input_schema.clone(),
            };
            Arc::new(harness_tools::UnknownTool {
                descriptor: tool_desc,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_workspace::FsWorkspace;

    fn descriptor(name: &str) -> harness_protocol::tools::ToolDescriptor {
        harness_protocol::tools::ToolDescriptor {
            id: harness_protocol::ids::ToolId::new(),
            name: name.into(),
            description: "test".into(),
            input_schema: serde_json::json!({}),
        }
    }

    #[test]
    fn code_graph_tools_are_unknown_without_explicit_authorized_graph() {
        let workspace = Arc::new(FsWorkspace::new(std::env::temp_dir()));
        let descriptor = descriptor("code_graph.refresh");
        let executor = build_executor_for_with_graph(&descriptor, workspace, None);
        assert_eq!(executor.descriptor().name, "code_graph.refresh");
        assert_eq!(executor.descriptor().description, "test");
    }

    #[test]
    fn creates_known_and_unknown_executors() {
        let workspace = Arc::new(FsWorkspace::new(std::env::temp_dir()));
        let known = build_executor_for(&descriptor("fs.read"), workspace.clone());
        assert_eq!(known.descriptor().name, "Read file");

        let unknown = build_executor_for(&descriptor("custom.tool"), workspace);
        assert_eq!(unknown.descriptor().name, "custom.tool");
        assert_eq!(unknown.descriptor().description, "test");
    }
}
