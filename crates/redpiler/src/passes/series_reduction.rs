use std::collections::VecDeque;

use super::Pass;
use crate::compile_graph::{
    CompileGraph, CompileLink, CompileNode, LinkType, NodeIdx, NodeState, NodeType,
};
use crate::passes::AnalysisInfos;
use crate::{CompilerInput, CompilerOptions};
use itertools::Itertools;
use mchprs_world::World;
use petgraph::Direction;
use petgraph::visit::EdgeRef;
use rustc_hash::FxHashSet;

pub struct SeriesReduction;

impl<W: World> Pass<W> for SeriesReduction {
    fn run_pass(
        &self,
        graph: &mut CompileGraph,
        _: &CompilerOptions,
        _: &CompilerInput<'_, W>,
        _: &mut AnalysisInfos,
    ) {
        let mut chains = Vec::new();
        for idx in graph.node_indices() {
            chains.extend(discover_chain(graph, idx));
        }

        for (head_idx, mut chain_elements, tail_idx) in chains {
            if chain_elements
                .iter()
                .any(|element| !graph.contains_node(element.idx))
            {
                continue; // Some nodes were already removed.
            }
            reduce_chain(graph, chain_elements.make_contiguous(), head_idx, tail_idx);
        }
    }

    fn status_message(&self) -> &'static str {
        "Merging repeaters"
    }
}

fn discover_chain(
    graph: &CompileGraph,
    idx: NodeIdx,
) -> Option<(NodeIdx, VecDeque<Element>, NodeIdx)> {
    if side_input_degree(graph, idx) != 0
        || input_degree(graph, idx) != 1
        || output_degree(graph, idx) != 1
    {
        return None;
    }

    let mut chain = VecDeque::new();
    chain.push_back(get_chain_element(graph, idx)?);

    let mut tail_idx = get_single_output(graph, idx)?;
    while let Some(tail_element) = get_chain_element(graph, tail_idx)
        && side_input_degree(graph, tail_idx) == 0
        && input_degree(graph, tail_idx) == 1
        && let Some(new_tail_idx) = get_single_output(graph, tail_idx)
    {
        chain.push_back(tail_element);
        tail_idx = new_tail_idx;
    }

    let mut head_idx = get_single_input(graph, idx)?;
    while let Some(head_element) = get_chain_element(graph, head_idx)
        && side_input_degree(graph, head_idx) == 0
        && output_degree(graph, head_idx) == 1
        && let Some(new_head_idx) = get_single_input(graph, head_idx)
    {
        chain.push_back(head_element);
        head_idx = new_head_idx;
    }

    Some((head_idx, chain, tail_idx))
}

fn reduce_chain(
    graph: &mut CompileGraph,
    chain: &[Element],
    mut head_idx: NodeIdx,
    tail_idx: NodeIdx,
) {
    let mut pulse_profile = PulseProfile::new();
    for element in chain {
        match element.ty {
            Type::Torch => pulse_profile.add_torch(),
            Type::Repeater(delay) => pulse_profile.add_repeater(delay),
        }
    }

    // Catch some common unoptimizable cases early so we don't waste time trying to optimize them.
    if pulse_profile.total_delay == chain.len() as u32 {
        return; // Chain consists of only 1-tick elements so it cannot be reduced further.
    }
    if pulse_profile.total_delay == chain.len() as u32 * 4 {
        return; // Chain consists of only 4-tick elements so it cannot be reduced further.
    }

    let new_chain = find_shortest_chain_with_profile(&pulse_profile)
        .expect("The original chain is valid so there's always at least one solution.");
    if new_chain.len() >= chain.len() {
        return;
    }

    // Remove the old chain.
    for element in chain {
        graph.remove_node(element.idx);
    }

    // Create the new chain
    let mut powered = graph[head_idx].state.powered;
    for elem_type in new_chain {
        let new_idx = match elem_type {
            Type::Torch => {
                powered = !powered;
                graph.add_node(CompileNode {
                    ty: NodeType::Torch,
                    block: None,
                    state: NodeState::simple(powered),
                    is_input: false,
                    is_output: false,
                })
            }
            Type::Repeater(delay) => graph.add_node(CompileNode {
                ty: NodeType::Repeater {
                    delay,
                    facing_diode: false,
                },
                block: None,
                state: NodeState::simple(powered),
                is_input: false,
                is_output: false,
            }),
        };
        graph.add_edge(head_idx, new_idx, CompileLink::default(0));
        head_idx = new_idx;
    }
    graph.add_edge(head_idx, tail_idx, CompileLink::default(0));
}

