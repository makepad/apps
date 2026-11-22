//! Tree supports as the reference builds them (R14), stage by stage, each in its own module and
//! with R14's numbers. Not wired into slicing yet: the generator in [`crate::support`] stays live
//! until this one replaces it whole.
//!
//! 0. [`overhangs`] (§2): the part's slices as the reference prepares them (§2.0), and the faces
//!    tips go under, as the reference's own detector finds them.
//! 1. [`settings`] (§1): the keys and every number derived from them.
//! 2. [`volumes`] (§3): collision, hole-free collision, placeable areas, avoidance to the bed and
//!    to the model per rung and kind, built on first ask and kept.
//! 3. [`tips`] (§4): the roofs under each overhang's faces and the tips that hold them.
//! 4. [`descent`] and [`merge`] (§5): every element moved a layer down at a time, and merged
//!    where branches meet.
//! 5. [`place`] (§6): bottom-up, a point for every element, and those that cannot stand deleted.
//! 6. [`smooth`] (§7): the placed points pushed off the part and averaged with their neighbours.
//! 7. [`draw`] (§8): branches into tubes, cut at the layers' middles less the part, roots' floors
//!    and drops, and per layer the base and floors.
//! 8. [`interface`] (§9): contacts and floors into the base near them as interface, the first
//!    layer's pad, the bed's clip, and the lines each layer prints.
//!
//! The parts stages share: [`element`] (one branch on one layer), [`base_fill`] (the support-base
//! fill tips are sampled on, §4.4), [`sample`] (points along a line at most a spacing apart,
//! §4.4) and [`offset`] (every offset, its rings first rid of their close points, §1.5).
//!
//! A [`Cache`] keeps a part's stages up to the descent, and what the descent made, for its next
//! run: a run that only drops tips ([`Input::dropped`]) reuses them and comes out as a fresh
//! run. What the runs on it make afresh rather than take from it is their [`Work`].

pub mod base_fill;
pub mod clip;
pub mod descent;
pub mod draw;
mod drip;
pub mod element;
pub mod interface;
pub mod merge;
pub mod offset;
pub mod overhangs;
pub mod place;
pub mod sample;
pub mod settings;
pub mod smooth;
pub mod tips;
pub mod volumes;

/// The stages of [`generate`] that tell how far they are ([`crate::Emit::Tree`]), in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// §2.0: the part's slices made ready.
    Slices,
    /// §2: the overhangs found.
    Overhangs,
    /// §4: the tips and roofs laid under them.
    Tips,
    /// §5: the descent, a layer at a time from the top.
    Descent,
    /// §7: the smoothing rounds.
    Smooth,
    /// §8: the branches drawn.
    Draw,
    /// §9.5: the lines each layer prints.
    Paths,
}

/// What a stage made of one layer, in the part's frame: what the view shows of the trees as
/// they grow.
pub enum Grown<'a> {
    /// Nothing to show: how far the stage is, alone.
    Nothing,
    /// §2: the faces tips go under.
    Overhangs(&'a [ExPolygon]),
    /// §4: the tips an overhang layer starts (each on its own layer, as found: before the tips
    /// of all overhang layers are de-duplicated), and its roofs as (layer, contact, polygons).
    Tips { tips: &'a [element::Element], roofs: &'a [(usize, bool, Vec<ExPolygon>)] },
    /// §5: every layer's branches as the descent has them so far, each carrying its indices on
    /// the layer above ([`element::Element::carries`]), the told layer's as it just left them; and
    /// the settings their radii follow ([`element::Element::drawn_radius`]).
    Branches { layers: &'a [Vec<element::Element>], settings: &'a settings::Settings },
    /// §9.5: the layer's lines.
    Paths(&'a [ExtrusionPath]),
    /// §5 to §7: every layer's branches as placed and smoothed, each carrying its indices on the
    /// layer above ([`element::Element::carries`]): which tree each belongs to, and the tips each
    /// grew from. Told once, as the trees are drawn; it moves no stage on.
    Trees { layers: &'a [Vec<element::Element>], settings: &'a settings::Settings },
}

/// `stage` made one of the `of` shares it tells, `grown` on layer `layer`: told to `ctl`'s emit.
pub(crate) fn tell(ctl: Ctl, stage: Stage, layer: usize, of: usize, grown: Grown) {
    if let Some(emit) = ctl.emit {
        emit(crate::Emit::Tree { stage, layer, of, grown, frame: Pt::default() });
    }
}

use std::sync::Arc;

use makepad_fab_slicer::geom::{ExPolygon, Pt};

use crate::par::{Cancelled, Ctl};
use crate::ExtrusionPath;

/// What [`generate`] takes for one part, in the part's frame (R14 §1.4): its mesh box's XY centre,
/// in whole nm, at (0, 0). Every fill the support prints runs a line through that point (§9.5),
/// and what comes out is in the same frame.
pub struct Input<'a> {
    pub keys: settings::Keys,
    pub print: interface::Print,
    /// The part's layer slices as cut: each layer's loops at its middle from the part's lowest
    /// point, neither closed nor simplified. [`generate`] prepares them as §2.0 does
    /// ([`overhangs::slices`]), and every stage reads them so.
    pub slices: &'a [Vec<ExPolygon>],
    /// Each layer's print z and height (mm).
    pub heights: &'a [(f64, f64)],
    /// The areas enforced and blocked per layer: none on a layer past either list's end.
    pub enforce: &'a [Vec<ExPolygon>],
    pub block: &'a [Vec<ExPolygon>],
    /// Per layer, where no tree may stand or grow: Fab's, not the reference's, and none on a
    /// layer past the list's end. Every stage takes it as the part on its layer
    /// ([`volumes::Volumes::keeping_out`]) and the first layer's pad keeps clear of it, but the
    /// overhangs are the part's own, and a branch may reach into it: a tip under a face in it
    /// stays when its branch gets straight out at the fast move ([`tips`]), and sees the part
    /// alone until it is out ([`element::Element::exit`]). Fab makes it from the part's footprint
    /// when the part keeps its trees out of it ([`crate::OUTSIDE_FOOTPRINT`]).
    pub keep_out: &'a [Vec<ExPolygon>],
    /// The bed's area.
    pub bed: &'a [ExPolygon],
    /// The tips the user deleted, as (x, y, z) in mm: each drops the tips §4 gathered on the
    /// layer whose print z is nearest z within [`DROP_REACH`] of (x, y), and a roof piece goes
    /// when all its tips do ([`tips::without`]). Fab's, not the reference's.
    pub dropped: &'a [[f64; 3]],
}

