use itertools::Itertools;
use petgraph::Direction;

use crate::compile_graph::{CompileGraph, LinkType, NodeIdx};

pub fn has_side_inputs(graph: &CompileGraph, idx: NodeIdx) -> bool {
    graph
        .edges_directed(idx, Direction::Incoming)
        .any(|e| e.weight().ty == LinkType::Side)
}

pub fn has_exactly_one_input(graph: &CompileGraph, idx: NodeIdx, link_type: LinkType) -> bool {
    graph
        .edges_directed(idx, Direction::Incoming)
        .filter(|e| e.weight().ty == link_type)
        .exactly_one()
        .is_ok()
}

pub fn has_exactly_one_output(graph: &CompileGraph, idx: NodeIdx) -> bool {
    graph
        .edges_directed(idx, Direction::Outgoing)
        .exactly_one()
        .is_ok()
}

pub fn coalesce(graph: &mut CompileGraph, node: NodeIdx, into: NodeIdx) {
    let mut walk_outgoing: petgraph::stable_graph::WalkNeighbors<u32> =
        graph.neighbors_directed(node, Direction::Outgoing).detach();
    while let Some(edge_idx) = walk_outgoing.next_edge(graph) {
        let dest = graph.edge_endpoints(edge_idx).unwrap().1;
        let weight = graph.remove_edge(edge_idx).unwrap();
        graph.add_edge(into, dest, weight);
    }
    graph.remove_node(node);
}
