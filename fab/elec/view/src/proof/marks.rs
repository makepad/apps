//! Drill holes, placement dots, and failed checks as proof-canvas instances.

use super::inst::Inst;
use super::scene::{Dot, Hit, Marker};
use crate::canvas::draw::{ArcInst, BoxInst, Common, LineInst};
use crate::canvas::looks::{Look, Tokens};
use fab_elec_core::Side;
use std::f32::consts::TAU;

/// Drill holes and slots in world millimetres, with plating shown by the rim.
pub fn drill_insts(hits: &[Hit], tokens: &Tokens) -> Vec<Inst> {
    let mut out = Vec::with_capacity(hits.len());
    for hit in hits {
        let ink = if hit.plated == Some(false) { tokens.ink_dim } else { tokens.ink };
        let r = hit.dia / 2.0;
        let mut c = Common::at([0.0; 3], ink);
        c.item = hit.id as f32;
        if let Some(to) = hit.to {
            let dx = to[0] - hit.at[0];
            let dy = to[1] - hit.at[1];
            c = Common::at([(hit.at[0] + to[0]) / 2.0, (hit.at[1] + to[1]) / 2.0, dy.atan2(dx)], ink);
            c.item = hit.id as f32;
            out.push(Inst::Box(BoxInst {
                c,
                rect: [0.0, 0.0, dx.hypot(dy) / 2.0 + r, r],
                corner: r,
                stroke: [1.0, 0.0],
                fill: tokens.canvas,
            }));
        } else {
            out.push(Inst::Arc(ArcInst {
                c,
                circle: [hit.at[0], hit.at[1], r, 0.0],
                sweep: [0.0, TAU],
                stroke: [1.0, 0.0],
                fill: tokens.canvas,
                ..ArcInst::default()
            }));
        }
    }
    out
}

/// A side-coloured disc followed by an orientation tick for each placed part.
pub fn dot_insts(dots: &[Dot], tokens: &Tokens) -> Vec<Inst> {
    let mut out = Vec::with_capacity(dots.len() * 2);
    for dot in dots {
        let fill = match dot.side {
            Side::Top => [tokens.selection[0], tokens.selection[1], tokens.selection[2], 0.9],
            Side::Bottom => tokens.hover,
        };
        let mut c = Common::at([0.0; 3], fill);
        c.item = dot.id as f32;
        out.push(Inst::Arc(ArcInst {
            c,
            circle: [dot.at[0], dot.at[1], 0.2, 0.0],
            sweep: [0.0, TAU],
            fill,
            ..ArcInst::default()
        }));
        c.ink = tokens.ink;
        let (sin, cos) = dot.rot.sin_cos();
        out.push(Inst::Line(LineInst {
            c,
            seg: [dot.at[0], dot.at[1], dot.at[0] + 0.8 * cos, dot.at[1] + 0.8 * sin],
            width: [1.0, 0.08],
            ..LineInst::default()
        }));
    }
    out
}

/// A violation ring followed by horizontal and vertical crosshairs per failure.
pub fn marker_insts(markers: &[Marker], tokens: &Tokens) -> Vec<Inst> {
    let mut out = Vec::with_capacity(markers.len() * 3);
    for marker in markers {
        let [x, y] = marker.at;
        let mut c = Common::at([0.0; 3], tokens.violation);
        c.item = marker.id as f32;
        c.look = Look::Violation.code();
        out.push(Inst::Arc(ArcInst {
            c,
            circle: [x, y, 0.8, 0.0],
            sweep: [0.0, TAU],
            stroke: [2.0, 0.0],
            fill: [0.0; 4],
            ..ArcInst::default()
        }));
        for seg in [[x - 1.2, y, x + 1.2, y], [x, y - 1.2, x, y + 1.2]] {
            out.push(Inst::Line(LineInst { c, seg, width: [1.0, 0.0], ..LineInst::default() }));
        }
    }
    out
}

#[cfg(test)]
#[path = "marks_tests.rs"]
mod tests;
