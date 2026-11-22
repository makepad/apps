//! Cups: pockets of a print that open toward the film and seal against it (R7 §6.5).
//!
//! The print grows toward the film. A pocket whose open end faces the film, closed all round by
//! what is already printed, traps resin between the print and the film. Each lift pulls on it
//! like a suction cup: up to about the air's pressure on the sealed area, many times the film's
//! own peel. Once the layers close over a pocket, it holds liquid resin that leaks out later or
//! cracks the part when it cures.
//!
//! On each layer, the holes are the empty pixels the cured ones close in. Going up from the
//! plate, a hole joins the holes of the layer below it overlaps. A hole that overlaps the open
//! space of the next layer, below or above, is vented: resin flows in round the side. The plate
//! and the pad vent nothing. A set of holes never vented is a cup on every layer it reaches; when
//! a layer covers all of its holes, the layers have closed over it and what it holds is trapped.

use crate::grid::{Layers, PixelGrid, PixelRect};
use crate::parts::{Holes, LayerParts, Run};
use makepad_fab_slicer::par;

/// A cup: its first and last layers, the layer it seals the most on, how much it seals there
/// (mm²) and where (pixels), and the resin it traps once the layers close over it (mm³; 0 when it
/// is vented first or stays open to the print's last layer).
#[derive(Clone, Debug, PartialEq)]
pub struct Cup {
    pub first: u32,
    pub last: u32,
    pub peak: u32,
    pub area: f64,
    pub rect: PixelRect,
    pub trapped: f64,
}

/// The sealed area a cup counts from (mm²): a square centimetre.
pub const CUP_LEAST: f64 = 100.0;
/// The resin a closed pocket counts from (mm³): a microlitre.
pub const TRAPPED_LEAST: f64 = 1.0;
/// The pull of a sealed pocket per mm² (N): about the air's pressure.
pub const SUCTION: f64 = 0.1;

/// A set of holes being followed up the layers: while it is sealed, its cup so far.
#[derive(Clone, Debug)]
struct Set {
    vented: bool,
    first: u32,
    last: u32,
    peak: u32,
    pixels: u64,
    rect: PixelRect,
    /// Pixels over all its layers so far.
    volume: u64,
}

/// A union-find root, halving the path on the way.
fn root(up: &mut [u32], mut a: u32) -> u32 {
    while up[a as usize] != a {
        up[a as usize] = up[up[a as usize] as usize];
        a = up[a as usize];
    }
    a
}

/// How a hole's run meets the next layer (`other`, its holes `holes`), below or above: the holes
/// it overlaps go to `on`, and whether it overlaps the open space round them.
fn meet(run: &Run, other: &LayerParts, holes: &Holes, on: &mut Vec<u32>) -> bool {
    let len = run.x1 - run.x0;
    let overlap = |r: &Run| r.x1.min(run.x1).saturating_sub(r.x0.max(run.x0));
    let cured: u32 = other.row(run.y).iter().map(overlap).sum();
    let (runs, of) = holes.row(run.y);
    let mut held = 0;
    for (r, h) in runs.iter().zip(of) {
        let o = overlap(r);
        if o > 0 {
            held += o;
            on.push(*h);
        }
    }
    cured + held < len
}

