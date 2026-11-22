//! The explorer's font, measured once per process (the font set is fixed for a process) and handed
//! to the layout (`fab_elec_explore::text`) before its first layout, so every box the layout keeps
//! clear is the box its text covers when drawn.
//!
//! Cost: about a hundred one-character layouts, once, on the UI thread.

use super::scene::text_points;
use fab_elec_explore::text::{self, Advances};
use makepad_widgets_core::*;

/// Characters beyond ASCII that values and net names use.
const OTHER: [char; 6] = ['°', '±', 'µ', '·', 'Ω', '→'];

/// The size the advances are measured at, in points: large, so rounding does not show.
const SIZE: f32 = 100.0;

/// Measures `draw`'s font unless it is measured already.
pub fn measure_once(cx: &mut Cx, draw: &mut DrawText) {
    if text::measured() {
        return;
    }
    let old = draw.text_style.font_size;
    draw.text_style.font_size = SIZE;
    // A text `s` high draws at text_points(s, ppu) points and covers its laid-out width in logical
    // pixels over ppu in world units: per unit of size, the width at SIZE points times this.
    let k = text_points(1.0, 1.0) as f32 / SIZE;
    let mut width = |s: &str| draw.layout(cx, 0.0, 0.0, None, false, Align::default(), s).size_in_lpxs.width as f32 * k;
    let mut ascii = [0.0f32; 95];
    for (i, advance) in ascii.iter_mut().enumerate() {
        let c = (b' ' + i as u8) as char;
        // A lone space lays out empty: take it between two letters.
        *advance = if c == ' ' { width("n n") - width("nn") } else { width(c.encode_utf8(&mut [0; 4])) };
    }
    let mut other: Vec<(char, f32)> = OTHER.iter().map(|&c| (c, width(c.encode_utf8(&mut [0; 4])))).collect();
    other.sort_by_key(|&(c, _)| c);
    draw.text_style.font_size = old;
    let capitals = &ascii[('A' as usize - ' ' as usize)..=('Z' as usize - ' ' as usize)];
    let fallback = capitals.iter().copied().fold(0.0, f32::max);
    let m = ascii['M' as usize - ' ' as usize];
    let i = ascii['i' as usize - ' ' as usize];
    if text::set_advances(Advances { ascii, other, fallback }) {
        log!("fab.explorer: font measured: M {m:.3}, i {i:.3}, widest capital {fallback:.3} of the text size");
    }
}
