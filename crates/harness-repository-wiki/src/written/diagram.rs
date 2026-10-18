//! Component diagrams drawn from the index, never by a model, so they are right by construction.
//! Nodes are components (packages, folders), arrows are imports between them (thicker for more),
//! laid out in layers from what depends on nothing to what everything depends on. The SVG uses
//! theme variables with fallbacks, so it follows the app's colours, and is the same bytes for the
//! same index.
use std::collections::{BTreeMap, HashMap};

use harness_workspace::repository_intelligence::{RepositorySnapshot, RootRelativePath};
use serde::{Deserialize, Serialize};

const NODE_WIDTH: f64 = 176.0;
const NODE_HEIGHT: f64 = 50.0;
const LAYER_GAP: f64 = 120.0;
const ROW_GAP: f64 = 26.0;
const MARGIN: f64 = 24.0;
/// Components drawn before the smallest are merged into one node.
pub const MAX_NODES: usize = 18;
/// Arrows that get a count next to them.
const LABELLED_EDGES: usize = 12;

#[derive(Clone, Debug)]
pub struct DiagramNode {
    pub label: String,
    pub members: Vec<RootRelativePath>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagram {
    pub key: String,
    pub title: String,
    pub svg: String,
    pub nodes: usize,
    pub edges: usize,
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(max - 1).collect::<String>())
    }
}

/// Imports between nodes, counted by the node that holds each file: `(importer, imported)`.
pub fn component_weights(
    snapshot: &RepositorySnapshot,
    nodes: &[DiagramNode],
) -> BTreeMap<(usize, usize), usize> {
    let owner: HashMap<&RootRelativePath, usize> = nodes
        .iter()
        .enumerate()
        .flat_map(|(i, n)| n.members.iter().map(move |m| (m, i)))
        .collect();
    let mut out: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for edge in &snapshot.imports.edges {
        let Some(target) = edge.resolution.target_file() else {
            continue;
        };
        if let (Some(&from), Some(&to)) = (owner.get(&edge.fact.importing_file), owner.get(target))
        {
            if from != to {
                *out.entry((from, to)).or_default() += 1;
            }
        }
    }
    out
}

/// Layers: a node sits after everything it imports from. Cycles are broken at the edge that closes
/// them (drawn dashed), in label order so the result is stable.
fn layers(
    count: usize,
    labels: &[&str],
    edges: &BTreeMap<(usize, usize), usize>,
) -> (Vec<usize>, Vec<(usize, usize)>) {
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|i| labels[*i]);
    let mut adjacent: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (from, to) in edges.keys() {
        adjacent[*from].push(*to);
    }
    for list in &mut adjacent {
        list.sort_by_key(|i| labels[*i]);
    }
    // Depth-first from every node: an edge into a node still on the stack closes a cycle.
    let (mut state, mut back, mut post) = (vec![0u8; count], Vec::new(), Vec::new());
    for &start in &order {
        if state[start] != 0 {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        state[start] = 1;
        while let Some((node, next)) = stack.pop() {
            if next < adjacent[node].len() {
                stack.push((node, next + 1));
                let target = adjacent[node][next];
                match state[target] {
                    0 => {
                        state[target] = 1;
                        stack.push((target, 0));
                    }
                    1 => back.push((node, target)),
                    _ => {}
                }
            } else {
                state[node] = 2;
                post.push(node);
            }
        }
    }
    // Longest path in the acyclic graph, from the nodes that import nothing (layer 0) onwards.
    let mut layer = vec![0usize; count];
    for &node in &post {
        for &target in &adjacent[node] {
            if !back.contains(&(node, target)) {
                layer[node] = layer[node].max(layer[target] + 1);
            }
        }
    }
    (layer, back)
}

