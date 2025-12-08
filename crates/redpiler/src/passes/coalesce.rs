use super::Pass;
use crate::compile_graph::{CompileGraph, LinkType, NodeIdx, NodeType};
use crate::passes::AnalysisInfos;
use crate::{CompilerInput, CompilerOptions};
use mchprs_world::World;
use petgraph::visit::{EdgeRef, NodeIndexable};
use petgraph::Direction;
use rustc_hash::FxHashMap;
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
            let num_coalesced = run_iteration(graph);
            trace!("Iteration combined {} nodes", num_coalesced);
            if num_coalesced == 0 {
                break;
            }
        }
    }

    fn status_message(&self) -> &'static str {
        "Combining duplicate logic"
    }
}

fn run_iteration(graph: &mut CompileGraph) -> usize {
    let mut num_coalesced = 0;
    for i in 0..graph.node_bound() {
        let idx = NodeIdx::new(i);
        if !graph.contains_node(idx) {
            continue;
        }
        num_coalesced += coalesce_outgoing(graph, idx);
    }
    num_coalesced
}

fn coalesce_outgoing(graph: &mut CompileGraph, idx: NodeIdx) -> usize {
    let source_is_analog = matches!(graph[idx].ty, NodeType::Comparator { .. } | NodeType::Wire);
    let mut num_coalesced = 0;
    let mut groups: FxHashMap<(NodeType, u8), Vec<NodeIdx>> = FxHashMap::default();
    for outgoing in graph
        .edges_directed(idx, Direction::Outgoing)
        .filter(|e| e.weight().ty == LinkType::Default)
    {
        let target_idx = outgoing.target();
        if !graph[target_idx].is_removable() {
            continue;
        }
        let target_in_degree = graph
            .edges_directed(target_idx, Direction::Incoming)
            .count();
        if target_in_degree > 1 {
            continue; // Exclude nodes with in-degree larger than 1, counting side inputs.
        }
        let target_is_analog = matches!(
            graph[target_idx].ty,
            NodeType::Comparator { .. } | NodeType::Wire
        );
        let distance = if source_is_analog || target_is_analog {
            outgoing.weight().ss
        } else {
            0 // Both source and target are digital, so distance does not matter.
        };
        groups
            .entry((graph[target_idx].ty, distance))
            .or_default()
            .push(target_idx);
    }
    for group in groups.into_values() {
        let coalesce_into_idx = group[0];
        num_coalesced += group.len() - 1;
        for &coalesced_idx in &group[1..] {
            coalesce(graph, coalesced_idx, coalesce_into_idx);
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
