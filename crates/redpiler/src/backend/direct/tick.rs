use super::node::NodeId;
use super::*;

impl DirectBackend {
    pub fn tick_node(&mut self, node_id: NodeId) {
        let node = &mut self.nodes[node_id];
        node.set_pending_tick(false);
        match node.ty() {
            NodeType::Repeater => {
                let properties = node.get_repeater_properties();
                if properties.locked() {
                    return;
                }
                let should_be_powered = node.digital_input().get();
                if node.powered() && !should_be_powered {
                    self.set_node(node_id, false, 0);
                } else if !node.powered() {
                    if !should_be_powered {
                        schedule_tick(
                            &mut self.scheduler,
                            node_id,
                            node,
                            properties.delay() as usize,
                            TickPriority::Higher,
                        );
                    }
                    self.set_node(node_id, true, 15);
                }
            }
            NodeType::Torch => {
                let should_be_powered = !node.digital_input().get();
                if node.powered() != should_be_powered {
                    self.set_node(node_id, should_be_powered, bool_to_ss(should_be_powered));
                }
            }
            NodeType::Comparator => {
                let properties = node.get_comparator_properties();
                let mut input_power = self.analog_inputs[node.analog_input_idx() as usize].get();
                let side_input_power =
                    self.analog_inputs[node.analog_input_idx() as usize].get_side();
                if input_power < 15 && properties.has_far_input() {
                    input_power = properties.far_input();
                }
                let old_strength = node.output_power();
                let new_strength =
                    calculate_comparator_output(properties.mode(), input_power, side_input_power);
                if new_strength != old_strength {
                    self.set_node(node_id, new_strength > 0, new_strength);
                }
            }
            NodeType::Lamp => {
                let should_be_lit = node.digital_input().get();
                if node.powered() && !should_be_lit {
                    self.set_node(node_id, false, 0);
                }
            }
            NodeType::Button => {
                if node.powered() {
                    self.set_node(node_id, false, 0);
                }
            }
            _ => {} //unreachable!("Node {:?} should not be ticked!", node.ty),
        }
    }
}
