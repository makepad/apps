//! `fab.compare`: the model against a reference picture, framed as one of fab.render's views, in
//! numbers. The reference's figure is what a flood fill from its border over the background colour
//! leaves, less the background it encloses (a window seen through it); the model's is the view's
//! own: its meshes and the field parts' tiles the view traced (sdf_tiles, as the render draws
//! them), drawn by [`crate::raster`] with the view's camera, its own openings open too. The
//! model is scaled and moved onto the reference by height, vertical middle and centre of area
//! across, and the two compared there: the silhouettes' IoU, their widths and middles in height
//! bands, and the reference's spots as plate points. With `fit` the view itself is searched for:
//! yaw and pitch, coarse to fine, over what eight survey views traced.
//!
//! The call reads the picture and asks the app for the views ([`crate::RenderRequest::Compare`]);
//! the app's model worker answers it from what they drew ([`answer`]).

use crate::camera::{named_view, Frame, Placed, View, ViewCamera, PITCH_LIMIT};
use crate::out::{arr, flag, int, mm, mm3, n, obj, s, S};
use crate::raster::DepthMap;
use crate::surface::SolidSkin;
use crate::{Camera, RenderRequest, Reply, Session, RENDER_FOV, VIEW_NAMES};
use fab_model::eval::Evaluated;
use makepad_ai_services::pictures::{crop, fit_pictures, RgbaPicture};
use makepad_ai_services::wire::MAX_RESULT_IMAGE_BYTES;
use makepad_model::json::Value;
use std::collections::HashSet;
use std::sync::Arc;

pub const TOOL: &str = "compare";
/// The width table's height bands.
pub const BANDS: usize = 16;
/// How far a pixel's colour may be from the background's and still be background, unless asked
/// (RGB distance, 0 to 441).
pub const BACKGROUND: f64 = 40.0;
/// A background's shadow is its colour this dark at the darkest (a share of its brightness).
const SHADOW_DARKEST: f64 = 0.3;
/// How far a shadow's colour may be from the background's, darkened, per channel.
const SHADOW_TINT: f64 = 14.0;
/// A shadow has no edge in it: no pixel of it differs from its neighbours by more than this, per
/// channel.
const SHADOW_SOFT: u8 = 10;
/// The shadow test is for light backgrounds: one at least this bright (the sum of its channels).
const SHADOW_LIGHT: f64 = 150.0;
/// The widest crack between traced tiles a model's silhouette closes, each side of it, in pixels.
const CRACK_PX: usize = 2;
/// How near a solid's surface a view's pixel's surface point is still the solid's: this many of
/// the view's pixels at its depth, and [`SOLID_MM`] (a drawn surface is within about a pixel of
/// the true one, a mesh within its chord).
const SOLID_PX: f64 = 2.0;
const SOLID_MM: f64 = 0.1;
/// The reference is read at most this many pixels on its longest side.
const READ_SIDE: usize = 1024;
/// The fit's views: yaw and pitch in degrees, round the figure and from above and below.
const SURVEY: [(f32, f32); 8] = [(0., -15.), (60., -15.), (120., -15.), (180., -15.), (240., -15.), (300., -15.), (0., -75.), (0., 60.)];
/// The fit's silhouettes, pixels square: the coarse search, then the finer steps.
const FIT_COARSE: usize = 160;
const FIT_FINE: usize = 240;
/// The fit's coarse grid, degrees.
const FIT_STEP: f32 = 15.0;
/// The fit's pitches stay this far inside the views' limit, degrees.
const FIT_PITCH: f32 = 60.0;
/// The survey's samples of the model are about this share of its size apart.
const FIT_SAMPLES: f64 = 1.0 / 200.0;
/// The most of a survey view's own drawing the samples may miss before the answer says so.
pub const FIT_MISS: f64 = 0.01;

/// A `fab.compare` call, read: the reference picture and what to ask of the views.
pub struct CompareJob {
    /// The plate as the views draw it.
    pub ev: Arc<Evaluated>,
    /// The solids shown (empty: all).
    pub objects: Vec<String>,
    /// The views to draw: the one asked, or the survey's.
    pub views: Vec<String>,
    pub camera: Camera,
    reference: Arc<Reference>,
    fit: bool,
    points: Vec<(String, [f64; 2])>,
    /// The solids the model's silhouette is cut to: what the views show of them (empty: all).
    solids: Arc<Vec<SolidSkin>>,
    /// What the alignment matches; None: the two whole figures.
    align: Option<Arc<AlignBy>>,
}

/// What fab.compare's alignment matches, when not the two whole figures.
enum AlignBy {
    /// The reference figure's rows from `band[0]` to `band[1]` of its height (from its top)
    /// against what the view shows of these solids.
    Band { band: [f64; 2], solids: Vec<SolidSkin> },
    /// Picture points (fractions of the picture) onto plate points (mm), by name.
    Points(Vec<(String, [f64; 2], [f64; 3])>),
}

impl std::fmt::Debug for CompareJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CompareJob({} at {:?})", self.reference.name, self.views)
    }
}

impl PartialEq for CompareJob {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl CompareJob {
    /// The same reference, region and points against another plate, seen from `view`.
    pub(crate) fn at(&self, ev: Arc<Evaluated>, view: String) -> CompareJob {
        CompareJob { ev, objects: self.objects.clone(), views: vec![view], camera: self.camera, reference: self.reference.clone(), fit: false, points: self.points.clone(), solids: self.solids.clone(), align: self.align.clone() }
    }

    /// The reference picture's name.
    pub fn picture(&self) -> &str {
        &self.reference.name
    }

    /// Whether the call searches for the view.
    pub fn fits(&self) -> bool {
        self.fit
    }
}

/// What one view drew, for the compare to read: its camera and each field part's tiles.
pub struct CapturedView<'a> {
    pub camera: ViewCamera,
    pub fields: Vec<FieldTiles<'a>>,
}

/// A field part's tiles as a view traced them: quads in the part's own frame, `stride` floats
/// each, the four corners first as (x, y, z, packed normal) round the quad, then its middle's
/// (x, y, z).
pub struct FieldTiles<'a> {
    /// Row-major, `p_printer = to_printer * p_shape`.
    pub to_printer: [[f64; 4]; 3],
    pub quads: Vec<&'a [f32]>,
    pub stride: usize,
}

/// The reference picture, read: its pixels at the size compared and its figure.
struct Reference {
    name: String,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    /// Its figure: what the flood fill from the border did not reach.
    mask: Vec<bool>,
    /// The background colour the fill took; None: transparent.
    background: Option<[u8; 3]>,
    tolerance: f64,
    /// The share of the border that is background.
    border: f64,
    /// The region of the picture read ([x0 y0 x1 y1] fractions); None: all of it.
    rect: Option<[f64; 4]>,
    /// The figure's pieces left out, all but the largest: each one's box (fractions of the
    /// picture) and pixels.
    dropped: Vec<([f64; 4], usize)>,
    /// The share of what was read taken as the background's shadow.
    shadow: f64,
    /// The background the figure encloses, seen through it: its pieces and pixels.
    holes: (usize, usize),
}

impl Reference {
    /// A point of the picture (fractions of it all) in the pixels read.
    fn pixel(&self, [u, v]: [f64; 2]) -> [f64; 2] {
        let [x0, y0, x1, y1] = self.rect.unwrap_or([0.0, 0.0, 1.0, 1.0]);
        [(u - x0) / (x1 - x0) * self.width as f64, (v - y0) / (y1 - y0) * self.height as f64]
    }

    /// A pixel read, in fractions of the picture.
    fn fraction(&self, [x, y]: [f64; 2]) -> [f64; 2] {
        let [x0, y0, x1, y1] = self.rect.unwrap_or([0.0, 0.0, 1.0, 1.0]);
        [x0 + x / self.width as f64 * (x1 - x0), y0 + y / self.height as f64 * (y1 - y0)]
    }
}

/// How a reference's figure is read.
#[derive(Clone, Copy, Debug)]
struct Reading {
    tolerance: f64,
    rect: Option<[f64; 4]>,
    /// Every piece of the figure, not only its largest.
    islands: bool,
    /// The background's soft shadow taken as background.
    shadows: bool,
    /// The background the figure encloses taken as background.
    holes: bool,
}

/// A silhouette's extent: its rows (first, past the last) and its centre of area across.
#[derive(Clone, Copy, Debug)]
struct Extent {
    top: f64,
    bottom: f64,
    across: f64,
}

