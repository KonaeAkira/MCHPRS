//! The direct backend does not do code generation and operates on the `CompileNode` graph directly

mod compile;
mod node;
mod node_inputs;
mod node_type;
mod tick;
mod update;

use super::JITBackend;
use crate::backend::direct::node::ForwardLink;
use crate::backend::direct::node_inputs::AnalogInput;
use crate::backend::direct::node_type::NodeType;
use crate::compile_graph::{CompileGraph, LinkType};
use crate::task_monitor::TaskMonitor;
use crate::{block_powered_mut, CompilerOptions};
use mchprs_blocks::block_entities::BlockEntity;
use mchprs_blocks::blocks::{Block, ComparatorMode, Instrument};
use mchprs_blocks::BlockPos;
use mchprs_redstone::{bool_to_ss, noteblock};
use mchprs_world::{TickEntry, TickPriority, World};
use node::{Node, NodeId, Nodes};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::{fmt, mem};
use tracing::{debug, warn};

#[derive(Default, Clone)]
struct Queues([Vec<NodeId>; TickScheduler::NUM_PRIORITIES]);

impl Queues {
    fn drain_iter(&mut self) -> impl Iterator<Item = NodeId> + '_ {
        let [q0, q1, q2, q3] = &mut self.0;
        let [q0, q1, q2, q3] = [q0, q1, q2, q3].map(|q| q.drain(..));
        q0.chain(q1).chain(q2).chain(q3)
    }
}

#[derive(Default)]
struct TickScheduler {
    queues_deque: [Queues; Self::NUM_QUEUES],
    pos: usize,
}

impl TickScheduler {
    const NUM_PRIORITIES: usize = 4;
    const NUM_QUEUES: usize = 16;

    fn reset<W: World>(&mut self, world: &mut W, blocks: &[Option<(BlockPos, Block)>]) {
        for (idx, queues) in self.queues_deque.iter().enumerate() {
            let delay = if self.pos >= idx {
                idx + Self::NUM_QUEUES
            } else {
                idx
            } - self.pos;
            for (entries, priority) in queues.0.iter().zip(Self::priorities()) {
                for node in entries {
                    let Some((pos, _)) = blocks[node.index()] else {
                        warn!("Cannot schedule tick for node {:?} because block information is missing", node);
                        continue;
                    };
                    world.schedule_tick(pos, delay as u32, priority);
                }
            }
        }
        for queues in self.queues_deque.iter_mut() {
            for queue in queues.0.iter_mut() {
                queue.clear();
            }
        }
    }

    fn schedule_tick(&mut self, node: NodeId, delay: usize, priority: TickPriority) {
        self.queues_deque[(self.pos + delay) % Self::NUM_QUEUES].0[priority as usize].push(node);
    }

    fn queues_this_tick(&mut self) -> Queues {
        self.pos = (self.pos + 1) % Self::NUM_QUEUES;
        mem::take(&mut self.queues_deque[self.pos])
    }

    fn end_tick(&mut self, mut queues: Queues) {
        for queue in &mut queues.0 {
            queue.clear();
        }
        self.queues_deque[self.pos] = queues;
    }

    fn priorities() -> [TickPriority; Self::NUM_PRIORITIES] {
        [
            TickPriority::Highest,
            TickPriority::Higher,
            TickPriority::High,
            TickPriority::Normal,
        ]
    }

    fn has_pending_ticks(&self) -> bool {
        for queues in &self.queues_deque {
            for queue in &queues.0 {
                if !queue.is_empty() {
                    return true;
                }
            }
        }
        false
    }
}

enum Event {
    NoteBlockPlay { noteblock_id: u16 },
}

#[derive(Default)]
pub struct DirectBackend {
    nodes: Nodes,
    forward_links: Vec<ForwardLink>,
    analog_inputs: Vec<AnalogInput>,
    blocks: Vec<Option<(BlockPos, Block)>>,
    pos_map: FxHashMap<BlockPos, NodeId>,
    scheduler: TickScheduler,
    events: Vec<Event>,
    noteblock_info: Vec<(BlockPos, Instrument, u32)>,
}