/// Find the shortest sequence of Torches and Repeaters that exactly match the given pulse profile.
fn find_shortest_chain_with_profile(target_profile: &PulseProfile) -> Option<Vec<Type>> {
    let mut seen_profiles = FxHashSet::default();
    let mut queue = VecDeque::new();

    let mut create_next = |mut elems: Vec<Type>, mut profile: PulseProfile, elem| -> Option<_> {
        match elem {
            Type::Torch => profile.add_torch(),
            Type::Repeater(delay) => profile.add_repeater(delay),
        };
        if profile.total_delay > target_profile.total_delay {
            return None;
        }
        if profile.mapping.len() < target_profile.mapping.len() {
            return None; // The current profile discards some signals that the target profile doesn't discard.
        }
        if !seen_profiles.insert(profile.clone()) {
            return None;
        }
        elems.push(elem);
        Some((elems, profile))
    };

    const CANDIDATES: [Type; 5] = [
        Type::Torch,
        Type::Repeater(1),
        Type::Repeater(2),
        Type::Repeater(3),
        Type::Repeater(4),
    ];

    queue.push_back((Vec::new(), PulseProfile::new()));
    while let Some((elems, profile)) = queue.pop_front() {
        for elem in CANDIDATES {
            if let Some((next_elems, next_profile)) =
                create_next(elems.clone(), profile.clone(), elem)
            {
                if next_profile == *target_profile {
                    return Some(next_elems);
                }
                queue.push_back((next_elems, next_profile));
            }
        }
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Element {
    idx: NodeIdx,
    ty: Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Type {
    Torch,
    Repeater(u8),
}

fn get_chain_element(graph: &CompileGraph, idx: NodeIdx) -> Option<Element> {
    match graph[idx].ty {
        NodeType::Torch => Some(Element {
            idx,
            ty: Type::Torch,
        }),
        NodeType::Repeater {
            delay,
            facing_diode,
        } if !facing_diode => Some(Element {
            idx,
            ty: Type::Repeater(delay),
        }),
        _ => None,
    }
}

fn get_single_input(graph: &CompileGraph, idx: NodeIdx) -> Option<NodeIdx> {
    let edges_iter = graph
        .edges_directed(idx, Direction::Incoming)
        .filter(|e| e.weight().ty == LinkType::Default);
    Some(edges_iter.exactly_one().ok()?.source())
}

fn get_single_output(graph: &CompileGraph, idx: NodeIdx) -> Option<NodeIdx> {
    let edges_iter = graph
        .edges_directed(idx, Direction::Outgoing)
        .filter(|e| e.weight().ty == LinkType::Default);
    Some(edges_iter.exactly_one().ok()?.target())
}

fn input_degree(graph: &CompileGraph, idx: NodeIdx) -> usize {
    graph
        .edges_directed(idx, Direction::Incoming)
        .filter(|e| e.weight().ty == LinkType::Default)
        .count()
}

fn side_input_degree(graph: &CompileGraph, idx: NodeIdx) -> usize {
    graph
        .edges_directed(idx, Direction::Incoming)
        .filter(|e| e.weight().ty == LinkType::Side)
        .count()
}

fn output_degree(graph: &CompileGraph, idx: NodeIdx) -> usize {
    graph.edges_directed(idx, Direction::Outgoing).count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Priority {
    High,
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Pulse {
    signal: bool,
    duration: u8,
    priority: Priority,
}

impl Pulse {
    pub fn new(signal: bool, duration: u8, priority: Priority) -> Self {
        Self {
            signal,
            duration,
            priority,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PulseProfile {
    total_delay: u32,
    mapping: Vec<(Pulse, Pulse)>,
}

impl PulseProfile {
    pub fn new() -> Self {
        let mut mapping = Vec::new();
        let add_identity = |mapping: &mut Vec<_>, pulse| mapping.push((pulse, pulse));
        add_identity(&mut mapping, Pulse::new(true, 1, Priority::High));
        add_identity(&mut mapping, Pulse::new(false, 1, Priority::High));
        for signal in [true, false] {
            for duration in 1..=4 {
                add_identity(&mut mapping, Pulse::new(signal, duration, Priority::Normal));
            }
        }
        Self {
            total_delay: 0,
            mapping: mapping,
        }
    }

    pub fn add_torch(&mut self) {
        self.total_delay += 1;
        self.mapping.retain_mut(|(_from, to)| {
            if to.priority == Priority::High && to.duration <= 1 {
                return false; // Torches don't react to 1-tick high-priority pulses.
            }
            to.signal = !to.signal;
            true
        });
    }

    pub fn add_repeater(&mut self, repeater_delay: u8) {
        self.total_delay += u32::from(repeater_delay);
        self.mapping.retain_mut(|(_from, to)| {
            if !to.signal && to.duration < repeater_delay {
                return false; // Repeaters don't react to OFF pulses shorter than their delay.
            }
            to.duration = to.duration.max(repeater_delay);
            to.priority = Priority::High;
            true
        });
    }
}