impl Extent {
    fn of(width: usize, height: usize, inside: impl Fn(usize, usize) -> bool) -> Option<Extent> {
        let (mut top, mut bottom, mut sum, mut count) = (usize::MAX, 0, 0.0, 0usize);
        for y in 0..height {
            for x in 0..width {
                if inside(x, y) {
                    top = top.min(y);
                    bottom = y + 1;
                    sum += x as f64 + 0.5;
                    count += 1;
                }
            }
        }
        (count > 0).then(|| Extent { top: top as f64, bottom: bottom as f64, across: sum / count as f64 })
    }

    fn height(&self) -> f64 {
        self.bottom - self.top
    }

    fn middle(&self) -> f64 {
        (self.top + self.bottom) * 0.5
    }
}

/// The model's view laid on the reference: reference pixels per model pixel, and the two
/// silhouettes' points that meet (centre of area across, vertical middle).
#[derive(Clone, Copy, Debug)]
struct Align {
    scale: f64,
    reference: [f64; 2],
    model: [f64; 2],
}

impl Align {
    fn of(reference: Extent, model: Extent) -> Align {
        Align { scale: reference.height() / model.height(), reference: [reference.across, reference.middle()], model: [model.across, model.middle()] }
    }

    /// The scale and move that best lay model points on reference points (pairs of pixels,
    /// least squares, no turn); None: the model points are one.
    fn through(pairs: &[([f64; 2], [f64; 2])]) -> Option<Align> {
        let mean = |k: usize| -> [f64; 2] { [0, 1].map(|c| pairs.iter().map(|p| [p.0, p.1][k][c]).sum::<f64>() / pairs.len() as f64) };
        let (reference, model) = (mean(0), mean(1));
        let (mut along, mut spread) = (0.0, 0.0);
        for (r, m) in pairs {
            let (dr, dm) = ([r[0] - reference[0], r[1] - reference[1]], [m[0] - model[0], m[1] - model[1]]);
            along += dr[0] * dm[0] + dr[1] * dm[1];
            spread += dm[0] * dm[0] + dm[1] * dm[1];
        }
        (spread >= 1.0 && along > 0.0).then(|| Align { scale: along / spread, reference, model })
    }

    /// A model point (its view's pixels) on the reference.
    fn to_reference(&self, m: [f64; 2]) -> [f64; 2] {
        [(m[0] - self.model[0]) * self.scale + self.reference[0], (m[1] - self.model[1]) * self.scale + self.reference[1]]
    }

    /// A reference point (its pixels) in the model's view.
    fn to_model(&self, p: [f64; 2]) -> [f64; 2] {
        [(p[0] - self.reference[0]) / self.scale + self.model[0], (p[1] - self.reference[1]) / self.scale + self.model[1]]
    }
}

/// The survey's views by name ("yaw,pitch" in degrees).
pub fn survey_views() -> Vec<String> {
    SURVEY.iter().map(|(yaw, pitch)| format!("{yaw},{pitch}")).collect()
}

/// `fab.compare`: reads the picture and asks for the views.
pub(crate) fn tool(session: &mut Session, args: &Value) -> Reply {
    match read_call(session, args) {
        Ok(job) => Reply { images: Vec::new(), ok: true, text: String::new(), update: None, render: Some(RenderRequest::Compare(Arc::new(job))) },
        Err(e) => Reply::err(e),
    }
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::F64(f) if f.is_finite() => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

fn fractions<const N: usize>(v: &Value) -> Option<[f64; N]> {
    let a = v.as_arr().filter(|a| a.len() == N)?;
    let mut out = [0.0; N];
    for (o, v) in out.iter_mut().zip(a) {
        *o = number(v).filter(|f| (0.0..=1.0).contains(f))?;
    }
    Some(out)
}

fn read_call(session: &mut Session, args: &Value) -> Result<CompareJob, String> {
    let ev = session.review_plate()?;
    if ev.plate.parts.is_empty() {
        return Err("nothing on the plate to compare yet".into());
    }
    let fit = args.get("fit").and_then(Value::as_bool) == Some(true);
    let view = match args.get("view").and_then(Value::as_str) {
        Some(v) if named_view(v).is_some() => Some(v.to_string()),
        Some(v) => return Err(format!("unknown view '{v}': {} or \"yaw,pitch\" in degrees", VIEW_NAMES.join(", "))),
        None => None,
    };
    let views = match (fit, view) {
        (true, _) => survey_views(),
        (false, Some(v)) => vec![v],
        (false, None) => return Err("give the view the reference is seen from: view (a fab.render view name or \"yaw,pitch\" in degrees), or fit: true to search for it".into()),
    };
    let objects = objects_arg(&ev, args)?;
    let (fov, ortho) = lens(args)?;
    let solids = match args.get("solid") {
        None => Vec::new(),
        Some(v) => skins(session, v, "solid")?,
    };
    let align = match args.get("align") {
        None => None,
        Some(_) if fit => return Err("'align' is for a view given: fit first ('fit': true), then compare at the view it finds with 'align'".into()),
        Some(v) => Some(Arc::new(align_arg(session, v)?)),
    };
    let mut job = read_job(ev, args, objects, views, fit, Camera { focus: None, distance: None, fov, ortho, ..Default::default() })?;
    job.solids = Arc::new(solids);
    job.align = align;
    Ok(job)
}

/// The solids a `key` names (one, or a list), each's surface on the plate.
fn skins(session: &mut Session, v: &Value, key: &str) -> Result<Vec<SolidSkin>, String> {
    let names: Vec<&str> = match v {
        Value::Str(name) => vec![name.as_str()],
        Value::Arr(a) if !a.is_empty() => a.iter().map(|n| n.as_str().ok_or(format!("'{key}' is a solid's name (fab.tree lists them) or a list of them"))).collect::<Result<_, _>>()?,
        _ => return Err(format!("'{key}' is a solid's name (fab.tree lists them) or a list of them")),
    };
    names.iter().map(|name| session.solid_skin(name).map_err(|e| format!("'{key}': {e}"))).collect()
}

/// `align`: {band: [from to] solid: name(s)} or {points: {name: {picture: [u v] plate: [x y z]} ...}}.
fn align_arg(session: &mut Session, v: &Value) -> Result<AlignBy, String> {
    let wrong = "'align' is {band: [from to] solid: \"head\"} (the reference figure's rows from..to of its height, from its top, against what the view shows of the solid or solids) or {points: {name: {picture: [u v] plate: [x y z]} ...}} (two or more picture points, fractions of the picture, onto plate points, mm)";
    match (v.get("band"), v.get("points")) {
        (Some(band), None) => {
            let band = fractions::<2>(band).filter(|[a, b]| b > a).ok_or(wrong)?;
            let solids = skins(session, v.get("solid").ok_or(wrong)?, "align.solid")?;
            Ok(AlignBy::Band { band, solids })
        }
        (None, Some(Value::Obj(fields))) if fields.len() >= 2 => {
            let mut points = Vec::new();
            for (name, p) in fields {
                let picture = p.get("picture").and_then(fractions::<2>).ok_or(wrong)?;
                let plate = p.get("plate").and_then(|q| q.as_arr().filter(|a| a.len() == 3)?.iter().map(number).collect::<Option<Vec<f64>>>()).ok_or(wrong)?;
                points.push((name.clone(), picture, [plate[0], plate[1], plate[2]]));
            }
            Ok(AlignBy::Points(points))
        }
        _ => Err(wrong.into()),
    }
}

/// The printed objects `object` names (one, or a list; none: all), each on the plate.
pub(crate) fn objects_arg(ev: &Evaluated, args: &Value) -> Result<Vec<String>, String> {
    let parts = &ev.plate.parts;
    let resolve = |object: &str| crate::printed_part_index(parts, object).map(|i| parts[i].object.clone());
    match args.get("object") {
        Some(Value::Str(o)) => Ok(vec![resolve(o)?]),
        Some(Value::Arr(a)) => a.iter().filter_map(Value::as_str).map(resolve).collect(),
        _ => Ok(Vec::new()),
    }
}

/// The picture's lens: `fov` (degrees, a photo's) or `ortho` (a drawing's), checked.
pub(crate) fn lens(args: &Value) -> Result<(Option<f64>, bool), String> {
    let fov = match args.get("fov") {
        None => None,
        Some(v) => match number(v) {
            Some(f) if (RENDER_FOV.0..=RENDER_FOV.1).contains(&f) => Some(f),
            _ => return Err(format!("'fov' is degrees across the view, {} to {}", RENDER_FOV.0, RENDER_FOV.1)),
        },
    };
    let ortho = args.get("ortho").and_then(Value::as_bool) == Some(true);
    if ortho && fov.is_some() {
        return Err("'fov' is a perspective view's: an orthographic one ('ortho': true) has none".into());
    }
    Ok((fov, ortho))
}

/// A compare of `ev` against the picture `args` names (`picture`, with its `background`, `rect`
/// and `points`), from `views` with `camera`.
pub(crate) fn read_job(ev: Arc<Evaluated>, args: &Value, objects: Vec<String>, views: Vec<String>, fit: bool, camera: Camera) -> Result<CompareJob, String> {
    let path = args.get("picture").and_then(Value::as_str).ok_or("give the reference: picture (the name of a picture sent in this chat, or a picture file's path)")?;
    let rect = match args.get("rect") {
        None => None,
        Some(v) => match fractions::<4>(v) {
            Some([x0, y0, x1, y1]) if x1 > x0 && y1 > y0 => Some([x0, y0, x1, y1]),
            _ => return Err("'rect' is [x0 y0 x1 y1] in fractions of the picture (0 to 1, from its top left), x0 < x1 and y0 < y1".into()),
        },
    };
    let tolerance = match args.get("background") {
        None => BACKGROUND,
        Some(v) => match number(v) {
            Some(t) if (0.0..=441.0).contains(&t) => t,
            _ => return Err("'background' is how far a colour may be from the background's and still be background: 0 to 441 (RGB distance; default 40)".into()),
        },
    };
    let points = match args.get("points") {
        None => Vec::new(),
        Some(Value::Obj(fields)) => {
            let mut points = Vec::new();
            for (name, v) in fields {
                let p = fractions::<2>(v).ok_or_else(|| format!("point '{name}' is [u v] in fractions of the picture (0 to 1, from its top left)"))?;
                points.push((name.clone(), p));
            }
            points
        }
        Some(_) => return Err("'points' is {name: [u v] ...}, each in fractions of the picture (0 to 1, from its top left)".into()),
    };
    let flag = |key: &str, default: bool| match args.get(key) {
        None => Ok(default),
        Some(v) => v.as_bool().ok_or(format!("'{key}' is true or false")),
    };
    let reading = Reading { tolerance, rect, islands: flag("islands", false)?, shadows: flag("shadows", true)?, holes: flag("holes", true)? };
    let bytes = std::fs::read(path).map_err(|e| format!("the picture '{path}' does not read: {e}"))?;
    let name = std::path::Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().to_string());
    let reference = Arc::new(read_reference(name, &bytes, reading)?);
    Ok(CompareJob { ev, objects, views, camera, reference, fit, points, solids: Arc::new(Vec::new()), align: None })
}

