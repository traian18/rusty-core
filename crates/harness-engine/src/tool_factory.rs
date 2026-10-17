use std::sync::Arc;

use harness_code_analysis::CallGraph;
use harness_tool_code_analysis::tool;
use harness_tool_filesystem::{EditTool, ReadTool, SearchTool};
use harness_tool_git::{GitDiffTool, GitLogTool, GitShowTool, GitStatusTool};
use harness_tool_repository_intelligence::{is_repository_tool, RepositoryIntelligence};
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
    build_executor_with_context(descriptor, workspace, graph, None)
}

/// An executor that fails closed as an unknown tool, carrying the requested descriptor.
fn unknown_tool(
    descriptor: &harness_protocol::tools::ToolDescriptor,
) -> Arc<dyn harness_runtime::traits::ToolExecutor> {
    Arc::new(harness_tools::UnknownTool {
        descriptor: harness_tools::ToolDescriptor {
            id: harness_tools::ToolId::new(&descriptor.name),
            name: descriptor.name.clone(),
            description: descriptor.description.clone(),
            input_schema: descriptor.input_schema.clone(),
        },
    })
}

/// Builds the executor for `descriptor`. Graph-backed tools (`code_graph.*`, `repo_graph.*`,
/// `repo_git.*`) exist only when their dependency was explicitly supplied.
pub(crate) fn build_executor_with_context(
    descriptor: &harness_protocol::tools::ToolDescriptor,
    workspace: Arc<dyn harness_runtime::traits::Workspace>,
    graph: Option<Arc<dyn CallGraph>>,
    repository: Option<&RepositoryIntelligence>,
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
            || unknown_tool(descriptor),
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
        name if is_repository_tool(name) => repository
            .and_then(|repository| repository.executor_for(name))
            .unwrap_or_else(|| unknown_tool(descriptor)),
        _ => unknown_tool(descriptor),
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

    use harness_code_analysis::{
        CallGraph, DefaultCallGraph, FindCallablesRequest, RefreshRequest, SnapshotId,
    };
    use harness_tools::{CancellationToken, ToolInput};

    fn workspace_with_source(name: &str) -> (std::path::PathBuf, Arc<FsWorkspace>) {
        let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def a():\n    pass\ndef b():\n    a()\n").unwrap();
        (root.clone(), Arc::new(FsWorkspace::new(root)))
    }

    #[tokio::test]
    async fn graph_tools_without_a_graph_fail_closed_like_unknown_tools() {
        let workspace = Arc::new(FsWorkspace::new(std::env::temp_dir()));
        for name in [
            "code_graph.refresh",
            "code_graph.capabilities",
            "code_graph.find_callables",
            "code_graph.calls",
            "code_graph.traverse",
        ] {
            let executor =
                build_executor_for_with_graph(&descriptor(name), workspace.clone(), None);
            let result = executor
                .execute(
                    ToolInput {
                        arguments: serde_json::json!({}),
                    },
                    CancellationToken::new(),
                )
                .await;
            assert!(
                result.is_err(),
                "{name} must not run without an authorized graph"
            );
        }
    }

    #[tokio::test]
    async fn executors_built_separately_share_the_one_authorized_graph() {
        let (root, workspace) = workspace_with_source("cg-share");
        let graph: Arc<dyn CallGraph> = Arc::new(DefaultCallGraph::new(&root));
        // Two executors, as two construction paths (new session / restored session) would build.
        let refresh = build_executor_for_with_graph(
            &descriptor("code_graph.refresh"),
            workspace.clone(),
            Some(graph.clone()),
        );
        let find = build_executor_for_with_graph(
            &descriptor("code_graph.find_callables"),
            workspace,
            Some(graph),
        );
        let refreshed = refresh
            .execute(
                ToolInput {
                    arguments: serde_json::json!({}),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!refreshed.is_error, "{}", refreshed.output);
        let found = find
            .execute(
                ToolInput {
                    arguments: serde_json::json!({"limit": 10}),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!found.is_error, "{}", found.output);
        assert_eq!(
            found.output["result"]["page"]["items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn separate_contexts_sharing_a_root_stay_isolated() {
        let (root, workspace) = workspace_with_source("cg-isolate");
        let first: Arc<dyn CallGraph> = Arc::new(DefaultCallGraph::new(&root));
        let second: Arc<dyn CallGraph> = Arc::new(DefaultCallGraph::new(&root));
        let snapshot = first
            .refresh(RefreshRequest::default(), CancellationToken::new())
            .await
            .unwrap()
            .snapshot;
        let tool = build_executor_for_with_graph(
            &descriptor("code_graph.find_callables"),
            workspace,
            Some(second),
        );
        let result = tool
            .execute(
                ToolInput {
                    arguments: serde_json::json!({"snapshot": snapshot.0, "limit": 5}),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert_eq!(result.output["error"]["code"], "snapshot_not_found");
        // The first context is unaffected.
        let ok = first
            .find_callables(
                FindCallablesRequest {
                    snapshot: Some(SnapshotId(snapshot.0.clone())),
                    language: None,
                    name: None,
                    path: None,
                    cursor: None,
                    limit: 5,
                },
                CancellationToken::new(),
            )
            .await;
        assert!(ok.is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_tools_are_unaffected_by_graph_availability() {
        let workspace = Arc::new(FsWorkspace::new(std::env::temp_dir()));
        let graph: Arc<dyn CallGraph> = Arc::new(DefaultCallGraph::new(std::env::temp_dir()));
        for name in [
            "fs.read",
            "fs.edit",
            "workspace.search",
            "shell.exec",
            "git.status",
            "web_fetch",
        ] {
            let without = build_executor_for_with_graph(&descriptor(name), workspace.clone(), None);
            let with = build_executor_for_with_graph(
                &descriptor(name),
                workspace.clone(),
                Some(graph.clone()),
            );
            assert_eq!(without.descriptor().name, with.descriptor().name, "{name}");
        }
        let unknown =
            build_executor_for_with_graph(&descriptor("custom.tool"), workspace, Some(graph));
        assert_eq!(unknown.descriptor().name, "custom.tool");
    }
}
