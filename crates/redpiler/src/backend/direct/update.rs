use mchprs_world::TickPriority;

use super::node::NodeId;
use super::*;

#[inline(always)]
pub(super) fn update_node(
    scheduler: &mut TickScheduler,
    events: &mut Vec<Event>,
    nodes: &mut Nodes,
    analog_inputs: &[AnalogInput],
    node_id: NodeId,
) {
    let node = &mut nodes[node_id];

    match node.ty() {
        NodeType::Repeater => {
            let mut properties = node.get_repeater_properties();
            let should_be_locked = node.digital_input().get_side();
            if should_be_locked != properties.locked() {
                properties.set_locked(should_be_locked);
                node.set_type_specific_properties(properties.into_bits());
                node.set_changed(true);
            }
            if properties.locked() || node.pending_tick() {
                return;
            }
            let should_be_powered = node.digital_input().get();
            if should_be_powered != node.powered() {
                let priority = if properties.facing_diode() {
                    TickPriority::Highest
                } else if !should_be_powered {
                    TickPriority::Higher
                } else {
                    TickPriority::High
                };
                schedule_tick(
                    scheduler,
                    node_id,
                    node,
                    properties.delay() as usize,
                    priority,
                );
            }
        }
        NodeType::Torch => {
            if node.pending_tick() {
                return;
            }
            let should_be_powered = !node.digital_input().get();
            if node.powered() != should_be_powered {
                schedule_tick(scheduler, node_id, node, 1, TickPriority::Normal);
            }
        }
        NodeType::Comparator => {
            if node.pending_tick() {
                return;
            }
            let properties = node.get_comparator_properties();
            let mut input_power = analog_inputs[node.analog_input_idx() as usize].get();
            let side_input_power = analog_inputs[node.analog_input_idx() as usize].get_side();
            if input_power < 15 && properties.has_far_input() {
                input_power = properties.far_input();
            }
            let old_strength = node.output_power();
            let output_power =
                calculate_comparator_output(properties.mode(), input_power, side_input_power);
            if output_power != old_strength {
                let priority = if properties.facing_diode() {
                    TickPriority::High
                } else {
                    TickPriority::Normal
                };
                schedule_tick(scheduler, node_id, node, 1, priority);
            }
        }
        NodeType::Lamp => {
            let should_be_lit = node.digital_input().get();
            let lit = node.powered();
            if lit && !should_be_lit {
                schedule_tick(scheduler, node_id, node, 2, TickPriority::Normal);
            } else if !lit && should_be_lit {
                set_node(node, true);
            }
        }
        NodeType::Trapdoor => {
            let should_be_powered = node.digital_input().get();
            if node.powered() != should_be_powered {
                set_node(node, should_be_powered);
            }
        }
        NodeType::Wire => {
            let input_power = analog_inputs[node.analog_input_idx() as usize].get();
            if node.output_power() != input_power {
                node.set_output_power(input_power);
                node.set_changed(true);
            }
        }
        NodeType::NoteBlock => {
            let should_be_powered = node.digital_input().get();
            if node.powered() != should_be_powered {
                set_node(node, should_be_powered);
                if should_be_powered {
                    let noteblock_id = node.get_noteblock_properties().noteblock_id();
                    events.push(Event::NoteBlockPlay { noteblock_id });
                }
            }
        }
        _ => {} // unreachable!("Node {:?} should not be updated!", node.ty),
    }
}
