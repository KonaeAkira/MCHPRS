use mchprs_blocks::blocks::ComparatorMode;

#[bitfield_struct::bitenum]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    #[fallback]
    Constant,
    Repeater,
    Torch,
    Comparator,
    Lamp,
    Button,
    Lever,
    PressurePlate,
    Trapdoor,
    Wire,
    NoteBlock,
}

impl NodeType {
    pub const fn is_analog(self) -> bool {
        matches!(self, Self::Comparator | Self::Wire)
    }
}

#[bitfield_struct::bitfield(u16)]
pub struct RepeaterProperties {
    #[bits(6)]
    _padding: u8,
    pub facing_diode: bool,
    pub locked: bool,
    pub delay: u8,
}

#[bitfield_struct::bitfield(u16)]
pub struct ComparatorProperties {
    #[bits(4)]
    _padding: u8,
    #[bits(2)]
    pub mode: ComparatorMode,
    pub facing_diode: bool,
    pub has_far_input: bool,
    pub far_input: u8,
}

#[bitfield_struct::bitfield(u16)]
pub struct NoteblockProperties {
    pub noteblock_id: u16,
}