/// The picture's figure: the region asked cut out first, so its own edges are where the
/// background is read; a fill from those edges over the background's colour (and its soft
/// shadow) leaves the figure, less the background it encloses, and of that only its largest
/// piece is kept.
fn read_reference(name: String, bytes: &[u8], reading: Reading) -> Result<Reference, String> {
    let (w, h, rgba) = fab_colour::decode_rgba(bytes)?;
    let (w, h, rgba) = match reading.rect {
        Some(rect) => {
            let cut = crop(&RgbaPicture { width: w as u32, height: h as u32, rgba }, rect);
            (cut.width as usize, cut.height as usize, cut.rgba)
        }
        None => (w, h, rgba),
    };
    let longest = w.max(h);
    let (width, height, rgba) = if longest > READ_SIDE {
        let (sw, sh) = ((w * READ_SIDE).div_ceil(longest).max(1), (h * READ_SIDE).div_ceil(longest).max(1));
        let small = crate::img2mesh::shrink(&makepad_model::RgbaImage { width: w as u32, height: h as u32, pixels: rgba }, sw as u32, sh as u32);
        (sw, sh, small.pixels)
    } else {
        (w, h, rgba)
    };
    let tolerance = reading.tolerance;
    let px = |i: usize| [rgba[i * 4], rgba[i * 4 + 1], rgba[i * 4 + 2], rgba[i * 4 + 3]];
    let border: Vec<usize> = (0..width).flat_map(|x| [x, (height - 1) * width + x]).chain((0..height).flat_map(|y| [y * width, y * width + width - 1])).collect();
    let clear = border.iter().filter(|&&i| px(i)[3] < 128).count();
    let background = if clear * 2 >= border.len() {
        None
    } else {
        let median = |c: usize| {
            let mut v: Vec<u8> = border.iter().filter(|&&i| px(i)[3] >= 128).map(|&i| px(i)[c]).collect();
            v.sort_unstable();
            v[v.len() / 2]
        };
        Some([median(0), median(1), median(2)])
    };
    let is_background = |i: usize| {
        let p = px(i);
        p[3] < 128
            || background.is_some_and(|b| {
                let d: f64 = (0..3).map(|c| (p[c] as f64 - b[c] as f64).powi(2)).sum();
                d.sqrt() <= tolerance
            })
    };
    // A shadow on the background: its colour darker, the same tint, and soft, with no edge in it
    // (a figure's outline is one). A tolerance wide enough for a shadow would take a figure's
    // darker parts as well; the shadow's own two marks keep them.
    let soft = |i: usize| {
        let (x, y, p) = (i % width, i / width, px(i));
        [(x > 0).then(|| i - 1), (x + 1 < width).then(|| i + 1), (y > 0).then(|| i - width), (y + 1 < height).then(|| i + width)]
            .into_iter()
            .flatten()
            .all(|j| (0..3).all(|c| px(j)[c].abs_diff(p[c]) <= SHADOW_SOFT))
    };
    let is_shadow = |i: usize| {
        let Some(b) = background.filter(|b| reading.shadows && b.iter().map(|&c| c as f64).sum::<f64>() >= SHADOW_LIGHT) else { return false };
        let p = px(i);
        let k = p[..3].iter().map(|&c| c as f64).sum::<f64>() / b.iter().map(|&c| c as f64).sum::<f64>();
        p[3] >= 128 && (SHADOW_DARKEST..1.0).contains(&k) && (0..3).all(|c| (p[c] as f64 - k * b[c] as f64).abs() <= SHADOW_TINT) && soft(i)
    };
    let mut filled = vec![false; width * height];
    let mut shadow = 0usize;
    let mut stack: Vec<usize> = border.iter().copied().filter(|&i| is_background(i) || is_shadow(i)).collect();
    let border_share = border.iter().filter(|&&i| is_background(i)).count() as f64 / border.len() as f64;
    while let Some(i) = stack.pop() {
        if filled[i] {
            continue;
        }
        filled[i] = true;
        if !is_background(i) {
            shadow += 1;
        }
        let (x, y) = (i % width, i / width);
        let near = [(x > 0).then(|| i - 1), (x + 1 < width).then(|| i + 1), (y > 0).then(|| i - width), (y + 1 < height).then(|| i + width)];
        stack.extend(near.into_iter().flatten().filter(|&j| !filled[j] && (is_background(j) || is_shadow(j))));
    }
    let mut mask: Vec<bool> = filled.iter().map(|f| !f).collect();
    if !mask.iter().any(|&m| m) {
        let colour = background.map_or("transparent".to_string(), |b| format!("{b:?}"));
        return Err(format!("the picture reads as all background ({colour}, within {tolerance}): give 'background' a smaller tolerance"));
    }
    // The background the figure encloses is seen through it (a window, an arch's opening closed
    // below): background, where it is wider than the model's silhouette closes cracks ([`close`]),
    // so a speck of the background's colour on the figure stays figure.
    let mut holes = (0, 0);
    if reading.holes {
        let figure: Vec<bool> = (0..width * height).map(|i| mask[i] && !is_background(i)).collect();
        let closed = close(&figure, width, height);
        let through: Vec<bool> = (0..width * height).map(|i| mask[i] && !closed[i]).collect();
        holes = (pieces(&through, width, height).len(), through.iter().filter(|&&t| t).count());
        mask.iter_mut().zip(&through).for_each(|(m, t)| *m &= !t);
    }
    let mut r = Reference { name, width, height, rgba, mask: Vec::new(), background, tolerance, border: border_share, rect: reading.rect, dropped: Vec::new(), shadow: shadow as f64 / (width * height) as f64, holes };
    if !reading.islands {
        let pieces = pieces(&mask, width, height);
        let largest = pieces.iter().enumerate().max_by_key(|(_, p)| p.len()).map_or(0, |(k, _)| k);
        for piece in pieces.iter().enumerate().filter(|(k, _)| *k != largest).map(|(_, piece)| piece) {
            let (lo, hi) = piece.iter().fold(([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]), |(lo, hi), &i| {
                let (x, y) = ((i % width) as f64, (i / width) as f64);
                ([lo[0].min(x), lo[1].min(y)], [hi[0].max(x + 1.0), hi[1].max(y + 1.0)])
            });
            let ([x0, y0], [x1, y1]) = (r.fraction(lo), r.fraction(hi));
            r.dropped.push(([x0, y0, x1, y1], piece.len()));
            for &i in piece {
                mask[i] = false;
            }
        }
        r.dropped.sort_by(|a, b| b.1.cmp(&a.1));
    }
    r.mask = mask;
    Ok(r)
}

