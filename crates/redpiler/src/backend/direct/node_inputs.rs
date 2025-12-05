use crate::compile_graph::LinkType;

#[bitfield_struct::bitfield(u16)]
pub struct DigitalInput {
    count_default: u8,
    count_side: u8,
}

impl DigitalInput {
    pub fn set(&mut self, link_type: LinkType, value: bool) {
        if link_type == LinkType::Default {
            if value {
                self.set_count_default(self.count_default() + 1);
            } else {
                self.set_count_default(self.count_default() - 1);
            }
        } else {
            if value {
                self.set_count_side(self.count_side() + 1);
            } else {
                self.set_count_side(self.count_side() - 1);
            }
        }
    }

    pub fn get(&self) -> bool {
        self.count_default() > 0
    }

    pub fn get_side(&self) -> bool {
        self.count_side() > 0
    }
}

#[repr(align(16))]
#[derive(Debug, Clone)]
pub struct AnalogInput {
    pub ss_counts_default: [u8; 16],
    pub ss_counts_side: [u8; 16],
}

impl AnalogInput {
    pub fn set(&mut self, link_type: LinkType, old_value: u8, new_value: u8) {
        let ss_counts = match link_type {
            LinkType::Default => &mut self.ss_counts_default,
            LinkType::Side => &mut self.ss_counts_side,
        };
        ss_counts[old_value as usize] -= 1;
        ss_counts[new_value as usize] += 1;
    }

    pub fn get(&self) -> u8 {
        let value = u128::from_le_bytes(self.ss_counts_default);
        15 - (value.leading_zeros() / 8) as u8
    }

    pub fn get_side(&self) -> u8 {
        let value = u128::from_le_bytes(self.ss_counts_side);
        15 - (value.leading_zeros() / 8) as u8
    }
}

impl Default for AnalogInput {
    fn default() -> Self {
        Self {
            ss_counts_default: [255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            ss_counts_side: [255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        }
    }
}
