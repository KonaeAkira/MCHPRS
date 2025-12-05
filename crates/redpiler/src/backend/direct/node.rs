use std::ops::{Index, IndexMut};

use crate::{
    backend::direct::{
        node_inputs::DigitalInput,
        node_type::{ComparatorProperties, NodeType, NoteblockProperties, RepeaterProperties},
    },
    compile_graph::LinkType,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// Safety: index must be within bounds of nodes array
    pub unsafe fn from_index(index: usize) -> NodeId {
        NodeId(index as u32)
    }
}

// This is Pretty Bad:tm: because one can create a NodeId using another instance of Nodes,
// but at least some type system protection is better than none.
#[derive(Default)]
pub struct Nodes {
    pub nodes: Box<[Node]>,
}

impl Nodes {
    pub fn new(nodes: Box<[Node]>) -> Nodes {
        Nodes { nodes }
    }

    pub fn get(&self, idx: usize) -> NodeId {
        if self.nodes.get(idx).is_some() {
            NodeId(idx as u32)
        } else {
            panic!("node index out of bounds: {}", idx)
        }
    }

    pub fn inner(&self) -> &[Node] {
        &self.nodes
    }

    pub fn inner_mut(&mut self) -> &mut [Node] {
        &mut self.nodes
    }

    pub fn into_inner(self) -> Box<[Node]> {
        self.nodes
    }
}

impl Index<NodeId> for Nodes {
    type Output = Node;

    // The index here MUST have been created by this instance, otherwise scary things will happen !
    fn index(&self, index: NodeId) -> &Self::Output {
        unsafe { self.nodes.get_unchecked(index.0 as usize) }
    }
}

impl IndexMut<NodeId> for Nodes {
    fn index_mut(&mut self, index: NodeId) -> &mut Self::Output {
        unsafe { self.nodes.get_unchecked_mut(index.0 as usize) }
    }
}

#[bitfield_struct::bitfield(u32)]
pub struct ForwardLink {
    #[bits(1)]
    pub ty: LinkType,
    #[bits(4)]
    pub distance: u8,
    #[bits(27)]
    raw_target: u32,
}

impl ForwardLink {
    pub fn with_target(self, id: NodeId) -> Self {
        self.with_raw_target_checked(id.0)
            .expect("NodeId does not fit in 27 bits.")
    }

    pub fn target(self) -> NodeId {
        unsafe { NodeId::from_index(self.raw_target() as usize) }
    }
}

#[bitfield_struct::bitfield(u128)]
pub struct Node {
    pub type_specific_properties: u16,
    #[bits(4)]
    pub ty: NodeType,
    pub is_io: bool,
    /// Powered or lit
    pub powered: bool,
    pub changed: bool,
    pub pending_tick: bool,

    pub output_power: u8,

    #[bits(16)]
    pub digital_input: DigitalInput,

    /// The index to after the last forward link of this node.
    pub fwd_link_count: u16,
    /// The index to the first forward link of this node.
    pub fwd_link_begin: u32,
    /// The index to the analog input values of this node.
    pub analog_input_idx: u32,
}

impl Node {
    pub fn get_repeater_properties(&self) -> RepeaterProperties {
        RepeaterProperties::from_bits(self.type_specific_properties())
    }

    pub fn get_comparator_properties(&self) -> ComparatorProperties {
        ComparatorProperties::from_bits(self.type_specific_properties())
    }

    pub fn get_noteblock_properties(&self) -> NoteblockProperties {
        NoteblockProperties::from_bits(self.type_specific_properties())
    }
}