/// The mask's pieces: its pixels connected side to side or corner to corner, each a list.
fn pieces(mask: &[bool], width: usize, height: usize) -> Vec<Vec<usize>> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        let mut piece = Vec::new();
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(i) = stack.pop() {
            piece.push(i);
            let (x, y) = ((i % width) as isize, (i / width) as isize);
            for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx >= 0 && ny >= 0 && (nx as usize) < width && (ny as usize) < height {
                    let j = ny as usize * width + nx as usize;
                    if mask[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        out.push(piece);
    }
    out
}

/// The model's silhouette in a view, solid as the reference's figure is read: what `drawn`
/// covers, the cracks traced tiles leave between them closed ([`close`]). A gap it encloses is
/// seen through, background as the reference's holes are, unless the view's whole drawing
/// (`whole`, when `drawn` is what it shows of some solids) covers it: another solid there.
fn solid(drawn: &DepthMap, whole: Option<&DepthMap>) -> Vec<bool> {
    let (w, h) = (drawn.width, drawn.height);
    let covers = |map: &DepthMap| close(&(0..w * h).map(|i| map.covered(i % w, i / w)).collect::<Vec<_>>(), w, h);
    let shown = covers(drawn);
    let Some(whole) = whole else { return shown };
    let whole = covers(whole);
    let mut outside = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w).flat_map(|x| [x, (h - 1) * w + x]).chain((0..h).flat_map(|y| [y * w, y * w + w - 1])).filter(|&i| !shown[i]).collect();
    while let Some(i) = stack.pop() {
        if outside[i] {
            continue;
        }
        outside[i] = true;
        let (x, y) = (i % w, i / w);
        let near = [(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1), (y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)];
        stack.extend(near.into_iter().flatten().filter(|&j| !outside[j] && !shown[j]));
    }
    (0..w * h).map(|i| shown[i] || (!outside[i] && whole[i])).collect()
}

/// A mask closed: grown by [`CRACK_PX`], then shrunk back by it, so a gap up to twice that wide
/// fills and a wider one keeps its shape.
fn close(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    spread(&spread(mask, w, h, CRACK_PX, true), w, h, CRACK_PX, false)
}

/// A mask grown (`grow`: any pixel within `r` set) or shrunk (all within `r` set) by a square
/// of `r` pixels each way, across then down. Beyond the edges counts as unset when growing and set
/// when shrinking, so shrinking undoes growing there too.
fn spread(mask: &[bool], w: usize, h: usize, r: usize, grow: bool) -> Vec<bool> {
    let pass = |get: &dyn Fn(usize, usize) -> bool, w: usize, h: usize| -> Vec<bool> {
        let mut out = vec![false; w * h];
        for y in 0..h {
            // the count of set pixels in a window of 2r + 1 along the row
            let mut run = (0..=r.min(w - 1)).filter(|&x| get(x, y)).count();
            for x in 0..w {
                let span = (x + r).min(w - 1) - x.saturating_sub(r) + 1;
                let beyond = 2 * r + 1 - span;
                out[y * w + x] = if grow { run > 0 } else { run + beyond == 2 * r + 1 };
                if x + r + 1 < w && get(x + r + 1, y) {
                    run += 1;
                }
                if x >= r && get(x - r, y) {
                    run -= 1;
                }
            }
        }
        out
    };
    let across = pass(&|x, y| mask[y * w + x], w, h);
    // down: the same pass over the transposed mask
    let down = pass(&|x, y| across[x * w + y], h, w);
    (0..w * h).map(|i| down[(i % w) * h + i / w]).collect()
}

/// The plate's meshes the job shows: triangles of plate points (mm).
fn mesh_triangles(job: &CompareJob) -> Vec<[[f64; 3]; 3]> {
    plate_triangles(&job.ev, &job.objects)
}

/// The meshes of `objects` (empty: all) on the plate, as the review draws them: triangles of
/// plate points (mm). A field part is none: each view traces it.
fn plate_triangles(ev: &Evaluated, objects: &[String]) -> Vec<[[f64; 3]; 3]> {
    let mut out = Vec::new();
    for (i, (part, rm)) in ev.plate.parts.iter().zip(&ev.render).enumerate() {
        if (!objects.is_empty() && !objects.iter().any(|o| *o == part.object)) || ev.placed_field(i).is_some() {
            continue;
        }
        for t in rm.indices.chunks_exact(3) {
            out.push([0, 1, 2].map(|k| rm.positions[t[k] as usize].map(f64::from)));
        }
    }
    out
}

fn to_plate(m: &[[f64; 4]; 3], p: &[f32]) -> [f64; 3] {
    let (x, y, z) = (p[0] as f64, p[1] as f64, p[2] as f64);
    std::array::from_fn(|r| m[r][0] * x + m[r][1] * y + m[r][2] * z + m[r][3])
}

/// Each tile's corners and middle, plate mm.
fn tiles<'a>(fields: &'a [FieldTiles<'a>]) -> impl Iterator<Item = ([[f64; 3]; 4], [f64; 3])> + 'a {
    fields.iter().flat_map(|f| {
        f.quads.iter().flat_map(move |chunk| {
            chunk.chunks_exact(f.stride).map(move |q| ([0, 1, 2, 3].map(|k| to_plate(&f.to_printer, &q[k * 4..k * 4 + 3])), to_plate(&f.to_printer, &q[16..19])))
        })
    })
}

/// A view as it drew the model: its meshes and its field parts' tiles.
fn draw_view(meshes: &[[[f64; 3]; 3]], view: &CapturedView) -> DepthMap {
    let camera = &view.camera;
    let mut map = DepthMap::new(camera.side, camera.side);
    for t in meshes {
        map.triangle_mm(camera, *t);
    }
    for (corners, middle) in tiles(&view.fields) {
        for k in 0..4 {
            map.triangle_mm(camera, [middle, corners[k], corners[(k + 1) % 4]]);
        }
    }
    map
}

/// What the app's model worker answers a [`CompareJob`] with, from what its views drew: the
/// answer's text and its pictures, none more than `max_side` pixels a side (the chat's limit).
pub fn answer(job: &CompareJob, views: &[CapturedView], max_side: u32) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    if views.len() != job.views.len() {
        return Err(format!("the compare asked for {} views and {} came back", job.views.len(), views.len()));
    }
    let meshes = mesh_triangles(job);
    if job.fit {
        return fit_answer(job, &meshes, views, max_side);
    }
    let view = &views[0];
    let map = draw_view(&meshes, view);
    compare_answer(job, &view.camera, &map, max_side)
}

/// A compare's numbers, the reference's pixels its unit.
#[derive(Clone, Debug)]
pub struct Comparison {
    /// The silhouettes' intersection over their union, in the region compared.
    pub iou: f64,
    /// mm a reference pixel spans on the model, aligned.
    pub mm_per_px: f64,
    /// The reference figure's height, pixels.
    pub figure: f64,
    /// The height bands compared, those that differ most first.
    pub bands: Vec<BandRow>,
    /// The picture's points, as plate points.
    pub points: Vec<(String, PointAt)>,
    /// With `align` points: how far each plate point lands from its picture point, aligned, mm.
    pub align_off: Vec<(String, f64)>,
}

/// One height band of [`Comparison::bands`], pixels.
#[derive(Clone, Copy, Debug)]
pub struct BandRow {
    /// From the top, from 1.
    pub band: usize,
    pub reference: f64,
    pub model: f64,
    /// The model's middle less the reference's (+: to the picture's right); None: one has none.
    pub offset: Option<f64>,
    differs: f64,
}

/// A picture's point on the model.
#[derive(Clone, Copy, Debug)]
pub struct PointAt {
    /// Plate mm.
    pub at: [f64; 3],
    /// On the model's surface; else on the view plane.
    pub surface: bool,
    /// Off the view.
    pub outside: bool,
}

/// The reference against one view of the model ([`answer`]'s numbers).
pub fn measure(job: &CompareJob, views: &[CapturedView]) -> Result<Comparison, String> {
    let map = draw_view(&mesh_triangles(job), &views[0]);
    Ok(measure_map(job, &views[0].camera, &map)?.0)
}

