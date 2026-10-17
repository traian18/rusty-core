use async_trait::async_trait;
use harness_tools::CancellationToken;
use harness_workspace::{
    dependency_groups::{
        BoundaryDirection, BoundaryQuery, GroupId, GroupKind, GroupListQuery, GroupMembersQuery,
    },
    import_graph::{ImportCursor, ImportFilter},
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::GraphContext;
use crate::{
    args,
    failure::Failure,
    operation::{Operation, Success},
};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
    Members,
    OfFile,
    Boundary,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    snapshot: Option<String>,
    action: Action,
    kind: Option<GroupKind>,
    group: Option<String>,
    file: Option<String>,
    direction: Option<BoundaryDirection>,
    cursor: Option<String>,
    limit: Option<usize>,
}

/// `repo_graph.groups`: group lookup in four forms.
pub(super) struct Groups(pub GraphContext);

impl Groups {
    fn group_id(args: &Arguments) -> Result<GroupId, Failure> {
        args.group
            .clone()
            .map(GroupId)
            .ok_or_else(|| Failure::invalid("group is required for this action"))
    }
}

#[async_trait]
impl Operation for Groups {
    fn name(&self) -> &'static str {
        "repo_graph.groups"
    }

    fn description(&self) -> &'static str {
        "Dependency groups. action=list: groups (optionally of one kind). members: files of a group. of_file: every group a file belongs to, with the evidence behind each. boundary: imports crossing a group's edge (outgoing or incoming). Cycle groups state reachability, not intent; directory groups are inferred."
    }

    fn schema(&self) -> Value {
        json!({"type":"object","required":["action"],"properties":{
            "snapshot":{"type":"string"},
            "action":{"type":"string","enum":["list","members","of_file","boundary"]},
            "kind":{"type":"string","enum":["declared_package","source_root","directory","import_cycle"],"description":"Filter for action=list"},
            "group":{"type":"string","description":"Group id for members and boundary"},
            "file":{"type":"string","description":"Workspace-relative file for of_file"},
            "direction":{"type":"string","enum":["outgoing","incoming"],"description":"For boundary; default outgoing"},
            "cursor":{"type":"string"},
            "limit":{"type":"integer","minimum":1,"maximum":500}},
            "additionalProperties":false})
    }

    async fn run(&self, arguments: Value, _: &CancellationToken) -> Result<Success, Failure> {
        let args: Arguments = args::parse(arguments)?;
        let snapshot = self.0.snapshot(&args.snapshot)?;
        let id = snapshot.id.clone();
        let limit = args.limit.unwrap_or(50);
        let cursor = args::optional_cursor::<ImportCursor>(&args.cursor)?;
        let graph = &self.0.graph;
        let result = match args.action {
            Action::List => {
                let page = graph.groups(&GroupListQuery {
                    snapshot: id,
                    kind: args.kind,
                    cursor,
                    limit,
                })?;
                json!({"groups": page.groups, "next_cursor": args::next_cursor(&page.next_cursor)})
            }
            Action::Members => {
                let group = Self::group_id(&args)?;
                let page = graph.group_members(&GroupMembersQuery {
                    snapshot: id,
                    group,
                    cursor,
                    limit,
                })?;
                json!({"group": page.group, "members": page.members, "next_cursor": args::next_cursor(&page.next_cursor)})
            }
            Action::OfFile => {
                let file = args::file(
                    args.file
                        .as_deref()
                        .ok_or_else(|| Failure::invalid("file is required for of_file"))?,
                )?;
                json!({"file": file, "assignments": graph.groups_of(&id, &file)?})
            }
            Action::Boundary => {
                let page = graph.boundary_edges(&BoundaryQuery {
                    snapshot: id,
                    group: Self::group_id(&args)?,
                    direction: args.direction.unwrap_or(BoundaryDirection::Outgoing),
                    filter: ImportFilter::default(),
                    cursor,
                    limit,
                })?;
                json!({"edges": page.edges, "next_cursor": args::next_cursor(&page.next_cursor)})
            }
        };
        Ok(self.0.success(&snapshot, None, result))
    }
}