impl DirectBackend {
    fn schedule_tick(&mut self, node_id: NodeId, delay: usize, priority: TickPriority) {
        self.scheduler.schedule_tick(node_id, delay, priority);
    }

    fn set_node(&mut self, node_id: NodeId, powered: bool, new_power: u8) {
        let node = &mut self.nodes[node_id];
        let old_power = node.output_power();

        node.set_changed(true);
        node.set_powered(powered);
        node.set_output_power(new_power);

        let fwd_link_begin = node.fwd_link_begin() as usize;
        let fwd_link_end = fwd_link_begin + node.fwd_link_count() as usize;
        for forward_link in &self.forward_links[fwd_link_begin..fwd_link_end] {
            let old_power = old_power.saturating_sub(forward_link.distance());
            let new_power = new_power.saturating_sub(forward_link.distance());

            if old_power == new_power {
                continue;
            }

            let target_node = &mut self.nodes[forward_link.target()];
            if target_node.ty().is_analog() {
                let analog_input = &mut self.analog_inputs[target_node.analog_input_idx() as usize];
                analog_input.set(forward_link.ty(), old_power, new_power);
            } else {
                if (old_power != 0) == (new_power != 0) {
                    continue;
                }
                let mut digital_input = target_node.digital_input();
                digital_input.set(forward_link.ty(), new_power != 0);
                target_node.set_digital_input(digital_input);
            }

            update::update_node(
                &mut self.scheduler,
                &mut self.events,
                &mut self.nodes,
                &self.analog_inputs,
                forward_link.target(),
            );
        }
    }
}

impl JITBackend for DirectBackend {
    fn inspect(&mut self, pos: BlockPos) {
        let Some(node_id) = self.pos_map.get(&pos) else {
            debug!("could not find node at pos {}", pos);
            return;
        };

        debug!("Node {:?}: {:#?}", node_id, self.nodes[*node_id]);
    }

    fn reset<W: World>(&mut self, world: &mut W, io_only: bool) {
        self.scheduler.reset(world, &self.blocks);

        let nodes = std::mem::take(&mut self.nodes);

        for (i, node) in nodes.into_inner().iter().enumerate() {
            let Some((pos, block)) = self.blocks[i] else {
                continue;
            };
            if matches!(node.ty(), NodeType::Comparator) {
                let block_entity = BlockEntity::Comparator {
                    output_strength: node.output_power(),
                };
                world.set_block_entity(pos, block_entity);
            }

            if io_only && !node.is_io() {
                world.set_block(pos, block);
            }
        }

        self.forward_links.clear();
        self.analog_inputs.clear();
        self.pos_map.clear();
        self.noteblock_info.clear();
        self.events.clear();
    }

    fn on_use_block(&mut self, pos: BlockPos) {
        let node_id = self.pos_map[&pos];
        let node = &self.nodes[node_id];
        match node.ty() {
            NodeType::Button => {
                if node.powered() {
                    return;
                }
                self.schedule_tick(node_id, 10, TickPriority::Normal);
                self.set_node(node_id, true, 15);
            }
            NodeType::Lever => {
                self.set_node(node_id, !node.powered(), bool_to_ss(!node.powered()));
            }
            _ => warn!("Tried to use a {:?} redpiler node", node.ty()),
        }
    }

    fn set_pressure_plate(&mut self, pos: BlockPos, powered: bool) {
        let node_id = self.pos_map[&pos];
        let node = &self.nodes[node_id];
        match node.ty() {
            NodeType::PressurePlate => {
                self.set_node(node_id, powered, bool_to_ss(powered));
            }
            _ => warn!("Tried to set pressure plate state for a {:?}", node.ty()),
        }
    }

    fn tick(&mut self) {
        let mut queues = self.scheduler.queues_this_tick();

        for node_id in queues.drain_iter() {
            self.tick_node(node_id);
        }

        self.scheduler.end_tick(queues);
    }