/// The model's view laid on the reference, the numbers, and the view's own solid silhouette.
fn measure_map(job: &CompareJob, camera: &ViewCamera, map: &DepthMap) -> Result<(Comparison, Align, Vec<bool>, Vec<bool>), String> {
    let r = &job.reference;
    let (w, h) = (r.width, r.height);
    let reference = Extent::of(w, h, |x, y| r.mask[y * w + x]).expect("a reference has a figure");
    let silhouette = match job.solids.is_empty() {
        true => solid(map, None),
        false => solid(&only(map, camera, &job.solids), Some(map)),
    };
    let Some(model) = Extent::of(map.width, map.height, |x, y| silhouette[y * map.width + x]) else {
        return Err(format!("nothing of {} shows in the view {}", shown_names(&job.solids), job.views[0]));
    };
    let (align, align_off) = align_of(job, camera, map, reference, model)?;
    let shown = aligned(&silhouette, (map.width, map.height), &align, w, h);
    let region = [0, 0, w, h];
    let iou = iou(&r.mask, &shown, w, region);
    let mut rows: Vec<BandRow> = bands(&r.mask, &shown, w, reference, region)
        .iter()
        .enumerate()
        .filter_map(|(b, band)| {
            let band = band.as_ref()?;
            let offset = band.offset();
            let differs = (band.model.width - band.reference.width).abs() + offset.map_or(band.model.width.max(band.reference.width), f64::abs);
            Some(BandRow { band: b + 1, reference: band.reference.width, model: band.model.width, offset, differs })
        })
        .collect();
    rows.sort_by(|a, b| b.differs.total_cmp(&a.differs));
    let points = job.points.iter().map(|(name, p)| (name.clone(), point(camera, map, &align, r.pixel(*p)))).collect();
    let mm_per_px = camera.mm_per_pixel(camera.middle_depth()) / align.scale;
    let align_off = align_off.into_iter().map(|(name, px)| (name, px * mm_per_px)).collect();
    Ok((Comparison { iou, mm_per_px, figure: reference.height(), bands: rows, points, align_off }, align, shown, silhouette))
}

/// The solids named, or "the model".
fn shown_names(solids: &[SolidSkin]) -> String {
    match solids {
        [] => "the model".to_string(),
        _ => solids.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", "),
    }
}

/// What of a view's drawing shows the solids: its pixels whose surface point is on one of them
/// (within [`SOLID_PX`] of the view's pixels and [`SOLID_MM`]).
fn only(map: &DepthMap, camera: &ViewCamera, solids: &[SolidSkin]) -> DepthMap {
    let mut out = DepthMap::new(map.width, map.height);
    for y in 0..map.height {
        for x in 0..map.width {
            let Some(d) = map.depth_at(x, y) else { continue };
            let depth = d as f64;
            let p = camera.point_at(x as f32 + 0.5, y as f32 + 0.5, depth);
            let near = SOLID_PX * camera.mm_per_pixel(depth) + SOLID_MM;
            if solids.iter().any(|s| s.distance(p) <= near) {
                let i = y * map.width + x;
                out.depth[i] = d;
                out.normal[i] = map.normal[i];
            }
        }
    }
    out
}

/// The model's view laid on the reference as the job asks: the whole figures (the model's
/// `model`), a band of the reference against solids, or points; with points, how far each
/// lands from its picture point, in reference pixels.
fn align_of(job: &CompareJob, camera: &ViewCamera, map: &DepthMap, reference: Extent, model: Extent) -> Result<(Align, Vec<(String, f64)>), String> {
    let r = &job.reference;
    match job.align.as_deref() {
        None => Ok((Align::of(reference, model), Vec::new())),
        Some(AlignBy::Band { band, solids }) => {
            let (top, bottom) = (reference.top + band[0] * reference.height(), reference.top + band[1] * reference.height());
            let rows = Extent::of(r.width, r.height, |x, y| (top..bottom).contains(&(y as f64 + 0.5)) && r.mask[y * r.width + x]).ok_or("the 'align' band holds none of the reference figure")?;
            let theirs = solid(&only(map, camera, solids), Some(map));
            let them = Extent::of(map.width, map.height, |x, y| theirs[y * map.width + x]).ok_or_else(|| format!("nothing of {} shows in the view {} to align on", shown_names(solids), job.views[0]))?;
            Ok((Align::of(rows, them), Vec::new()))
        }
        Some(AlignBy::Points(points)) => {
            let mut pairs = Vec::new();
            for (name, picture, plate) in points {
                let q = camera.view_point(*plate).ok_or_else(|| format!("align point '{name}' is not in front of the view's camera"))?;
                pairs.push((r.pixel(*picture), [q[0] as f64, q[1] as f64]));
            }
            let align = Align::through(&pairs).ok_or("the 'align' plate points show as one point in the view: give points apart (the crown and the chin)")?;
            let off = points.iter().zip(&pairs).map(|((name, _, _), (p, m))| {
                let q = align.to_reference(*m);
                (name.clone(), ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt())
            });
            Ok((align, off.collect()))
        }
    }
}

/// How the model was laid on the reference, in words.
fn aligned_how(job: &CompareJob) -> String {
    let what = match job.solids.as_slice() {
        [] => "the model".to_string(),
        solids => format!("what the view shows of {}", shown_names(solids)),
    };
    match job.align.as_deref() {
        None => format!("{what} scaled and moved so its height and vertical middle match the reference figure's, and its centre of area across"),
        Some(AlignBy::Band { band, solids }) => format!("{what} scaled and moved so what the view shows of {} matches the reference figure's rows {:.3} to {:.3} of its height (from its top): heights, vertical middles and centres of area across", shown_names(solids), band[0], band[1]),
        Some(AlignBy::Points(points)) => format!("{what} scaled and moved so the plate points {} land on their picture points as near as they can (least squares, no turn)", points.iter().map(|p| p.0.as_str()).collect::<Vec<_>>().join(", ")),
    }
}

fn compare_answer(job: &CompareJob, camera: &ViewCamera, map: &DepthMap, max_side: u32) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    let r = &job.reference;
    let view_name = &job.views[0];
    let (c, align, shown, silhouette) = measure_map(job, camera, map)?;
    let share = |px: f64| n(px / c.figure);
    let length = |px: f64| mm(px * c.mm_per_px);
    let rows = c.bands.iter().map(|b| {
        obj(vec![
            ("band", int(b.band)),
            ("from", n((b.band - 1) as f64 / BANDS as f64)),
            ("to", n(b.band as f64 / BANDS as f64)),
            ("reference", obj(vec![("share", share(b.reference)), ("mm", length(b.reference))])),
            ("model", obj(vec![("share", share(b.model)), ("mm", length(b.model))])),
            ("offset", b.offset.map_or(S::Nil, |o| obj(vec![("share", share(o)), ("mm", length(o))]))),
        ])
    });
    let points: Vec<(&str, S)> = c
        .points
        .iter()
        .map(|(name, p)| {
            let mut fields = vec![("at", mm3(p.at)), ("on", s(if p.surface { "surface" } else { "view plane" }))];
            if p.outside {
                fields.push(("outside", flag(true)));
            }
            (name.as_str(), obj(fields))
        })
        .collect();
    let notes = notes(r);
    let mut fields = vec![
        ("picture", s(&r.name)),
        ("view", s(view_name)),
        ("iou", n(c.iou)),
        ("aligned", obj(vec![("how", s(aligned_how(job))), ("scale", s(format!("{:.4}mm per reference pixel", c.mm_per_px))), ("scale_holds", s("at the middle of what the view frames: in perspective nearer parts look larger (a narrow fov, as a photo's long lens, keeps them close)")), ("height", length(c.figure))])),
        ("background", background(r)),
        ("bands", arr(rows)),
        ("bands_are", s("16 bands of the reference figure's height from its top (from, to: shares of it); widths and the model's middle less the reference's (offset, + to the picture's right) as shares of the figure's height and in mm; the bands that differ most first")),
    ];
    if let Some([x0, y0, x1, y1]) = r.rect {
        fields.push(("rect", arr([x0, y0, x1, y1].map(n))));
    }
    if !job.solids.is_empty() {
        fields.push(("solids", arr(job.solids.iter().map(|sk| s(&sk.id)))));
        fields.push(("solids_are", s("the model's silhouette is what the view shows of these solids: its pixels whose surface is theirs; the rest of the model is left out (but hides what it is in front of)")));
    }
    if !c.align_off.is_empty() {
        fields.push(("align_off", obj(c.align_off.iter().map(|(name, off)| (name.as_str(), mm(*off))).collect())));
        fields.push(("align_off_are", s("how far each plate point lands from its picture point once aligned, mm: large, and the points do not match the view or each other")));
    }
    if !points.is_empty() {
        fields.push(("points", obj(points)));
        fields.push(("points_are", s("plate mm: on the surface, where the view's ray through the spot first meets the model; on the view plane, where it crosses the plane through the middle of what the view frames, square to the view (the ray misses the model)")));
    }
    if !notes.is_empty() {
        fields.push(("notes", arr(notes.into_iter().map(s))));
    }
    let images = sheet(r, &shown, |x, y| shade(map, &silhouette, camera, &align, x, y), &format!("fab.compare of {} at {view_name}", r.name), max_side)?;
    Ok((crate::out::text(&obj(fields)), images))
}

