use super::Pass;
use crate::compile_graph::{CompileGraph, CompileNode, LinkType, NodeIdx, NodeState, NodeType};
use crate::passes::AnalysisInfos;
use crate::{CompilerInput, CompilerOptions};
use itertools::Itertools;
use mchprs_blocks::blocks::ComparatorMode;
use mchprs_world::World;
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use tracing::trace;

pub struct CircuitNormalization;

impl<W: World> Pass<W> for CircuitNormalization {
    fn run_pass(
        &self,
        graph: &mut CompileGraph,
        _: &CompilerOptions,
        _: &CompilerInput<'_, W>,
        _: &mut AnalysisInfos,
    ) {
        let node_idxs = graph.node_indices().collect_vec();
        loop {
            let mut num_converted = 0;
            for idx in &node_idxs {
                if try_convert_comparator_to_torch(graph, *idx) {
                    num_converted += 1;
                }
            }
            if num_converted == 0 {
                break;
            } else {
                trace!("Converted {} comparators to torches", num_converted);
            }
        }
        // loop {
        //     let mut num_converted = 0;
        //     for idx in &node_idxs {
        //         if try_swap_torch_in_front_of_repeater(graph, *idx) {
        //             num_converted += 1;
        //         }
        //     }
        //     if num_converted == 0 {
        //         break;
        //     }
        // }
        // loop {
        //     let mut num_converted = 0;
        //     for idx in &node_idxs {
        //         num_converted += try_convert_repeater_to_torch(graph, *idx);
        //     }
        //     if num_converted == 0 {
        //         break;
        //     } else {
        //         trace!("Converted {} repeaters to torches", num_converted);
        //     }
        // }
    }

    fn should_run(&self, options: &CompilerOptions) -> bool {
        options.io_only && options.optimize
    }

    fn status_message(&self) -> &'static str {
        "Normalizing circuits"
    }
}

