//! Filament sets and the inks they print.
//!
//! An ink is a stack: the filament seen (level 0), the ones under it (levels 1..depth) and a
//! backing behind them. A surface gives each cell one ink; side by side the inks mix in linear
//! light. Each ink's colour is measured (the swatch print) or predicted from its filaments'
//! optics; a set keeps its measurements by the exact physical stack they were taken of.

use crate::oklab::{self, Lab, Rgb};
use crate::optics;

/// The deepest stack a palette makes (dithered levels).
pub const MAX_LEVELS: usize = 3;

/// How strongly a filament scatters before it is measured (per mm): a filament then hides what
/// is under it at a few mm, as most do, and a 0.16 mm layer of white passes about two thirds of
/// the light. The swatch print measures each filament's own.
pub const DEFAULT_SCATTER: f32 = 3.0;

/// One loaded filament: its print colour's name, its colour when thick (linear) and its
/// scattering per mm.
#[derive(Clone, Debug, PartialEq)]
pub struct Filament {
    pub name: String,
    pub colour: Rgb,
    pub scatter: Rgb,
}

impl Filament {
    /// A first guess before the swatch: the spool's colour as sRGB, the default scattering.
    pub fn guess(name: &str, srgb: [u8; 3]) -> Filament {
        Filament { name: name.to_string(), colour: oklab::linear_of_srgb8(srgb), scatter: [DEFAULT_SCATTER; 3] }
    }

    /// The same from a linear colour.
    pub fn guess_linear(name: &str, colour: Rgb) -> Filament {
        Filament { name: name.to_string(), colour, scatter: [DEFAULT_SCATTER; 3] }
    }

    /// A layer of it `mm` thick.
    pub fn layer(&self, mm: f64) -> optics::Layer {
        optics::layer(self.colour, self.scatter, mm)
    }

    /// Light and near neutral: what can back a picture without tinting it.
    pub fn is_white(&self) -> bool {
        let l = oklab::oklab(self.colour);
        l[0] > 0.85 && l[1].hypot(l[2]) < 0.04
    }

    pub fn lightness(&self) -> f32 {
        oklab::oklab(self.colour)[0]
    }
}

/// What lies behind a stack's dithered levels.
#[derive(Clone, Debug, PartialEq)]
pub enum Backing {
    /// That filament (an index into the set), `mm` thick.
    Filament { index: u8, mm: f64 },
    /// The dithered levels again, a layer `layer` thick each, `levels` saying which from the face
    /// back: a deeper colour under the seen one. (A tile repeats in each backing layer the level
    /// whose lines run its way, so its runs keep the dither's rules.)
    Repeat { levels: Vec<usize>, layer: f64 },
}

/// How a surface stacks its filaments: each dithered level's thickness (mm, the seen one first),
/// the backing, and the light behind it all (what the part lies on or holds inside, linear).
#[derive(Clone, Debug, PartialEq)]
pub struct StackModel {
    pub levels: Vec<f64>,
    pub backing: Backing,
    pub background: Rgb,
}

impl StackModel {
    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    /// The physical stack of an ink (`stack`: a filament per level), seen face first: each run of
    /// one filament as one layer of its whole thickness.
    pub fn physical(&self, stack: &[u8]) -> Vec<(u8, f64)> {
        let mut out: Vec<(u8, f64)> = Vec::new();
        let mut push = |f: u8, mm: f64| match out.last_mut() {
            Some((g, t)) if *g == f => *t += mm,
            _ => out.push((f, mm)),
        };
        for (f, mm) in stack.iter().zip(&self.levels) {
            push(*f, *mm);
        }
        match &self.backing {
            Backing::Filament { index, mm } => push(*index, *mm),
            Backing::Repeat { levels, layer } => {
                for &l in levels {
                    push(stack[l.min(stack.len().saturating_sub(1))], *layer);
                }
            }
        }
        out
    }
}

/// A colour measured off a print: the physical stack (filament and thickness, seen face first)
/// over the background it was taken on, and the colour (linear).
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    pub stack: Vec<(u8, f64)>,
    pub background: Rgb,
    pub colour: Rgb,
}

/// A named set of filaments, as loaded on a printer, with what was measured of it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FilamentSet {
    pub name: String,
    pub filaments: Vec<Filament>,
    pub measured: Vec<Measured>,
}

impl FilamentSet {
    pub fn new(name: &str, filaments: Vec<Filament>) -> FilamentSet {
        FilamentSet { name: name.to_string(), filaments, measured: Vec::new() }
    }