/// How near (mm) a dropped tip's x and y are to the tip they drop: 1 µm.
pub const DROP_REACH: f64 = 0.001;

/// Per layer, which of `tips` the `dropped` marks ([`Input::dropped`]), with `heights` the
/// layers' print z and height.
fn gone(tips: &tips::Tips, heights: &[(f64, f64)], dropped: &[[f64; 3]]) -> Vec<Vec<bool>> {
    let mut gone: Vec<Vec<bool>> = tips.tips.iter().map(|t| vec![false; t.len()]).collect();
    let reach = (DROP_REACH * makepad_fab_slicer::geom::SCALE) as i128;
    for &[x, y, z] in dropped {
        let Some(l) = (0..heights.len().min(gone.len())).min_by(|&a, &b| (heights[a].0 - z).abs().total_cmp(&(heights[b].0 - z).abs())) else { continue };
        let at = Pt::from_mm(x, y);
        for (i, e) in tips.tips[l].iter().enumerate() {
            let (dx, dy) = ((e.point.x - at.x) as i128, (e.point.y - at.y) as i128);
            if dx * dx + dy * dy <= reach * reach {
                gone[l][i] = true;
            }
        }
    }
    gone
}

/// What [`generate`] keeps of one part's run for its next. The stages up to the descent are
/// kept with the inputs they were made from, and reused only by a run whose inputs equal them
/// (all but the print, the bed and the dropped tips, which they do not read). Any other run makes
/// them afresh, on the same settings taking what reads only what it left: each layer's slice,
/// overhang passes and tips, and the volumes' layers ([`overhangs::detect`], [`tips::tips`],
/// [`volumes::Volumes::taking`]); a run cut short there leaves them as they were. The memos of
/// the stages after go on with every run on the same settings, each taking what was made of the
/// same where the volume layers it read are the run's: the descent's steps and merges
/// ([`descent::Memo`]), the smoothing's trees ([`smooth::Memo`]), the drawing's branches and
/// groups ([`draw::Memo`], at the same heights) and the printed layers ([`interface::Memo`], keyed
/// by all they are made from); the drawing's and the printed layers' hold the bed they were made
/// on, and serve another whose box holds all they put on the bed as it was, and the printed
/// layers' hold their print. A run on all the last whole run was made from takes what that run
/// gave ([`Whole`]). What the part's run prepared the trees from goes with them
/// ([`crate::TreePrep`]), and what its runs made afresh ([`Cache::take_work`]).
#[derive(Default)]
pub struct Cache {
    kept: Option<Kept>,
    pub(crate) prep: Option<crate::TreePrep>,
    pub(crate) work: Work,
    /// Per layer, what the part's last plan printed of the trees ([`crate::TreeIsland`]).
    pub(crate) islands: Vec<crate::TreeIsland>,
}

impl Cache {
    /// What the runs of [`generate`] on it made afresh since this was last asked.
    pub fn take_work(&mut self) -> Work {
        std::mem::take(&mut self.work)
    }
}

/// What runs of [`generate`] made afresh, stage by stage, in the stages they finished, rather
/// than took from what their [`Cache`] kept: none of it in a run where nothing a stage reads
/// changed. Placement (§6) keeps nothing and is not counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    /// The layers of what keeps a tree off the part made afresh, for where it would be locked in ([`crate::TreePrep`]).
    pub locked: usize,
    /// The runs that made the stages up to the descent again (§2 to §4).
    pub prepared: usize,
    /// The layers of §2 worked afresh: each layer's slice prepared (§2.0) and first pass (§2.1 to
    /// §2.4), and each layer added to the part below a first layer (§2.7), count once.
    pub faced: usize,
    /// The volumes' layers built (§3), each volume's: as many as were asked for or raised ahead,
    /// less those taken from the run before ([`volumes::Volumes::taking`]).
    pub built: usize,
    /// The avoidances' layers raised on an ask past what was built (§3.3): what a branch on a
    /// layer waited on ([`volumes::Volumes::raise_ahead`]).
    pub asked: usize,
    /// The overhang layers with faces whose tips and roofs were found (§4).
    pub tipped: usize,
    /// The steps of the descent (§5.1): its elements moved a layer down, less those taken from
    /// the run before ([`descent::Memo`]).
    pub stepped: usize,
    /// The trees smoothed a round or more (§7).
    pub smoothed: usize,
    /// The branches drawn and the groups of a layer's pieces joined (§8).
    pub drawn: usize,
    /// The layers put together (§9.2 to §9.4).
    pub put: usize,
    /// The pieces of the layers printed (§9.5).
    pub printed: usize,
}

impl std::ops::AddAssign for Work {
    fn add_assign(&mut self, w: Work) {
        self.locked += w.locked;
        self.prepared += w.prepared;
        self.faced += w.faced;
        self.built += w.built;
        self.asked += w.asked;
        self.tipped += w.tipped;
        self.stepped += w.stepped;
        self.smoothed += w.smoothed;
        self.drawn += w.drawn;
        self.put += w.put;
        self.printed += w.printed;
    }
}

/// One run's stages up to the descent, and the inputs they were made from.
struct Kept {
    keys: settings::Keys,
    slices: Vec<Vec<ExPolygon>>,
    heights: Vec<(f64, f64)>,
    enforce: Vec<Vec<ExPolygon>>,
    block: Vec<Vec<ExPolygon>>,
    keep_out: Vec<Vec<ExPolygon>>,
    /// The slices as §2.0 prepares them.
    prepared: Vec<Vec<ExPolygon>>,
    overhangs: overhangs::Overhangs,
    /// What §2 found on each layer, and how many of its layers it worked afresh.
    passes: overhangs::Passes,
    faced: usize,
    v: volumes::Volumes,
    /// The tips and roofs before any is dropped, their pieces, and what each overhang layer found.
    tips: tips::Tips,
    pieces: tips::Pieces,
    under: tips::Under,
    memo: descent::Memo,
    smoothed: smooth::Memo,
    drawn: draw::Memo,
    printed: interface::Memo,
    /// The last run on these stages that went through.
    whole: Option<Whole>,
}

