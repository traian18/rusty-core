mod support;

use std::sync::Arc;

use harness_workspace::{
    dependency_groups::{
        BoundaryDirection, BoundaryQuery, DefaultGroupAnalyzer, GraphDirection, GraphQuery, Group,
        GroupBasis, GroupId, GroupInput, GroupKind, GroupListQuery, GroupMembersQuery,
        GroupQueryError, GroupRule, OverviewQuery, Truncation,
    },
    import_graph::{ImportFilter, ImportKind},
    repository_intelligence::{RepositoryGraph, SnapshotId},
};
use support::*;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn gid(value: &str) -> GroupId {
    GroupId(value.to_owned())
}

fn list(
    graph: &impl RepositoryGraph,
    id: &SnapshotId,
    kind: Option<GroupKind>,
) -> Vec<(String, usize)> {
    let mut all = Vec::new();
    let mut cursor = None;
    loop {
        let page = graph
            .groups(&GroupListQuery {
                snapshot: id.clone(),
                kind,
                cursor,
                limit: 2,
            })
            .unwrap();
        all.extend(page.groups.iter().map(|g| (g.id.0.clone(), g.member_count)));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return all,
        }
    }
}

fn members(graph: &impl RepositoryGraph, id: &SnapshotId, group: &str) -> Vec<String> {
    graph
        .group_members(&GroupMembersQuery {
            snapshot: id.clone(),
            group: gid(group),
            cursor: None,
            limit: 100,
        })
        .unwrap()
        .members
        .iter()
        .map(|m| m.as_path().to_string_lossy().into_owned())
        .collect()
}

#[tokio::test]
async fn groups_cover_packages_roots_directories_and_cycles() {
    let dir = tempdir().unwrap();
    groups_workspace(dir.path());
    let graph = grouped_service(dir.path());
    let id = refresh(&graph).await;

    // Declared packages: nested packages take their own files.
    let packages = list(&graph, &id, Some(GroupKind::DeclaredPackage));
    assert_eq!(
        packages,
        vec![
            ("declared_package:outer/Cargo.toml:outer".into(), 5),
            ("declared_package:outer/nested/Cargo.toml:nested".into(), 1),
            ("declared_package:web/package.json:web".into(), 5),
        ]
    );
    assert!(members(
        &graph,
        &id,
        "declared_package:outer/nested/Cargo.toml:nested"
    )
    .contains(&"outer/nested/src/lib.rs".to_owned()));
    assert!(
        !members(&graph, &id, "declared_package:outer/Cargo.toml:outer")
            .contains(&"outer/nested/src/lib.rs".to_owned())
    );

    // Source roots: tests/ is in the package but outside any declared source root.
    let roots = list(&graph, &id, Some(GroupKind::SourceRoot));
    assert_eq!(
        roots,
        vec![
            ("source_root:outer/nested/src".into(), 1),
            ("source_root:outer/src".into(), 4),
            ("source_root:web/src".into(), 5),
        ]
    );
    assert!(
        !members(&graph, &id, "source_root:outer/src").contains(&"outer/tests/it.rs".to_owned())
    );

    // Directory groups exist only for files no manifest owns.
    assert_eq!(
        list(&graph, &id, Some(GroupKind::Directory)),
        vec![
            ("directory:loose".into(), 2),
            ("directory:loose/deep".into(), 1)
        ]
    );

    // Cycles: runtime ones first, the type-only one flagged.
    let cycles = list(&graph, &id, Some(GroupKind::ImportCycle));
    assert_eq!(
        cycles,
        vec![
            ("import_cycle:outer/src/a.rs".into(), 2),
            ("import_cycle:web/src/p.ts".into(), 2),
            ("import_cycle:web/src/x.ts+types".into(), 2),
        ]
    );
    let runtime_cycle = graph
        .group_members(&GroupMembersQuery {
            snapshot: id.clone(),
            group: gid("import_cycle:outer/src/a.rs"),
            cursor: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(runtime_cycle.group.basis, GroupBasis::Derived);
    assert!(!runtime_cycle.group.involves_type_only);
    let type_cycle = graph
        .group_members(&GroupMembersQuery {
            snapshot: id.clone(),
            group: gid("import_cycle:web/src/x.ts+types"),
            cursor: None,
            limit: 10,
        })
        .unwrap();
    assert!(type_cycle.group.involves_type_only);
    assert!(type_cycle.group.explanation.contains("type-only"));
}

#[tokio::test]
async fn every_assignment_is_explained_with_evidence() {
    let dir = tempdir().unwrap();
    groups_workspace(dir.path());
    let graph = grouped_service(dir.path());
    let id = refresh(&graph).await;

    let a = graph.groups_of(&id, &path("outer/src/a.rs")).unwrap();
    let kinds = a.iter().map(|x| x.group.kind).collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            GroupKind::DeclaredPackage,
            GroupKind::SourceRoot,
            GroupKind::ImportCycle
        ]
    );
    assert_eq!(a[0].evidence, vec![path("outer/Cargo.toml")]);
    assert_eq!(a[1].evidence, vec![path("outer/src/lib.rs")]);
    assert_eq!(a[1].group.basis, GroupBasis::Declared);
    // Cycle evidence names the exact import sites inside the component.
    assert_eq!(a[2].cycle_sites.len(), 2);
    assert!(!a[2].evidence_truncated);
    let snapshot = graph.get(&id).unwrap();
    let sites = snapshot
        .imports
        .edges
        .iter()
        .filter(|e| a[2].cycle_sites.contains(&e.fact.id))
        .map(|e| e.fact.spelling.clone())
        .collect::<Vec<_>>();
    assert_eq!(sites, vec!["crate::b::B", "crate::a::A"]);

    let loose = graph.groups_of(&id, &path("loose/a.ts")).unwrap();
    assert_eq!(loose.len(), 1);
    assert_eq!(loose[0].group.basis, GroupBasis::Inferred);
    assert!(loose[0].group.explanation.contains("no manifest"));

    assert!(matches!(
        graph.groups_of(&id, &path("missing.rs")),
        Err(GroupQueryError::UnknownFile(_))
    ));
}