    /// The set's white: its most reflective near-neutral filament, if it has one.
    pub fn white(&self) -> Option<u8> {
        let i = (0..self.filaments.len()).filter(|&i| self.filaments[i].is_white()).max_by(|&a, &b| self.filaments[a].lightness().total_cmp(&self.filaments[b].lightness()))?;
        Some(i as u8)
    }

    /// The colour of a physical stack over `background`: measured, or predicted layer by layer
    /// from the back.
    pub fn colour_of(&self, stack: &[(u8, f64)], background: Rgb) -> (Rgb, bool) {
        let same = |m: &Measured| m.background == background && m.stack.len() == stack.len() && m.stack.iter().zip(stack).all(|(a, b)| a.0 == b.0 && (a.1 - b.1).abs() < 1e-3);
        if let Some(m) = self.measured.iter().find(|m| same(m)) {
            return (m.colour, true);
        }
        let mut c = background;
        for (f, mm) in stack.iter().rev() {
            c = optics::over(self.filaments[*f as usize].layer(*mm), c);
        }
        (c, false)
    }
}

/// One ink: a filament per dithered level, the colour it shows (linear and OKLab), and whether
/// that colour was measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ink {
    pub stack: [u8; MAX_LEVELS],
    pub colour: Rgb,
    pub lab: Lab,
    pub measured: bool,
}

/// Every ink a set prints in a stack model: each filament at each level, so any choice of one
/// level's filament has an ink for every choice of the others.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub filaments: usize,
    pub depth: usize,
    pub inks: Vec<Ink>,
}

impl Palette {
    pub fn new(set: &FilamentSet, model: &StackModel) -> Palette {
        let (n, depth) = (set.filaments.len(), model.depth().clamp(1, MAX_LEVELS));
        let count = n.pow(depth as u32);
        let inks = (0..count)
            .map(|i| {
                let mut stack = [0u8; MAX_LEVELS];
                let mut rest = i;
                for level in (0..depth).rev() {
                    stack[level] = (rest % n) as u8;
                    rest /= n;
                }
                let (colour, measured) = set.colour_of(&model.physical(&stack[..depth]), model.background);
                Ink { stack, colour, lab: oklab::oklab(colour), measured }
            })
            .collect();
        Palette { filaments: n, depth, inks }
    }

    /// The ink of a stack (a filament per level).
    pub fn index(&self, stack: &[u8]) -> usize {
        stack[..self.depth].iter().fold(0, |i, &f| i * self.filaments + f as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmyw() -> FilamentSet {
        FilamentSet::new("cmyw", vec![Filament::guess("Cyan", [0, 160, 224]), Filament::guess("Magenta", [224, 0, 122]), Filament::guess("Yellow", [255, 224, 0]), Filament::guess("White", [242, 242, 242])])
    }

    /// A set of four two levels deep has sixteen inks, each found by its stack; white is the
    /// set's white; a stack of one filament merges into one layer, a repeated one keeps its layers in turn; cyan over yellow is yellower
    /// than cyan over white; and a measured stack's colour replaces the prediction.
    #[test]
    fn a_palette_holds_every_stack_and_takes_what_was_measured() {
        let mut set = cmyw();
        assert_eq!(set.white(), Some(3));
        let model = StackModel { levels: vec![0.16, 0.16], backing: Backing::Filament { index: 3, mm: 0.32 }, background: [0.8; 3] };
        assert_eq!(model.physical(&[2, 3]), vec![(2, 0.16), (3, 0.48)]);
        let p = Palette::new(&set, &model);
        assert_eq!(p.inks.len(), 16);
        for (i, ink) in p.inks.iter().enumerate() {
            assert_eq!(p.index(&ink.stack), i);
        }
        let (cy, cw) = (p.inks[p.index(&[0, 2])], p.inks[p.index(&[0, 3])]);
        assert!(cy.lab[2] > cw.lab[2] + 0.02, "cyan over yellow {:?} is yellower than over white {:?}", cy.lab, cw.lab);
        set.measured.push(Measured { stack: model.physical(&[0, 2]), background: [0.8; 3], colour: [0.1, 0.4, 0.1] });
        let m = Palette::new(&set, &model);
        let ink = m.inks[m.index(&[0, 2])];
        assert!(ink.measured && ink.colour == [0.1, 0.4, 0.1]);
        let repeat = StackModel { backing: Backing::Repeat { levels: vec![0, 1], layer: 0.16 }, ..model };
        assert_eq!(repeat.physical(&[0, 2]), vec![(0, 0.16), (2, 0.16), (0, 0.16), (2, 0.16)]);
        assert_eq!(repeat.physical(&[1, 1]), vec![(1, 0.64)]);
    }
}
