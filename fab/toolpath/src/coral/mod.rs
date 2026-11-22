//! Coral supports: tube stems from a branch plan, grown up from a footing a support reaches,
//! and over each a crown that folds out to fill the part's underside.

pub mod crown;
pub mod line;
pub mod material;
pub mod plan;
pub mod prune;
pub mod reach;
#[cfg(test)]
mod scenes;
mod steady;
pub mod stem;
pub mod weigh;

use crate::par::{Cancelled, Ctl};
use crate::skeleton::{GivenUp, Hold};
use crate::support::{SupportParams, SPECK};
use crate::support_rules::{Legal, CONTACT_GAP};
use makepad_fab_slicer::clip::{difference_ex, intersection_ex};
use makepad_fab_slicer::geom::{dist_point_segment, total_area_mm2, ExPolygon, Pt, SCALE};

/// One part's coral supports.
pub struct Supports {
    /// Per layer (one per part layer), its paths.
    pub layers: Vec<Vec<material::SupportPath>>,
    /// The contacts it holds: on each one's layer, material within half the contact gap of it.
    pub contacts: Vec<plan::Contact>,
    /// The contacts it could not reach or print, and why.
    pub lost: Vec<plan::Lost>,
    /// The overhang it leaves unheld, by part layer: what no path on its contact's layer comes
    /// within half the contact gap of, paint's blocks aside.
    pub given_up: Vec<GivenUp>,
    /// The overhang paint blocks support under, by part layer.
    pub blocked: Vec<GivenUp>,
    /// The overhang it was asked to hold, and where it may stand.
    pub hold: Hold,
}

/// Coral supports for a part's `slices` at `heights`, keeping `p`, kept out of `blocks` (paint
/// blockers, per layer): the plan, the body it asks for, and the crown over it.
pub fn generate(ctl: Ctl, slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], p: &SupportParams, blocks: &[Vec<ExPolygon>]) -> Result<Supports, Cancelled> {
    let legal = Legal::new(slices, heights, p, blocks, &[]);
    let demand = crate::support::demand(ctl, slices, heights, p)?;
    let reach = reach_of(ctl, &legal, &demand)?;
    let coral = plan::Coral::default();
    let plan = plan::plan(ctl, &legal, &reach, &demand, &coral)?;
    let keep = line::Keep::new((0..legal.layers()).map(|l| reach.keep(l).to_vec()));
    let body = line::body(ctl, &legal, &keep, &plan)?;
    let crown = crown::crown(ctl, &legal, &keep, &plan, &body, &coral)?;
    let mut surfaces = vec![Vec::new(); body.layers.len()];
    for (d, need) in demand.iter().enumerate() {
        if let Some(t) = legal.contact(d).filter(|&t| t < surfaces.len()) {
            surfaces[t].extend(need.iter().cloned());
        }
    }
    let (layers, shaky) = steady::steady(ctl, &legal, &plan, &coral, &surfaces, body.layers, crown)?;
    let (given_up, blocked, need, held) = unheld(ctl, &legal, &demand, &layers)?;
    let mut lost = plan.lost;
    let mut carried = Vec::with_capacity(plan.contacts.len());
    for (q, gone) in plan.contacts.into_iter().zip(shaky) {
        match gone {
            Some(cause) => lost.push(plan::Lost { at: q.at, part: q.part, cause }),
            None => carried.push(q),
        }
    }
    let (contacts, unprinted) = printed(carried, &layers);
    lost.extend(unprinted);
    Ok(Supports { layers, contacts, lost, given_up, blocked, hold: Hold::of(need, held, p) })
}

/// The plan's `contacts` that printed material comes within half the contact gap of on their
/// layer, and those it does not, lost as unprinted.
fn printed(contacts: Vec<plan::Contact>, layers: &[Vec<material::SupportPath>]) -> (Vec<plan::Contact>, Vec<plan::Lost>) {
    let held = |q: &plan::Contact| layers.get(q.top).into_iter().flatten().any(|p| holds(q.at, p));
    let (kept, gone): (Vec<plan::Contact>, Vec<plan::Contact>) = contacts.into_iter().partition(held);
    (kept, gone.into_iter().map(|q| plan::Lost { at: q.at, part: q.part, cause: plan::Cause::Unprinted }).collect())
}