fn background(r: &Reference) -> S {
    obj(vec![("colour", r.background.map_or(s("transparent"), |b| arr(b.map(|c| int(c))))), ("tolerance", n(r.tolerance))])
}

fn notes(r: &Reference) -> Vec<String> {
    let mut notes = Vec::new();
    if r.rect.is_some() {
        notes.push("the picture was cut to 'rect' before its figure was read: the background is read at the cut's edges, and the figure, the IoU and the bands are the cut's".to_string());
    }
    if !r.dropped.is_empty() {
        let figure = r.mask.iter().filter(|&&m| m).count();
        let left: usize = r.dropped.iter().map(|(_, px)| px).sum();
        let at: Vec<String> = r.dropped.iter().take(4).map(|(b, _)| format!("[{:.2} {:.2} {:.2} {:.2}]", b[0], b[1], b[2], b[3])).collect();
        notes.push(format!("the figure is its largest piece: {} smaller one(s) were left out ({:.1}% of the figure's size), at {} (picture fractions); 'islands': true keeps them", r.dropped.len(), left as f64 / figure.max(1) as f64 * 100.0, at.join(" ")));
    }
    if r.holes.0 > 0 {
        let outline = r.mask.iter().filter(|&&m| m).count() + r.holes.1;
        notes.push(format!("{} piece(s) of the background colour the figure encloses ({:.1}% of its outline) read as seen through it, background (a window, an opening); 'holes': false keeps them as figure", r.holes.0, r.holes.1 as f64 / outline.max(1) as f64 * 100.0));
    }
    if r.shadow >= 0.001 {
        notes.push(format!("{:.1}% of what was read is the background's shadow (its colour darker, with no edge in it) and was left out of the figure; 'shadows': false keeps it", r.shadow * 100.0));
    }
    // a card in a frame reads as one big block: most of the picture, and filling its own box
    if let Some(e) = Extent::of(r.width, r.height, |x, y| r.mask[y * r.width + x]) {
        let (x0, x1) = ((0..r.width).find(|&x| (0..r.height).any(|y| r.mask[y * r.width + x])), (0..r.width).rev().find(|&x| (0..r.height).any(|y| r.mask[y * r.width + x])));
        if let (Some(x0), Some(x1)) = (x0, x1) {
            let (across, down) = ((x1 + 1 - x0) as f64, e.height());
            let filled = r.mask.iter().filter(|&&m| m).count() as f64 / (across * down);
            if filled > 0.9 && across > 0.6 * r.width as f64 && down > 0.6 * r.height as f64 {
                notes.push("the figure is one block filling most of the picture: if the picture is a card in a frame (a screenshot), cut to the card with 'rect' (a little inside its edges); the background is then read at the cut's edges".to_string());
            }
        }
    }
    if r.border < 0.5 {
        notes.push(format!("only {:.0}% of the picture's border is its background colour: the figure may run off the picture or the background is not one colour; check the silhouette picture (give 'background' a larger tolerance for a shaded backdrop)", r.border * 100.0));
    }
    notes
}

/// The model's silhouette laid on the reference's pixels.
fn aligned(model: &[bool], (mw, mh): (usize, usize), align: &Align, w: usize, h: usize) -> Vec<bool> {
    let mut out = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let [mx, my] = align.to_model([x as f64 + 0.5, y as f64 + 0.5]);
            out[y * w + x] = mx >= 0.0 && my >= 0.0 && (mx as usize) < mw && (my as usize) < mh && model[my as usize * mw + mx as usize];
        }
    }
    out
}

fn iou(a: &[bool], b: &[bool], w: usize, [x0, y0, x1, y1]: [usize; 4]) -> f64 {
    let (mut both, mut either) = (0usize, 0usize);
    for y in y0..y1 {
        for x in x0..x1 {
            let (p, q) = (a[y * w + x], b[y * w + x]);
            both += (p && q) as usize;
            either += (p || q) as usize;
        }
    }
    if either == 0 { 0.0 } else { both as f64 / either as f64 }
}

/// A band's rows of one silhouette: its mean width and middle across (pixels).
#[derive(Clone, Copy, Debug, Default)]
struct Rows {
    width: f64,
    middle: f64,
    rows: usize,
}

#[derive(Clone, Copy, Debug)]
struct Band {
    reference: Rows,
    model: Rows,
}

impl Band {
    fn offset(&self) -> Option<f64> {
        (self.reference.rows > 0 && self.model.rows > 0).then(|| self.model.middle - self.reference.middle)
    }
}

/// The two silhouettes in [`BANDS`] bands of the reference figure's height, within `region`; a band
/// outside it, or empty in both, is None.
fn bands(reference: &[bool], model: &[bool], w: usize, figure: Extent, [x0, y0, x1, y1]: [usize; 4]) -> Vec<Option<Band>> {
    let row = |mask: &[bool], y: usize| -> Option<(f64, f64)> {
        let first = (x0..x1).find(|&x| mask[y * w + x])?;
        let last = (x0..x1).rev().find(|&x| mask[y * w + x])?;
        Some(((last + 1 - first) as f64, (first + last + 1) as f64 * 0.5))
    };
    (0..BANDS)
        .map(|b| {
            let top = (figure.top + figure.height() * b as f64 / BANDS as f64).round() as usize;
            let bottom = (figure.top + figure.height() * (b + 1) as f64 / BANDS as f64).round() as usize;
            let (top, bottom) = (top.max(y0), bottom.min(y1));
            if top >= bottom {
                return None;
            }
            let sum = |mask: &[bool]| {
                let mut r = Rows::default();
                for y in top..bottom {
                    if let Some((width, middle)) = row(mask, y) {
                        r.width += width;
                        r.middle += middle;
                        r.rows += 1;
                    }
                }
                if r.rows > 0 {
                    r.width /= r.rows as f64;
                    r.middle /= r.rows as f64;
                }
                r
            };
            let band = Band { reference: sum(reference), model: sum(model) };
            (band.reference.rows > 0 || band.model.rows > 0).then_some(band)
        })
        .collect()
}

/// A reference spot (its pixels) as a plate point: where the view's ray through it first meets
/// the model, else where it crosses the view plane.
fn point(camera: &ViewCamera, map: &DepthMap, align: &Align, p: [f64; 2]) -> PointAt {
    let [mx, my] = align.to_model(p);
    let inside = mx >= 0.0 && my >= 0.0 && mx < map.width as f64 && my < map.height as f64;
    // on the surface, or in a crack between tiles: the nearest drawn within its width
    let hit = if inside {
        let r = CRACK_PX as isize;
        (-r..=r).flat_map(|dy| (-r..=r).map(move |dx| (dx, dy))).filter_map(|(dx, dy)| {
            let (x, y) = (mx as isize + dx, my as isize + dy);
            (x >= 0 && y >= 0).then(|| map.depth_at(x as usize, y as usize)).flatten().map(|d| (dx * dx + dy * dy, d))
        }).min_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))).map(|(_, d)| d)
    } else {
        None
    };
    let depth = hit.map_or(camera.middle_depth(), f64::from);
    PointAt { at: camera.point_at(mx as f32, my as f32, depth), surface: hit.is_some(), outside: !inside }
}

/// The model's shade at a reference pixel: lit from the camera, by the facing of what the view
/// drew there, flat where its silhouette holds what the drawing left; None where nothing shows.
fn shade(map: &DepthMap, silhouette: &[bool], camera: &ViewCamera, align: &Align, x: usize, y: usize) -> Option<f32> {
    let [mx, my] = align.to_model([x as f64 + 0.5, y as f64 + 0.5]);
    if mx < 0.0 || my < 0.0 || mx as usize >= map.width || my as usize >= map.height || !silhouette[my as usize * map.width + mx as usize] {
        return None;
    }
    if !map.covered(mx as usize, my as usize) {
        return Some(0.7);
    }
    let normal = map.normal[my as usize * map.width + mx as usize];
    let (_, dir) = camera.ray_mm(mx as f32, my as f32);
    let facing = (normal[0] as f64 * dir[0] + normal[1] as f64 * dir[1] + normal[2] as f64 * dir[2]).abs() as f32;
    Some(if normal == [0.0; 3] { 0.7 } else { 0.25 + 0.75 * facing })
}