fn try_convert_comparator_to_torch(graph: &mut CompileGraph, idx: NodeIdx) -> bool {
    if !matches!(
        graph[idx].ty,
        NodeType::Comparator {
            mode: ComparatorMode::Subtract,
            far_input: _,
            facing_diode: false
        }
    ) {
        return false;
    }

    let Some(constant_input) = get_constant_input(graph, idx, LinkType::Default) else {
        return false;
    };

    let mut min_side_input_strength = 15;
    for side_edge in graph
        .edges_directed(idx, Direction::Incoming)
        .filter(|e| e.weight().ty == LinkType::Side)
    {
        if matches!(graph[side_edge.source()].ty, NodeType::Comparator { .. }) {
            return false; // Cannot deduce min input strength of a comparator.
        }
        let input_strength = 15u8.saturating_sub(side_edge.weight().ss);
        min_side_input_strength = min_side_input_strength.min(input_strength);
    }

    if min_side_input_strength < constant_input {
        return false; // One of the side inputs is not strong enough to turn off the comparator.
    }

    for output_edge in graph.edges_directed(idx, Direction::Outgoing) {
        if constant_input != 15
            && matches!(graph[output_edge.source()].ty, NodeType::Comparator { .. })
        {
            return false; // Converting to torch changes the output power and one the the outputs is analog.
        }
        if constant_input < output_edge.weight().ss && 15 >= output_edge.weight().ss {
            return false; // The conversion causes a previously unreachable connection to be reachable.
        }
    }

    // It's safe to convert this comparator to a torch now.
    remove_incoming_edges(graph, idx, LinkType::Default);
    convert_side_edges_to_default_edges(graph, idx);
    let input_nodes = graph
        .edges_directed(idx, Direction::Incoming)
        .map(|e| e.source())
        .collect_vec();
    let mut torch_is_lit = true;
    for input_node_idx in input_nodes {
        let input_node = &mut graph[input_node_idx];
        match &mut input_node.ty {
            NodeType::Repeater { facing_diode, .. } => *facing_diode = false,
            NodeType::Comparator { facing_diode, .. } => *facing_diode = false,
            _ => (),
        }
        if input_node.state.output_strength != 0 {
            torch_is_lit = false; // TODO: make sure the signal strength is high enough to connect.
        }
    }
    graph[idx].ty = NodeType::Torch;
    graph[idx].state = NodeState::simple(torch_is_lit);
    true
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

fn has_side_inputs(graph: &CompileGraph, idx: NodeIdx) -> bool {
    graph
        .edges_directed(idx, Direction::Incoming)
        .any(|e| e.weight().ty == LinkType::Side)
}

fn try_convert_repeater_to_torch(graph: &mut CompileGraph, idx: NodeIdx) -> usize {
    if graph[idx].ty != NodeType::Torch {
        return 0;
    }
    let mut converted = 0;
    let repeater1_idxs = graph
        .neighbors_directed(idx, Direction::Outgoing)
        .filter(|&target_idx| is_non_diode_facing_1_tick_repeater(&graph[target_idx]))
        .filter(|&target_idx| !has_side_inputs(graph, target_idx))
        .filter(|&target_idx| has_exactly_one_input(graph, target_idx, LinkType::Default))
        .collect_vec();
    'repeater1: for repeater1_idx in repeater1_idxs {
        let repeater2_idxs = graph
            .neighbors_directed(repeater1_idx, Direction::Outgoing)
            .collect_vec();
        for &repeater2_idx in &repeater2_idxs {
            if !is_non_diode_facing_1_tick_repeater(&graph[repeater2_idx]) {
                continue 'repeater1;
            }
            if has_side_inputs(graph, repeater2_idx) {
                continue 'repeater1;
            }
            if !has_exactly_one_input(graph, repeater2_idx, LinkType::Default) {
                continue 'repeater1;
            }
        }
        converted += repeater2_idxs.len() + 1;
        for reapeter2_idx in repeater2_idxs {
            graph[reapeter2_idx].ty = NodeType::Torch;
        }
        graph[repeater1_idx].ty = NodeType::Torch;
        graph[repeater1_idx].state = NodeState::simple(!graph[repeater1_idx].state.powered);
    }
    converted
}

/// Returns `None` if at least one inputs is non-const.
fn get_constant_input(graph: &CompileGraph, idx: NodeIdx, link_type: LinkType) -> Option<u8> {
    let mut constant_input = 0;
    for input_edge in graph
        .edges_directed(idx, Direction::Incoming)
        .filter(|e| e.weight().ty == link_type)
    {
        let input_node = &graph[input_edge.source()];
        if input_node.ty == NodeType::Constant {
            let input_strength = 15u8.saturating_sub(input_edge.weight().ss);
            constant_input = constant_input.max(input_strength);
        } else {
            return None;
        }
    }
    Some(constant_input)
}

fn has_exactly_one_input(graph: &CompileGraph, idx: NodeIdx, link_type: LinkType) -> bool {
    graph
        .edges_directed(idx, Direction::Incoming)
        .filter(|e| e.weight().ty == link_type)
        .exactly_one()
        .is_ok()
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

fn remove_incoming_edges(graph: &mut CompileGraph, idx: NodeIdx, link_type: LinkType) {
    let to_be_removed = graph
        .edges_directed(idx, Direction::Incoming)
        .filter(|e| e.weight().ty == link_type)
        .map(|e| e.id())
        .collect_vec();
    for edge in to_be_removed {
        graph.remove_edge(edge);
    }
}

fn convert_side_edges_to_default_edges(graph: &mut CompileGraph, idx: NodeIdx) {
    let to_be_converted = graph
        .edges_directed(idx, Direction::Incoming)
        .filter(|e| e.weight().ty == LinkType::Side)
        .map(|e| e.id())
        .collect_vec();
    for edge in to_be_converted {
        graph[edge].ty = LinkType::Default;
    }
}
