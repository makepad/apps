//! What of the body and the crown stands on what is under it and still under the nozzle.
//!
//! A piece that touches nothing under it (no bead of the layer below, no part top it may stand
//! on) goes, and so does a piece whose column wobbles past what the coral allows, with all that
//! then stands on air over it, until nothing more goes. Then what dead-ends goes too: a stem that
//! holds nothing, and the crown's lines that carried what went, pruned down to where they leave
//! one that still carries something. But what is one column with live material braces it, and
//! stays, and none of it goes unless, settled again without it, all that was held still is. A
//! contact is lost so only once nothing holds it.

use super::material::SupportPath;
use super::plan::{Cause, Coral, Plan, P2};
use super::{holds, line, prune, weigh};
use crate::par::{Cancelled, Ctl};
use crate::support_rules::{Legal, CONTACT_GAP};
use makepad_fab_slicer::geom::{ExPolygon, Pt, SCALE};
use makepad_fab_slicer::offset::{offset_ex, Join};

/// A path, and whether the crown printed it.
#[derive(Clone)]
struct Piece {
    path: SupportPath,
    crown: bool,
}

fn mm(p: &SupportPath) -> Vec<P2> {
    p.pts.iter().map(|q| (q.x_mm(), q.y_mm())).collect()
}

/// What standing asks of a support: what it may stand on, and how still it stands.
struct Stand<'a, 'b> {
    ctl: Ctl<'a>,
    legal: &'a Legal<'b>,
    limit: f64,
    z: Vec<f64>,
    h: Vec<f64>,
    reach: f64,
}

impl Stand<'_, '_> {
    fn wobbles(&self, layers: &[Vec<Piece>]) -> Vec<Vec<f64>> {
        let paths: Vec<Vec<SupportPath>> = layers.iter().map(|ps| ps.iter().map(|p| p.path.clone()).collect()).collect();
        weigh::wobbles(&paths, &self.z, &self.h, self.reach)
    }

    /// Takes from `layers` what stands on air and what wobbles past the limit, with what then
    /// stands on air over it, until nothing more goes: into `fallen` for why, and its lines into
    /// `gone`. Once something wobbled, `cause` is that.
    fn settle(&self, layers: &mut [Vec<Piece>], fallen: &mut [Vec<(SupportPath, Cause)>], gone: &mut [Vec<Vec<P2>>], cause: &mut Cause) -> Result<(), Cancelled> {
        loop {
            if self.ctl.cancelled() {
                return Err(Cancelled);
            }
            let mark: Vec<usize> = fallen.iter().map(Vec::len).collect();
            on_air(self.legal, layers, fallen, *cause);
            let wobbles = self.wobbles(layers);
            let mut shaky = false;
            for ((ps, w), fell) in layers.iter_mut().zip(&wobbles).zip(fallen.iter_mut()) {
                let (still, go): (Vec<(Piece, &f64)>, Vec<(Piece, &f64)>) = std::mem::take(ps).into_iter().zip(w).partition(|&(_, &by)| by <= self.limit);
                shaky |= !go.is_empty();
                *ps = still.into_iter().map(|(p, _)| p).collect();
                fell.extend(go.into_iter().map(|(p, _)| (p.path, Cause::Wobbles)));
            }
            for ((went, fell), &from) in gone.iter_mut().zip(fallen.iter()).zip(&mark) {
                went.extend(fell[from..].iter().map(|(p, _)| mm(p)));
            }
            if !shaky {
                return Ok(());
            }
            *cause = Cause::Wobbles;
        }
    }
}

