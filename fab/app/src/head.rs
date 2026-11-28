//! The print head's envelope for non-planar tops (NONPLANAR-DESIGN §2, §4).
//!
//! The user's measured head lives in `~/Documents/Fab/head-envelope.conf` (the
//! `HeadEnvelope::parse` format, with `measured = true`). Without it, slicing and the preview
//! run on the pessimistic placeholder, and the print check refuses every non-planar move.

use fab_printer_link::safety::nonplanar::HeadEnvelope;
use makepad_fab_slicer::nonplanar::NpBox;

/// The installed envelope, when the file is there and reads. Whether it may allow
/// non-planar moves is the print check's call (`HeadEnvelope::usable`).
pub fn installed_from(path: &std::path::Path) -> Option<HeadEnvelope> {
    let text = std::fs::read_to_string(path).ok()?;
    HeadEnvelope::parse(&text).ok()
}

/// What the slicer plans with: the installed envelope, else the placeholder.
pub fn for_slicing_from(path: &std::path::Path) -> HeadEnvelope {
    installed_from(path).unwrap_or_else(HeadEnvelope::placeholder_two_nozzle)
}

/// The slicer's head boxes: both nozzles' parts together (a part may print on either, and
/// the union is the stricter head).
pub fn boxes(h: &HeadEnvelope) -> Vec<NpBox> {
    let mut out: Vec<NpBox> = Vec::new();
    for (side, parts) in h.parts.iter().enumerate() {
        for p in parts {
            out.push(NpBox { name: format!("{} ({})", p.name, if side == 0 { "left" } else { "right" }), ox: p.ox, oy: p.oy, hx: p.hx, hy: p.hy, hole: p.hole, e: p.e, margin: p.margin });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placeholder_slices_but_never_prints() {
        let h = HeadEnvelope::placeholder_two_nozzle();
        assert!(h.usable().is_err());
        let b = boxes(&h);
        assert_eq!(b.len(), h.parts.iter().map(Vec::len).sum::<usize>());
        assert!(b.iter().all(|x| x.e >= x.margin && x.margin > 0.0));
        // The other nozzle on both sides.
        assert!(b.iter().any(|x| x.ox > 20.0) && b.iter().any(|x| x.ox < -20.0));
    }
}
