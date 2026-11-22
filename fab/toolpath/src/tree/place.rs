//! Placement (R14 §6): bottom-up, one point per element, and the elements that cannot stand
//! deleted.
//!
//! - On layer 0 each element stands at its point, moved inside its area.
//! - Above, an element keeps a placed point it has (a tip's own, kept by the moves that stayed,
//!   or its child's). One without (a root) is deleted when it is bound for the bed (it strayed)
//!   or too short to stand on the model with no roof over it; an on-top root walks up its chain
//!   to the highest element still over a placeable area; any other stands at its point, moved
//!   inside its area. A tip no child carries is deleted.
//! - Deleting an element turns the elements it carries into roots, the root an on-top walk ends
//!   at included: it is tested again on its own layer.
//! - Each survivor gives each element it carries that has no point the point of its area nearest
//!   its own.

use super::clip::intersection_ex;
use super::element::{moved_inside, Element};
use super::settings::Settings;
use super::volumes::Volumes;
use crate::par::{Cancelled, Ctl};

/// Places every element of `layers` (per layer, each element's carries indexing the layer
/// above) and removes those deleted, renumbering the carries.
pub fn place(ctl: Ctl, s: &Settings, v: &Volumes, layers: &mut [Vec<Element>]) -> Result<(), Cancelled> {
    crate::prof::scope_fanned(crate::prof::Stage::TreePlace, || {
        let n = layers.len();
        let mut gone: Vec<Vec<bool>> = layers.iter().map(|l| vec![false; l.len()]).collect();
        // Whether a surviving element on the layer below carries it.
        let mut used: Vec<Vec<bool>> = gone.clone();
        for l in 0..n {
            for i in 0..layers[l].len() {
                if gone[l][i] {
                    continue;
                }
                if l == 0 {
                    let e = &mut layers[0][i];
                    e.placed = Some(moved_inside(&e.area, e.point));
                } else {
                    if layers[l][i].placed.is_none() {
                        root(ctl, s, v, layers, &mut gone, l, i)?;
                        if gone[l][i] {
                            continue;
                        }
                    }
                    if !used[l][i] && layers[l][i].highest_tip == l {
                        delete(layers, &mut gone, l, i);
                        continue;
                    }
                }
                give(layers, &mut used, l, i);
            }
        }
        // Removed, and the carries renumbered.
        for l in (0..n).rev() {
            let keep: Vec<Option<usize>> = {
                let mut k = 0;
                gone[l].iter().map(|&g| if g { None } else { k += 1; Some(k - 1) }).collect()
            };
            let mut i = 0;
            layers[l].retain(|_| {
                i += 1;
                !gone[l][i - 1]
            });
            if l > 0 {
                for e in &mut layers[l - 1] {
                    e.carries = e.carries.iter().filter_map(|&c| keep[c]).collect();
                }
            }
        }
        Ok(())
    })
}

/// Each element `layers[l][i]` carries is used, and one with no point gets the point of its area
/// nearest `i`'s placed point.
fn give(layers: &mut [Vec<Element>], used: &mut [Vec<bool>], l: usize, i: usize) {
    let Some(p) = layers[l][i].placed else { return };
    if l + 1 >= layers.len() {
        return;
    }
    let carries = layers[l][i].carries.clone();
    for c in carries {
        used[l + 1][c] = true;
        let up = &mut layers[l + 1][c];
        if up.placed.is_none() {
            up.placed = Some(moved_inside(&up.area, p));
        }
    }
}

/// `layers[l][i]` deleted: the elements it carries lose their points.
fn delete(layers: &mut [Vec<Element>], gone: &mut [Vec<bool>], l: usize, i: usize) {
    gone[l][i] = true;
    if l + 1 < layers.len() {
        let carries = layers[l][i].carries.clone();
        for c in carries {
            layers[l + 1][c].placed = None;
        }
    }
}