/// A dependency diagram of `nodes`, imports pointing from the importer to what it imports.
pub fn dependency_diagram(
    snapshot: &RepositorySnapshot,
    key: &str,
    title: &str,
    nodes: &[DiagramNode],
) -> Diagram {
    let edges = component_weights(snapshot, nodes);
    let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
    let (layer, back) = layers(nodes.len(), &labels, &edges);
    // Importers on the left, what they import on the right: flip so layer 0 (imports nothing) is rightmost.
    let top = layer.iter().copied().max().unwrap_or(0);
    let column = |i: usize| top - layer[i];
    let mut rows: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..nodes.len() {
        rows.entry(column(i)).or_default().push(i);
    }
    let mut position = vec![(0.0, 0.0); nodes.len()];
    let mut height: f64 = 0.0;
    for (col, members) in &mut rows {
        members.sort_by(|a, b| {
            nodes[*b]
                .members
                .len()
                .cmp(&nodes[*a].members.len())
                .then(labels[*a].cmp(labels[*b]))
        });
        for (row, i) in members.iter().enumerate() {
            let x = MARGIN + *col as f64 * (NODE_WIDTH + LAYER_GAP);
            let y = MARGIN + row as f64 * (NODE_HEIGHT + ROW_GAP);
            position[*i] = (x, y);
            height = height.max(y + NODE_HEIGHT);
        }
    }
    let width = MARGIN * 2.0 + (top as f64 + 1.0) * NODE_WIDTH + top as f64 * LAYER_GAP;
    let height = height + MARGIN;

    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width:.0} {height:.0}\" width=\"{width:.0}\" height=\"{height:.0}\" role=\"img\" aria-label=\"{}\">\n",
        escape(title)
    );
    svg.push_str("<defs><marker id=\"arrow\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"7\" markerHeight=\"7\" orient=\"auto-start-reverse\"><path d=\"M 0 0 L 10 5 L 0 10 z\" fill=\"var(--color-fg-muted, #888)\"/></marker></defs>\n");
    let mut heaviest: Vec<(&(usize, usize), &usize)> = edges.iter().collect();
    heaviest.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let labelled: Vec<(usize, usize)> = heaviest
        .iter()
        .take(LABELLED_EDGES)
        .map(|(k, _)| **k)
        .collect();
    for ((from, to), weight) in &edges {
        let ((x1, y1), (x2, y2)) = (position[*from], position[*to]);
        let backwards = back.contains(&(*from, *to));
        // Leave from the side facing the target, so arrows do not cross their own node.
        let (sx, tx) = if x1 <= x2 {
            (x1 + NODE_WIDTH, x2)
        } else {
            (x1, x2 + NODE_WIDTH)
        };
        let (sy, ty) = (y1 + NODE_HEIGHT / 2.0, y2 + NODE_HEIGHT / 2.0);
        let bend = ((tx - sx).abs() / 2.0).max(30.0) * if tx >= sx { 1.0 } else { -1.0 };
        let stroke = (1.0 + (*weight as f64).log2()).clamp(1.0, 5.0);
        svg.push_str(&format!(
            "<path d=\"M {sx:.1} {sy:.1} C {:.1} {sy:.1} {:.1} {ty:.1} {tx:.1} {ty:.1}\" fill=\"none\" stroke=\"var(--color-fg-muted, #888)\" stroke-opacity=\"0.65\" stroke-width=\"{stroke:.1}\"{} marker-end=\"url(#arrow)\"><title>{} imports {} ({weight} imports)</title></path>\n",
            sx + bend,
            tx - bend,
            if backwards { " stroke-dasharray=\"6 4\"" } else { "" },
            escape(labels[*from]),
            escape(labels[*to]),
        ));
        if labelled.contains(&(*from, *to)) {
            svg.push_str(&format!("<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"11\" fill=\"var(--color-fg-muted, #888)\">{weight}</text>\n", (sx + tx) / 2.0, (sy + ty) / 2.0 - 5.0));
        }
    }
    for (i, node) in nodes.iter().enumerate() {
        let (x, y) = position[i];
        svg.push_str(&format!(
            "<g><title>{} — {} files</title><rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{NODE_WIDTH}\" height=\"{NODE_HEIGHT}\" rx=\"8\" fill=\"var(--color-surface-elevated, #222)\" stroke=\"var(--color-border-default, #555)\"/><text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"13\" font-weight=\"600\" fill=\"var(--color-fg-strong, #eee)\">{}</text><text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"11\" fill=\"var(--color-fg-muted, #888)\">{} files</text></g>\n",
            escape(&node.label),
            node.members.len(),
            x + NODE_WIDTH / 2.0,
            y + 21.0,
            escape(&shorten(&node.label, 22)),
            x + NODE_WIDTH / 2.0,
            y + 38.0,
            node.members.len(),
        ));
    }
    svg.push_str("</svg>");
    Diagram {
        key: key.to_owned(),
        title: title.to_owned(),
        svg,
        nodes: nodes.len(),
        edges: edges.len(),
    }
}

/// The largest `MAX_NODES - 1` nodes and one node holding the rest.
pub fn limit_nodes(mut nodes: Vec<DiagramNode>) -> Vec<DiagramNode> {
    if nodes.len() <= MAX_NODES {
        return nodes;
    }
    nodes.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(a.label.cmp(&b.label))
    });
    let rest = nodes.split_off(MAX_NODES - 1);
    nodes.push(DiagramNode {
        label: format!("other ({})", rest.len()),
        members: rest.into_iter().flat_map(|n| n.members).collect(),
    });
    nodes
}