/// Whether `p` comes within half the contact gap of a contact at `at` (mm) on its layer.
fn holds(at: plan::P2, p: &material::SupportPath) -> bool {
    let at = Pt::from_mm(at.0, at.1);
    let r = (p.width / 2.0 + CONTACT_GAP / 2.0) * SCALE;
    p.segments().any(|(a, b)| dist_point_segment(at, a, b) <= r)
}

/// Per part layer, the overhang no path holds (what no path on its contact's layer comes within
/// half the contact gap of) and what paint blocks, as the tree tells them, the area (mm2) of all
/// of it that paint does not block, and of that the area the paths hold.
/// Where coral's lines get down from to the bed, a line wide at [`plan::CAP`] of the steep angle:
/// round what keeps a bead off (see [`Legal`]), and the side gap past xy_min with it, but on a
/// layer a contact is on. The side gap is a preference a route keeps where it can; until the
/// pathfinder the tree shares takes it as one, coral's routes keep it throughout, as the tree's
/// trunks do, and what holds the part's underside comes to xy_min, as the tree's roofs do.
pub(crate) fn reach_of(ctl: Ctl, legal: &Legal, demand: &[Vec<ExPolygon>]) -> Result<reach::Reach, Cancelled> {
    let p = legal.params();
    let n = legal.layers();
    let mut contact = vec![false; n];
    for (d, need) in demand.iter().enumerate() {
        if let Some(t) = legal.contact(d).filter(|&t| t < n && !need.is_empty()) {
            contact[t] = true;
        }
    }
    let routed = p.line_width + 2.0 * (p.xy_gap - p.xy_min()).max(0.0);
    let keep = crate::par::try_map(ctl, n, |l| legal.keep_off(l, if contact[l] { p.line_width } else { routed }))?;
    let rests = crate::par::try_map(ctl, n, |l| legal.rests(l))?;
    let lean: Vec<f64> = (0..n).map(|l| plan::CAP * legal.lean(l)).collect();
    reach::Reach::new(ctl, &keep, &rests, &lean)
}

fn unheld(ctl: Ctl, legal: &Legal, demand: &[Vec<ExPolygon>], layers: &[Vec<material::SupportPath>]) -> Result<(Vec<GivenUp>, Vec<GivenUp>, f64, f64), Cancelled> {
    let rows = crate::par::try_map(ctl, demand.len(), |d| {
        let need = &demand[d];
        if need.is_empty() {
            return (None, None, 0.0, 0.0);
        }
        let block = if legal.blocked(d).is_empty() { Vec::new() } else { intersection_ex(need, legal.blocked(d)) };
        let rest = if block.is_empty() { need.clone() } else { difference_ex(need, &block) };
        let held = legal.contact(d).and_then(|t| layers.get(t)).map_or_else(Vec::new, |ps| material::grown(ps, CONTACT_GAP / 2.0));
        let asked = total_area_mm2(&rest);
        let left = if held.is_empty() { rest } else { difference_ex(&rest, &held) };
        let kept = (asked - total_area_mm2(&left)).max(0.0);
        let left: Vec<ExPolygon> = left.into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
        (GivenUp::of(d, &left), GivenUp::of(d, &block), asked, kept)
    })?;
    Ok(rows.into_iter().fold((Vec::new(), Vec::new(), 0.0, 0.0), |(mut g, mut b, a, h), (l, k, asked, kept)| {
        g.extend(l);
        b.extend(k);
        (g, b, a + asked, h + kept)
    }))
}

#[cfg(test)]
mod tests {
    use super::scenes::Scene;
    use super::*;
    use makepad_fab_slicer::geom::Pt;

    /// Of `s`'s contacts, those no printed material on their layer comes within half the contact
    /// gap of, by the material's own grown outline.
    fn bare(s: &Supports) -> Vec<&plan::Contact> {
        s.contacts.iter().filter(|q| {
            let held = s.layers.get(q.top).filter(|p| !p.is_empty()).map_or_else(Vec::new, |p| material::grown(p, CONTACT_GAP / 2.0));
            !held.iter().any(|e| e.contains(Pt::from_mm(q.at.0, q.at.1)))
        }).collect()
    }