/// The root `layers[l][i]` (no point from a child): deleted, walked up, or placed where it is.
fn root(ctl: Ctl, s: &Settings, v: &Volumes, layers: &mut [Vec<Element>], gone: &mut [Vec<bool>], l: usize, i: usize) -> Result<(), Cancelled> {
    let e = &layers[l][i];
    if e.to_bed || (e.depth < s.min_height_on_model && !e.under_roof) {
        delete(layers, gone, l, i);
        return Ok(());
    }
    if e.on_top {
        // Up the chain through each element's first carried one, while it stands over a
        // placeable area; it stops after one that carries none or more than one.
        let over = |e: &Element, at: usize| -> Result<bool, Cancelled> {
            let placeable = v.placeable(ctl, at, s.rung_for(e.collision_radius(s), false))?;
            Ok(!intersection_ex(&e.area, placeable).is_empty())
        };
        let mut chain = vec![(l, i)];
        if over(e, l)? {
            loop {
                let (k, j) = chain[chain.len() - 1];
                let at = &layers[k][j];
                if at.carries.len() != 1 || k + 1 >= layers.len() {
                    break;
                }
                let next = (k + 1, at.carries[0]);
                if !over(&layers[next.0][next.1], next.0)? {
                    break;
                }
                chain.push(next);
            }
            let (k, j) = chain[chain.len() - 1];
            let top = &mut layers[k][j];
            top.placed = Some(moved_inside(&top.area, top.point));
            // Q82 (R14 §6 step 2), a quirk of the reference's order: the new root is placed
            // first, so deleting the element under it clears that point again, and the new root
            // is a root once more on its own layer.
            for &(a, b) in &chain[..chain.len() - 1] {
                delete(layers, gone, a, b);
            }
            return Ok(());
        }
        layers[l][i].on_top = false;
    }
    let e = &mut layers[l][i];
    e.placed = Some(moved_inside(&e.area, e.point));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::descent::{descend, Memo};
    use super::super::element::disc;
    use super::super::settings::tests::r14;
    use super::super::settings::Keys;
    use super::super::tips::Tips;
    use super::*;
    use makepad_fab_slicer::geom::*;

    fn ctl() -> Ctl<'static> {
        Ctl::default()
    }

    fn bed_only() -> Settings {
        Settings::new(Keys { bed_only: true, ..r14(0.8).keys })
    }

    /// An element on layer `l` at (`x`, 0) mm with a 0.5 mm disc for its area, no placed point.
    fn at(s: &Settings, l: usize, x: f64, depth: usize, carries: Vec<usize>) -> Element {
        let mut e = Element::tip(s, l, Pt::from_mm(x, 0.0));
        (e.depth, e.carries, e.placed, e.highest_tip) = (depth, carries, None, l + 10);
        e.area = vec![disc(Pt::from_mm(x, 0.0), 0.5)];
        e
    }

    /// §6 on a trunk down to the bed: layer 0 stands at its point, and every element above gets
    /// the point of its area nearest the one below; nothing is deleted.
    #[test]
    fn a_trunk_to_the_bed_stands_where_its_tip_is() {
        let s = bed_only();
        let part = vec![Vec::new(); 8];
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let mut t = Tips { tips: vec![Vec::new(); 8], contact: vec![Vec::new(); 8], interface: vec![Vec::new(); 8], bed: Vec::new() };
        let mut tip = Element::tip(&s, 6, Pt::from_mm(1.0, 2.0));
        (tip.to_bed, tip.on_top, tip.hole_free) = (true, true, true);
        t.tips[6].push(tip);
        let mut layers = descend(ctl(), &s, &v, &t, &mut Memo::default()).unwrap();
        place(ctl(), &s, &v, &mut layers).unwrap();
        for l in 0..=6 {
            assert_eq!(layers[l].len(), 1, "one element on #{l}");
            assert_eq!(layers[l][0].placed, Some(Pt::from_mm(1.0, 2.0)), "placed under the tip on #{l}");
        }
    }

    /// §6 step 2 and 4: a root bound for the bed strayed and is deleted, and so is everything it
    /// carried, layer by layer up to its tip; a root too short to stand with no roof over it is
    /// deleted; under a roof it stands at its point moved inside its area.
    #[test]
    fn roots_that_cannot_stand_are_deleted_with_what_they_carry() {
        let s = bed_only();
        let part = vec![Vec::new(); 8];
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let mut layers: Vec<Vec<Element>> = vec![Vec::new(); 8];
        // A strayed chain: to-bed root on #2 carrying #3, carrying the tip on #4.
        let mut root = at(&s, 2, 0.0, 6, vec![0]);
        root.to_bed = true;
        layers[2].push(root);
        let mut mid = at(&s, 3, 0.0, 5, vec![0]);
        mid.to_bed = true;
        layers[3].push(mid);
        let mut tip = at(&s, 4, 0.0, 4, Vec::new());
        (tip.highest_tip, tip.to_bed) = (4, true);
        layers[4].push(tip);
        // Short roots on #2: one bare (depth 3), one under a roof, its point outside its area.
        layers[2].push(at(&s, 2, 10.0, 3, Vec::new()));
        let mut roofed = at(&s, 2, 20.0, 3, Vec::new());
        (roofed.under_roof, roofed.on_top) = (true, true);
        roofed.point = Pt::from_mm(21.0, 0.0);
        layers[2].push(roofed);
        place(ctl(), &s, &v, &mut layers).unwrap();
        assert_eq!(layers[2].len(), 1, "only the roofed root stands on #2");
        assert!(layers[3].is_empty() && layers[4].is_empty(), "the strayed chain is gone up to its tip");
        let p = layers[2][0].placed.expect("placed");
        assert!((p.x as f64 / SCALE - 20.5).abs() < 0.01 && !layers[2][0].on_top, "moved onto its area's edge, and no longer on-top (none is placeable from the bed only)");
    }

    /// Pushes a chain bottom-up from layer `from` at (`x`, 0) mm, one element per depth in
    /// `depths` (the last its tip, placed at its point), all on-top and to the model.
    fn chain(s: &Settings, layers: &mut [Vec<Element>], from: usize, x: f64, depths: &[usize], under_roof: bool) {
        let top = from + depths.len() - 1;
        for (k, &d) in depths.iter().enumerate() {
            let l = from + k;
            let carries = if l == top { Vec::new() } else { vec![layers[l + 1].len()] };
            let mut e = at(s, l, x, d, carries);
            (e.on_top, e.under_roof, e.highest_tip) = (true, under_roof, top);
            if l == top {
                e.placed = Some(e.point);
            }
            layers[l].push(e);
        }
    }

    /// §6 step 2, the on-top walk on R14's case (Q82): over a two-layer slab, roots stand on
    /// #2 and #3 only. The walk from a root on #2 ends on #3, the new root, which is tested
    /// again: at depth 4 (under 5, no roof) it is deleted, and with it the chain up to its tip; at
    /// depth 7 it stands at its point. A walk that ends at a tip under a roof loses the tip too
    /// (step 3).
    #[test]
    fn an_on_top_walk_tests_its_new_root_again() {
        let s = r14(0.8);
        let slab = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-10.0, -10.0), Pt::from_mm(10.0, 10.0)))];
        let mut part = vec![Vec::new(); 12];
        (part[0], part[1]) = (slab.clone(), slab);
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let p0 = s.bottom_gap_layers + 1;
        assert!(!v.placeable(ctl(), p0 + 1, 1).unwrap().is_empty() && v.placeable(ctl(), p0 + 2, 0).unwrap().is_empty(), "placeable on #{p0} and #{} only", p0 + 1);
        let mut layers: Vec<Vec<Element>> = vec![Vec::new(); 12];
        chain(&s, &mut layers, p0, 0.0, &[5, 4, 3, 2, 1, 0], false);
        chain(&s, &mut layers, p0, -5.0, &[8, 7, 6, 5, 4, 3, 2, 1, 0], false);
        chain(&s, &mut layers, p0, 5.0, &[1, 0], true);
        place(ctl(), &s, &v, &mut layers).unwrap();
        let xs = |l: usize| layers[l].iter().map(|e| e.point.x as f64 / SCALE).collect::<Vec<_>>();
        assert!(xs(p0).is_empty(), "the walks delete every root on #{p0}");
        assert_eq!(xs(p0 + 1), vec![-5.0], "only the deep new root stands on #{}", p0 + 1);
        assert_eq!(layers[p0 + 1][0].placed, Some(Pt::from_mm(-5.0, 0.0)), "at its point, the walk redone");
        for l in p0 + 2..12 {
            assert!(xs(l).iter().all(|&x| x == -5.0), "nothing of the shallow chains is left on #{l}");
        }
        assert_eq!(layers[p0 + 8].len(), 1, "the deep chain keeps its tip");
    }

    /// §6 step 3 and the renumbering: a tip no child carries is deleted, and the carries of the
    /// layer below are renumbered past it.
    #[test]
    fn an_unused_tip_is_deleted_and_the_carries_renumbered() {
        let s = bed_only();
        let part = vec![Vec::new(); 8];
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let mut layers: Vec<Vec<Element>> = vec![Vec::new(); 8];
        let mut base = at(&s, 0, 0.0, 8, vec![0, 2]);
        base.to_bed = true;
        layers[0].push(base);
        layers[1].push(at(&s, 1, 0.0, 7, Vec::new()));
        let mut lone = at(&s, 1, 5.0, 0, Vec::new());
        (lone.highest_tip, lone.placed) = (1, Some(Pt::from_mm(5.0, 0.0)));
        layers[1].push(lone);
        layers[1].push(at(&s, 1, 0.3, 7, Vec::new()));
        place(ctl(), &s, &v, &mut layers).unwrap();
        assert_eq!(layers[1].len(), 2, "the unused tip is gone");
        assert_eq!(layers[0][0].carries, vec![0, 1], "renumbered");
        assert_eq!((layers[1][0].placed, layers[1][1].placed), (Some(Pt::from_mm(0.0, 0.0)), Some(Pt::from_mm(0.0, 0.0))), "the base's point, inside both areas");
    }
}