/// What a run gave, with what it was made from besides the stages it stands on ([`Kept`]): a run
/// on the same makes nothing, and gives this.
struct Whole {
    print: interface::Print,
    bed: Vec<ExPolygon>,
    dropped: Vec<[f64; 3]>,
    made: Arc<Generated>,
}

impl Kept {
    /// The stages up to the descent for `input`, each taking what it can from `old`'s, made on
    /// the same settings.
    fn new(ctl: Ctl, input: &Input, old: Option<&Kept>) -> Result<Kept, Cancelled> {
        let s = settings::Settings::new(input.keys.clone());
        let was = old.filter(|o| o.keys == input.keys);
        let (prepared, sliced) = overhangs::slices(ctl, input.slices, was.map(|o| (&o.slices[..], &o.prepared[..])))?;
        let faces_before = was.map(|o| overhangs::Before { slices: &o.prepared, heights: &o.heights, enforce: &o.enforce, passes: &o.passes });
        let volumes_before = was.map(|o| &o.v);
        // §2's overhangs beside the volumes and the ramp's avoidances, which the tips ask for
        // first: neither reads the other.
        let (found, v) = crate::par::try_join(
            ctl,
            || -> Result<_, Cancelled> {
                let found = overhangs::detect(ctl, &s, &prepared, input.heights, input.enforce, input.block, faces_before.as_ref())?;
                tell_faces(ctl, &found.0);
                Ok(found)
            },
            || -> Result<_, Cancelled> {
                let v = volumes::Volumes::new(ctl, &s, &prepared, input.block)?.keeping_out(input.keep_out);
                let v = match volumes_before {
                    Some(was) => v.taking(was),
                    None => v,
                };
                v.ramp(ctl)?;
                Ok(v)
            },
        )?;
        let ((overhangs, passes), v) = (found?, v?);
        let before = was.map(|o| tips::Before { overhangs: &o.overhangs.layers, keeps_out: o.v.keeps_out(), under: &o.under });
        // The tips beside the rungs past the ramp, raised as high as a branch under the highest
        // face can ask for them.
        let top = overhangs.layers.iter().rposition(|faces| !faces.is_empty());
        let (made, raised) = crate::par::try_join(ctl, || tips::tips(ctl, &s, &v, &overhangs.layers, before.as_ref()), || top.map_or(Ok(()), |top| v.raise_ahead(ctl, top)))?;
        raised?;
        let (tips, pieces, under) = made?;
        let faced = sliced + passes.passed + passes.added;
        Ok(Kept {
            keys: input.keys.clone(),
            slices: input.slices.to_vec(),
            heights: input.heights.to_vec(),
            enforce: input.enforce.to_vec(),
            block: input.block.to_vec(),
            keep_out: input.keep_out.to_vec(),
            prepared,
            overhangs,
            passes,
            faced,
            v,
            tips,
            pieces,
            under,
            memo: descent::Memo::default(),
            smoothed: smooth::Memo::default(),
            drawn: draw::Memo::default(),
            printed: interface::Memo::default(),
            whole: None,
        })
    }

    fn made_from(&self, input: &Input) -> bool {
        self.keys == input.keys && self.slices == input.slices && self.heights == input.heights && self.enforce == input.enforce && self.block == input.block && self.keep_out == input.keep_out
    }
}

/// §2's faces, a layer at a time, as the view shows them.
fn tell_faces(ctl: Ctl, overhangs: &overhangs::Overhangs) {
    let n = overhangs.layers.len();
    for (l, faces) in overhangs.layers.iter().enumerate() {
        tell(ctl, Stage::Overhangs, l, 3 * n, Grown::Overhangs(faces));
    }
}

/// One part's tree supports, as [`generate`] makes them.
pub struct Generated {
    /// §2: per layer, the faces tips go under.
    pub overhangs: overhangs::Overhangs,
    /// §4: per layer, the tips as they start and the top contacts and top interfaces; and the
    /// roofs that reach the bed.
    pub roofs: tips::Tips,
    /// §5 to §7: per layer, the elements that stand, each at its placed point, smoothed, and
    /// carrying indices into the layer over it.
    pub trees: Vec<Vec<element::Element>>,
    /// §8, §9: per layer, the base and the one interface region, and the first layer's pad.
    pub layers: interface::Layers,
    /// §9.5: per layer, what it prints: as the run before had it, where that printed the layer
    /// from the same.
    pub paths: Vec<Arc<Vec<ExtrusionPath>>>,
}