    /// On the six scenes, nothing coral prints stands on air and no column of it wobbles past the
    /// limit under the nozzle, and it still holds as many contacts as it holds now.
    #[test]
    fn every_scene_stands_on_something_and_still() {
        let mut faults = Vec::new();
        for (name, most) in HELD {
            let scene = Scene::named(name);
            let s = generate(Ctl::default(), &scene.slices, &scene.heights, &scene.p, &scene.blocks).unwrap();
            let air: Vec<_> = crate::support_rules::prove(Ctl::default(), &scene.legal(), &s.layers).unwrap().into_iter().filter(|f| f.contract == crate::support_rules::Contract::Footing).collect();
            if !air.is_empty() {
                faults.push(format!("{name}: {} pieces on air, as {:?}", air.len(), air.iter().take(2).map(|f| (f.layer, f.at)).collect::<Vec<_>>()));
            }
            let (z, h): (Vec<f64>, Vec<f64>) = scene.heights[..s.layers.len()].iter().copied().unzip();
            let reach = h.iter().fold(0.0, |a: f64, &x| a.max(x * scene.p.angle_max_deg.to_radians().tan()));
            let (w, on, _) = weigh::wobble(&s.layers, &z, &h, reach);
            if w > plan::Coral::default().wobble {
                faults.push(format!("{name}: wobbles {:.0} um on layer {on}", w * 1e3));
            }
            let dropped = s.lost.iter().filter(|l| matches!(l.cause, plan::Cause::Wobbles | plan::Cause::OnAir)).count();
            eprintln!("{name}: holds {} contacts, {dropped} dropped standing, wobble {:.0} um", s.contacts.len(), w * 1e3);
            if s.contacts.len() < most {
                faults.push(format!("{name}: holds {} contacts, fewer than {most}", s.contacts.len()));
            }
        }
        assert!(faults.is_empty(), "{}", faults.join("; "));
    }

    /// How many of each scene's contacts coral holds standing now. Unweighed, before it stood
    /// still, it held shelf 51, bar 26, branches 27, tower 53, pads 16 and mushroom 44: what it
    /// holds no more stood on crown fans of single lines that wobble past the limit.
    const HELD: [(&str, usize); 6] = [("shelf", 50), ("bar", 26), ("branches", 22), ("tower", 53), ("pads", 12), ("mushroom", 32)];

    /// How many of each scene's contacts the plan carries the crown leaves unprinted now: those in
    /// its holes. Closing them is the crown's next step. Of the mushroom's 56 its crown held 6 and
    /// lost 50, 10 of them unprinted, while it kept the tip's gap off the stem on the contact's
    /// layer; at xy_min it holds 32 and loses 24, 18 of them unprinted.
    const UNPRINTED: [(&str, usize); 6] = [("shelf", 9), ("bar", 2), ("branches", 4), ("tower", 9), ("pads", 4), ("mushroom", 18)];

    /// On the six scenes every contact kept gets material within half the contact gap of it on its
    /// layer, and those the plan carries but no material reaches are lost and told, no more of them
    /// than now: what is kept is what prints.
    #[test]
    fn every_contact_kept_is_printed() {
        let mut faults = Vec::new();
        for (name, most) in UNPRINTED {
            let scene = Scene::named(name);
            let s = generate(Ctl::default(), &scene.slices, &scene.heights, &scene.p, &scene.blocks).unwrap();
            let bare = bare(&s);
            if !bare.is_empty() {
                faults.push(format!("{name}: {} kept with no material near, as {:?}", bare.len(), bare.iter().take(3).map(|q| (q.at, q.top)).collect::<Vec<_>>()));
            }
            let unprinted: Vec<&plan::Lost> = s.lost.iter().filter(|l| l.cause == plan::Cause::Unprinted).collect();
            eprintln!("{name}: {} contacts kept, {} unprinted, {} lost in all", s.contacts.len(), unprinted.len(), s.lost.len());
            if unprinted.len() > most {
                faults.push(format!("{name}: {} of {} carried went unprinted, as {:?}", unprinted.len(), s.contacts.len() + unprinted.len(), unprinted.iter().take(3).map(|l| (l.at, l.part)).collect::<Vec<_>>()));
            }
        }
        assert!(faults.is_empty(), "{}", faults.join("; "));
    }
}
