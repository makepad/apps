//! Molecules: small parts that belong to a pin or to each other, found from connectivity alone
//! (E2.2). A molecule is rigid: it is laid out once in its own frame and hangs on its host's pin,
//! so the anchors' placement never sees its parts. The kinds:
//!
//! - decoupling: supply-to-ground capacitors in a row under their chip, joined by rail tags.
//! - pull: a rail-ended part in line with its host pin, with a rail tag at its far end.
//! - chain: small parts (an LED and its resistor) in a vertical stack, rail at the rail's side.
//! - series: a two-pin part in line with its host pin, its far signal end handed to the router.
//! - crystal: parallel to its chip side, wired to both host pins, load caps outward and down.
//! - flag: a one-pin supply marker on its net's first chip or connector pin, else its first pin.
//!
//! Detection visits each block's own parts in source order (chains, crystals, decoupling, pulls,
//! then series). Flags follow their nets across blocks. Each part joins at most one molecule.

use crate::graph::{LayoutGraph, NetKind, Role};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MolKind {
    Decoupling,
    /// To a supply (up) or to ground.
    Pull { up: bool },
    Chain,
    Series,
    Crystal,
    Flag,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Molecule {
    pub kind: MolKind,
    /// Member parts; chains run from first free end to last; crystals precede caps in pin order.
    pub parts: Vec<u32>,
    /// The pin it hangs on: (node, port).
    pub host: Option<(u32, u32)>,
    pub block: u32,
}

/// Facts about nets the detection needs from the caller: per net, whether a block port binds it
/// (so it leaves its block and is not private).
pub fn find(g: &LayoutGraph, port_nets: &[Vec<Option<u32>>], bound: &[bool]) -> Vec<Molecule> {
    let mut used = vec![false; g.nodes.len()];
    let mut out = Vec::new();
    let two_pin = |n: usize| g.nodes[n].role.is_small() && g.nodes[n].ports.len() == 2;
    let kind_of = |net: Option<u32>| net.map(|n| g.nets[n as usize].kind);
    let private = |net: u32| {
        let n = &g.nets[net as usize];
        n.kind == NetKind::Signal && !bound[net as usize] && g.unflagged_pins(net).take(3).count() == 2
    };
    for (bi, parts) in g.parts_of().iter().enumerate() {
        // Chains: small parts joined through private nets.
        for &start in parts {
            let s = start as usize;
            if used[s] || !two_pin(s) {
                continue;
            }
            let mut chain = vec![start];
            // Walk both ways from the start through private nets to other small parts.
            for dir in 0..2 {
                let mut cur = start as usize;
                let mut came: Option<u32> = None;
                loop {
                    let next = (0..2).find_map(|p| {
                        let net = port_nets[cur][p]?;
                        if Some(net) == came || !private(net) {
                            return None;
                        }
                        let other = g.unflagged_pins(net).find(|&(n, _)| n as usize != cur)?.0 as usize;
                        (two_pin(other) && !used[other] && g.nodes[other].block as usize == bi && !chain.contains(&(other as u32)))
                            .then_some((other, net))
                    });
                    let Some((other, net)) = next else { break };
                    if dir == 0 {
                        chain.push(other as u32);
                    } else {
                        chain.insert(0, other as u32);
                    }
                    came = Some(net);
                    cur = other;
                }
                if dir == 0 && chain.len() == 1 {
                    continue;
                }
            }
            if chain.len() >= 2 {
                for &p in &chain {
                    used[p as usize] = true;
                }
                out.push(Molecule { kind: MolKind::Chain, parts: chain, host: None, block: bi as u32 });
            }
        }
        // Hosts: the block's anchors (not small), chips first, in source order.
        let is_anchor = |n: u32| {
            let role = g.nodes[n as usize].role;
            !role.is_small() && role != Role::Flag
        };
        let mut hosts: Vec<u32> = parts.iter().copied().filter(|&n| is_anchor(n)).collect();
        hosts.sort_by_key(|&n| (g.nodes[n as usize].role != Role::Ic, g.nodes[n as usize].order, n));
        let host_on = |net: u32| -> Option<(u32, u32)> {
            hosts.iter().find_map(|&h| {
                (0..g.nodes[h as usize].ports.len()).find(|&p| port_nets[h as usize][p] == Some(net)).map(|p| (h, p as u32))
            })
        };
        // Crystals: two private oscillator nets on the same side of one chip. Unlike a
        // chain's links, these nets may also carry the crystal's load capacitors.
        for &n in parts {
            let ni = n as usize;
            if used[ni] || g.nodes[ni].role != Role::Crystal || !two_pin(ni) {
                continue;
            }
            let [Some(a), Some(b)] = [port_nets[ni][0], port_nets[ni][1]] else { continue };
            if a == b || [a, b].iter().any(|&net| bound[net as usize] || g.nets[net as usize].kind != NetKind::Signal) {
                continue;
            }
            let chip_pin = |net: u32| {
                let mut pins = g.nets[net as usize].pins.iter().copied().filter(|&(h, _)| is_anchor(h));
                let (h, hp) = pins.next()?;
                (pins.next().is_none() && g.nodes[h as usize].role == Role::Ic && g.nodes[h as usize].block as usize == bi)
                    .then_some((h, hp))
            };
            let (Some((h, hp)), Some((other, op))) = (chip_pin(a), chip_pin(b)) else { continue };
            if h != other || hp == op || g.nodes[h as usize].ports[hp as usize].side != g.nodes[h as usize].ports[op as usize].side {
                continue;
            }
            let mut members = vec![n];
            for net in [a, b] {
                let mut caps: Vec<u32> = g.nets[net as usize].pins.iter().filter_map(|&(cap, pin)| {
                    let ci = cap as usize;
                    (g.nodes[ci].block as usize == bi && !used[ci] && g.nodes[ci].role == Role::Capacitor && two_pin(ci)
                        && kind_of(port_nets[ci][1 - pin as usize]) == Some(NetKind::Ground))
                        .then_some(cap)
                }).collect();
                caps.sort_by_key(|&cap| (g.nodes[cap as usize].order, cap));
                members.extend(caps);
            }
            for &member in &members {
                used[member as usize] = true;
            }
            out.push(Molecule { kind: MolKind::Crystal, parts: members, host: Some((h, hp)), block: bi as u32 });
        }
        // Decoupling precedes every pull, independently of component source order.
        for &n in parts {
            let ni = n as usize;
            if used[ni] || !two_pin(ni) {
                continue;
            }
            let nets = [port_nets[ni][0], port_nets[ni][1]];
            let kinds = [kind_of(nets[0]), kind_of(nets[1])];
            // Decoupling: supply to ground.
            if g.nodes[ni].role == Role::Capacitor
                && (kinds == [Some(NetKind::Power), Some(NetKind::Ground)] || kinds == [Some(NetKind::Ground), Some(NetKind::Power)])
            {
                let supply = if kinds[0] == Some(NetKind::Power) { nets[0] } else { nets[1] };
                // A `near` intent names the chip (and maybe the pin) it decouples; else the block's
                // first chip on that supply.
                let near = g.nodes[ni].near.and_then(|(h, pin)| {
                    let hu = h as usize;
                    if hu >= g.nodes.len() || g.nodes[hu].block as usize != bi || !is_anchor(h) {
                        return None;
                    }
                    let on_supply = |p: usize| supply.is_some() && port_nets[hu][p] == supply;
                    match pin {
                        Some(p) if (p as usize) < port_nets[hu].len() && on_supply(p as usize) => Some((h, p)),
                        _ => (0..port_nets[hu].len()).find(|&p| on_supply(p)).map(|p| (h, p as u32)),
                    }
                });
                used[ni] = true;
                out.push(Molecule { kind: MolKind::Decoupling, parts: vec![n], host: near.or_else(|| supply.and_then(host_on)), block: bi as u32 });
            }
        }
        for &n in parts {
            let ni = n as usize;
            if used[ni] || !two_pin(ni) {
                continue;
            }
            let nets = [port_nets[ni][0], port_nets[ni][1]];
            let kinds = [kind_of(nets[0]), kind_of(nets[1])];
            let rail = |k: Option<NetKind>| matches!(k, Some(NetKind::Power | NetKind::Ground));
            // Pull: a rail at one end, a net that joins only a host pin at the other.
            for (r, s) in [(0, 1), (1, 0)] {
                let (Some(sig), true) = (nets[s], rail(kinds[r])) else { continue };
                if !private(sig) {
                    continue;
                }
                let other = g.unflagged_pins(sig).find(|&(o, _)| o != n);
                let Some((h, hp)) = other else { continue };
                if g.nodes[h as usize].role.is_small() || g.nodes[h as usize].block as usize != bi {
                    continue;
                }
                used[ni] = true;
                out.push(Molecule {
                    kind: MolKind::Pull { up: kinds[r] == Some(NetKind::Power) },
                    parts: vec![n],
                    host: Some((h, hp)),
                    block: bi as u32,
                });
                break;
            }
        }
        // Series parts run from an otherwise private host pin to a signal that continues.
        // Pulls have already claimed their parts, so a rail can never become a series port.
        for &n in parts {
            let ni = n as usize;
            if used[ni] || !two_pin(ni)
                || !matches!(g.nodes[ni].role, Role::Resistor | Role::Capacitor | Role::Inductor | Role::Fuse | Role::Diode)
            {
                continue;
            }
            for near in 0..2 {
                let (Some(net), Some(far)) = (port_nets[ni][near], port_nets[ni][1 - near]) else { continue };
                if !private(net) || g.nets[far as usize].kind != NetKind::Signal
                    || !(bound[far as usize] || g.unflagged_pins(far).any(|(other, _)| other != n))
                {
                    continue;
                }
                let Some((h, hp)) = g.unflagged_pins(net).find(|&(other, _)| other != n) else { continue };
                if g.nodes[h as usize].role.is_small() || g.nodes[h as usize].block as usize != bi {
                    continue;
                }
                used[ni] = true;
                out.push(Molecule { kind: MolKind::Series, parts: vec![n], host: Some((h, hp)), block: bi as u32 });
                break;
            }
        }
    }
    for (net, wire) in g.nets.iter().enumerate() {
        let mut flags = wire.pins.iter().filter(|&&(n, _)| g.nodes[n as usize].role == Role::Flag).peekable();
        if flags.peek().is_none() { continue }
        let host = g.unflagged_pins(net as u32).min_by_key(|&(n, p)| {
            let node = &g.nodes[n as usize];
            (!matches!(node.role, Role::Ic | Role::Connector), node.order, n, p)
        });
        if let Some((h, hp)) = host {
            for &(flag, _) in flags {
                out.push(Molecule { kind: MolKind::Flag, parts: vec![flag], host: Some((h, hp)), block: g.nodes[h as usize].block });
            }
        }
    }
    out
}