#[tokio::test]
async fn boundary_edges_and_overview_agree_with_the_import_edges() {
    let dir = tempdir().unwrap();
    groups_workspace(dir.path());
    let graph = grouped_service(dir.path());
    let id = refresh(&graph).await;
    let boundary = |group: &str, direction| BoundaryQuery {
        snapshot: id.clone(),
        group: gid(group),
        direction,
        filter: ImportFilter::default(),
        cursor: None,
        limit: 50,
    };

    let outgoing = graph
        .boundary_edges(&boundary(
            "declared_package:outer/Cargo.toml:outer",
            BoundaryDirection::Outgoing,
        ))
        .unwrap();
    assert_eq!(
        outgoing
            .edges
            .iter()
            .map(|e| e.fact.spelling.as_str())
            .collect::<Vec<_>>(),
        vec!["nested::n"]
    );
    let incoming = graph
        .boundary_edges(&boundary(
            "declared_package:outer/nested/Cargo.toml:nested",
            BoundaryDirection::Incoming,
        ))
        .unwrap();
    assert_eq!(incoming.edges.len(), 1);
    assert_eq!(
        incoming.edges[0].fact.importing_file,
        path("outer/src/c.rs")
    );
    // Imports inside a group are not boundary crossings.
    assert!(graph
        .boundary_edges(&boundary(
            "declared_package:web/package.json:web",
            BoundaryDirection::Outgoing
        ))
        .unwrap()
        .edges
        .is_empty());

    let overview = graph
        .group_overview(&OverviewQuery {
            snapshot: id.clone(),
            kind: GroupKind::DeclaredPackage,
            node_limit: 10,
            edge_limit: 10,
            filter: ImportFilter::default(),
        })
        .unwrap();
    assert_eq!(overview.nodes.len(), 3);
    assert_eq!(overview.truncation, Truncation::None);
    assert_eq!(overview.edges.len(), 1);
    let edge = &overview.edges[0];
    assert_eq!(edge.from, gid("declared_package:outer/Cargo.toml:outer"));
    assert_eq!(
        edge.to,
        gid("declared_package:outer/nested/Cargo.toml:nested")
    );
    assert_eq!((edge.runtime, edge.type_only), (1, 0));

    // Type-only imports are counted separately.
    let directories = graph
        .group_overview(&OverviewQuery {
            snapshot: id.clone(),
            kind: GroupKind::Directory,
            node_limit: 10,
            edge_limit: 10,
            filter: ImportFilter::default(),
        })
        .unwrap();
    assert_eq!(directories.edges.len(), 1, "loose/deep -> loose");
    let limited = graph
        .group_overview(&OverviewQuery {
            snapshot: id.clone(),
            kind: GroupKind::DeclaredPackage,
            node_limit: 2,
            edge_limit: 10,
            filter: ImportFilter::default(),
        })
        .unwrap();
    assert_eq!(limited.truncation, Truncation::NodeLimit);
    assert_eq!(limited.nodes.len(), 2);
    let typed = graph
        .group_overview(&OverviewQuery {
            snapshot: id.clone(),
            kind: GroupKind::SourceRoot,
            node_limit: 10,
            edge_limit: 10,
            filter: ImportFilter {
                kind: Some(ImportKind::TypeOnly),
                ..ImportFilter::default()
            },
        })
        .unwrap();
    assert!(
        typed.edges.is_empty(),
        "type-only imports stay inside web/src"
    );
}

