use super::Pass;
use crate::compile_graph::{CompileGraph, LinkType, NodeIdx, NodeType};
use crate::passes::AnalysisInfos;
use crate::passes::analysis::pulse_length_analysis::PulseLengthInfo;
use crate::passes::utils::{
    coalesce, has_exactly_one_input, has_exactly_one_output, has_side_inputs,
};
use crate::{CompilerInput, CompilerOptions};
use itertools::Itertools;
use mchprs_world::World;
use petgraph::Direction::{Incoming, Outgoing};
use petgraph::visit::EdgeRef;

pub struct MergeRepeaters;

impl<W: World> Pass<W> for MergeRepeaters {
    fn run_pass(
        &self,
        graph: &mut CompileGraph,
        _: &CompilerOptions,
        _: &CompilerInput<'_, W>,
        analysis_infos: &mut AnalysisInfos,
    ) {
        let analysis = analysis_infos
            .get_analysis()
            .expect("Pulse length analysis needs to run before this pass.");
        let idxs = graph.node_indices().collect_vec();
        for idx in idxs {
            if graph.contains_node(idx) {
                try_merge_repeater_chain(graph, idx, analysis);
                try_merge_repeaters(graph, idx, analysis);
            }
        }
    }

    fn status_message(&self) -> &'static str {
        "Merging repeaters"
    }
}

fn try_merge_repeater_chain(graph: &mut CompileGraph, idx: NodeIdx, analysis: &PulseLengthInfo) {
    let NodeType::Repeater {
        mut delay,
        mut facing_diode,
    } = graph[idx].ty
    else {
        return;
    };

    let max_total_delay = analysis.min_pulse_duration(idx).min(4);

    while !facing_diode
        && let Ok(outgoing) = graph.edges_directed(idx, Outgoing).exactly_one()
        && !has_side_inputs(graph, outgoing.target())
        && has_exactly_one_input(graph, outgoing.target(), LinkType::Default)
    {
        let target_idx = outgoing.target();
        let NodeType::Repeater {
            delay: target_delay,
            facing_diode: target_facing_diode,
        } = graph[target_idx].ty
        else {
            break;
        };
        if delay + target_delay > max_total_delay {
            break;
        }
        delay += target_delay;
        facing_diode |= target_facing_diode;
        coalesce(graph, target_idx, idx);
    }

    graph[idx].ty = NodeType::Repeater {
        delay,
        facing_diode,
    };
}

fn try_merge_repeaters(
    graph: &mut CompileGraph,
    idx: NodeIdx,
    analysis: &PulseLengthInfo,
) -> Option<()> {
    if graph[idx].ty != NodeType::Torch {
        return None;
    }

    let source_idx = graph.neighbors_directed(idx, Incoming).exactly_one().ok()?;
    let NodeType::Repeater { delay: 1, .. } = graph[source_idx].ty else {
        return None;
    };
    if has_side_inputs(graph, source_idx) || !has_exactly_one_output(graph, source_idx) {
        return None;
    }
    if analysis.min_pulse_duration(source_idx) < 2 {
        return None;
    }

    let target_idx = graph.neighbors_directed(idx, Outgoing).exactly_one().ok()?;
    let NodeType::Repeater {
        delay: 1,
        facing_diode,
    } = graph[target_idx].ty
    else {
        return None;
    };
    if has_side_inputs(graph, target_idx)
        || !has_exactly_one_input(graph, target_idx, LinkType::Default)
    {
        return None;
    }

    let incoming = graph
        .edges_directed(source_idx, Incoming)
        .map(|e| (e.source(), *e.weight()))
        .collect_vec();
    graph.remove_node(source_idx);
    for (source_idx, weight) in incoming {
        graph.add_edge(source_idx, idx, weight);
    }

    graph[target_idx].ty = NodeType::Repeater {
        delay: 2,
        facing_diode,
    };

    Some(())
}
