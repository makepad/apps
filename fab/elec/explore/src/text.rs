//! Text widths as the explorer draws them. The font set is fixed for a process, so the view measures
//! its font's advances once, before its first layout, and every width here comes from them: a box
//! the layout keeps clear is the box the text covers. Until then (and in tests) an estimate stands in.
//!
//! Cost: one table lookup per character; no allocation.

use std::sync::OnceLock;

/// Advance per character as a fraction of the text size, before the font is measured.
pub const ESTIMATE: f32 = 0.62;

/// The drawn font's advances, each as a fraction of the text size.
#[derive(Clone, Debug, PartialEq)]
pub struct Advances {
    /// `' '..='~'`.
    pub ascii: [f32; 95],
    /// Further characters the view measured (a value's `µ` or `Ω`), sorted by character.
    pub other: Vec<(char, f32)>,
    /// Any character not measured: the widest capital, so a box never comes out short.
    pub fallback: f32,
}

impl Advances {
    pub fn advance(&self, c: char) -> f32 {
        match c {
            ' '..='~' => self.ascii[c as usize - ' ' as usize],
            _ => self.other.binary_search_by_key(&c, |&(k, _)| k).map_or(self.fallback, |i| self.other[i].1),
        }
    }
}

static ADVANCES: OnceLock<Advances> = OnceLock::new();

/// Sets the font's advances. The first call wins: there is one font set per process.
pub fn set_advances(advances: Advances) -> bool {
    ADVANCES.set(advances).is_ok()
}

/// Whether the font's advances are known (the view measures before its first layout).
pub fn measured() -> bool {
    ADVANCES.get().is_some()
}

/// The width of `text` drawn at `size`, in the units of `size`.
pub fn width(text: &str, size: f64) -> f64 {
    match ADVANCES.get() {
        Some(a) => text.chars().map(|c| a.advance(c) as f64).sum::<f64>() * size,
        None => text.chars().count() as f64 * size * ESTIMATE as f64,
    }
}