/// What of `body` and `crown` (per layer, their paths) stands, and for each of `plan`'s contacts
/// that something held before, why nothing holds it now, if nothing does. `surfaces` are, per
/// layer, the underside the crown is to touch there.
pub(super) fn steady(ctl: Ctl, legal: &Legal, plan: &Plan, coral: &Coral, surfaces: &[Vec<ExPolygon>], body: Vec<Vec<SupportPath>>, crown: Vec<Vec<SupportPath>>) -> Result<(Vec<Vec<SupportPath>>, Vec<Option<Cause>>), Cancelled> {
    let n = body.len();
    let (z, h): (Vec<f64>, Vec<f64>) = legal.heights()[..n].iter().copied().unzip();
    let tan = legal.params().angle_max_deg.to_radians().tan();
    let reach = h.iter().fold(0.0, |a: f64, &x| a.max(x * tan));
    let stand = Stand { ctl, legal, limit: coral.wobble, z, h, reach };
    let mut layers: Vec<Vec<Piece>> = body.into_iter().zip(crown).map(|(b, c)| {
        b.into_iter().map(|path| Piece { path, crown: false }).chain(c.into_iter().map(|path| Piece { path, crown: true })).collect()
    }).collect();
    let none = || -> Vec<Vec<(SupportPath, Cause)>> { (0..n).map(|_| Vec::new()).collect() };
    let mut fallen = none();
    let held = |layers: &[Vec<Piece>]| -> Vec<bool> {
        plan.contacts.iter().map(|q| layers.get(q.top).is_some_and(|ps| ps.iter().any(|p| holds(q.at, &p.path)))).collect()
    };
    let first = held(&layers);
    // What goes on air before anything wobbled stood on air from the start; after, it stood on
    // what wobbled.
    let mut cause = Cause::OnAir;
    let mut gone: Vec<Vec<Vec<P2>>> = vec![Vec::new(); n];
    stand.settle(&mut layers, &mut fallen, &mut gone, &mut cause)?;
    // What dead-ends goes: stems left holding none of their contacts, with no crown over them
    // that still touches the underside, and the crown's lines under what went. Each goes only if, settled again without it, all that was held still
    // is: what braces a live column is not dead.
    let now = held(&layers);
    let dead = dead(legal, plan, surfaces, &layers, &now);
    let mut trial = layers.clone();
    let mut went: Vec<Vec<Vec<P2>>> = vec![Vec::new(); n];
    take_dead(&mut trial, &dead, &mut went);
    if went.iter().any(|w| !w.is_empty()) {
        let (mut fell, mut why) = (none(), cause);
        stand.settle(&mut trial, &mut fell, &mut went, &mut why)?;
        if held(&trial) == now {
            layers = trial;
            cause = why;
            for (((all, w), f), into) in gone.iter_mut().zip(went).zip(fell).zip(&mut fallen) {
                all.extend(w);
                into.extend(f);
            }
        }
    }
    while gone.iter().any(|g| !g.is_empty()) {
        let mut trial = layers.clone();
        prune_under(ctl, &mut trial, surfaces, &gone, reach, legal.params().line_width)?;
        let (mut fell, mut why) = (none(), cause);
        let mut went: Vec<Vec<Vec<P2>>> = vec![Vec::new(); n];
        stand.settle(&mut trial, &mut fell, &mut went, &mut why)?;
        if held(&trial) != now {
            break;
        }
        layers = trial;
        cause = why;
        for (f, into) in fell.into_iter().zip(&mut fallen) {
            into.extend(f);
        }
        gone = went;
    }
    let now = held(&layers);
    let lost = plan.contacts.iter().enumerate().map(|(i, q)| {
        if !first[i] || now[i] {
            return None;
        }
        let why: Vec<Cause> = fallen[q.top].iter().filter(|(p, _)| holds(q.at, p)).map(|&(_, c)| c).collect();
        [Cause::Wobbles, Cause::OnAir].into_iter().find(|c| why.contains(c))
    }).collect();
    Ok((layers.into_iter().map(|ps| ps.into_iter().map(|p| p.path).collect()).collect(), lost))
}

/// Which of `plan`'s stems hold nothing: none of their contacts is held (`held`), and no crown
/// over them in `layers` comes within half the contact gap of the underside it is to touch
/// (`surfaces`).
fn dead(legal: &Legal, plan: &Plan, surfaces: &[Vec<ExPolygon>], layers: &[Vec<Piece>], held: &[bool]) -> Vec<bool> {
    let by = (legal.params().line_width / 2.0 + CONTACT_GAP / 2.0) * SCALE;
    let mut touching = vec![false; plan.stems.len()];
    for (ps, surface) in layers.iter().zip(surfaces) {
        if surface.is_empty() || !ps.iter().any(|p| p.crown && !touching[p.path.frond]) {
            continue;
        }
        let near = offset_ex(surface, by, Join::Round(0.02 * SCALE));
        for p in ps.iter().filter(|p| p.crown) {
            if !touching[p.path.frond] && p.path.pts.iter().any(|&q| near.iter().any(|e| e.contains(q))) {
                touching[p.path.frond] = true;
            }
        }
    }
    fn live(plan: &Plan, s: usize, held: &[bool], touching: &[bool]) -> bool {
        touching[s] || plan.stems[s].contacts.iter().any(|&q| held[q]) || plan.stems[s].kids.iter().any(|&k| live(plan, k, held, touching))
    }
    (0..plan.stems.len()).map(|s| !live(plan, s, held, &touching)).collect()
}

