//! Strongly connected components with an iterative Tarjan, safe for very deep import chains.

const UNVISITED: u32 = u32::MAX;

/// Components of the directed graph `adjacency` (node → successors), each sorted ascending,
/// returned in ascending order of their smallest node.
pub(crate) fn strongly_connected_components(adjacency: &[Vec<u32>]) -> Vec<Vec<u32>> {
    let mut search = Search::new(adjacency);
    for start in 0..adjacency.len() as u32 {
        if search.index_of[start as usize] == UNVISITED {
            search.explore_from(start);
        }
    }
    let mut components = search.components;
    components.sort_by_key(|component| component[0]);
    components
}

/// The state of one Tarjan run. The recursion is replaced by an explicit list of frames so a chain
/// of 200,000 imports does not overflow the call stack.
struct Search<'a> {
    adjacency: &'a [Vec<u32>],
    index_of: Vec<u32>,
    low_link: Vec<u32>,
    on_stack: Vec<bool>,
    stack: Vec<u32>,
    components: Vec<Vec<u32>>,
    next_index: u32,
}

impl<'a> Search<'a> {
    fn new(adjacency: &'a [Vec<u32>]) -> Self {
        let node_count = adjacency.len();
        Self {
            adjacency,
            index_of: vec![UNVISITED; node_count],
            low_link: vec![0; node_count],
            on_stack: vec![false; node_count],
            stack: Vec::new(),
            components: Vec::new(),
            next_index: 0,
        }
    }

    /// Visits everything reachable from `start`. A frame is a node and the position of the next
    /// successor to look at.
    fn explore_from(&mut self, start: u32) {
        let mut frames = vec![(start, 0usize)];
        self.enter(start);
        while let Some(&mut (node, ref mut cursor)) = frames.last_mut() {
            let successor = self.adjacency[node as usize].get(*cursor).copied();
            match successor {
                Some(successor) => {
                    *cursor += 1;
                    if self.index_of[successor as usize] == UNVISITED {
                        self.enter(successor);
                        frames.push((successor, 0));
                    } else if self.on_stack[successor as usize] {
                        self.lower_link(node, self.index_of[successor as usize]);
                    }
                }
                None => {
                    frames.pop();
                    if let Some(&(parent, _)) = frames.last() {
                        self.lower_link(parent, self.low_link[node as usize]);
                    }
                    self.close_if_root(node);
                }
            }
        }
    }

    fn enter(&mut self, node: u32) {
        self.index_of[node as usize] = self.next_index;
        self.low_link[node as usize] = self.next_index;
        self.next_index += 1;
        self.stack.push(node);
        self.on_stack[node as usize] = true;
    }

    fn lower_link(&mut self, node: u32, candidate: u32) {
        let low = &mut self.low_link[node as usize];
        *low = (*low).min(candidate);
    }

    /// A node whose link never dropped below its own index is the root of a component: everything
    /// above it on the stack belongs with it.
    fn close_if_root(&mut self, node: u32) {
        if self.low_link[node as usize] != self.index_of[node as usize] {
            return;
        }
        let mut component = Vec::new();
        while let Some(member) = self.stack.pop() {
            self.on_stack[member as usize] = false;
            component.push(member);
            if member == node {
                break;
            }
        }
        component.sort_unstable();
        self.components.push(component);
    }
}

#[cfg(test)]
mod tests {
    use super::strongly_connected_components;

    #[test]
    fn finds_cycles_self_loops_and_singletons() {
        // 0 -> 1 -> 2 -> 0, 2 -> 3, 3 -> 3, 4 isolated.
        let adjacency = vec![vec![1], vec![2], vec![0, 3], vec![3], vec![]];
        assert_eq!(
            strongly_connected_components(&adjacency),
            vec![vec![0, 1, 2], vec![3], vec![4]]
        );
    }

    #[test]
    fn handles_a_chain_deeper_than_the_call_stack() {
        let depth = 200_000u32;
        let mut adjacency = (0..depth).map(|n| vec![n + 1]).collect::<Vec<_>>();
        adjacency.push(vec![0]);
        let components = strongly_connected_components(&adjacency);
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].len(), depth as usize + 1);
    }

    #[test]
    fn two_disjoint_cycles_are_separate_components() {
        let adjacency = vec![vec![1], vec![0], vec![3], vec![2]];
        assert_eq!(
            strongly_connected_components(&adjacency),
            vec![vec![0, 1], vec![2, 3]]
        );
    }
}