#[tokio::test]
async fn projection_is_bounded_deterministic_and_cycle_safe() {
    let dir = tempdir().unwrap();
    groups_workspace(dir.path());
    let graph = grouped_service(dir.path());
    let id = refresh(&graph).await;
    let query = |roots: &[&str], direction, depth, nodes, edges| GraphQuery {
        snapshot: id.clone(),
        roots: roots.iter().map(|r| path(r)).collect(),
        direction,
        depth,
        node_limit: nodes,
        edge_limit: edges,
        filter: ImportFilter::default(),
    };

    // A cycle terminates: two nodes, two edges, nothing left over.
    let cycle = graph
        .graph(&query(
            &["outer/src/a.rs"],
            GraphDirection::Dependencies,
            5,
            100,
            100,
        ))
        .unwrap();
    assert_eq!(
        cycle
            .nodes
            .iter()
            .map(|n| n.file.as_path().to_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["outer/src/a.rs", "outer/src/b.rs"]
    );
    assert_eq!(cycle.edges.len(), 2);
    assert_eq!(cycle.truncation, Truncation::None);
    assert_eq!(cycle.nodes[1].depth, 1);
    assert_eq!(
        cycle,
        graph
            .graph(&query(
                &["outer/src/a.rs"],
                GraphDirection::Dependencies,
                5,
                100,
                100
            ))
            .unwrap()
    );

    // Depth limit is reported when more edges exist beyond it.
    let shallow = graph
        .graph(&query(
            &["outer/src/lib.rs"],
            GraphDirection::Dependencies,
            1,
            100,
            100,
        ))
        .unwrap();
    assert_eq!(shallow.truncation, Truncation::DepthLimit);
    assert!(shallow.nodes.iter().all(|n| n.depth <= 1));
    let deep = graph
        .graph(&query(
            &["outer/src/lib.rs"],
            GraphDirection::Dependencies,
            3,
            100,
            100,
        ))
        .unwrap();
    assert_eq!(deep.truncation, Truncation::None);
    assert!(deep
        .nodes
        .iter()
        .any(|n| n.file == path("outer/nested/src/lib.rs")));

    // Dependents and both directions.
    let dependents = graph
        .graph(&query(
            &["outer/nested/src/lib.rs"],
            GraphDirection::Dependents,
            2,
            100,
            100,
        ))
        .unwrap();
    assert!(dependents
        .nodes
        .iter()
        .any(|n| n.file == path("outer/src/c.rs")));
    let both = graph
        .graph(&query(
            &["outer/src/c.rs"],
            GraphDirection::Both,
            1,
            100,
            100,
        ))
        .unwrap();
    assert!(both
        .nodes
        .iter()
        .any(|n| n.file == path("outer/src/lib.rs")));
    assert!(both
        .nodes
        .iter()
        .any(|n| n.file == path("outer/nested/src/lib.rs")));

    // Node and edge limits are explicit.
    let nodes = graph
        .graph(&query(
            &["outer/src/lib.rs"],
            GraphDirection::Dependencies,
            3,
            2,
            100,
        ))
        .unwrap();
    assert_eq!(
        (nodes.nodes.len(), nodes.truncation),
        (2, Truncation::NodeLimit)
    );
    let edges = graph
        .graph(&query(
            &["outer/src/lib.rs"],
            GraphDirection::Dependencies,
            3,
            100,
            1,
        ))
        .unwrap();
    assert_eq!(
        (edges.edges.len(), edges.truncation),
        (1, Truncation::EdgeLimit)
    );
}

#[tokio::test]
async fn invalid_group_queries_get_typed_errors() {
    let dir = tempdir().unwrap();
    groups_workspace(dir.path());
    let graph = grouped_service(dir.path());
    let id = refresh(&graph).await;
    let list_query = |limit, cursor| GroupListQuery {
        snapshot: id.clone(),
        kind: None,
        cursor,
        limit,
    };
    assert!(matches!(
        graph.groups(&list_query(0, None)),
        Err(GroupQueryError::InvalidLimit { .. })
    ));
    assert!(matches!(
        graph.groups(&list_query(10_000, None)),
        Err(GroupQueryError::InvalidLimit { .. })
    ));
    let first = graph.groups(&list_query(1, None)).unwrap();
    let cursor = first.next_cursor.clone().unwrap();
    let mut foreign = cursor.clone();
    foreign.snapshot = SnapshotId("other".into());
    assert_eq!(
        graph.groups(&list_query(1, Some(foreign))).unwrap_err(),
        GroupQueryError::ForeignCursor
    );
    let mismatched = GroupListQuery {
        snapshot: id.clone(),
        kind: Some(GroupKind::Directory),
        cursor: Some(cursor),
        limit: 1,
    };
    assert_eq!(
        graph.groups(&mismatched).unwrap_err(),
        GroupQueryError::InvalidCursor
    );
    assert!(matches!(
        graph.group_members(&GroupMembersQuery {
            snapshot: id.clone(),
            group: gid("nope"),
            cursor: None,
            limit: 1
        }),
        Err(GroupQueryError::UnknownGroup(_))
    ));
    assert!(matches!(
        graph.graph(&GraphQuery {
            snapshot: id.clone(),
            roots: vec![],
            direction: GraphDirection::Both,
            depth: 1,
            node_limit: 1,
            edge_limit: 1,
            filter: ImportFilter::default()
        }),
        Err(GroupQueryError::InvalidQuery(_))
    ));
    assert!(matches!(
        graph.graph(&GraphQuery {
            snapshot: id.clone(),
            roots: vec![path("nope.rs")],
            direction: GraphDirection::Both,
            depth: 1,
            node_limit: 1,
            edge_limit: 1,
            filter: ImportFilter::default()
        }),
        Err(GroupQueryError::UnknownFile(_))
    ));
    assert!(matches!(
        graph.graph(&GraphQuery {
            snapshot: id.clone(),
            roots: vec![path("loose/a.ts")],
            direction: GraphDirection::Both,
            depth: 99,
            node_limit: 1,
            edge_limit: 1,
            filter: ImportFilter::default()
        }),
        Err(GroupQueryError::InvalidQuery(_))
    ));
    let gone = GroupListQuery {
        snapshot: SnapshotId("gone".into()),
        kind: None,
        cursor: None,
        limit: 1,
    };
    assert!(matches!(
        graph.groups(&gone),
        Err(GroupQueryError::SnapshotNotFound(_))
    ));

    // Without an analyzer the group queries say so instead of returning nothing.
    let plain = service(dir.path());
    let plain_id = refresh(&plain).await;
    let query = GroupListQuery {
        snapshot: plain_id,
        kind: None,
        cursor: None,
        limit: 1,
    };
    assert_eq!(
        plain.groups(&query).unwrap_err(),
        GroupQueryError::NotConfigured
    );
}

struct Versioned(u32);
impl GroupRule for Versioned {
    fn id(&self) -> &'static str {
        "fixed-group"
    }
    fn version(&self) -> u32 {
        self.0
    }
    fn kind(&self) -> GroupKind {
        GroupKind::Directory
    }
    fn groups(&self, _: &GroupInput<'_>) -> Vec<Group> {
        vec![Group {
            id: gid("directory:custom"),
            kind: GroupKind::Directory,
            label: "custom".into(),
            basis: GroupBasis::Inferred,
            explanation: "supplied by a substitute rule".into(),
            evidence: vec![],
            cycle_sites: vec![],
            evidence_truncated: false,
            involves_type_only: false,
            members: vec![path("loose/a.ts")],
        }]
    }
}

