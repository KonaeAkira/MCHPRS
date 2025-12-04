use super::Pass;
use crate::compile_graph::{CompileGraph, CompileNode, LinkType, NodeIdx, NodeState, NodeType};
use crate::passes::AnalysisInfos;
use crate::{CompilerInput, CompilerOptions};
use itertools::Itertools;
use mchprs_world::World;
use petgraph::visit::{EdgeRef, NodeIndexable};
use petgraph::Direction;
use tracing::trace;

pub struct Coalesce;

impl<W: World> Pass<W> for Coalesce {
    fn run_pass(
        &self,
        graph: &mut CompileGraph,
        _: &CompilerOptions,
        _: &CompilerInput<'_, W>,
        _: &mut AnalysisInfos,
    ) {
        loop {
            let num_coalesced = run_coalescing_iteration(graph);
            if num_coalesced > 0 {
                trace!("Iteration coalesced {} nodes", num_coalesced);
                continue;
            }
            let num_normalized = run_normalization_iteration(graph);
            if num_normalized > 0 {
                trace!("Iteration normalized {} nodes", num_normalized);
            } else {
                break;
            }
        }
    }

    fn status_message(&self) -> &'static str {
        "Combining duplicate logic"
    }
}

fn run_coalescing_iteration(graph: &mut CompileGraph) -> usize {
    let mut num_coalesced = 0;
    for i in 0..graph.node_bound() {
        let idx = NodeIdx::new(i);
        if !graph.contains_node(idx) {
            continue;
        }

        let node = &graph[idx];
        // Comparators depend on the link weight as well as the type,
        // we could implement that later if it's beneficial enough.
        if matches!(node.ty, NodeType::Comparator { .. }) || !node.is_removable() {
            continue;
        }

        let Ok(edge) = graph.edges_directed(idx, Direction::Incoming).exactly_one() else {
            continue;
        };

        if edge.weight().ty != LinkType::Default {
            continue;
        }

        let source = edge.source();
        // Comparators might output less than 15 ss
        if matches!(graph[source].ty, NodeType::Comparator { .. }) {
            continue;
        }
        num_coalesced += coalesce_outgoing(graph, source, idx);
    }
    num_coalesced
}

fn coalesce_outgoing(graph: &mut CompileGraph, source_idx: NodeIdx, into_idx: NodeIdx) -> usize {
    let mut num_coalesced = 0;
    let mut walk_outgoing = graph
        .neighbors_directed(source_idx, Direction::Outgoing)
        .detach();
    while let Some(edge_idx) = walk_outgoing.next_edge(graph) {
        let dest_idx = graph.edge_endpoints(edge_idx).unwrap().1;
        if dest_idx == into_idx {
            continue;
        }

        let dest = &graph[dest_idx];
        let into = &graph[into_idx];

        if dest.ty == into.ty
            && dest.is_removable()
            && graph
                .neighbors_directed(dest_idx, Direction::Incoming)
                .count()
                == 1
        {
            coalesce(graph, dest_idx, into_idx);
            num_coalesced += 1;
        }
    }
    num_coalesced
}

fn coalesce(graph: &mut CompileGraph, node: NodeIdx, into: NodeIdx) {
    let mut walk_outgoing: petgraph::stable_graph::WalkNeighbors<u32> =
        graph.neighbors_directed(node, Direction::Outgoing).detach();
    while let Some(edge_idx) = walk_outgoing.next_edge(graph) {
        let dest = graph.edge_endpoints(edge_idx).unwrap().1;
        let weight = graph.remove_edge(edge_idx).unwrap();
        graph.add_edge(into, dest, weight);
    }
    graph.remove_node(node);
}

fn run_normalization_iteration(graph: &mut CompileGraph) -> usize {
    let mut num_normalized = 0;
    let idxs = graph.node_indices().collect_vec();
    for idx in idxs {
        let self_is_torch = graph[idx].ty == NodeType::Torch;
        let target_idxs = graph
            .neighbors_directed(idx, Direction::Outgoing)
            .collect_vec();
        for target_idx in target_idxs {
            if try_swap_torch_in_front_of_repeater(graph, target_idx) {
                num_normalized += 1;
            } else if self_is_torch && try_convert_repeater_to_torch(graph, target_idx) {
                num_normalized += 1;
            }
        }
    }
    num_normalized
}

fn try_swap_torch_in_front_of_repeater(graph: &mut CompileGraph, idx: NodeIdx) -> bool {
    if !is_non_diode_facing_1_tick_repeater(&graph[idx]) {
        return false;
    }
    if has_side_inputs(graph, idx) {
        return false; // Don't swap if this repeater could become locked.
    }
    let target_idxs = graph
        .edges_directed(idx, Direction::Outgoing)
        .map(|e| e.target())
        .collect_vec();
    for &target_idx in &target_idxs {
        if graph[target_idx].ty != NodeType::Torch {
            return false; // Don't swap if one of the target nodes is not a torch.
        }
        if !has_exactly_one_input(graph, target_idx, LinkType::Default) {
            return false; // Don't swap if this node is not the only input of the target node.
        }
    }
    for target_idx in target_idxs {
        graph[target_idx].ty = NodeType::Repeater {
            delay: 1,
            facing_diode: false,
        };
    }
    graph[idx].ty = NodeType::Torch;
    graph[idx].state = NodeState::simple(!graph[idx].state.powered);
    true
}

fn try_convert_repeater_to_torch(graph: &mut CompileGraph, idx: NodeIdx) -> bool {
    if !is_non_diode_facing_1_tick_repeater(&graph[idx]) {
        return false;
    }
    if has_side_inputs(graph, idx) {
        return false;
    }
    if !has_exactly_one_input(graph, idx, LinkType::Default) {
        return false;
    }

    let target_idxs = graph
        .neighbors_directed(idx, Direction::Outgoing)
        .collect_vec();
    for &target_idx in &target_idxs {
        if !is_non_diode_facing_1_tick_repeater(&graph[target_idx]) {
            return false;
        }
        if has_side_inputs(graph, target_idx) {
            return false;
        }
        if !has_exactly_one_input(graph, target_idx, LinkType::Default) {
            return false;
        }
    }

    for target_idx in target_idxs {
        graph[target_idx].ty = NodeType::Torch;
    }
    graph[idx].ty = NodeType::Torch;
    graph[idx].state = NodeState::simple(!graph[idx].state.powered);
    true
}

fn has_side_inputs(graph: &CompileGraph, idx: NodeIdx) -> bool {
    graph
        .edges_directed(idx, Direction::Incoming)
        .any(|e| e.weight().ty == LinkType::Side)
}

fn is_non_diode_facing_1_tick_repeater(node: &CompileNode) -> bool {
    matches!(
        node.ty,
        NodeType::Repeater {
            delay: 1,
            facing_diode: false,
        }
    )
}

fn has_exactly_one_input(graph: &CompileGraph, idx: NodeIdx, link_type: LinkType) -> bool {
    graph
        .edges_directed(idx, Direction::Incoming)
        .filter(|e| e.weight().ty == link_type)
        .exactly_one()
        .is_ok()
}
