//! Strongly connected components with an iterative Tarjan, safe for very deep import chains.

/// Components of the directed graph `adjacency` (node → successors), each sorted ascending,
/// returned in ascending order of their smallest node.
pub(crate) fn strongly_connected_components(adjacency: &[Vec<u32>]) -> Vec<Vec<u32>> {
    let node_count = adjacency.len();
    let unvisited = u32::MAX;
    let mut index_of = vec![unvisited; node_count];
    let mut low_link = vec![0u32; node_count];
    let mut on_stack = vec![false; node_count];
    let mut stack: Vec<u32> = Vec::new();
    let mut components: Vec<Vec<u32>> = Vec::new();
    let mut next_index = 0u32;

    for start in 0..node_count as u32 {
        if index_of[start as usize] != unvisited {
            continue;
        }
        // Each frame is (node, position of the next successor to visit).
        let mut frames = vec![(start, 0usize)];
        index_of[start as usize] = next_index;
        low_link[start as usize] = next_index;
        next_index += 1;
        stack.push(start);
        on_stack[start as usize] = true;
        while let Some(&mut (node, ref mut cursor)) = frames.last_mut() {
            if let Some(&successor) = adjacency[node as usize].get(*cursor) {
                *cursor += 1;
                if index_of[successor as usize] == unvisited {
                    index_of[successor as usize] = next_index;
                    low_link[successor as usize] = next_index;
                    next_index += 1;
                    stack.push(successor);
                    on_stack[successor as usize] = true;
                    frames.push((successor, 0));
                } else if on_stack[successor as usize] {
                    low_link[node as usize] =
                        low_link[node as usize].min(index_of[successor as usize]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low_link[parent as usize] = low_link[parent as usize].min(low_link[node as usize]);
            }
            if low_link[node as usize] == index_of[node as usize] {
                let mut component = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member as usize] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                component.sort_unstable();
                components.push(component);
            }
        }
    }
    components.sort_by_key(|component| component[0]);
    components
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