/// The cups of `layers` (the print's cured pixels, parts and supports, from the plate up) on
/// `grid`, in layers of `layers_of`: those that seal [`CUP_LEAST`] or trap [`TRAPPED_LEAST`].
pub fn cups(layers: &[LayerParts], grid: &PixelGrid, layers_of: &Layers) -> Vec<Cup> {
    let holes: Vec<Holes> = par::map(layers.len(), |k| layers[k].holes());
    let base: Vec<u32> = holes.iter().scan(0u32, |n, h| {
        let at = *n;
        *n += h.pixels.len() as u32;
        Some(at)
    }).collect();
    let total = holes.iter().map(|h| h.pixels.len()).sum::<usize>();
    let mut up: Vec<u32> = (0..total as u32).collect();
    let mut sets: Vec<Option<Set>> = vec![None; total];
    let mut found: Vec<Cup> = Vec::new();
    let (px, height) = (grid.pixel_area(), layers_of.height);
    let keep = |set: &Set, closed: bool, found: &mut Vec<Cup>| {
        let area = set.pixels as f64 * px;
        let trapped = if closed { set.volume as f64 * px * height } else { 0.0 };
        if area >= CUP_LEAST || trapped >= TRAPPED_LEAST {
            found.push(Cup { first: set.first, last: set.last, peak: set.peak, area, rect: set.rect, trapped });
        }
    };
    // the sets with holes on the layer before
    let mut active: Vec<u32> = Vec::new();
    let mut on = Vec::new();
    for (k, layer) in holes.iter().enumerate() {
        let n = layer.pixels.len();
        let id = |h: u32| base[k] + h;
        // which holes each hole of this layer stands on, and whether it is vented
        let mut vent = vec![false; n];
        let mut links: Vec<(u32, u32)> = Vec::new();
        if k > 0 {
            for (run, &h) in layer.runs.iter().zip(&layer.of) {
                on.clear();
                vent[h as usize] |= meet(run, &layers[k - 1], &holes[k - 1], &mut on);
                links.extend(on.iter().map(|&b| (h, base[k - 1] + b)));
            }
        }
        links.sort_unstable();
        links.dedup();
        for h in 0..n as u32 {
            sets[id(h) as usize] = Some(Set { vented: vent[h as usize], first: k as u32, last: k as u32, peak: k as u32, pixels: 0, rect: PixelRect::EMPTY, volume: 0 });
        }
        for (h, b) in links {
            let (x, y) = (root(&mut up, id(h)), root(&mut up, b));
            if x == y {
                continue;
            }
            let (keep_at, gone) = (x.min(y), x.max(y));
            up[gone as usize] = keep_at;
            let g = sets[gone as usize].take().expect("a root's set");
            let s = sets[keep_at as usize].as_mut().expect("a root's set");
            // a sealed set vented now was a cup up to the layer before
            if g.vented != s.vented {
                keep(if g.vented { &*s } else { &g }, false, &mut found);
            }
            s.vented |= g.vented;
            s.first = s.first.min(g.first);
            s.volume += g.volume;
            if g.pixels > s.pixels {
                (s.peak, s.pixels, s.rect) = (g.peak, g.pixels, g.rect);
            }
        }
        // a hole of the layer before that this layer's open space overlaps is vented from above
        if k > 0 {
            for (run, &h) in holes[k - 1].runs.iter().zip(&holes[k - 1].of) {
                on.clear();
                if !meet(run, &layers[k], layer, &mut on) {
                    continue;
                }
                let r = root(&mut up, base[k - 1] + h);
                let s = sets[r as usize].as_mut().expect("a root's set");
                if !s.vented {
                    keep(s, false, &mut found);
                    s.vented = true;
                }
            }
        }
        // what each set seals on this layer
        let mut here: Vec<(u32, u64, PixelRect)> = Vec::new();
        for h in 0..n as u32 {
            let r = root(&mut up, id(h));
            let (p, rect) = (layer.pixels[h as usize], layer.rects[h as usize]);
            match here.iter_mut().find(|e| e.0 == r) {
                Some(e) => {
                    e.1 += p;
                    e.2 = e.2.union(&rect);
                }
                None => here.push((r, p, rect)),
            }
        }
        for &(r, p, rect) in &here {
            let s = sets[r as usize].as_mut().expect("a root's set");
            if s.vented {
                continue;
            }
            s.last = k as u32;
            s.volume += p;
            if p > s.pixels {
                (s.peak, s.pixels, s.rect) = (k as u32, p, rect);
            }
        }
        // the sets of the layer before with no hole on this one have closed
        for a in std::mem::take(&mut active) {
            let r = root(&mut up, a);
            if here.iter().any(|e| e.0 == r) {
                continue;
            }
            if let Some(s) = sets[r as usize].take() {
                if !s.vented {
                    keep(&s, true, &mut found);
                }
            }
        }
        active = here.iter().map(|e| e.0).collect();
    }
    // open to the last layer: it drains when the print lifts out
    for a in active {
        let r = root(&mut up, a);
        if let Some(s) = sets[r as usize].take() {
            if !s.vented {
                keep(&s, false, &mut found);
            }
        }
    }
    found.sort_by_key(|c| c.first);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spans::Spans;

    /// A layer from rows of text, `#` cured, on a 0.05 mm grid.
    fn layer(rows: &[&str]) -> LayerParts {
        let (w, h) = (rows[0].len() as u32, rows.len() as u32);
        let rect = PixelRect { x0: 0, y0: 0, x1: w, y1: h };
        let cov: Vec<u8> = rows.iter().flat_map(|r| r.bytes().map(|c| if c == b'#' { 255 } else { 0 })).collect();
        LayerParts::of(&Spans::of(rect, &cov))
    }

    /// Big pixels, so a hole of a few counts: 20 mm² each.
    fn grid() -> PixelGrid {
        PixelGrid { pixels: [100, 100], pitch: [20f64.sqrt(); 2] }
    }

    fn stack(n: usize) -> Layers {
        Layers { count: n as u32, height: 0.05 }
    }

    /// A ring's middle is a hole, a U's notch is open, and a hole's runs carry it.
    #[test]
    fn holes_are_what_cured_pixels_close_in() {
        let ring = layer(&["#####", "#..##", "#.#.#", "#####"]);
        let h = ring.holes();
        assert_eq!(h.pixels, vec![3, 1]);
        assert_eq!(h.rects[0], PixelRect { x0: 1, y0: 1, x1: 3, y1: 3 });
        assert_eq!(h.row(2), (&[Run { y: 2, x0: 1, x1: 2 }, Run { y: 2, x0: 3, x1: 4 }][..], &[0, 1][..]));
        let u = layer(&["#...#", "#...#", "#####"]);
        assert!(u.holes().pixels.is_empty());
        // touching the rect's side opens it
        let side = layer(&["####", "...#", "####"]);
        assert!(side.holes().pixels.is_empty());
    }

    /// A pocket on a floor seals its middle from the floor's next layer on. Opened at the side,
    /// it is vented from there; capped, what it held is trapped; with no floor, it is open below.
    #[test]
    fn a_pocket_on_a_floor_is_a_cup_until_vented() {
        let floor = layer(&["#######"; 5]);
        let wall = layer(&["#######", "#.....#", "#.....#", "#.....#", "#######"]);
        let open = layer(&["#######", "#......", "#.....#", "#.....#", "#######"]);
        let cup = cups(&[floor.clone(), wall.clone(), wall.clone(), wall.clone()], &grid(), &stack(4));
        assert_eq!(cup.len(), 1);
        assert_eq!((cup[0].first, cup[0].last, cup[0].peak, cup[0].trapped), (1, 3, 1, 0.0));
        assert!((cup[0].area - 300.0).abs() < 1e-9, "{}", cup[0].area);
        // vented on layer 2: a cup on layer 1 only, and nothing trapped
        let vented = cups(&[floor.clone(), wall.clone(), open, wall.clone()], &grid(), &stack(4));
        assert_eq!(vented.len(), 1);
        assert_eq!((vented[0].first, vented[0].last, vented[0].trapped), (1, 1, 0.0));
        // capped on layer 3: 15 pixels on two layers trapped
        let capped = cups(&[floor.clone(), wall.clone(), wall.clone(), floor.clone()], &grid(), &stack(4));
        assert_eq!(capped.len(), 1);
        assert!((capped[0].trapped - 30.0 * 20.0 * 0.05).abs() < 1e-9, "{}", capped[0].trapped);
        // with no floor the wall's middle is open below: no cup
        let empty = layer(&["......."; 5]);
        assert!(cups(&[empty, wall.clone(), wall], &grid(), &stack(3)).is_empty());
    }

    /// Two pockets that join keep sealing as one; joined to a vented one, a pocket stops.
    #[test]
    fn pockets_join_up_the_layers() {
        let floor = layer(&["#############"; 3]);
        let two = layer(&["#############", "#.....#.....#", "#############"]);
        let one = layer(&["#############", "#...........#", "#############"]);
        let c = cups(&[floor.clone(), two, one.clone()], &grid(), &stack(3));
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!((c[0].first, c[0].last, c[0].peak), (1, 2, 2));
        assert!((c[0].area - 220.0).abs() < 1e-9);
        // the left pocket is open to the side: the right one, joined to it, seals layer 1 only
        let left_open = layer(&["#############", "......#.....#", "#############"]);
        let c = cups(&[floor, left_open, one], &grid(), &stack(3));
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!((c[0].first, c[0].last), (1, 1));
    }
}
