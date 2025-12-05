use crate::backend::direct::node_inputs::{AnalogInput, DigitalInput};
use crate::backend::direct::node_type::{
    ComparatorProperties, NodeType, NoteblockProperties, RepeaterProperties,
};
use crate::compile_graph::{CompileGraph, LinkType, NodeIdx};
use crate::{CompilerOptions, TaskMonitor};
use itertools::Itertools;
use mchprs_blocks::blocks::{Block, Instrument};
use mchprs_blocks::BlockPos;
use mchprs_world::TickEntry;
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use tracing::trace;

use super::node::{ForwardLink, Node, NodeId, Nodes};
use super::DirectBackend;

#[derive(Debug, Default)]
struct FinalGraphStats {
    update_link_count: usize,
    side_link_count: usize,
    default_link_count: usize,
    nodes_bytes: usize,
}

fn compile_node(
    graph: &CompileGraph,
    node_idx: NodeIdx,
    nodes_len: usize,
    nodes_map: &FxHashMap<NodeIdx, usize>,
    noteblock_info: &mut Vec<(BlockPos, Instrument, u32)>,
    forward_links: &mut Vec<ForwardLink>,
    analog_inputs: &mut Vec<AnalogInput>,
    stats: &mut FinalGraphStats,
) -> Node {
    let node = &graph[node_idx];

    const MAX_INPUTS: usize = 255;

    let mut default_input_count = 0;
    let mut side_input_count = 0;

    let mut digital_input = DigitalInput::default();
    let mut analog_input = AnalogInput::default();

    for edge in graph.edges_directed(node_idx, Direction::Incoming) {
        let weight = edge.weight();
        let distance = weight.ss;
        let source = edge.source();
        let ss = graph[source].state.output_strength.saturating_sub(distance);
        match weight.ty {
            LinkType::Default => {
                if default_input_count >= MAX_INPUTS {
                    panic!(
                        "Exceeded the maximum number of default inputs {}",
                        MAX_INPUTS
                    );
                }
                default_input_count += 1;
            }
            LinkType::Side => {
                if side_input_count >= MAX_INPUTS {
                    panic!("Exceeded the maximum number of side inputs {}", MAX_INPUTS);
                }
                side_input_count += 1;
            }
        }
        if ss != 0 {
            digital_input.set(weight.ty, true);
            analog_input.set(weight.ty, 0, ss);
        }
    }
    stats.default_link_count += default_input_count;
    stats.side_link_count += side_input_count;

    use crate::compile_graph::NodeType as CNodeType;
    let fwd_link_begin = forward_links.len();
    if node.ty != CNodeType::Constant {
        let new_links = graph
            .edges_directed(node_idx, Direction::Outgoing)
            .sorted_by_key(|edge| nodes_map[&edge.target()])
            .into_group_map_by(|edge| std::mem::discriminant(&graph[edge.target()].ty))
            .into_values()
            .flatten()
            .map(|edge| unsafe {
                let idx = edge.target();
                let idx = nodes_map[&idx];
                assert!(idx < nodes_len);
                // Safety: bounds checked
                let target_id = NodeId::from_index(idx);
                let weight = edge.weight();
                ForwardLink::new()
                    .with_target(target_id)
                    .with_ty(weight.ty)
                    .with_distance(weight.ss)
            });
        forward_links.extend(new_links);
    };
    let fwd_link_end = forward_links.len();
    stats.update_link_count += fwd_link_end - fwd_link_begin;

    let (node_type, properties) = match &node.ty {
        CNodeType::Repeater {
            delay,
            facing_diode,
        } => (
            NodeType::Repeater,
            RepeaterProperties::new()
                .with_delay(*delay)
                .with_facing_diode(*facing_diode)
                .with_locked(node.state.repeater_locked)
                .into_bits(),
        ),
        CNodeType::Torch => (NodeType::Torch, 0),
        CNodeType::Comparator {
            mode,
            far_input,
            facing_diode,
        } => (
            NodeType::Comparator,
            ComparatorProperties::new()
                .with_mode(*mode)
                .with_has_far_input(far_input.is_some())
                .with_far_input(far_input.unwrap_or(0))
                .with_facing_diode(*facing_diode)
                .into_bits(),
        ),
        CNodeType::Lamp => (NodeType::Lamp, 0),
        CNodeType::Button => (NodeType::Button, 0),
        CNodeType::Lever => (NodeType::Lever, 0),
        CNodeType::PressurePlate => (NodeType::PressurePlate, 0),
        CNodeType::Trapdoor => (NodeType::Trapdoor, 0),
        CNodeType::Wire => (NodeType::Wire, 0),
        CNodeType::Constant => (NodeType::Constant, 0),
        CNodeType::NoteBlock { instrument, note } => {
            let noteblock_id = noteblock_info.len().try_into().unwrap();
            noteblock_info.push((node.block.unwrap().0, *instrument, *note));
            (
                NodeType::NoteBlock,
                NoteblockProperties::new()
                    .with_noteblock_id(noteblock_id)
                    .into_bits(),
            )
        }
    };

    let analog_input_idx = if node_type.is_analog() {
        analog_inputs.push(analog_input);
        analog_inputs.len() - 1
    } else {
        0
    };

    Node::new()
        .with_ty(node_type)
        .with_type_specific_properties(properties)
        .with_powered(node.state.powered)
        .with_is_io(node.is_input || node.is_output)
        .with_output_power(node.state.output_strength)
        .with_digital_input(digital_input)
        .with_fwd_link_begin(fwd_link_begin as u32)
        .with_fwd_link_count((fwd_link_end - fwd_link_begin) as u16)
        .with_analog_input_idx(analog_input_idx as u32)
}

pub fn compile(
    backend: &mut DirectBackend,
    graph: CompileGraph,
    ticks: Vec<TickEntry>,
    options: &CompilerOptions,
    _monitor: Arc<TaskMonitor>,
) {
    // Create a mapping from compile to backend node indices
    let mut nodes_map = FxHashMap::with_capacity_and_hasher(graph.node_count(), Default::default());
    for node in graph.node_indices() {
        nodes_map.insert(node, nodes_map.len());
    }
    let nodes_len = nodes_map.len();

    // Lower nodes
    let mut stats = FinalGraphStats::default();
    let nodes = graph
        .node_indices()
        .map(|idx| {
            compile_node(
                &graph,
                idx,
                nodes_len,
                &nodes_map,
                &mut backend.noteblock_info,
                &mut backend.forward_links,
                &mut backend.analog_inputs,
                &mut stats,
            )
        })
        .collect();
    stats.nodes_bytes = nodes_len * std::mem::size_of::<Node>();
    trace!("{:#?}", stats);

    backend.blocks = graph
        .node_weights()
        .map(|node| node.block.map(|(pos, id)| (pos, Block::from_id(id))))
        .collect();
    backend.nodes = Nodes::new(nodes);

    // Create a mapping from block pos to backend NodeId
    for i in 0..backend.blocks.len() {
        if let Some((pos, _)) = backend.blocks[i] {
            backend.pos_map.insert(pos, backend.nodes.get(i));
        }
    }

    // Schedule backend ticks
    for entry in ticks {
        if let Some(node) = backend.pos_map.get(&entry.pos) {
            backend
                .scheduler
                .schedule_tick(*node, entry.ticks_left as usize, entry.tick_priority);
            backend.nodes[*node].set_pending_tick(true);
        }
    }

    // Dot file output
    if options.export_dot_graph {
        std::fs::write("backend_graph.dot", format!("{}", backend)).unwrap();
    }
}