/// One part's tree supports as the reference builds them (R14), stage by stage, less the tips
/// dropped; with what `cache` kept from the part's run before, and keeping this run's.
pub fn generate(ctl: Ctl, input: &Input, cache: &mut Cache) -> Result<Arc<Generated>, Cancelled> {
    let k = match cache.kept.take() {
        Some(k) if k.made_from(input) => {
            tell_faces(ctl, &k.overhangs);
            cache.kept.insert(k)
        }
        old => match Kept::new(ctl, input, old.as_ref()) {
            Ok(mut k) => {
                cache.work.prepared += 1;
                cache.work.faced += k.faced;
                cache.work.tipped += k.under.made;
                if let Some(old) = old.filter(|o| o.keys == k.keys) {
                    (k.memo, k.smoothed, k.drawn, k.printed) = (old.memo, old.smoothed, old.drawn, old.printed);
                }
                cache.kept.insert(k)
            }
            Err(c) => {
                // Cut short: the run before's, whose volumes the next run takes from as this one did.
                cache.kept = old;
                return Err(c);
            }
        },
    };
    if let Some(w) = k.whole.as_ref().filter(|w| w.print == input.print && w.bed == input.bed && w.dropped == input.dropped) {
        tell(ctl, Stage::Draw, 0, 0, Grown::Trees { layers: &w.made.trees, settings: k.v.settings() });
        return Ok(w.made.clone());
    }
    let roofs = tips::without(&k.tips, &k.pieces, &gone(&k.tips, input.heights, input.dropped));
    let (s, v) = (k.v.settings(), &k.v);
    let mut trees = descent::descend(ctl, s, v, &roofs, &mut k.memo)?;
    cache.work.stepped += k.memo.made;
    let input = &Input { keys: input.keys.clone(), slices: &k.prepared, ..*input };
    place::place(ctl, s, v, &mut trees)?;
    let tops: Vec<f64> = input.heights.iter().map(|h| h.0).collect();
    smooth::smooth(ctl, s, v, &mut trees, &tops, &mut k.smoothed)?;
    cache.work.smoothed += k.smoothed.made;
    tell(ctl, Stage::Draw, 0, 0, Grown::Trees { layers: &trees, settings: s });
    let drawn = draw::draw(ctl, v, input.heights, input.bed, &trees, &roofs.contact, &mut k.drawn)?;
    cache.work.drawn += k.drawn.made;
    let (layers, paths) = crate::prof::scope_fanned(crate::prof::Stage::TreeInterface, || -> Result<_, Cancelled> {
        let layers = interface::layers(ctl, s, v, input, drawn, &roofs, &mut k.printed)?;
        let paths = interface::paths(ctl, s, input, &layers, &mut k.printed)?;
        Ok((layers, paths))
    })?;
    cache.work.put += k.printed.made_layers;
    cache.work.printed += k.printed.made_pieces;
    cache.work.built += k.v.take_made();
    cache.work.asked += k.v.take_asked();
    let made = Arc::new(Generated { overhangs: k.overhangs.clone(), roofs, trees, layers, paths });
    k.whole = Some(Whole { print: input.print, bed: input.bed.to_vec(), dropped: input.dropped.to_vec(), made: made.clone() });
    Ok(made)
}

#[cfg(test)]
mod tests {
    use makepad_fab_slicer::geom::{total_area_mm2, Polygon, Pt};

    use super::*;

