//! Design frames share the board's fixed-point geometry. The source stays in its register.
use makepad_town_core::entity::{field, Entity};
use makepad_town_core::event::Value;

// Sizes in the same 1/64 canvas units as W and H.
const PRESETS: &[(&str, (i64, i64))] = &[
    ("phone", (390 * 64, 844 * 64)),
    ("tablet", (820 * 64, 1180 * 64)),
    ("desktop", (1440 * 64, 900 * 64)),
    ("window", (1024 * 64, 768 * 64)),
];

pub(crate) fn initial_size(name: &str) -> Result<(i64, i64), String> {
    if name == "custom" { return Ok((640 * 64, 480 * 64)) }
    PRESETS.iter().find(|(preset, _)| *preset == name).map(|&(_, size)| size)
        .ok_or_else(|| format!("unknown design preset `{name}`; choose phone, tablet, desktop, window or custom"))
}

pub(crate) fn preset_for(size: (i64, i64)) -> &'static str {
    PRESETS.iter().find(|(_, dimensions)| *dimensions == size).map_or("custom", |&(name, _)| name)
}

pub(crate) fn size(item: &Entity) -> (i64, i64) {
    let dimension = |key| match item.value(key) { Some(Value::Int(n)) => *n, _ => 0 };
    (dimension(field::W), dimension(field::H))
}

pub(crate) fn preset(item: &Entity) -> &'static str {
    // Partial resizes from different peers and selective undo can combine dimensions that
    // neither writer saw. Classify the folded size on every seat.
    preset_for(size(item))
}

pub(super) fn label(item: &Entity) -> String {
    let title = item.text(field::TITLE);
    format!("{} · {}", if title.is_empty() { "Design" } else { title }, preset(item))
}