#[tokio::test]
async fn rules_are_replaceable_and_versions_change_snapshot_identity() {
    let dir = tempdir().unwrap();
    groups_workspace(dir.path());
    let with_rule = |version| {
        builder(dir.path())
            .dependency_groups(Arc::new(DefaultGroupAnalyzer::with_rules(
                vec![Arc::new(Versioned(version))],
                Default::default(),
            )))
            .build()
    };
    let first = with_rule(1);
    let id1 = first.refresh(CancellationToken::new()).await.unwrap().id;
    assert_eq!(
        list(&first, &id1, None),
        vec![("directory:custom".into(), 1)]
    );
    let id2 = with_rule(2)
        .refresh(CancellationToken::new())
        .await
        .unwrap()
        .id;
    assert_ne!(id1, id2);

    // The standard analyzer is deterministic across services and differs from no analyzer.
    let a = refresh(&grouped_service(dir.path())).await;
    let b = refresh(&grouped_service(dir.path())).await;
    assert_eq!(a, b);
    assert_ne!(a, refresh(&service(dir.path())).await);
}

#[tokio::test]
async fn editing_imports_updates_cycles() {
    let dir = tempdir().unwrap();
    groups_workspace(dir.path());
    let graph = grouped_service(dir.path());
    let before = refresh(&graph).await;
    assert_eq!(list(&graph, &before, Some(GroupKind::ImportCycle)).len(), 3);
    write(dir.path(), "outer/src/b.rs", "pub struct B;\n");
    let after = refresh(&graph).await;
    assert_ne!(before, after);
    assert_eq!(list(&graph, &after, Some(GroupKind::ImportCycle)).len(), 2);
    // The earlier snapshot still answers with its own groups.
    assert_eq!(list(&graph, &before, Some(GroupKind::ImportCycle)).len(), 3);
}
