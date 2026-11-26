//! What holds parts together beyond their nets: the block instance each part belongs to, the
//! decoupling capacitor each IC power pin gets, and the circuit's `near` intents.
//! Replication pairs the placed members of repeated blocks by relative path and footprint.
//! Indexing their paths and selecting disjoint outermost instances costs O(parts log parts).

use super::{PinRef, PlaceProblem};
use fab_elec_board::Board;
use fab_elec_circuit::{Circuit, Intent, NetKind, Target};
use fab_elec_core::{BlockId, CompId, Nm, PadIx};
use fab_elec_parts::{PartKind, PinKind};
use std::collections::{BTreeMap, BTreeSet};

/// A block's relative component paths, with a footprint at each leaf. Interning the branches
/// lets a parent reuse its children's signatures even in deeply nested instances.
#[derive(Default)]
struct MemberPaths<'a> {
    footprint: Option<&'a str>,
    children: BTreeMap<&'a str, Self>,
    signature: Option<(usize, usize)>, // height, interned id
}

impl<'a> MemberPaths<'a> {
    fn insert(&mut self, path: &'a str, subtree: Self) {
        let mut node = self;
        for name in path.trim_start_matches('.').split('.') {
            node.signature = None;
            node = node.children.entry(name).or_default();
        }
        if node.children.is_empty() && node.footprint.is_none() {
            *node = subtree;
        } else {
            // A directly named part and a child block can share a path prefix.
            node.signature = None;
            node.footprint = node.footprint.or(subtree.footprint);
            for (name, child) in subtree.children { node.insert(name, child) }
        }
    }

    fn signature(&mut self, known: &mut BTreeMap<(Option<&'a str>, Vec<(&'a str, usize)>), usize>) -> (usize, usize) {
        if let Some(sig) = self.signature { return sig }
        let mut height = 0;
        let children = self.children.iter_mut().map(|(&name, child)| {
            let (h, id) = child.signature(known);
            height = height.max(h + 1);
            (name, id)
        }).collect();
        let next = known.len();
        let id = *known.entry((self.footprint, children)).or_insert(next);
        self.signature = Some((height, id));
        (height, id)
    }
}

/// One placed copy of a repeated block. Members are paired by relative component path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub block: BlockId,
    pub members: Vec<CompId>,
}

/// Matching instances of the same definition, in first-component order. Members may be
/// movable or standing obstacles. A missing placement excludes the incomplete block.
pub fn identical_instances(board: &Board, circuit: &Circuit) -> Vec<Vec<Instance>> {
    let n = circuit.tree.len();
    let placed: BTreeMap<_, _> = board.placed.iter().map(|p| (p.comp, p)).collect();
    let mut direct = vec![Vec::new(); n];
    let mut children = vec![Vec::new(); n];
    let mut paths: Vec<_> = (0..n).map(|_| Some(MemberPaths::default())).collect();
    for c in &circuit.comps {
        let b = c.block.ix();
        let Some(part) = placed.get(&c.id) else { paths[b] = None; continue };
        direct[b].push(c.id);
        if let Some(tree) = &mut paths[b] {
            if let Some(path) = c.path.strip_prefix(&circuit.tree[b].path) {
                tree.insert(path, MemberPaths { footprint: Some(&part.footprint.name), ..MemberPaths::default() });
            } else {
                paths[b] = None;
            }
        }
    }

    // Each subtree contains exactly block_comps(b), including through unnamed definitions.
    let mut known = BTreeMap::new();
    let mut candidates = BTreeMap::new();
    let mut order: Vec<_> = (0..n).collect();
    order.sort_unstable_by_key(|&b| std::cmp::Reverse(circuit.tree[b].path.len()));
    for b in order {
        let block = &circuit.tree[b];
        let mut subtree = paths[b].take();
        if let Some(tree) = &mut subtree {
            if tree.children.is_empty() { continue }
            if let Some(def) = block.def {
                let (height, sig) = tree.signature(&mut known);
                candidates.entry((std::cmp::Reverse(height), def, sig)).or_insert_with(Vec::new).push(b);
            }
        }
        if let Some(parent) = block.parent {
            let p = parent.ix();
            children[p].push(b);
            match (subtree, block.path.strip_prefix(&circuit.tree[p].path)) {
                (Some(tree), Some(path)) => { if let Some(parent) = &mut paths[p] { parent.insert(path, tree) } }
                _ => paths[p] = None,
            }
        }
    }

    // A nonempty child's relative paths are shorter than its parent's. Select taller matches
    // first, then collect and sort only the members that will actually be returned.
    let mut covered = vec![false; n];
    let mut groups = Vec::new();
    for mut blocks in candidates.into_values() {
        blocks.retain(|&b| !covered[b]);
        if blocks.len() < 2 { continue }
        let mut group = Vec::new();
        for block in blocks {
            let mut stack = vec![block];
            let mut members = Vec::new();
            while let Some(b) = stack.pop() {
                covered[b] = true;
                members.append(&mut direct[b]);
                stack.extend_from_slice(&children[b]);
            }
            let first = members.iter().copied().min().expect("a nonempty instance");
            members.sort_unstable_by_key(|c| &circuit.comps[c.ix()].path);
            group.push((first, Instance { block: circuit.tree[block].id, members }));
        }
        group.sort_unstable_by_key(|instance| instance.0);
        groups.push((group[0].0, group.into_iter().map(|(_, members)| members).collect()));
    }
    groups.sort_unstable_by_key(|group| group.0);
    groups.into_iter().map(|(_, group)| group).collect()
}