/// Takes from `layers` the body of every `dead` stem into `went`, but what of it is one column
/// with live material: that braces it, and stays.
fn take_dead(layers: &mut [Vec<Piece>], dead: &[bool], went: &mut [Vec<Vec<P2>>]) {
    for (ps, went) in layers.iter_mut().zip(went) {
        if !ps.iter().any(|p| !p.crown && dead[p.path.frond]) {
            continue;
        }
        let (mut stem, mut live): (Vec<Piece>, Vec<Piece>) = std::mem::take(ps).into_iter().partition(|p| !p.crown && dead[p.path.frond]);
        loop {
            let lines: Vec<Vec<P2>> = live.iter().map(|p| mm(&p.path)).collect();
            let near = line::Near::new(lines.iter().map(|v| &v[..]));
            let (brace, rest): (Vec<Piece>, Vec<Piece>) = stem.into_iter().partition(|p| beside(&near, &p.path, weigh::COLUMN));
            stem = rest;
            if brace.is_empty() {
                break;
            }
            live.extend(brace);
        }
        *ps = live;
        went.extend(stem.iter().map(|p| mm(&p.path)));
    }
}

/// Whether some of `p` stands within `d` (mm) of `near`'s lines.
fn beside(near: &line::Near, p: &SupportPath, d: f64) -> bool {
    p.segments().any(|(a, b)| {
        let (a, b) = ((a.x_mm(), a.y_mm()), (b.x_mm(), b.y_mm()));
        let n = ((b.0 - a.0).hypot(b.1 - a.1) / 0.5).ceil().max(1.0) as usize;
        (0..=n).any(|k| {
            let t = k as f64 / n as f64;
            near.nearest((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t), d).0 <= d
        })
    })
}

/// Takes from `layers`, bottom up, every piece that touches no bead of the layer below and no
/// part top it may stand on, into `fallen` for `cause`, so what stood on one goes with it.
fn on_air(legal: &Legal, layers: &mut [Vec<Piece>], fallen: &mut [Vec<(SupportPath, Cause)>], cause: Cause) {
    let grow = legal.params().line_width / 2.0 * SCALE;
    for l in 1..layers.len() {
        let below: Vec<Vec<P2>> = layers[l - 1].iter().map(|p| mm(&p.path)).collect();
        let under = line::Near::new(below.iter().map(|v| &v[..]));
        let mut rest: Option<Vec<ExPolygon>> = None;
        let (stand, air): (Vec<Piece>, Vec<Piece>) = std::mem::take(&mut layers[l]).into_iter().partition(|p| {
            if p.path.pts.iter().any(|q| under.nearest((q.x_mm(), q.y_mm()), p.path.width).0 <= p.path.width) {
                return true;
            }
            let rest = rest.get_or_insert_with(|| {
                let tops = legal.rests(l);
                if tops.is_empty() { tops } else { offset_ex(&tops, grow, Join::Round(0.02 * SCALE)) }
            });
            p.path.pts.iter().any(|&q| rest.iter().any(|e| e.contains(q)))
        });
        layers[l] = stand;
        fallen[l].extend(air.into_iter().map(|p| (p.path, cause)));
    }
}

/// Prunes the crown's lines in `layers` that stood under what is `gone` (per layer) to what
/// touches `surfaces` or carries what is kept over it, so none ends holding nothing.
fn prune_under(ctl: Ctl, layers: &mut [Vec<Piece>], surfaces: &[Vec<ExPolygon>], gone: &[Vec<Vec<P2>>], reach: f64, lw: f64) -> Result<(), Cancelled> {
    let of = |crown: bool| -> Vec<Vec<Vec<P2>>> { layers.iter().map(|ps| ps.iter().filter(|p| p.crown == crown).map(|p| mm(&p.path)).collect()).collect() };
    let (crown, body) = (of(true), of(false));
    let kept = prune::prune(ctl, &crown, &body, surfaces, Some(prune::Under { gone, brace: weigh::COLUMN }), reach, lw)?;
    for (ps, kept) in layers.iter_mut().zip(kept) {
        let (from, body): (Vec<Piece>, Vec<Piece>) = std::mem::take(ps).into_iter().partition(|p| p.crown);
        *ps = body;
        ps.extend(kept.into_iter().map(|(i, line)| {
            let p = &from[i].path;
            let pts = line.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect();
            Piece { path: SupportPath { pts, width: p.width, role: p.role, support: p.support, frond: p.frond }, crown: true }
        }));
    }
    Ok(())
}