/// The pictures: the reference, the model (shaded, aligned) and the overlay (the reference faded,
/// its outline orange, the model's cyan) side by side; then the reference's silhouette. None
/// more than `max_side` pixels a side.
fn sheet(r: &Reference, shown: &[bool], shade: impl Fn(usize, usize) -> Option<f32>, title: &str, max_side: u32) -> Result<Vec<(String, Vec<u8>)>, String> {
    let (w, h) = (r.width, r.height);
    let mut rgba = vec![0u8; w * 3 * h * 4];
    let thick = (w.max(h) / 400).max(1) as isize;
    let edge = |mask: &[bool], x: usize, y: usize| -> bool {
        if !mask[y * w + x] {
            return false;
        }
        (-thick..=thick).any(|dy| {
            (-thick..=thick).any(|dx| {
                let (nx, ny) = (x as isize + dx, y as isize + dy);
                nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize || !mask[ny as usize * w + nx as usize]
            })
        })
    };
    for y in 0..h {
        for x in 0..w {
            let src = &r.rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
            let a = src[3] as f32 / 255.0;
            let over = |c: u8, bg: f32| (c as f32 * a + bg * (1.0 - a)) as u8;
            let picture = [over(src[0], 255.0), over(src[1], 255.0), over(src[2], 255.0)];
            let model = match shade(x, y) {
                Some(v) => [(v * 205.0) as u8, (v * 215.0) as u8, (v * 230.0) as u8],
                None => [20, 23, 28],
            };
            let faded = picture.map(|c| (c as f32 * 0.35 + 255.0 * 0.65) as u8);
            let overlay = match (edge(&r.mask, x, y), edge(shown, x, y)) {
                (_, true) => [0, 190, 255],
                (true, false) => [255, 130, 0],
                (false, false) if shown[y * w + x] => faded.map(|c| (c as f32 * 0.85) as u8),
                _ => faded,
            };
            for (panel, c) in [picture, model, overlay].into_iter().enumerate() {
                let at = (y * w * 3 + panel * w + x) * 4;
                rgba[at..at + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
    }
    let mask: Vec<u8> = r.mask.iter().flat_map(|&m| if m { [255, 255, 255, 255] } else { [0, 0, 0, 255] }).collect();
    let pictures = [(RgbaPicture { width: (w * 3) as u32, height: h as u32, rgba }, [3, 1]), (RgbaPicture { width: w as u32, height: h as u32, rgba: mask }, [1, 1])];
    let pngs = fit_pictures(&pictures, MAX_RESULT_IMAGE_BYTES, max_side, |_, _, _| {})?;
    let [cell, mask_cell] = [pngs[0].1, pngs[1].1];
    Ok(vec![
        (format!("{title}: the reference, the model (shaded, laid on it) and the overlay (the reference faded, its outline orange, the model's cyan); each {} x {} px", cell[0], cell[1]), pngs[0].0.clone()),
        (format!("the reference's figure as read (white), {} x {} px", mask_cell[0], mask_cell[1]), pngs[1].0.clone()),
    ])
}

/// Samples of the model's surface about `cell` mm apart: its meshes' and the survey's tiles',
/// only those on `solids` when there are any.
fn samples(meshes: &[[[f64; 3]; 3]], views: &[CapturedView], solids: &[SolidSkin]) -> Sampled {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut grow = |p: [f64; 3]| {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    };
    for t in meshes {
        t.iter().for_each(|p| grow(*p));
    }
    for v in views {
        for (_, middle) in tiles(&v.fields) {
            grow(middle);
        }
    }
    let size = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max);
    let cell = (size * FIT_SAMPLES).max(1e-3);
    let mut seen: HashSet<[i64; 3]> = HashSet::new();
    let mut out = Vec::new();
    let mut take = |p: [f64; 3]| {
        if seen.insert(p.map(|c| (c / cell).floor() as i64)) {
            out.push(p);
        }
    };
    for [a, b, c] in meshes {
        let longest = [(a, b), (b, c), (c, a)].iter().map(|(p, q)| ((0..3).map(|k| (p[k] - q[k]).powi(2)).sum::<f64>()).sqrt()).fold(0.0, f64::max);
        let k = (longest / cell).ceil().max(1.0) as usize;
        for i in 0..=k {
            for j in 0..=k - i {
                let (u, v) = (i as f64 / k as f64, j as f64 / k as f64);
                take(std::array::from_fn(|d| a[d] + (b[d] - a[d]) * u + (c[d] - a[d]) * v));
            }
        }
    }
    for v in views {
        for (_, middle) in tiles(&v.fields) {
            take(middle);
        }
    }
    let theirs = (!solids.is_empty()).then(|| out.iter().copied().filter(|p| solids.iter().any(|s| s.distance(*p) <= cell + SOLID_MM)).collect());
    Sampled { all: out, theirs, cell }
}

/// The fit's samples of the model ([`samples`]): all of it, and of the solids its silhouette is
/// cut to (None: all of it), about `cell` apart.
struct Sampled {
    all: Vec<[f64; 3]>,
    theirs: Option<Vec<[f64; 3]>>,
    cell: f64,
}

impl Sampled {
    /// The samples the silhouette is of.
    fn shown(&self) -> &[[f64; 3]] {
        self.theirs.as_deref().unwrap_or(&self.all)
    }

    /// Their solid silhouette ([`solid`]) seen from `camera`, its side square.
    fn silhouette(&self, camera: &ViewCamera) -> Vec<bool> {
        let whole = self.theirs.as_ref().map(|_| splat(&self.all, self.cell, camera));
        solid(&splat(self.shown(), self.cell, camera), whole.as_ref())
    }
}

/// The samples seen from `camera`: each a disc about as wide as the samples are apart.
fn splat(samples: &[[f64; 3]], cell: f64, camera: &ViewCamera) -> DepthMap {
    let mut map = DepthMap::new(camera.side, camera.side);
    let radius = (0.75 * cell / camera.mm_per_pixel(camera.middle_depth())).max(0.6) as f32;
    for p in samples {
        if let Some(q) = camera.view_point(*p) {
            map.disc(q, radius, [0.0; 3]);
        }
    }
    map
}

/// The reference's figure at a fit's size: its pixels' middles, at most `side` on its longest side.
fn small_mask(r: &Reference, side: usize) -> (usize, usize, Vec<bool>) {
    let k = (r.width.max(r.height) as f64 / side as f64).max(1.0);
    let (w, h) = (((r.width as f64 / k).round() as usize).max(1), ((r.height as f64 / k).round() as usize).max(1));
    let mask = (0..w * h).map(|i| {
        let (x, y) = (((i % w) as f64 + 0.5) * k, ((i / w) as f64 + 0.5) * k);
        r.mask[(y as usize).min(r.height - 1) * r.width + (x as usize).min(r.width - 1)]
    }).collect();
    (w, h, mask)
}

/// What the views frame (glTF frame), from one of them.
fn shown(placed: &Placed) -> [[f32; 3]; 2] {
    let lo = [0, 1, 2].map(|k| placed.center[k] - [placed.half.x, placed.half.y, placed.half.z][k] / placed.scale);
    let hi = [0, 1, 2].map(|k| placed.center[k] + [placed.half.x, placed.half.y, placed.half.z][k] / placed.scale);
    [lo, hi]
}

/// A view from (yaw, pitch) degrees of what the views frame, as fab.render takes it with the
/// job's camera.
fn candidate(shown: [[f32; 3]; 2], camera: &Camera, yaw: f32, pitch: f32, side: usize) -> ViewCamera {
    let view = View { name: String::new(), yaw: yaw.to_radians(), pitch: pitch.to_radians() };
    Frame::of_render(shown, std::slice::from_ref(&view), camera).camera(&view, side)
}

/// How well the samples seen from (yaw, pitch) fit the reference's figure (`(w, h, mask)`).
fn fit_iou(samples: &Sampled, framed: ([[f32; 3]; 2], &Camera), yaw: f32, pitch: f32, side: usize, reference: &(usize, usize, Vec<bool>)) -> f64 {
    let (w, h, mask) = reference;
    let camera = candidate(framed.0, framed.1, yaw, pitch, side);
    let silhouette = samples.silhouette(&camera);
    let s = camera.side;
    let (Some(r), Some(m)) = (Extent::of(*w, *h, |x, y| mask[y * w + x]), Extent::of(s, s, |x, y| silhouette[y * s + x])) else { return 0.0 };
    iou(mask, &aligned(&silhouette, (s, s), &Align::of(r, m), *w, *h), *w, [0, 0, *w, *h])
}

/// Degrees, -180 to 180.
fn wrap(yaw: f32) -> f32 {
    (yaw + 180.0).rem_euclid(360.0) - 180.0
}

/// A fit's view, degrees.
#[derive(Clone, Debug)]
pub struct Fit {
    pub yaw: f32,
    pub pitch: f32,
    pub iou: f64,
    /// The best of another basin.
    pub runner_up: Option<(f32, f32, f64)>,
    /// The survey views whose own drawing the samples miss more than [`FIT_MISS`] of, and the
    /// share they miss.
    pub missed: Vec<(String, f64)>,
}

/// The fit's numbers ([`answer`]'s with `fit`).
pub fn fit(job: &CompareJob, views: &[CapturedView]) -> Result<Fit, String> {
    let meshes = mesh_triangles(job);
    let samples = samples(&meshes, views, &job.solids);
    search(job, &meshes, views, &samples)
}

/// The yaw and pitch whose silhouette fits the reference's best, coarse to fine.
fn search(job: &CompareJob, meshes: &[[[f64; 3]; 3]], views: &[CapturedView], samples: &Sampled) -> Result<Fit, String> {
    let r = &job.reference;
    let frame = (shown(&views[0].camera.placed), &job.camera);
    if samples.shown().is_empty() {
        return Err("nothing of the model shows in the survey's views".into());
    }
    let pitch_most = FIT_PITCH.min(PITCH_LIMIT.to_degrees() - 1.0);
    let coarse = small_mask(r, FIT_COARSE);
    let mut tried: Vec<(f32, f32, f64)> = Vec::new();
    let mut pitch = -pitch_most;
    while pitch <= pitch_most + 1e-3 {
        let mut yaw = -180.0;
        while yaw < 180.0 - 1e-3 {
            tried.push((yaw, pitch, fit_iou(samples, frame, yaw, pitch, FIT_COARSE, &coarse)));
            yaw += FIT_STEP;
        }
        pitch += FIT_STEP;
    }
    tried.sort_by(|a, b| b.2.total_cmp(&a.2));
    // the best few basins, each refined: no two within two coarse steps
    let apart = |a: (f32, f32), b: (f32, f32)| wrap(a.0 - b.0).abs() > 2.0 * FIT_STEP || (a.1 - b.1).abs() > 2.0 * FIT_STEP;
    let mut starts: Vec<(f32, f32)> = Vec::new();
    for &(yaw, pitch, _) in &tried {
        if starts.len() < 3 && starts.iter().all(|s| apart(*s, (yaw, pitch))) {
            starts.push((yaw, pitch));
        }
    }
    let fine = small_mask(r, FIT_FINE);
    let mut found: Vec<(f32, f32, f64)> = starts
        .into_iter()
        .map(|(mut yaw, mut pitch)| {
            let mut best = fit_iou(samples, frame, yaw, pitch, FIT_FINE, &fine);
            for step in [6.0f32, 3.0, 1.5, 0.75] {
                loop {
                    let mut moved = false;
                    for (dy, dp) in [(-1.0, -1.0), (0.0, -1.0), (1.0, -1.0), (-1.0, 0.0), (1.0, 0.0), (-1.0, 1.0), (0.0, 1.0), (1.0, 1.0)] {
                        let (y, p) = (wrap(yaw + dy * step), (pitch + dp * step).clamp(-pitch_most, pitch_most));
                        let score = fit_iou(samples, frame, y, p, FIT_FINE, &fine);
                        if score > best + 1e-9 {
                            (best, yaw, pitch, moved) = (score, y, p, true);
                        }
                    }
                    if !moved {
                        break;
                    }
                }
            }
            (yaw, pitch, best)
        })
        .collect();
    found.sort_by(|a, b| b.2.total_cmp(&a.2));
    // what the survey's samples miss: each survey view's own drawing against the samples seen from it
    let mut missed: Vec<(String, f64)> = Vec::new();
    for (name, v) in job.views.iter().zip(views) {
        let camera = candidate(frame.0, frame.1, v.camera.look.yaw.to_degrees(), v.camera.look.pitch.to_degrees(), FIT_FINE);
        let mut own = draw_view(meshes, &CapturedView { camera, fields: v.fields.iter().map(|f| FieldTiles { to_printer: f.to_printer, quads: f.quads.clone(), stride: f.stride }).collect() });
        if !job.solids.is_empty() {
            own = only(&own, &camera, &job.solids);
        }
        let union = splat(samples.shown(), samples.cell, &camera);
        let side = camera.side;
        let drawn = (0..side * side).filter(|&i| own.covered(i % side, i / side)).count();
        let lost = (0..side * side).filter(|&i| own.covered(i % side, i / side) && !union.covered(i % side, i / side)).count();
        let share = lost as f64 / drawn.max(1) as f64;
        if share > FIT_MISS {
            missed.push((name.clone(), share));
        }
    }
    let (yaw, pitch, iou) = found[0];
    Ok(Fit { yaw, pitch, iou, runner_up: found.get(1).copied(), missed })
}

impl Fit {
    /// The view found, as a view's name ("yaw,pitch", to a tenth of a degree).
    pub fn view(&self) -> String {
        let round = |a: f32| (a * 10.0).round() / 10.0;
        format!("{},{}", round(self.yaw), round(self.pitch))
    }
}

fn fit_answer(job: &CompareJob, meshes: &[[[f64; 3]; 3]], views: &[CapturedView], max_side: u32) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    let r = &job.reference;
    let samples = samples(meshes, views, &job.solids);
    let fit = search(job, meshes, views, &samples)?;
    let view = fit.view();
    let mut notes = notes(r);
    if !fit.missed.is_empty() {
        let list: Vec<String> = fit.missed.iter().map(|(v, a)| format!("{v} ({:.1}%)", a * 100.0)).collect();
        notes.push(format!("the fit's samples miss part of what the survey views drew of the model ({}): its silhouettes may lack a side none of them saw whole; check the fit with fab.compare at the view it found", list.join(", ")));
    }
    if let Some(next) = fit.runner_up.filter(|n| fit.iou - n.2 < 0.01) {
        notes.push(format!("{},{} fits nearly as well ({:.3}): a silhouette alone may not tell the two apart", next.0.round(), next.1.round(), next.2));
    }
    let mut fields = vec![
        ("picture", s(&r.name)),
        ("fit", obj(vec![("view", s(&view)), ("iou", n(fit.iou))])),
        ("how", s(format!("silhouette IoU after fab.compare's alignment, over yaw and pitch: a 15-degree grid, then the best three refined to under a degree; the silhouettes of {} are drawn from its meshes and from what the eight survey views traced of its field parts", shown_names(&job.solids)))),
        ("next", s(format!("fab.compare {{picture: \"{}\" view: \"{view}\"}} for the sheet, the width table and points at that view", r.name))),
        ("background", background(r)),
    ];
    if let Some(next) = fit.runner_up {
        fields.push(("runner_up", obj(vec![("view", s(format!("{},{}", next.0.round(), next.1.round()))), ("iou", n(next.2))])));
    }
    if !notes.is_empty() {
        fields.push(("notes", arr(notes.into_iter().map(s))));
    }
    // the best's sheet, from the samples, on the reference at its own size
    let camera = candidate(shown(&views[0].camera.placed), &job.camera, fit.yaw, fit.pitch, FIT_FINE * 2);
    let silhouette = samples.silhouette(&camera);
    let side = camera.side;
    let model = Extent::of(side, side, |x, y| silhouette[y * side + x]).ok_or("nothing of the model shows at the fit's view")?;
    let reference = Extent::of(r.width, r.height, |x, y| r.mask[y * r.width + x]).expect("a reference has a figure");
    let shown = aligned(&silhouette, (side, side), &Align::of(reference, model), r.width, r.height);
    let images = sheet(r, &shown, |x, y| shown[y * r.width + x].then_some(0.7), &format!("fab.compare fit of {} at {view} (the model from samples)", r.name), max_side)?;
    Ok((crate::out::text(&obj(fields)), images))
}

#[cfg(test)]
#[path = "compare_tests.rs"]
mod tests;
