//! `fab.variants`: two to six versions of the model side by side from one camera, each a batch of
//! fab.apply edits built as its dry run builds them (nothing commits, the head stays), so the AI
//! can pick one and apply it. With `compare` each is read against a reference picture as
//! fab.compare reads the model, from the same camera, and they come ranked by how well they
//! match it; with its `fit` the view the picture is seen from is searched for once, on the plate
//! as it is, and every variant is drawn from it.
//!
//! The call builds the variants and asks the app to draw them ([`crate::RenderRequest::Variants`]);
//! the app answers from what it drew ([`answer`]), the cells written with [`Answer::order`]'s words.

use crate::camera::named_view;
use crate::compare::{self, CapturedView, Comparison, CompareJob, Fit};
use crate::out::{arr, int, mm, n, obj, quote, s, text, S};
use crate::{camera_arg, plate_diff, str_arg, Camera, Focus, RenderRequest, Reply, Session, VIEW_NAMES};
use fab_model::eval::Evaluated;
use makepad_model::json::Value;
use std::sync::Arc;

pub const TOOL: &str = "variants";
/// The variants a call takes, at most.
pub const MOST: usize = 6;
/// The views each variant is drawn from, at most.
pub const MOST_VIEWS: usize = 2;
/// The bands a variant's compare names, those that differ most.
const BANDS_TOLD: usize = 3;

/// A `fab.variants` call, built: what the app draws and what the answer tells of each.
pub struct VariantJob {
    /// The head the variants were built on: what applying one expects.
    pub head: String,
    /// The views each variant is drawn from, as asked; a fit's comes before them ([`Self::rows`]).
    pub views: Vec<String>,
    /// The printed objects shown (empty: all).
    pub objects: Vec<String>,
    /// One camera for every variant: it frames all their boxes together.
    pub camera: Camera,
    /// The variants that built, in the order asked.
    pub built: Vec<Variant>,
    /// Those whose edits did not build: name and why.
    pub failed: Vec<(String, String)>,
    /// The reference each variant is read against. With a fit, of the plate as it is, seen from
    /// the survey's views: [`Self::survey`].
    pub compare: Option<CompareJob>,
}

/// One variant: its edits and the plate they make.
pub struct Variant {
    pub name: String,
    edits: String,
    script: Option<String>,
    /// The plate with the edits, as the review draws it.
    pub ev: Arc<Evaluated>,
    /// What the edits change against the plate as it is (fab.apply's dry run diff).
    diff: S,
}

impl std::fmt::Debug for VariantJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.built.iter().map(|v| v.name.as_str()).collect();
        write!(f, "VariantJob({names:?} at {:?})", self.views)
    }
}

impl PartialEq for VariantJob {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl VariantJob {
    /// The views each variant is drawn from, top row first: the fit's once found, then those asked.
    pub fn rows(&self, fit: Option<&Fit>) -> Vec<String> {
        fit.map(Fit::view).into_iter().chain(self.views.iter().cloned()).collect()
    }