    fn flush<W: World>(&mut self, world: &mut W, io_only: bool) {
        for event in self.events.drain(..) {
            match event {
                Event::NoteBlockPlay { noteblock_id } => {
                    let (pos, instrument, note) = self.noteblock_info[noteblock_id as usize];
                    noteblock::play_note(world, pos, instrument, note);
                }
            }
        }
        for (i, node) in self.nodes.inner_mut().iter_mut().enumerate() {
            let Some((pos, block)) = &mut self.blocks[i] else {
                continue;
            };
            if node.changed() && (!io_only || node.is_io()) {
                if let Some(powered) = block_powered_mut(block) {
                    *powered = node.powered()
                }
                if let Block::RedstoneWire { wire, .. } = block {
                    wire.power = node.output_power()
                };
                if let Block::RedstoneRepeater { repeater } = block {
                    repeater.locked = node.get_repeater_properties().locked();
                }
                world.set_block(*pos, *block);
            }
            node.set_changed(false);
        }
    }

    fn compile(
        &mut self,
        graph: CompileGraph,
        ticks: Vec<TickEntry>,
        options: &CompilerOptions,
        monitor: Arc<TaskMonitor>,
    ) {
        compile::compile(self, graph, ticks, options, monitor);
    }

    fn has_pending_ticks(&self) -> bool {
        self.scheduler.has_pending_ticks()
    }
}

/// Set node for use in `update`. None of the nodes here have usable output power,
/// so this function does not set that.
fn set_node(node: &mut Node, powered: bool) {
    node.set_powered(powered);
    node.set_changed(true);
}

fn schedule_tick(
    scheduler: &mut TickScheduler,
    node_id: NodeId,
    node: &mut Node,
    delay: usize,
    priority: TickPriority,
) {
    node.set_pending_tick(true);
    scheduler.schedule_tick(node_id, delay, priority);
}

// This function is optimized for input values from 0 to 15 and does not work correctly outside that
// range
fn calculate_comparator_output(mode: ComparatorMode, input_strength: u8, power_on_sides: u8) -> u8 {
    let difference = input_strength.wrapping_sub(power_on_sides);
    if difference <= 15 {
        match mode {
            ComparatorMode::Compare => input_strength,
            ComparatorMode::Subtract => difference,
        }
    } else {
        0
    }
}

impl fmt::Display for DirectBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "digraph {{")?;
        for (id, node) in self.nodes.inner().iter().enumerate() {
            if matches!(node.ty(), NodeType::Wire) {
                continue;
            }
            let label = match node.ty() {
                NodeType::Repeater => {
                    format!("Repeater({})", node.get_repeater_properties().delay())
                }
                NodeType::Torch => "Torch".to_string(),
                NodeType::Comparator => format!(
                    "Comparator({})",
                    match node.get_comparator_properties().mode() {
                        ComparatorMode::Compare => "Cmp",
                        ComparatorMode::Subtract => "Sub",
                    }
                ),
                NodeType::Lamp => "Lamp".to_string(),
                NodeType::Button => "Button".to_string(),
                NodeType::Lever => "Lever".to_string(),
                NodeType::PressurePlate => "PressurePlate".to_string(),
                NodeType::Trapdoor => "Trapdoor".to_string(),
                NodeType::Wire => "Wire".to_string(),
                NodeType::Constant => format!("Constant({})", node.output_power()),
                NodeType::NoteBlock { .. } => "NoteBlock".to_string(),
            };
            let pos = if let Some((pos, _)) = self.blocks[id] {
                format!("{}, {}, {}", pos.x, pos.y, pos.z)
            } else {
                "No Pos".to_string()
            };
            writeln!(f, "    n{} [ label = \"{}\\n({})\" ];", id, label, pos)?;
            let fwd_link_begin = node.fwd_link_begin() as usize;
            let fwd_link_end = fwd_link_begin + node.fwd_link_count() as usize;
            for link in &self.forward_links[fwd_link_begin..fwd_link_end] {
                let out_index = link.target().index();
                let distance = link.distance();
                let color = if link.ty() == LinkType::Side {
                    ",color=\"blue\""
                } else {
                    ""
                };
                writeln!(
                    f,
                    "    n{} -> n{} [ label = \"{}\"{} ];",
                    id, out_index, distance, color
                )?;
            }
        }
        writeln!(f, "}}")
    }
}
