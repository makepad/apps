//! Pure proof-layer painting in world millimetres, Y up.
//!
//! Each conversion takes O(items) time and output space, with one instance per item.

use super::inst::Inst;
use super::scene::{Layer, Prim, Role};
use crate::canvas::draw::{ArcInst, BoxInst, Common, LineInst};
use crate::canvas::looks::{Look, Tokens};
use fab_elec_core::Side;
use std::f32::consts::TAU;

/// The straight-alpha ink for a fabrication layer's role.
pub fn role_ink(role: Role, tokens: &Tokens) -> [f32; 4] {
    match role {
        Role::Copper { side: Some(Side::Top), .. } => tokens.wire_power,
        Role::Copper { side: Some(Side::Bottom), .. } => tokens.hover,
        Role::Copper { side: None, .. } => tokens.wire_ground,
        Role::Mask(_) => tokens.ok,
        Role::Paste(_) | Role::Other => tokens.ink_dim,
        Role::Silk(_) => tokens.ink,
        Role::Edge => tokens.warning,
    }
}

/// Paint a layer in item order, retaining world coordinates and item identities.
pub fn layer_insts(layer: &Layer, look: Look, alpha: f32, tokens: &Tokens) -> Vec<Inst> {
    let mut ink = role_ink(layer.role, tokens);
    ink[3] *= alpha;
    if matches!(layer.role, Role::Copper { side: Some(Side::Bottom), .. }) {
        ink[3] *= 0.85;
    }
    let stroke = if matches!(layer.role, Role::Mask(_) | Role::Edge) {
        [1.0, 0.0]
    } else {
        [0.0; 2]
    };
    let mut fill = ink;
    match layer.role {
        Role::Mask(_) => fill[3] *= 0.25,
        Role::Edge => fill = [0.0; 4],
        Role::Silk(_) => fill[3] = 1.0,
        _ => {}
    }

    let mut insts = Vec::with_capacity(layer.items.len());
    for item in &layer.items {
        let pose = match item.prim {
            Prim::Box { c, rot, .. } => [c[0], c[1], rot],
            _ => [0.0; 3],
        };
        let mut common = Common::at(pose, ink);
        common.look = look.code();
        common.item = item.id as f32;
        insts.push(match item.prim {
            Prim::Line { a, b, w } => Inst::Line(LineInst {
                c: common,
                seg: [a[0], a[1], b[0], b[1]],
                width: [1.0, w],
                cap: 0.0,
                ..Default::default()
            }),
            Prim::Arc { c, r, start, sweep, w } => Inst::Arc(ArcInst {
                c: common,
                circle: [c[0], c[1], r, 0.0],
                sweep: [start, sweep],
                stroke: [1.0, w],
                fill: [0.0; 4],
                ..Default::default()
            }),
            Prim::Disc { c, r } => Inst::Arc(ArcInst {
                c: common,
                circle: [c[0], c[1], r, 0.0],
                sweep: [0.0, TAU],
                stroke,
                fill,
                ..Default::default()
            }),
            Prim::Box { half, corner, .. } => {
                let mut fill = fill;
                if matches!(layer.role, Role::Paste(_)) {
                    fill[3] *= 0.55;
                }
                Inst::Box(BoxInst {
                    c: common,
                    rect: [0.0, 0.0, half[0], half[1]],
                    corner,
                    stroke,
                    fill,
                })
            }
        });
    }
    insts
}

#[cfg(test)]
#[path = "paint_tests.rs"]
mod tests;