    /// `generate` end to end on R14's profile: a 6 mm square slab 3 to 4 mm over the bed stands
    /// on trees. Its underside is an overhang, roofs go under it, and every layer from the bed up
    /// to a few under the roofs has a base.
    #[test]
    fn a_floating_slab_stands_on_trees() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let n = 21;
        let heights: Vec<(f64, f64)> = (0..n).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
        let square = ExPolygon::new(Polygon::rect(Pt::from_mm(-3.0, -3.0), Pt::from_mm(3.0, 3.0)));
        // Each layer's slice is cut at its middle.
        let slices: Vec<Vec<ExPolygon>> = heights.iter().map(|h| if (3.0..4.0).contains(&(h.0 - h.1 / 2.0)) { vec![square.clone()] } else { Vec::new() }).collect();
        let bed = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-100.0, -100.0), Pt::from_mm(100.0, 100.0)))];
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let input = Input { keys, print, slices: &slices, heights: &heights, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let g = generate(Ctl::default(), &input, &mut Cache::default()).unwrap();
        let first = slices.iter().position(|s| !s.is_empty()).unwrap();
        assert_eq!(first, 15);
        assert!(!g.overhangs.layers[first].is_empty(), "the underside is an overhang");
        assert!(g.roofs.contact.iter().any(|c| !c.is_empty()), "with roofs under it");
        let based: Vec<usize> = (0..n).filter(|&l| !g.layers.base[l].is_empty()).collect();
        assert!((0..=10).all(|l| based.contains(&l)), "a base from the bed up: {based:?}");
        assert!(based.iter().all(|&l| l < first), "none in the slab: {based:?}");
    }

    /// Fab's keep-out, end to end: a 20 mm box kept out of as its footprint would be, its lid's
    /// underside 9 mm up. Trees reach in under the lid from outside: none stands in the box, no
    /// branch goes back in once out, and the middle of the lid, too deep to get out from, has
    /// no tip.
    #[test]
    fn trees_reach_into_the_keep_out_only_to_get_straight_out() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, bed_only: true, xy_distance: 0.8, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let (n, lid) = (50, 45);
        let heights: Vec<(f64, f64)> = (0..n).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
        let square = ExPolygon::new(Polygon::rect(Pt::from_mm(-10.0, -10.0), Pt::from_mm(10.0, 10.0)));
        let slices: Vec<Vec<ExPolygon>> = (0..n).map(|l| if l <= 1 || l >= lid { vec![square.clone()] } else { Vec::new() }).collect();
        let keep = vec![vec![square.clone()]; n];
        let bed = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-100.0, -100.0), Pt::from_mm(100.0, 100.0)))];
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let input = Input { keys, print, slices: &slices, heights: &heights, enforce: &[], block: &[], keep_out: &keep, bed: &bed, dropped: &[] };
        let g = generate(Ctl::default(), &input, &mut Cache::default()).unwrap();
        let inside = |e: &element::Element| square.contains(e.placed.expect("placed"));
        // Top down: whether a branch through each element was out of the box over it.
        let mut out_over: Vec<Vec<bool>> = g.trees.iter().map(|t| vec![false; t.len()]).collect();
        let (mut back_in, mut reaching) = (0, 0);
        for l in (0..n).rev() {
            for (i, e) in g.trees[l].iter().enumerate() {
                out_over[l][i] = e.carries.iter().any(|&c| out_over[l + 1][c] || !inside(&g.trees[l + 1][c]));
                if inside(e) {
                    reaching += 1;
                    back_in += (out_over[l][i] || l == 0) as usize;
                }
            }
        }
        assert!(!g.trees[0].is_empty(), "trees stand on the bed");
        assert_eq!(back_in, 0, "no element in the box under where its branch got out, and no root in it");
        assert!(reaching > 0, "branches reach into the box");
        let tips: Vec<&element::Element> = g.roofs.tips.iter().flatten().collect();
        assert!(!tips.is_empty() && tips.iter().all(|e| e.leaving() && e.point.x_mm().abs().max(e.point.y_mm().abs()) > 2.5), "only tips that get out, none in the lid's middle");
        let in_box = |set: &[ExPolygon]| total_area_mm2(&clip::intersection_ex(set, std::slice::from_ref(&square)));
        assert!(!g.layers.base[0].is_empty() && in_box(&g.layers.base[0]) < 1e-6, "the trees' feet are all out of the box");
    }

    /// Everything a run gives, as text, to compare runs by.
    fn all(g: &Generated) -> String {
        format!("{:?} {:?} {:?} {:?} {:?} {:?} {:?}", g.paths, g.layers, g.trees, g.roofs.tips, g.roofs.contact, g.roofs.interface, g.overhangs)
    }

    /// An edit that adds layers on top of a part, a slab 3 mm over the bed made 1 mm taller: the
    /// run takes the volumes' layers the new ones do not reach from the run before, building
    /// fewer than half the layers a fresh run builds, and gives what a fresh run gives.
    #[test]
    fn an_edit_that_adds_layers_on_top_takes_the_volumes_under_them() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let square = ExPolygon::new(Polygon::rect(Pt::from_mm(-3.0, -3.0), Pt::from_mm(3.0, 3.0)));
        // The slab from 3 mm up to the top of `n` layers, each layer cut at its middle.
        let part = |n: usize| -> (Vec<(f64, f64)>, Vec<Vec<ExPolygon>>) {
            let heights: Vec<(f64, f64)> = (0..n).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
            let slices = heights.iter().map(|h| if h.0 - h.1 / 2.0 >= 3.0 { vec![square.clone()] } else { Vec::new() }).collect();
            (heights, slices)
        };
        let ((h0, s0), (h1, s1)) = (part(31), part(36));
        let bed = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-100.0, -100.0), Pt::from_mm(100.0, 100.0)))];
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let first = Input { keys, print, slices: &s0, heights: &h0, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let mut cache = Cache::default();
        generate(Ctl::default(), &first, &mut cache).unwrap();
        cache.take_work();
        let taller = Input { keys: first.keys.clone(), slices: &s1, heights: &h1, ..first };
        let warm = generate(Ctl::default(), &taller, &mut cache).unwrap();
        let mut afresh = Cache::default();
        let fresh = generate(Ctl::default(), &taller, &mut afresh).unwrap();
        assert!(all(&warm) == all(&fresh), "as a fresh run");
        let (built, fresh_built) = (cache.take_work().built, afresh.take_work().built);
        assert!(built > 0 && built * 2 < fresh_built, "{built} volume layers built against {fresh_built} afresh");
    }

    /// An edit that widens the upper of two slabs at two heights: the run takes from the run
    /// before the slices and overhang passes of the layers the edit left, and the tips and roofs
    /// under the lower slab, whose faces and the volumes under them it left. It works on fewer
    /// than half the layers of §2 a fresh run works on and finds tips under fewer overhang layers,
    /// and gives what a fresh run gives.
    #[test]
    fn an_edit_takes_the_faces_and_tips_it_left() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let heights: Vec<(f64, f64)> = (0..26).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)));
        // Each layer's slice is cut at its middle; the upper slab reaches `x1`.
        let part = |x1: f64| -> Vec<Vec<ExPolygon>> {
            heights
                .iter()
                .map(|h| {
                    let z = h.0 - h.1 / 2.0;
                    let mut at = Vec::new();
                    if (3.0..4.0).contains(&z) {
                        at.push(rect(-4.0, -3.0, 0.0, 3.0));
                    }
                    if (4.2..5.2).contains(&z) {
                        at.push(rect(1.0, -2.0, x1, 2.0));
                    }
                    at
                })
                .collect()
        };
        let (s0, s1) = (part(5.0), part(6.0));
        let bed = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-100.0, -100.0), Pt::from_mm(100.0, 100.0)))];
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let first = Input { keys, print, slices: &s0, heights: &heights, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let mut cache = Cache::default();
        generate(Ctl::default(), &first, &mut cache).unwrap();
        cache.take_work();
        let edited = Input { keys: first.keys.clone(), slices: &s1, ..first };
        let warm = generate(Ctl::default(), &edited, &mut cache).unwrap();
        let mut afresh = Cache::default();
        let fresh = generate(Ctl::default(), &edited, &mut afresh).unwrap();
        assert!(all(&warm) == all(&fresh), "as a fresh run");
        let (w, f) = (cache.take_work(), afresh.take_work());
        assert!(w.prepared == 1 && w.faced > 0 && w.faced * 2 < f.faced, "{} layers of §2 worked on against {} afresh", w.faced, f.faced);
        assert!(w.tipped > 0 && w.tipped < f.tipped, "tips found under {} overhang layers against {} afresh", w.tipped, f.tipped);
    }

    /// Edits in a row that widen and narrow the upper of two slabs at two heights, the second cut
    /// short in its descent: each run after it, on what the runs before kept, comes out as a fresh
    /// run (the last past four runs' new ids), stepping, smoothing and drawing afresh fewer
    /// elements, trees, branches and groups than a fresh run: the lower slab's trees, on volume
    /// layers the edits left, are taken.
    #[test]
    fn edits_in_a_row_one_cut_short_come_out_as_fresh_runs() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let heights: Vec<(f64, f64)> = (0..26).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)));
        // Each layer's slice is cut at its middle; the upper slab reaches `x1`.
        let part = |x1: f64| -> Vec<Vec<ExPolygon>> {
            heights
                .iter()
                .map(|h| {
                    let z = h.0 - h.1 / 2.0;
                    let mut at = Vec::new();
                    if (3.0..4.0).contains(&z) {
                        at.push(rect(-4.0, -3.0, 0.0, 3.0));
                    }
                    if (4.2..5.2).contains(&z) {
                        at.push(rect(1.0, -2.0, x1, 2.0));
                    }
                    at
                })
                .collect()
        };
        let edits: Vec<Vec<Vec<ExPolygon>>> = [5.0, 6.0, 5.5, 6.5, 4.5, 5.0].into_iter().map(part).collect();
        let bed = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-100.0, -100.0), Pt::from_mm(100.0, 100.0)))];
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let first = Input { keys, print, slices: &edits[0], heights: &heights, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let mut cache = Cache::default();
        generate(Ctl::default(), &first, &mut cache).unwrap();
        // The second cut short once its descent is a few layers down.
        let stop = std::sync::atomic::AtomicBool::new(false);
        let emit = |e: crate::Emit| {
            if let crate::Emit::Tree { stage: Stage::Descent, layer, .. } = e {
                if layer < 18 {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        };
        let cancel = || stop.load(std::sync::atomic::Ordering::Relaxed);
        let cut = Ctl { cancel: Some(&cancel), emit: Some(&emit), ..Default::default() };
        let second = Input { keys: first.keys.clone(), slices: &edits[1], ..first };
        assert!(generate(cut, &second, &mut cache).is_err(), "the second edit's run is cut short");
        cache.take_work();
        for (k, slices) in edits.iter().enumerate().skip(2) {
            let at = Input { keys: first.keys.clone(), slices, ..first };
            let warm = generate(Ctl::default(), &at, &mut cache).unwrap();
            let mut afresh = Cache::default();
            let fresh = generate(Ctl::default(), &at, &mut afresh).unwrap();
            assert!(all(&warm) == all(&fresh), "edit {k}: as a fresh run");
            let (w, f) = (cache.take_work(), afresh.take_work());
            assert!(w.stepped > 0 && w.stepped < f.stepped, "edit {k}: {} steps made afresh against {} afresh", w.stepped, f.stepped);
            assert!(w.smoothed < f.smoothed && w.drawn < f.drawn, "edit {k}: {} trees smoothed and {} branches and groups drawn afresh against {} and {}", w.smoothed, w.drawn, f.smoothed, f.drawn);
        }
    }

    /// A run on all the last whole run was made from gives what that run gave and makes nothing,
    /// telling the trees as any run does, also after a run on something else cut short. A run on
    /// another print, bed or dropped tips is a run of its own, and comes out as a fresh one.
    #[test]
    fn a_run_on_all_the_last_was_made_from_takes_it_whole() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let heights: Vec<(f64, f64)> = (0..21).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
        let square = ExPolygon::new(Polygon::rect(Pt::from_mm(-3.0, -3.0), Pt::from_mm(3.0, 3.0)));
        let slices: Vec<Vec<ExPolygon>> = heights.iter().map(|h| if (3.0..4.0).contains(&(h.0 - h.1 / 2.0)) { vec![square.clone()] } else { Vec::new() }).collect();
        let (bed, wide) = (vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-100.0, -100.0), Pt::from_mm(100.0, 100.0)))], vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-101.0, -100.0), Pt::from_mm(100.0, 100.0)))]);
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let input = Input { keys, print, slices: &slices, heights: &heights, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let mut cache = Cache::default();
        let first = generate(Ctl::default(), &input, &mut cache).unwrap();
        cache.take_work();

        let told = std::sync::Mutex::new(Vec::new());
        let emit = |e: crate::Emit| {
            if let crate::Emit::Tree { grown: Grown::Trees { layers, .. }, .. } = e {
                told.lock().unwrap().push(layers.iter().map(Vec::len).sum::<usize>());
            }
        };
        let again = generate(Ctl { emit: Some(&emit), ..Default::default() }, &input, &mut cache).unwrap();
        assert!(Arc::ptr_eq(&first, &again), "what the last run gave");
        assert_eq!(cache.take_work(), Work::default(), "it makes nothing");
        assert_eq!(*told.lock().unwrap(), [first.trees.iter().map(Vec::len).sum::<usize>()], "the trees are told, all of them");

        let tip = first.roofs.tips.iter().enumerate().find_map(|(l, t)| t.first().map(|e| [e.point.x_mm(), e.point.y_mm(), heights[l].0])).expect("a tip");
        let dropped = [tip];
        let other_print = interface::Print { speed: 100.0, ..print };
        for (what, at) in [
            ("another print", Input { keys: input.keys.clone(), print: other_print, ..input }),
            ("another bed", Input { keys: input.keys.clone(), bed: &wide, ..input }),
            ("a tip dropped", Input { keys: input.keys.clone(), dropped: &dropped, ..input }),
        ] {
            let warm = generate(Ctl::default(), &at, &mut cache).unwrap();
            let fresh = generate(Ctl::default(), &at, &mut Cache::default()).unwrap();
            assert!(!Arc::ptr_eq(&warm, &first), "{what}: a run of its own");
            assert!(all(&warm) == all(&fresh), "{what}: as a fresh run");
            let back = generate(Ctl::default(), &input, &mut cache).unwrap();
            assert!(all(&back) == all(&first), "{what}, then back: as the first");
            cache.take_work();
        }

        // Two edits in a row, the second cut short in its printing: the run back on the first's takes it.
        let last = generate(Ctl::default(), &input, &mut cache).unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let emit = |e: crate::Emit| {
            if let crate::Emit::Tree { stage: Stage::Paths, .. } = e {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        };
        let cancel = || stop.load(std::sync::atomic::Ordering::Relaxed);
        let cut = Ctl { cancel: Some(&cancel), emit: Some(&emit), ..Default::default() };
        let other = Input { keys: input.keys.clone(), print: other_print, ..input };
        assert!(generate(cut, &other, &mut cache).is_err(), "the second edit's run is cut short");
        cache.take_work();
        let back = generate(Ctl::default(), &input, &mut cache).unwrap();
        assert!(Arc::ptr_eq(&last, &back), "what the last whole run gave");
        assert_eq!(cache.take_work(), Work::default(), "back on the first's it makes nothing");
    }

    /// Dropped tips: each set of drops takes exactly its tips, in any order and given twice, and
    /// drops of nothing change nothing; with every tip dropped no tree stands.
    #[test]
    fn dropped_tips_go_and_nothing_else() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let n = 26;
        let heights: Vec<(f64, f64)> = (0..n).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
        // Two slabs at two heights, so trees from both meet on the way down.
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)));
        let slices: Vec<Vec<ExPolygon>> = heights
            .iter()
            .map(|h| {
                let z = h.0 - h.1 / 2.0;
                let mut at = Vec::new();
                if (3.0..4.0).contains(&z) {
                    at.push(rect(-4.0, -3.0, 0.0, 3.0));
                }
                if (4.2..5.2).contains(&z) {
                    at.push(rect(1.0, -2.0, 5.0, 2.0));
                }
                at
            })
            .collect();
        let bed = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-100.0, -100.0), Pt::from_mm(100.0, 100.0)))];
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let input = Input { keys, print, slices: &slices, heights: &heights, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let g = generate(Ctl::default(), &input, &mut Cache::default()).unwrap();
        let hs = &heights;
        let at = |g: &Generated| -> Vec<[f64; 3]> { g.roofs.tips.iter().enumerate().flat_map(|(l, t)| t.iter().map(move |e| [e.point.x_mm(), e.point.y_mm(), hs[l].0])).collect() };
        let tips = at(&g);
        assert!(tips.len() > 12, "tips under both slabs: {}", tips.len());
        let none = [[50.0, 50.0, 3.0]];
        let run = |dropped: &[[f64; 3]]| generate(Ctl::default(), &Input { keys: input.keys.clone(), dropped, ..input }, &mut Cache::default()).unwrap();
        assert!(all(&run(&none)) == all(&g), "a drop that marks no tip changes nothing");
        let drops: [Vec<[f64; 3]>; 3] = [tips.iter().step_by(3).copied().collect(), tips.iter().step_by(2).copied().collect(), tips[..tips.len() / 2].to_vec()];
        for dropped in &drops {
            let d = run(dropped);
            let left: Vec<[f64; 3]> = tips.iter().filter(|t| !dropped.contains(t)).copied().collect();
            assert_eq!(at(&d), left, "{} dropped: exactly they go", dropped.len());
            let again: Vec<[f64; 3]> = dropped.iter().rev().chain(dropped).copied().collect();
            assert!(all(&run(&again)) == all(&d), "{} dropped: in any order, given twice", dropped.len());
        }
        let bare = run(&tips);
        assert!(bare.trees.iter().all(Vec::is_empty), "every tip dropped: no tree stands");
    }

    /// Dropped tips on a kept cache: each set of drops, added to and taken back in turn, comes
    /// out as a fresh run with the same drops, and drops its tips.
    #[test]
    fn a_run_on_a_kept_cache_is_a_fresh_run() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let n = 26;
        let heights: Vec<(f64, f64)> = (0..n).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
        // Two slabs at two heights, so trees from both meet on the way down.
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)));
        let slices: Vec<Vec<ExPolygon>> = heights
            .iter()
            .map(|h| {
                let z = h.0 - h.1 / 2.0;
                let mut at = Vec::new();
                if (3.0..4.0).contains(&z) {
                    at.push(rect(-4.0, -3.0, 0.0, 3.0));
                }
                if (4.2..5.2).contains(&z) {
                    at.push(rect(1.0, -2.0, 5.0, 2.0));
                }
                at
            })
            .collect();
        let bed = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-100.0, -100.0), Pt::from_mm(100.0, 100.0)))];
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let input = Input { keys, print, slices: &slices, heights: &heights, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let mut cache = Cache::default();
        let g = generate(Ctl::default(), &input, &mut cache).unwrap();
        let tips: Vec<[f64; 3]> = g.roofs.tips.iter().enumerate().flat_map(|(l, t)| t.iter().map(move |e| [e.point.x_mm(), e.point.y_mm(), input.heights[l].0])).collect();
        assert!(tips.len() > 12, "tips under both slabs: {}", tips.len());
        let mut drops: Vec<Vec<[f64; 3]>> = Vec::new();
        drops.push(tips.iter().step_by(3).copied().collect());
        drops.push(tips.iter().step_by(2).copied().collect());
        drops.push(tips[..tips.len() / 2].to_vec());
        drops.push(Vec::new());
        drops.push(tips.clone());
        for dropped in &drops {
            let at = Input { keys: input.keys.clone(), dropped, ..input };
            let warm = generate(Ctl::default(), &at, &mut cache).unwrap();
            let fresh = generate(Ctl::default(), &at, &mut Cache::default()).unwrap();
            assert!(all(&warm) == all(&fresh), "{} dropped: a run on the kept cache is a fresh run", dropped.len());
            let left: usize = warm.roofs.tips.iter().map(Vec::len).sum();
            assert_eq!(left, tips.len() - dropped.len(), "each drop takes its tip");
        }
    }

    /// A fresh run under a slab 5 mm over the bed, whose branches grow through the two rungs past
    /// the ramp on their way down, the second in the foot zone: its descent asks for no rung past
    /// what was raised beside the tips.
    #[test]
    fn the_descent_waits_on_no_rung() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let heights: Vec<(f64, f64)> = (0..31).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
        let slab = ExPolygon::new(Polygon::rect(Pt::from_mm(-2.0, -2.0), Pt::from_mm(2.0, 2.0)));
        let slices: Vec<Vec<ExPolygon>> = heights.iter().map(|h| if (5.0..6.0).contains(&(h.0 - h.1 / 2.0)) { vec![slab.clone()] } else { Vec::new() }).collect();
        let bed = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-100.0, -100.0), Pt::from_mm(100.0, 100.0)))];
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let input = Input { keys, print, slices: &slices, heights: &heights, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let mut cache = Cache::default();
        let g = generate(Ctl::default(), &input, &mut cache).unwrap();
        let s = settings::Settings::new(input.keys.clone());
        let ramp = s.ramp_rungs().into_iter().max().unwrap_or(0);
        let widest = g.trees.iter().flatten().map(|e| e.rung(&s)).max().unwrap_or(0);
        assert!(widest >= ramp + 2, "the branches grow through two rungs past the ramp: to rung {widest}, the ramp's last {ramp}");
        assert_eq!(cache.take_work().asked, 0, "the descent asked for no rung past what was raised");
    }

    /// A run on a kept cache with the bed moved: on a bed whose box still holds every tree it
    /// makes nothing afresh and comes out as a fresh run on that bed; on one whose edge cuts into
    /// the trees it draws and prints them afresh, as a fresh run on it does, and so again back on
    /// a bed that holds them. No bed makes the stages before the drawing again.
    #[test]
    fn a_moved_bed_that_holds_the_trees_takes_them_as_they_were() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let n = 26;
        let heights: Vec<(f64, f64)> = (0..n).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
        // Two slabs at two heights, so trees from both meet on the way down.
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)));
        let slices: Vec<Vec<ExPolygon>> = heights
            .iter()
            .map(|h| {
                let z = h.0 - h.1 / 2.0;
                let mut at = Vec::new();
                if (3.0..4.0).contains(&z) {
                    at.push(rect(-4.0, -3.0, 0.0, 3.0));
                }
                if (4.2..5.2).contains(&z) {
                    at.push(rect(1.0, -2.0, 5.0, 2.0));
                }
                at
            })
            .collect();
        // The bed, then moved two ways its box holds the trees, one its edge cuts them, and back.
        let beds: Vec<Vec<ExPolygon>> = [(-100.0, -100.0, 100.0, 100.0), (-90.0, -110.0, 120.0, 95.0), (-40.0, -40.0, 40.0, 40.0), (-2.0, -100.0, 100.0, 100.0), (-90.0, -110.0, 120.0, 95.0)].iter().map(|&(x0, y0, x1, y1)| vec![rect(x0, y0, x1, y1)]).collect();
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let input = Input { keys, print, slices: &slices, heights: &heights, enforce: &[], block: &[], keep_out: &[], bed: &beds[0], dropped: &[] };
        let mut cache = Cache::default();
        generate(Ctl::default(), &input, &mut cache).unwrap();
        let first = cache.take_work();
        assert!(first.prepared == 1 && first.stepped > 0 && first.smoothed > 0 && first.drawn > 0 && first.put > 0 && first.printed > 0, "the first run makes every stage: {first:?}");
        for (k, bed) in beds.iter().enumerate().skip(1) {
            let on = Input { keys: input.keys.clone(), bed, ..input };
            let warm = generate(Ctl::default(), &on, &mut cache).unwrap();
            let fresh = generate(Ctl::default(), &on, &mut Cache::default()).unwrap();
            assert!(all(&warm) == all(&fresh), "bed {k}: as a fresh run on it");
            let work = cache.take_work();
            if k <= 2 {
                assert_eq!(work, Work::default(), "bed {k}: its box holds the trees, nothing is made afresh");
            } else {
                assert!(work.prepared == 0 && work.stepped == 0 && work.smoothed == 0 && work.drawn > 0 && work.put > 0 && work.printed > 0, "bed {k}: drawn and printed afresh, and nothing before: {work:?}");
            }
        }
    }

    /// Every stage tells how far it is as it goes, each of its shares once, in the stages'
    /// order; the descent tells each layer it makes from the top down, with its branches, and the
    /// lines go out with their layers.
    #[test]
    fn generate_tells_every_stage_as_it_goes() {
        let keys = settings::Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, ..settings::Keys::new(0.2, 0.42, 0.42) };
        let n = 21;
        let heights: Vec<(f64, f64)> = (0..n).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect();
        let square = ExPolygon::new(Polygon::rect(Pt::from_mm(-3.0, -3.0), Pt::from_mm(3.0, 3.0)));
        let slices: Vec<Vec<ExPolygon>> = heights.iter().map(|h| if (3.0..4.0).contains(&(h.0 - h.1 / 2.0)) { vec![square.clone()] } else { Vec::new() }).collect();
        let bed = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-100.0, -100.0), Pt::from_mm(100.0, 100.0)))];
        let print = interface::Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };
        let input = Input { keys, print, slices: &slices, heights: &heights, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let told = std::sync::Mutex::new(Vec::new());
        let trees = std::sync::Mutex::new(Vec::new());
        let emit = |e: crate::Emit| {
            if let crate::Emit::Tree { stage, layer, of, grown, .. } = e {
                if let Grown::Trees { layers, .. } = grown {
                    trees.lock().unwrap().push((told.lock().unwrap().len(), layers.iter().map(Vec::len).sum::<usize>()));
                    return;
                }
                let shown = match grown {
                    Grown::Branches { layers, .. } => layers[layer].len(),
                    Grown::Paths(paths) => paths.len(),
                    _ => 0,
                };
                told.lock().unwrap().push((stage, layer, of, shown));
            }
        };
        let ctl = Ctl { emit: Some(&emit), ..Default::default() };
        let g = generate(ctl, &input, &mut Cache::default()).unwrap();
        let told = told.into_inner().unwrap();
        let trees = trees.into_inner().unwrap();
        assert_eq!(trees.len(), 1, "the trees are told once");
        let (at, elements) = trees[0];
        assert!(told[at - 1].0 == Stage::Smooth && told[at].0 == Stage::Draw, "as they are drawn");
        assert_eq!(elements, g.trees.iter().map(Vec::len).sum::<usize>(), "all of them");
        let mut order: Vec<Stage> = told.iter().map(|t| t.0).collect();
        order.dedup();
        assert_eq!(order, [Stage::Slices, Stage::Overhangs, Stage::Tips, Stage::Descent, Stage::Smooth, Stage::Draw, Stage::Paths], "every stage tells, in order");
        for stage in order {
            let of: Vec<usize> = told.iter().filter(|t| t.0 == stage).map(|t| t.2).collect();
            assert!(of.windows(2).all(|w| w[0] == w[1]), "{stage:?}: one count");
            assert!(of.len() <= of[0] && (stage == Stage::Smooth || of.len() == of[0]), "{stage:?}: each share once, {} of {}", of.len(), of[0]);
        }
        let descent: Vec<usize> = told.iter().filter(|t| t.0 == Stage::Descent).map(|t| t.1).collect();
        assert_eq!(descent, (0..n - 1).rev().collect::<Vec<_>>(), "the descent from the top down");
        assert!(told.iter().any(|t| t.0 == Stage::Descent && t.3 > 0), "branches as they grow");
        let lines: usize = told.iter().filter(|t| t.0 == Stage::Paths).map(|t| t.3).sum();
        assert_eq!(lines, g.paths.iter().map(|p| p.len()).sum::<usize>(), "every layer's lines go out");
    }
}
