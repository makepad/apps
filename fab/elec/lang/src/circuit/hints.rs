//! Read evaluated sheet entries, retaining source keys for late path warnings.

use super::Here;
use super::read::{Obj, V};
use fab_elec_circuit::{Flow, NetStyle, PortSide, SheetHint, SheetHints};
use fab_elec_core::SchPt;
use makepad_script::syntax::{Field, Kind, Node};
use std::ops::Range;

type Error = (Here, String);

pub(super) fn read(
    value: &V,
    written: Option<&Node>,
    source: &str,
    call: &Here,
) -> Result<(SheetHints, Vec<(String, Here)>), Error> {
    let written = match written.map(|node| &node.kind) {
        Some(Kind::Entry { value, .. }) => Some(value.as_ref()),
        _ => written,
    };
    let table = object(value, written, call)?;
    let mut hints = SheetHints::default();
    let mut keys = Vec::new();
    for (key, value) in &table.fields {
        let entry = field(written, key);
        let at = at(call, entry.map(|f| &f.name_span));
        let node = entry.map(|f| &f.value);
        let opts = object(value, node, &at)?;
        if key == "nets" {
            for (name, value) in &opts.fields {
                let style = match value.id() {
                    Some("wire") => NetStyle::Wire,
                    Some("label") => NetStyle::Label,
                    _ => return Err((field_at(node, name, &at), format!("sheet net {name}: expected @label or @wire"))),
                };
                hints.nets.push((name.clone(), style));
            }
            continue;
        }
        if entry.is_some_and(|f| !source[f.name_span.clone()].starts_with('"')) {
            return Err((at, "sheet paths must be quoted strings".into()));
        }
        let path = key.strip_suffix(".pins").unwrap_or(key);
        let index = match hints.parts.iter().position(|(name, _)| name == path) {
            Some(index) => index,
            None => {
                hints.parts.push((path.to_string(), SheetHint::default()));
                keys.push((path.to_string(), at.clone()));
                hints.parts.len() - 1
            }
        };
        let hint = &mut hints.parts[index].1;
        if key.ends_with(".pins") {
            let mut pins = Vec::new();
            for (name, value) in &opts.fields {
                let side = match value.id() {
                    Some("left") => PortSide::Left,
                    Some("right") => PortSide::Right,
                    Some("top") => PortSide::Top,
                    Some("bottom") => PortSide::Bottom,
                    _ => return Err((field_at(node, name, &at), format!("sheet pin {name}: expected @left, @right, @top or @bottom"))),
                };
                pins.push((name.clone(), side));
            }
            hint.pins = pins;
            continue;
        }
        let mut next = SheetHint::default();
        for (name, value) in &opts.fields {
            let at = field_at(node, name, &at);
            let error = |message: &str| (at.clone(), format!("sheet {path}: {message}"));
            match name.as_str() {
                "at" => {
                    let point = value.list().filter(|items| items.len() == 2)
                        .and_then(|items| Some(SchPt::new(grid(&items[0])?, grid(&items[1])?)))
                        .ok_or_else(|| error("at must be two integers in grid steps"))?;
                    next.at = Some(point);
                }
                "flow" => next.flow = Some(match value.id() {
                    Some("right") => Flow::Right,
                    Some("down") => Flow::Down,
                    Some("left") => Flow::Left,
                    Some("up") => Flow::Up,
                    _ => return Err(error("flow must be @right, @down, @left or @up")),
                }),
                "turn" => {
                    let angle = value.num().filter(|n| n.is_finite())
                        .ok_or_else(|| error("turn must be an angle in multiples of 90deg"))?;
                    let quarters = (angle / std::f64::consts::FRAC_PI_2).round();
                    if (angle - quarters * std::f64::consts::FRAC_PI_2).abs() > 1e-9 {
                        return Err(error("turn must be a multiple of 90deg"));
                    }
                    next.turn = quarters.rem_euclid(4.0) as u8;
                }
                "mirror" => next.mirror = value.bool().ok_or_else(|| error("mirror: expected true or false"))?,
                "free" => next.free = value.bool().ok_or_else(|| error("free: expected true or false"))?,
                _ => return Err(error(&format!("unknown option {name} (the options are at, flow, turn, mirror, free)"))),
            }
        }
        next.pins = std::mem::take(&mut hint.pins);
        *hint = next;
    }
    Ok((hints, keys))
}

fn grid(value: &V) -> Option<i32> {
    let number = value.num()?;
    (number.fract() == 0.0 && number >= i32::MIN as f64 && number <= i32::MAX as f64)
        .then_some(number as i32)
}

fn object<'a>(value: &'a V, node: Option<&Node>, fallback: &Here) -> Result<&'a Obj, Error> {
    let location = at(fallback, node.map(|node| &node.span));
    let object = value.obj().ok_or_else(|| (location.clone(), "sheet entries must be objects".into()))?;
    if !object.items.is_empty() {
        let bare = node.and_then(Node::object).and_then(|(_, fields)| fields.iter().find(|field| field.name.is_none()));
        return Err((at(&location, bare.map(|field| &field.span)), "sheet entries need a key: value".into()));
    }
    Ok(object)
}

fn field<'a>(node: Option<&'a Node>, name: &str) -> Option<&'a Field> {
    node?.object()?.1.iter().rev().find(|field| field.name.as_deref() == Some(name))
}

fn field_at(node: Option<&Node>, name: &str, fallback: &Here) -> Here {
    at(fallback, field(node, name).map(|field| &field.span))
}

fn at(fallback: &Here, span: Option<&Range<usize>>) -> Here {
    let mut at = fallback.clone();
    if let Some(span) = span {
        at.span = span.start as u32..span.end as u32;
    }
    at
}