    /// With a fit: the survey of the plate as it is, which the reference's view is searched over
    /// ([`fit`]) before any variant is drawn.
    pub fn survey(&self) -> Option<&CompareJob> {
        self.compare.as_ref().filter(|c| c.fits())
    }
}

/// `fab.variants`: builds each variant and asks the app to draw them.
pub(crate) fn tool(session: &mut Session, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
    match read_call(session, args, cancelled) {
        Ok(job) => Reply { images: Vec::new(), ok: true, text: String::new(), update: None, render: Some(RenderRequest::Variants(Arc::new(job))) },
        Err(e) => Reply::err(e),
    }
}

fn read_call(session: &mut Session, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Result<VariantJob, String> {
    let before = session.last.clone().ok_or("nothing is built yet: the variants are of the plate as it is")?;
    let base = session.for_review(before.clone())?;
    if base.plate.parts.is_empty() {
        return Err("nothing on the plate to vary yet".into());
    }
    let objects = compare::objects_arg(&base, args)?;
    let reference = args.get("compare");
    if let Some(key) = reference.and_then(|c| ["solid", "align"].into_iter().find(|k| c.get(k).is_some())) {
        return Err(format!("'{key}' is fab.compare's: a variant's compare reads its whole silhouette, as the variants change it"));
    }
    if reference.is_some_and(|c| c.get("view").is_some()) {
        return Err("the picture is seen from the first of 'views' (or the fit's): 'compare' takes no view".into());
    }
    let fit = reference.and_then(|c| c.get("fit")).and_then(Value::as_bool) == Some(true);
    let views: Vec<String> = match args.get("views") {
        None if fit => Vec::new(),
        None => vec!["front".to_string()],
        Some(Value::Arr(a)) => a.iter().map(|v| v.as_str().map(str::to_string).ok_or("'views' is a list of view names")).collect::<Result<_, _>>()?,
        Some(_) => return Err("'views' is a list of view names".into()),
    };
    if let Some(v) = views.iter().find(|v| named_view(v).is_none()) {
        return Err(format!("unknown view '{v}': {} or \"yaw,pitch\" in degrees", VIEW_NAMES.join(", ")));
    }
    let most = if fit { MOST_VIEWS - 1 } else { MOST_VIEWS };
    if views.len() > most || (!fit && views.is_empty()) {
        return Err(format!("'views' takes 1 to {MOST_VIEWS} views{}", if fit { ", the fit's first among them" } else { "" }));
    }
    let mut camera = camera_arg(args, &|name| session.locate(name), &|n| session.mark_box(n))?;
    if let Some(c) = reference {
        let (fov, ortho) = compare::lens(c)?;
        if (fov.is_some() || ortho) && (camera.fov.is_some() || camera.ortho) {
            return Err("give the lens once: in 'compare' (the picture's) or beside it".into());
        }
        camera.fov = camera.fov.or(fov);
        camera.ortho |= ortho;
    }
    let list = args.get("variants").and_then(Value::as_arr).ok_or("give 'variants': [{name: \"narrow\" edits: \"set(head.radius, 9mm)\"} ...], each a batch of fab.apply edits")?;
    if !(2..=MOST).contains(&list.len()) {
        return Err(format!("'variants' takes 2 to {MOST} variants, not {}", list.len()));
    }
    let mut built: Vec<Variant> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for v in list {
        let name = str_arg(v, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("each variant has a 'name'")?.to_string();
        if built.iter().any(|b| b.name == name) || failed.iter().any(|(f, _)| *f == name) {
            return Err(format!("two variants are named '{name}'"));
        }
        let edits = str_arg(v, "edits").ok_or_else(|| format!("variant '{name}' has no 'edits': fab.apply's Splash statements"))?.to_string();
        let changes = match session.script_changes(&edits, v) {
            Ok(changes) => changes,
            Err(e) => {
                failed.push((name, e));
                continue;
            }
        };
        match session.model.preview(&mut session.eval, &changes, cancelled) {
            Ok((_, _, ev)) => {
                let diff = plate_diff(&before, &ev);
                let ev = session.for_review(Arc::new(ev))?;
                built.push(Variant { name, edits, script: str_arg(v, "script").map(str::to_string), ev, diff });
            }
            Err(e) => failed.push((name, e.to_string())),
        }
    }
    if built.is_empty() {
        let why: Vec<String> = failed.iter().map(|(name, e)| format!("{name}: {e}")).collect();
        return Err(format!("no variant built ({})", why.join("; ")));
    }
    if camera.focus.is_none() {
        camera.focus = shown_box(&built, &objects).map(|(lo, hi)| Focus::Box(lo, hi));
    }
    let compare = match reference {
        None => None,
        Some(c) => {
            let (views, lens) = match fit {
                true => (compare::survey_views(), Camera { focus: None, distance: None, ..camera }),
                false => (vec![views[0].clone()], camera),
            };
            Some(compare::read_job(base, c, objects.clone(), views, fit, lens)?)
        }
    };
    Ok(VariantJob { head: session.model.head().text(), views, objects, camera, built, failed, compare })
}

/// The box (plate mm) every variant's shown parts fill together.
fn shown_box(built: &[Variant], objects: &[String]) -> Option<([f64; 3], [f64; 3])> {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for part in built.iter().flat_map(|v| &v.ev.plate.parts).filter(|p| objects.is_empty() || objects.contains(&p.object)) {
        let b = part.bounds();
        for k in 0..3 {
            lo[k] = lo[k].min(b.min[k]);
            hi[k] = hi[k].max(b.max[k]);
        }
    }
    (0..3).all(|k| lo[k] <= hi[k]).then_some((lo, hi))
}

/// The view the reference is seen from, searched over the survey's views of the plate as it is.
pub fn fit(job: &VariantJob, survey: &[CapturedView]) -> Result<Fit, String> {
    compare::fit(job.survey().ok_or("the variants search for no view")?, survey)
}

/// The variants' answer.
#[derive(Clone, Debug)]
pub struct Answer {
    pub text: String,
    /// The variants as the sheet shows them ([`VariantJob::built`]'s indexes; best match first with
    /// a reference), each with the lines written on its top row's cell and the name its others show.
    pub order: Vec<(usize, Vec<String>, String)>,
}

/// What the app drew of each variant ([`VariantJob::built`]'s order; each its rows' views), read:
/// each against the reference, if any, from its first row's view, and ranked by it.
pub fn answer(job: &VariantJob, fit: Option<&Fit>, drawn: &[Vec<CapturedView>]) -> Result<Answer, String> {
    if drawn.len() != job.built.len() || drawn.iter().any(|d| d.is_empty()) {
        return Err(format!("{} variants were drawn and {} came back", job.built.len(), drawn.len()));
    }
    let rows = job.rows(fit);
    let compared: Vec<Option<Comparison>> = job
        .built
        .iter()
        .zip(drawn)
        .map(|(v, views)| job.compare.as_ref().map(|c| compare::measure(&c.at(v.ev.clone(), rows[0].clone()), &views[..1])).transpose())
        .collect::<Result<_, _>>()?;
    let mut order: Vec<usize> = (0..job.built.len()).collect();
    if job.compare.is_some() {
        let iou = |i: usize| compared[i].as_ref().map_or(0.0, |c| c.iou);
        order.sort_by(|&a, &b| iou(b).total_cmp(&iou(a)));
    }
    let mut variants = Vec::new();
    let mut cells = Vec::new();
    for (rank, &i) in order.iter().enumerate() {
        let v = &job.built[i];
        let mut fields = vec![("name", s(&v.name))];
        let mut top = vec![format!("{} {}", rank + 1, v.name)];
        if let Some(c) = &compared[i] {
            fields.push(("iou", n(c.iou)));
            let told: Vec<_> = c.bands.iter().take(BANDS_TOLD).collect();
            fields.push(("differs", arr(told.iter().map(|b| {
                let mut band = vec![("band", int(b.band)), ("wider", mm((b.model - b.reference) * c.mm_per_px))];
                if let Some(o) = b.offset {
                    band.push(("moved", mm(o * c.mm_per_px)));
                }
                obj(band)
            }))));
            top[0] = format!("{} IoU {:.3}", top[0], c.iou);
            top.push(told.iter().take(2).map(|b| format!("b{} {:+.1}", b.band, (b.model - b.reference) * c.mm_per_px)).collect::<Vec<_>>().join(" ") + "mm");
        }
        fields.push(("diff", v.diff.clone()));
        let script = v.script.as_ref().map_or(String::new(), |f| format!("script: {} ", quote(f)));
        fields.push(("apply", s(format!("fab.apply {{request_id: {} expected: {} {script}edits: {}}}", quote(&format!("variant {}", v.name)), quote(&job.head), quote(&v.edits)))));
        variants.push(obj(fields));
        cells.push((i, top, v.name.clone()));
    }
    let mut fields = vec![("head", s(&job.head)), ("views", arr(rows.iter().map(s)))];
    if let (Some(c), Some(fit)) = (&job.compare, fit) {
        fields.push(("fit", obj(vec![("view", s(fit.view())), ("iou", n(fit.iou))])));
        if !fit.missed.is_empty() {
            fields.push(("fit_note", s("the survey's samples miss part of the plate as it is: check the fit with fab.compare at its view")));
        }
        fields.push(("picture", s(c.picture())));
    } else if let Some(c) = &job.compare {
        fields.push(("picture", s(c.picture())));
    }
    fields.push(("variants", arr(variants)));
    if !job.failed.is_empty() {
        fields.push(("failed", arr(job.failed.iter().map(|(name, e)| obj(vec![("name", s(name)), ("error", s(e))])))));
    }
    let shown = match rows.len() {
        1 => "the variants as listed, two to a row, row by row; four to a sheet",
        _ => "two variants to a sheet, as listed, left and right; each row a view, as 'views' lists them",
    };
    fields.push(("sheets", s(shown)));
    if job.compare.is_some() {
        fields.push(("how", s("each variant against the picture as fab.compare reads the model, from the first view; best match (IoU) first. differs: the bands that differ most (wider: the variant's width less the picture's; moved: its middle less the picture's, + to the right)")));
    }
    fields.push(("next", s("apply the one you pick with its 'apply' call; nothing here was committed")));
    Ok(Answer { text: text(&obj(fields)), order: cells })
}

#[cfg(test)]
#[path = "variants_tests.rs"]
mod tests;
