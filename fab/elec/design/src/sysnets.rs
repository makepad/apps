//! System nets (C4, E2.8): the union of per-unit nets through links. Net names never merge
//! across units; only links join them. Computed on demand and carried by the snapshot, never
//! cached in the design (E4 item 2).
//!
//! Cost: O(nets + link pins) with a union-find.

use crate::{common_volts, Design};
use fab_elec_core::{NetRef, PinRef};
use fab_elec_parts::{PinKind, VRange};

/// One electrical node of the whole product.
#[derive(Clone, PartialEq, Debug)]
pub struct SystemNet {
    /// Its per-unit nets, sorted.
    pub members: Vec<NetRef>,
    /// The members' names, sorted and distinct.
    pub aliases: Vec<String>,
    /// Power output pins on it, sorted.
    pub sources: Vec<PinRef>,
    /// The members' declared volts, intersected (`None`: undeclared or in conflict).
    pub volts: Option<VRange>,
}

fn find(p: &mut [usize], mut i: usize) -> usize {
    while p[i] != i {
        p[i] = p[p[i]];
        i = p[i];
    }
    i
}

/// Every system net, sorted by first member (unlinked nets are system nets of one member).
pub fn sysnets(d: &Design) -> Vec<SystemNet> {
    let mut all: Vec<NetRef> = Vec::new();
    for u in &d.units {
        for n in &u.circuit.nets {
            all.push(NetRef { board: u.id, net: n.id });
        }
    }
    all.sort();
    let ix = |r: NetRef| all.binary_search(&r).ok();
    let mut parent: Vec<usize> = (0..all.len()).collect();
    for l in &d.links {
        for (a, b) in d.link_pairs(l) {
            if let (Some(na), Some(nb)) = (d.net_of(a).and_then(ix), d.net_of(b).and_then(ix)) {
                let (ra, rb) = (find(&mut parent, na), find(&mut parent, nb));
                if ra != rb {
                    parent[ra.max(rb)] = ra.min(rb);
                }
            }
        }
    }
    let mut groups: Vec<Vec<NetRef>> = Vec::new();
    let mut slot: Vec<Option<usize>> = vec![None; all.len()];
    for i in 0..all.len() {
        let r = find(&mut parent, i);
        let g = *slot[r].get_or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(all[i]);
    }
    groups
        .into_iter()
        .map(|members| {
            let mut aliases: Vec<String> = Vec::new();
            let mut sources: Vec<PinRef> = Vec::new();
            let mut volts: Vec<VRange> = Vec::new();
            for m in &members {
                let Some(u) = d.unit(m.board) else { continue };
                let net = &u.circuit.nets[m.net.ix()];
                aliases.push(net.name.clone());
                volts.extend(net.volts);
                for (comp, pin) in u.circuit.pins_of(m.net) {
                    if u.circuit.pin_kind(comp, pin) == Some(PinKind::PowerOut) {
                        sources.push(PinRef { board: m.board, comp, pin });
                    }
                }
            }
            aliases.sort();
            aliases.dedup();
            sources.sort();
            SystemNet { members, aliases, sources, volts: common_volts(volts) }
        })
        .collect()
}
