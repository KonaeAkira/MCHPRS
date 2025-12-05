use mchprs_world::World;
use petgraph::Direction;
use rustc_hash::{FxHashMap, FxHashSet};
use tracing::trace;

use crate::{
    CompilerInput, CompilerOptions,
    compile_graph::{CompileGraph, NodeIdx, NodeType},
    passes::utils::*,
    passes::{AnalysisInfo, AnalysisInfos, Pass},
};

#[derive(Default)]
pub struct PulseLengthInfo {
    min_on_pulse: FxHashMap<NodeIdx, u8>,
    min_off_pulse: FxHashMap<NodeIdx, u8>,
}

impl AnalysisInfo for PulseLengthInfo {}

impl PulseLengthInfo {
    /// Returns the calculated minimum pulse duration if this node were to produce an ON pulse.
    pub fn min_on_pulse_duration(&self, idx: NodeIdx) -> u8 {
        self.min_on_pulse.get(&idx).copied().unwrap_or(0)
    }

    /// Returns the calculated minimum pulse duration if this node were to produce on OFF pulse.
    pub fn min_off_pulse_duration(&self, idx: NodeIdx) -> u8 {
        self.min_off_pulse.get(&idx).copied().unwrap_or(0)
    }

    pub fn min_pulse_duration(&self, idx: NodeIdx) -> u8 {
        std::cmp::min(
            self.min_on_pulse_duration(idx),
            self.min_off_pulse_duration(idx),
        )
    }

    /// Updates the analyzed value for the given node.
    /// Returns `true` if the analysis for the node has improved, `false` otherwise.
    fn update(&mut self, idx: NodeIdx, on: u8, off: u8) -> bool {
        let current_on = self.min_on_pulse_duration(idx);
        let current_off = self.min_off_pulse_duration(idx);
        if on > current_on {
            self.min_on_pulse.insert(idx, on);
        }
        if off > current_off {
            self.min_off_pulse.insert(idx, off);
        }
        on > current_on || off > current_off
    }
}

pub struct PulseLengthAnalysis;

impl<W: World> Pass<W> for PulseLengthAnalysis {
    fn run_pass(
        &self,
        graph: &mut CompileGraph,
        _: &CompilerOptions,
        _: &CompilerInput<'_, W>,
        analysis_infos: &mut AnalysisInfos,
    ) {
        let mut analysis = PulseLengthInfo::default();
        let mut candidates: FxHashSet<NodeIdx> = graph.node_indices().collect();

        for &idx in &candidates {
            match graph[idx].ty {
                NodeType::Repeater { delay, .. } => analysis.update(idx, delay, delay),
                NodeType::Torch => analysis.update(idx, 1, 1),
                NodeType::Comparator { .. } => analysis.update(idx, 1, 1),
                NodeType::Button => analysis.update(idx, 10, 0),
                NodeType::Lever => analysis.update(idx, 0, 0),
                NodeType::PressurePlate => analysis.update(idx, 10, 0),
                NodeType::Constant => analysis.update(idx, u8::MAX, u8::MAX),
                _ => false,
            };
        }

        while !candidates.is_empty() {
            for idx in std::mem::take(&mut candidates) {
                analyze_node(&mut analysis, &mut candidates, graph, idx);
            }
        }

        let mut cnt = FxHashMap::default();
        for idx in graph.node_indices() {
            if matches!(graph[idx].ty, NodeType::Repeater { delay: 1, .. }) {
                let min_pulse = analysis.min_pulse_duration(idx);
                *cnt.entry(min_pulse).or_insert(0) += 1;
            }
        }
        dbg!(cnt);

        trace!(
            "Calculated min ON pulse length for {} nodes",
            analysis.min_on_pulse.len()
        );
        trace!(
            "Calculated min OFF pulse length for {} nodes",
            analysis.min_off_pulse.len()
        );
        analysis_infos.insert_analysis(analysis);
    }

    fn status_message(&self) -> &'static str {
        "Analyzing signal durations"
    }
}

fn analyze_node(
    analysis: &mut PulseLengthInfo,
    next_candidates: &mut FxHashSet<NodeIdx>,
    graph: &CompileGraph,
    idx: NodeIdx,
) {
    match graph[idx].ty {
        NodeType::Repeater { .. } => analyze_repeater(analysis, next_candidates, graph, idx),
        NodeType::Torch => analyze_torch(analysis, next_candidates, graph, idx),
        NodeType::Comparator { .. } => (), // No analysis for comparators for now.
        _ => (),
    }
}

fn analyze_repeater(
    analysis: &mut PulseLengthInfo,
    next_candidates: &mut FxHashSet<NodeIdx>,
    graph: &CompileGraph,
    idx: NodeIdx,
) {
    if has_side_inputs(graph, idx) {
        return; // Repeater locking can shorten pulse duration received from inputs.
    }
    let mut min_incoming_on = u8::MAX;
    let mut min_incoming_off = u8::MAX;
    for incoming_idx in graph.neighbors_directed(idx, Direction::Incoming) {
        min_incoming_on = min_incoming_on.min(analysis.min_on_pulse_duration(incoming_idx));
        min_incoming_off = min_incoming_off.min(analysis.min_off_pulse_duration(incoming_idx));
    }
    if graph.neighbors_directed(idx, Direction::Incoming).count() > 1 {
        // Even if all inputs have a long OFF pulse, the timing between the inputs could still lead
        // to a situation that generates a very quick OFF input pulse for this node.
        min_incoming_off = 0;
    }
    if analysis.update(idx, min_incoming_on, min_incoming_off) {
        next_candidates.extend(graph.neighbors_directed(idx, Direction::Outgoing));
    }
}

fn analyze_torch(
    analysis: &mut PulseLengthInfo,
    next_candidates: &mut FxHashSet<NodeIdx>,
    graph: &CompileGraph,
    idx: NodeIdx,
) {
    let mut min_incoming_on = u8::MAX;
    let mut min_incoming_off = u8::MAX;
    for incoming_idx in graph.neighbors_directed(idx, Direction::Incoming) {
        min_incoming_on = min_incoming_on.min(analysis.min_on_pulse_duration(incoming_idx));
        min_incoming_off = min_incoming_off.min(analysis.min_off_pulse_duration(incoming_idx));
    }
    if graph.neighbors_directed(idx, Direction::Incoming).count() > 1 {
        // Even if all inputs have a long OFF pulse, the timing between the inputs could still lead
        // to a situation that generates a very quick OFF input pulse for this node.
        min_incoming_off = 0;
    }
    let mut min_outgoing_on = min_incoming_off;
    let mut min_outgoing_off = min_incoming_on;
    let all_incoming_are_repeaters = graph
        .neighbors_directed(idx, Direction::Incoming)
        .all(|incoming_idx| matches!(graph[incoming_idx].ty, NodeType::Repeater { .. }));
    if all_incoming_are_repeaters {
        // Torches cannot be triggered by 0-rt or 1-rt pulses from repeaters.
        // So if all inputs are from repeaters, the ON/OFF pulses from this torch must be at least 2-rt.
        min_outgoing_on = min_outgoing_on.max(2);
        min_outgoing_off = min_outgoing_off.max(2);
    }
    if analysis.update(idx, min_outgoing_on, min_outgoing_off) {
        next_candidates.extend(graph.neighbors_directed(idx, Direction::Outgoing));
    }
}
