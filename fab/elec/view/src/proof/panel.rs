//! Monospace text for the checks, layer stack and BOM beside the proof canvas.

use super::scene::ProofScene;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Tone { Head, Ok, Fail, Dim, Normal }

#[derive(Clone, PartialEq, Debug)]
pub struct PanelLine {
    pub text: String,
    pub tone: Tone,
}

/// The panel: a summary head, every check (failures first, each with its cause and fix hint),
/// the layers (file, label, object count), drills and placement counts, then the BOM table.
pub fn panel_lines(scene: &ProofScene, chars: usize) -> Vec<PanelLine> {
    let mut lines = Vec::new();
    let mut push = |text: String, tone| lines.push(PanelLine { text: clip(&text, chars), tone });
    let failures = scene.validation.failures().count();
    push(if failures == 0 { "Export: ok".into() } else { format!("Export: {failures} checks failed") }, Tone::Head);
    for check in scene.validation.failures().chain(scene.validation.checks.iter().filter(|c| c.ok)) {
        let kind = format!("{:?}", check.kind).to_lowercase();
        let (status, tone) = if check.ok { ("ok ", Tone::Ok) } else { ("FAIL", Tone::Fail) };
        push(format!("{status} {} {kind}: {}", check.file, check.cause), tone);
        if !check.ok {
            push(format!("     fix: {}", check.hint), Tone::Dim);
        }
    }
    push(String::new(), Tone::Normal);
    push("Layers".into(), Tone::Head);
    for layer in scene.layers.iter().rev() {
        push(format!("{}  {}  {} objects", layer.label, layer.file, layer.items.len()), Tone::Normal);
    }
    if !scene.drills.is_empty() {
        let slots = scene.drills.iter().filter(|h| h.to.is_some()).count();
        push(format!("Drill  {} holes ({slots} slots)", scene.drills.len()), Tone::Normal);
    }
    if !scene.dots.is_empty() {
        push(format!("Placement  {} parts", scene.dots.len()), Tone::Normal);
    }
    push(String::new(), Tone::Normal);
    push("BOM".into(), Tone::Head);

    let mut widths = Vec::new();
    for row in &scene.bom {
        widths.resize(widths.len().max(row.len()), 0);
        for (col, cell) in row.iter().enumerate() {
            widths[col] = widths[col].max(cell.chars().take(24).count());
        }
    }
    for (row_index, row) in scene.bom.iter().enumerate() {
        let mut text = String::new();
        for (col, &width) in widths.iter().enumerate() {
            if col != 0 {
                text.push_str("  ");
            }
            let cell = clip(row.get(col).map(String::as_str).unwrap_or(""), width);
            text.push_str(&cell);
            text.extend(std::iter::repeat(' ').take(width - cell.chars().count()));
        }
        push(text, if row_index == 0 { Tone::Dim } else { Tone::Normal });
    }
    lines
}

/// One text line cut to `chars` characters with a trailing `~` when cut (chars >= 2).
pub fn clip(text: &str, chars: usize) -> String {
    let mut iter = text.chars();
    let mut out: String = iter.by_ref().take(chars).collect();
    if chars > 0 && iter.next().is_some() {
        out.pop();
        out.push('~');
    }
    out
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
