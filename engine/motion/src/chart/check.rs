//! Checks on a document's charts over time: labels that overlap each
//! other, and labels cut by the frame or run into the safe margins.
//! Text is measured with the chart's own estimate (Inter's advances), so
//! the checks run without a GPU.

use super::scale::text_box;
use super::TEXT_ROLES;
use crate::frame::{Item, ItemKind};
use crate::model::{Align, VAlign};
use crate::MotionDoc;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChartSeverity {
    Error,
    Warning,
}

/// One finding: a stable code (`chart.label_overlap`,
/// `chart.label_off_frame`, `chart.label_outside_safe`), the layer, what is
/// wrong, how to fix it, and when (seconds on the document's clock).
#[derive(Clone, Debug, PartialEq)]
pub struct ChartIssue {
    pub code: &'static str,
    pub severity: ChartSeverity,
    /// The label's layer path (the first of the pair for an overlap).
    pub path: String,
    /// The other label of an overlapping pair.
    pub other: Option<String>,
    pub message: String,
    pub hint: String,
    pub from: f64,
    pub to: f64,
}

/// A chart text's box in document units: `[x0, y0, x1, y1]`.
fn text_bounds(item: &Item, opacity: f64) -> Option<[f64; 4]> {
    let ItemKind::Text { text, size, weight, align, valign, line_height, wrap, fit, plate, reveal, .. } = &item.kind else { return None };
    if opacity < 0.6 || text.trim().is_empty() || reveal == &Some(0) {
        return None;
    }
    let [w, h] = text_box(text, *size * super::scale::PX, *weight, *wrap, *fit, *line_height);
    let af = match align {
        Align::Left => 0.0,
        Align::Center => 0.5,
        Align::Right => 1.0,
    };
    let vf = match valign {
        VAlign::Top => 0.0,
        VAlign::Center => 0.5,
        VAlign::Bottom => 1.0,
    };
    let pad = plate.as_ref().map_or([0.0, 0.0], |p| p.pad);
    let map = item.transform([w, h]);
    let (x0, y0) = (-w * af - pad[0], -h * vf - pad[1]);
    let corners = [[x0, y0], [x0 + w + 2.0 * pad[0], y0], [x0, y0 + h + 2.0 * pad[1]], [x0 + w + 2.0 * pad[0], y0 + h + 2.0 * pad[1]]].map(|p| map.apply(p));
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in corners {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    Some([lo[0], lo[1], hi[0], hi[1]])
}

/// Every chart text visible at a moment: (chart path, label path, text, box).
fn chart_texts(items: &[Item], parent_opacity: f64, roles: &HashMap<String, (String, String)>, out: &mut Vec<(String, String, String, [f64; 4])>) {
    for item in items {
        let opacity = parent_opacity * item.opacity;
        if let Some((root, name)) = roles.get(&item.path) {
            if let Some(b) = text_bounds(item, opacity) {
                out.push((root.clone(), item.path.clone(), name.clone(), b));
            }
        }
        if let ItemKind::Group { children, .. } = &item.kind {
            chart_texts(children, opacity, roles, out);
        }
    }
}

/// Checks every chart of `doc` at `times` (for example `doc.frame_times()`
/// or a coarser sampling). Overlaps count once they last 0.3 s, so labels
/// passing each other during a morph are fine.
pub fn chart_checks(doc: &MotionDoc, times: &[f64]) -> Vec<ChartIssue> {
    // Chart texts are named `<role> · <text>` by the chart; their parent
    // (the chart's Group) is the chart.
    let mut roles: HashMap<String, (String, String)> = HashMap::new();
    for layer in doc.all_layers() {
        let Some(name) = &layer.name else { continue };
        let Some((role, _)) = name.split_once(" · ") else { continue };
        if !TEXT_ROLES.contains(&role) {
            continue;
        }
        let Some(cut) = layer.path.rfind(".layers[") else { continue };
        roles.insert(layer.path.clone(), (layer.path[..cut].to_string(), name.clone()));
    }
    if roles.is_empty() || times.is_empty() {
        return Vec::new();
    }
    let [w, h] = doc.size;
    let step = times.windows(2).map(|p| p[1] - p[0]).fold(f64::MAX, f64::min).clamp(1e-3, 1.0);
    let side = (1.0 - crate::SAFE_WIDTH) / 2.0 * w;
    let (top, bottom) = (0.05 * h, 0.95 * h);
    // (a, b) → (first, last, seconds overlapping, names)
    let mut overlaps: BTreeMap<(String, String), (f64, f64, f64, String, String)> = BTreeMap::new();
    let mut off: BTreeMap<String, (f64, f64, f64, String, bool)> = BTreeMap::new();
    for &t in times {
        let frame = doc.eval(t);
        let mut texts = Vec::new();
        chart_texts(&frame.items, 1.0, &roles, &mut texts);
        for (i, (root, path, name, b)) in texts.iter().enumerate() {
            let cut = b[0] < -1.0 || b[1] < -1.0 || b[2] > w + 1.0 || b[3] > h + 1.0;
            let outside = b[0] < side - 1.0 || b[2] > w - side + 1.0 || b[1] < top - 1.0 || b[3] > bottom + 1.0;
            if cut || outside {
                let e = off.entry(path.clone()).or_insert((t, t, 0.0, name.clone(), false));
                e.1 = t;
                e.2 += step;
                e.4 |= cut;
            }
            for (root2, path2, name2, b2) in &texts[i + 1..] {
                if root != root2 {
                    continue;
                }
                let ix = (b[2].min(b2[2]) - b[0].max(b2[0])).max(0.0);
                let iy = (b[3].min(b2[3]) - b[1].max(b2[1])).max(0.0);
                let smaller = ((b[2] - b[0]) * (b[3] - b[1])).min((b2[2] - b2[0]) * (b2[3] - b2[1])).max(1.0);
                if ix * iy > 0.12 * smaller && ix > 2.0 && iy > 2.0 {
                    let e = overlaps.entry((path.clone(), path2.clone())).or_insert((t, t, 0.0, name.clone(), name2.clone()));
                    e.1 = t;
                    e.2 += step;
                }
            }
        }
    }
    let mut issues = Vec::new();
    let quote = |name: &str| name.split_once(" · ").map_or(name.to_string(), |(role, text)| format!("the {role} \"{text}\""));
    for ((a, b), (from, to, seconds, na, nb)) in overlaps {
        if seconds < 0.3 - 1e-9 {
            continue;
        }
        issues.push(ChartIssue {
            code: "chart.label_overlap",
            severity: ChartSeverity::Warning,
            path: a,
            other: Some(b),
            message: format!("{} overlaps {} for {seconds:.1} s", quote(&na), quote(&nb)),
            hint: "give the chart more room (`size`), a smaller `label_size`, fewer categories (`top: 8`), shorter labels, or `labels: false` with a `legend: true`".into(),
            from,
            to,
        });
    }
    for (path, (from, to, seconds, name, cut)) in off {
        if seconds < 0.3 - 1e-9 {
            continue;
        }
        issues.push(ChartIssue {
            code: if cut { "chart.label_off_frame" } else { "chart.label_outside_safe" },
            severity: ChartSeverity::Warning,
            path,
            other: None,
            message: if cut { format!("{} runs past the frame's edge", quote(&name)) } else { format!("{} runs into the frame's safe margins", quote(&name)) },
            hint: "move the chart inwards (`at`) or shrink it (`size`); a chart's default box is the safe width, centred".into(),
            from,
            to,
        });
    }
    issues
}