/// A point a part is pulled to once the part it names stands on the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    /// A pad: a movable's (placed once that movable is) or a fixed part's.
    Pad(PinRef),
    /// A movable's anchor.
    Part(usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Clusters {
    /// Each movable's cluster: its block instance, numbered in the order of first movable.
    pub of: Vec<usize>,
    /// Each movable's pulls, strongest first, each with the movable's own pad it pulls when one is
    /// named and, for a `near` intent, the most it allows: its IC power pin (on its power pad) for
    /// a decoupling capacitor, then the other ends of its `near` intents.
    pub pulls: Vec<Vec<(Pull, Option<PadIx>, Option<Nm>)>>,
    /// The inferred decoupling pairs as (capacitor, IC), for the answer: a wrong guess shows.
    pub decoupling: Vec<(CompId, CompId)>,
}

/// O(parts x pins). Decoupling: a two-pad capacitor between a power net and a ground net takes
/// an unclaimed power-in pad on that power net, preferring an IC in its own block, then by
/// (IC, pad); capacitors go in `CompId` order, so the pairing is deterministic. A capacitor the
/// circuit puts `near` parts pairs only with one of those: the written intent says where it
/// belongs (a regulator's output capacitor is not the MCU's decoupling).
pub fn clusters(board: &Board, circuit: &Circuit, problem: &PlaceProblem) -> Clusters {
    let movable: BTreeMap<CompId, usize> = problem.movables.iter().enumerate().map(|(i, m)| (m.comp, i)).collect();
    let comp = |c: CompId| circuit.comps.get(c.ix()).filter(|k| k.id == c);
    let mut blocks = BTreeMap::new();
    let of = problem.movables.iter().map(|m| {
        let block = comp(m.comp).map(|k| k.block);
        let next = blocks.len();
        *blocks.entry(block).or_insert(next)
    }).collect();
    let mut pulls = vec![Vec::new(); problem.movables.len()];

    // Power-in pads by net, as (IC, its block, pad, pull).
    let kind = |net: fab_elec_core::NetId| circuit.nets.get(net.ix()).map(|n| n.kind);
    let mut power_pads: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for part in &board.placed {
        let Some(k) = comp(part.comp) else { continue };
        if !matches!(k.part.kind, PartKind::Mcu | PartKind::Ic | PartKind::Regulator) { continue }
        for (i, (pad, net)) in part.footprint.pads.iter().zip(&part.pad_nets).enumerate() {
            let Some(net) = *net else { continue };
            let power_in = k.part.pins.iter().any(|pin| pin.kind == PinKind::PowerIn && pin.pads.contains(&pad.no));
            if !power_in || kind(net) != Some(NetKind::Power) { continue }
            let pull = match movable.get(&part.comp) {
                Some(&m) => Pull::Pad(PinRef::Mov(m, PadIx(i as u16))),
                None => Pull::Pad(PinRef::Fixed(part.to_board(pad.at))),
            };
            power_pads.entry(net).or_default().push((part.comp, k.block, i, pull));
        }
    }
    // The parts each part is written `near`.
    let part_of = |t: &Target| match *t { Target::Comp(c) | Target::Pin(c, _) => Some(c), Target::Block(_) => None };
    let mut near_parts: BTreeMap<CompId, BTreeSet<CompId>> = BTreeMap::new();
    for intent in &circuit.intents {
        let Intent::Near { a, b, .. } = intent else { continue };
        let (Some(a), Some(b)) = (part_of(a), part_of(b)) else { continue };
        near_parts.entry(a).or_default().insert(b);
        near_parts.entry(b).or_default().insert(a);
    }
    let mut claimed = BTreeSet::new();
    let mut decoupling = Vec::new();
    for (m, mv) in problem.movables.iter().enumerate() {
        let Some(k) = comp(mv.comp) else { continue };
        let nets: Vec<_> = mv.pad_nets.iter().flatten().copied().collect();
        if k.part.kind != PartKind::Capacitor || mv.pad_nets.len() != 2 || nets.len() != 2 { continue }
        let Some(&power) = nets.iter().find(|&&n| kind(n) == Some(NetKind::Power)) else { continue };
        let own = mv.pad_nets.iter().position(|&n| n == Some(power)).map(|i| PadIx(i as u16));
        if !nets.iter().any(|&n| kind(n) == Some(NetKind::Ground)) { continue }
        let Some(pads) = power_pads.get(&power) else { continue };
        let written = near_parts.get(&mv.comp);
        let pick = pads.iter().filter(|p| !claimed.contains(&(p.0, p.2)) && written.is_none_or(|w| w.contains(&p.0)))
            .min_by_key(|p| (p.1 != k.block, p.0, p.2));
        if let Some(&(ic, _, pad, pull)) = pick {
            claimed.insert((ic, pad));
            pulls[m].push((pull, own, None));
            decoupling.push((mv.comp, ic));
        }
    }

    // `near(a, b)`: each end pulls the other.
    let end = |t: &Target| -> Option<(Pull, Option<PadIx>)> {
        let (c, pin) = match *t {
            Target::Comp(c) => (c, None),
            Target::Pin(c, p) => (c, Some(p)),
            Target::Block(_) => return None,
        };
        let pad = pin.and_then(|p| {
            let k = comp(c)?;
            let no = k.part.pins.get(p.ix())?.pads.first()?;
            let part = board.placed_of(c)?;
            Some(PadIx(part.footprint.pads.iter().position(|pad| &pad.no == no)? as u16))
        });
        let pull = match (movable.get(&c), pad) {
            (Some(&m), Some(pad)) => Pull::Pad(PinRef::Mov(m, pad)),
            (Some(&m), None) => Pull::Part(m),
            (None, pad) => {
                let part = board.placed_of(c)?;
                Pull::Pad(PinRef::Fixed(pad.map_or(part.at, |pad| part.to_board(part.footprint.pads[pad.ix()].at))))
            }
        };
        Some((pull, pad))
    };
    for intent in &circuit.intents {
        let Intent::Near { a, b, max } = intent else { continue };
        let (Some(pa), Some(pb)) = (end(a), end(b)) else { continue };
        let own = |p: Pull| match p {
            Pull::Pad(PinRef::Mov(m, _)) | Pull::Part(m) => Some(m),
            Pull::Pad(PinRef::Fixed(_)) => None,
        };
        if let Some(m) = own(pa.0) { if own(pb.0) != Some(m) { pulls[m].push((pb.0, pa.1, Some(*max))) } }
        if let Some(m) = own(pb.0) { if own(pa.0) != Some(m) { pulls[m].push((pa.0, pb.1, Some(*max))) } }
    }
    Clusters { of, pulls, decoupling }
}
