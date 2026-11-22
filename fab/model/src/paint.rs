//! A script's paint: its `m.fill`, `m.facets`, `m.stroke`, `m.erase`, `m.support_block` and
//! `m.support_enforce` statements, compiled into one spatial program per emitted object
//! (`fab_paint_field`). Paint is no solid and never enters a shape's distance program: a part
//! is built the same painted or not.
//!
//! - `m.fill(region, material)`: a solid of the object (a feature: `holes`, `bores`) paints its
//!   own surface band and follows parameter edits; the object's own solid paints all of it, and
//!   a body of a jointed object all of that body's part only (a body filled whole prints in the
//!   material all through, as its own colour: [`ObjectPaint::whole`]); a solid outside every
//!   object (`m.box({..})`) is a region in the model frame.
//! - `m.fill(Flood{seed: [x y z] angle: 30deg}, material)`: the smooth surface region round a
//!   seed point, spreading while neighbouring normals stay within the angle (it stops at creases
//!   and edges); recomputed on the part's current surface at every build
//!   (`resolve_floods`).
//! - `m.fill(region, Mix{materials field})`: materials through the region's volume by a Splash
//!   fn of the point (model frame), its place in the list: whole numbers one material, fractions
//!   between two a blend the print dithers ([`fab_paint_field::Mix`]). The field may instead be
//!   a pattern grown on the part (`Turing{..}`, [`fab_paint_field::pattern`]), grown on its
//!   current surface at every build that changes it (`eval`).
//! - `m.stroke([[x y z] ...], radius, material)`: a brushed stroke, one capsule chain.
//! - `m.facets(of, materials)`: an import's painted leaves, state n in materials[n-1].
//! - `m.erase(region)`: back to the object's own material there.
//! - `m.support_block(region)`, `m.support_enforce(region)`: where supports never / always go.
//! - `m.settings(region, {sparse_infill_density: "100%" wall_loops: 4})`: print settings of a
//!   volume's own, in the values `place(..., settings: {...})` takes; only those a region can
//!   change ([`fab_plate::REGION_SETTINGS`], [`region_settings`]).
//! - `m.strokes("bracket.strokes")`: the hand-drawn strokes of a binary strokes file beside the
//!   script (`fab_paint_field::strokes`), one layer here; the last stroke holding a point
//!   decides it. The file's log is bound when the plate is evaluated ([`ObjectPaint::bind`]),
//!   so a stroke recompiles nothing.
//! - `m.paint_file("bracket.paint.splash")`: the regions and strokes of a paint file beside the
//!   script, in place (a long stroke list keeps out of the script's way).
//! - A picture part (`m.picture`, `m.colour_swatch`) paints itself first: a raster mix over all
//!   of it, made at build from the filaments the job loads ([`crate::picture`]).
//!
//! Statements apply in document order; the last one holding a point decides it.

use makepad_csg_graph::{BoolMode, Graph, NodeId, Op};
use makepad_csg_sdf::{SdfNode, SdfOp};
use makepad_model_script::lang::{self, Arg, Assets, Diag, Options, Program, Root, V};
use fab_paint_field::operand::Body;
use fab_paint_field::{Channel, Layer, Op as PaintOp, Operand, Region};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::program::{Emitted, FacetAssets, UNIT};

/// How far a feature fill reaches past the feature's own surface (mm): its surface band.
pub const FEATURE_BAND: f64 = 0.05;
/// A `Flood{seed}` without an angle: the largest angle between neighbouring normals it spreads
/// across (degrees).
pub const DEFAULT_FLOOD_ANGLE: f64 = 30.0;
/// How far a flood's seed may lie from a part's surface for the flood to start on it (mm).
pub const FLOOD_SEED_REACH: f64 = 1.5;

/// One paint material: its design colour (linear) and the print colour it names, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct Swatch {
    pub rgb: [f32; 3],
    pub name: Option<String>,
}

/// A painted body's paint (an emitted object's, or one body's of a jointed object): its program
/// (model frame, mm), the palette its colour slots index (slot `i` is `palette[i - 1]`) and the
/// region settings its settings slots index (slot `i` is `settings[i - 1]`).
#[derive(Clone, Debug)]
pub struct ObjectPaint {
    pub program: Arc<fab_paint_field::Program>,
    pub palette: Arc<Vec<Swatch>>,
    pub settings: Arc<Vec<RegionSettings>>,
}

/// The print settings an `m.settings` statement gives its region: checked values of
/// [`fab_plate::REGION_SETTINGS`] keys, in key order.
pub type RegionSettings = Vec<(String, String)>;

impl ObjectPaint {
    pub fn colors(&self) -> bool {
        self.program.has(Channel::Color)
    }

    /// The material a body is filled with whole (`m.fill(body, material)`), which it then prints
    /// in all through as its own, and the paint over it after that fill.
    pub fn whole(&self) -> Option<(Swatch, ObjectPaint)> {
        let (slot, rest) = self.program.whole()?;
        let w = self.palette.get(usize::from(slot).checked_sub(1)?)?.clone();
        Some((w, ObjectPaint { program: Arc::new(rest), palette: self.palette.clone(), settings: self.settings.clone() }))
    }

    /// The paint with its strokes files' logs bound (`files`), their palettes added to the
    /// script's (an equal material shares its slot). A file the project lacks paints nothing.
    pub fn bind(&self, files: &crate::StrokeFiles) -> ObjectPaint {
        if !self.program.has_strokes() {
            return self.clone();
        }
        let mut palette = (*self.palette).clone();
        let program = self.program.with_strokes(&mut |file, channel| {
            let log = files.get(file)?.clone();
            // a support file paints its channel on wherever a paint stroke holds
            if channel != Channel::Color {
                return Some((log.clone(), Arc::new(vec![1; log.palette.len()])));
            }
            let slots = log
                .palette
                .iter()
                .map(|s| {
                    let w = Swatch { rgb: s.rgb, name: None };
                    let i = palette.iter().position(|p| *p == w).unwrap_or_else(|| {
                        palette.push(w);
                        palette.len() - 1
                    });
                    (i + 1).min(255) as u8
                })
                .collect();
            Some((log, Arc::new(slots)))
        });
        ObjectPaint { program: Arc::new(program), palette: Arc::new(palette), settings: self.settings.clone() }
    }
}

/// The paint statements of a compiled program, per emitted object name.
/// The parts a paint layer is limited to: an object's (each of its bodies), or one body's.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Only {
    /// An emitted object, by index.
    Object(usize),
    /// A body, by solid.
    Body(usize),
}

/// Each emitted body's paint, by its solid (an unjointed object's one body is its solid); none
/// for an unpainted one.
pub fn compile(program: &Program, objects: &[Emitted], assets: &Assets, facets: &FacetAssets, pictures: &crate::picture::Pictures, files: &dyn Fn(&str) -> Option<String>) -> Result<HashMap<usize, ObjectPaint>, Vec<Diag>> {
    let mut palette: Vec<Swatch> = Vec::new();
    let mut settings: Vec<RegionSettings> = Vec::new();
    // (layer, the parts it is limited to)
    let mut layers: Vec<(Layer, Option<Only>)> = Vec::new();
    let mut errors = Vec::new();
    for body in objects.iter().flat_map(|o| o.bodies.iter().copied()) {
        let solid = &program.solids[body];
        let Some(arg) = &solid.picture else { continue };
        match picture_layer(program, body, arg, pictures) {
            Ok(layer) => layers.push((layer, Some(Only::Body(body)))),
            Err(message) => errors.push(diag(solid.loc, format!("m.{}: {message}", if arg.image.is_some() { "picture" } else { "colour_swatch" }))),
        }
    }
    statements(program, objects, &Bodies::default(), assets, facets, files, true, &mut palette, &mut settings, &mut layers, &mut errors);
    if !errors.is_empty() {
        return Err(errors);
    }
    if layers.is_empty() {
        return Ok(HashMap::new());
    }
    let (palette, settings) = (Arc::new(palette), Arc::new(settings));
    // Bodies under the same layers share their program: a program measures each field layer as it
    // is made and holds it until it goes, so a plate of many bodies paid for every layer per body.
    let mut programs: HashMap<Vec<usize>, Arc<fab_paint_field::Program>> = HashMap::new();
    Ok(objects
        .iter()
        .enumerate()
        .flat_map(|(oi, o)| o.bodies.iter().map(move |b| (oi, *b)))
        .map(|(oi, body)| {
            let own: Vec<usize> = layers
                .iter()
                .enumerate()
                .filter(|(_, (_, only))| match only {
                    None => true,
                    Some(Only::Object(k)) => *k == oi,
                    Some(Only::Body(k)) => *k == body,
                })
                .map(|(i, _)| i)
                .collect();
            let program = programs.entry(own).or_insert_with_key(|own| Arc::new(fab_paint_field::Program::new(own.iter().map(|&i| layers[i].0.clone()).collect()))).clone();
            (body, ObjectPaint { program, palette: palette.clone(), settings: settings.clone() })
        })
        .filter(|(_, p)| !p.program.is_empty())
        .collect())
}

/// A program's paint statements in order; a paint file's at its `m.paint_file` (one level: a
/// paint file reads no other).
#[allow(clippy::too_many_arguments)]
fn statements(program: &Program, objects: &[Emitted], bodies: &Bodies, assets: &Assets, facets: &FacetAssets, files: &dyn Fn(&str) -> Option<String>, top: bool, palette: &mut Vec<Swatch>, settings: &mut Vec<RegionSettings>, layers: &mut Vec<(Layer, Option<Only>)>, errors: &mut Vec<Diag>) {
    for root in &program.roots {
        let r = match root.kind {
            "fill" => region(program, objects, bodies, root, "region").and_then(|(region, only)| {
                let op = match root.get("material") {
                    Some(Arg::Mix(m)) => mix(m, palette)?,
                    _ => PaintOp::Paint { channel: Channel::Color, slot: swatch(material(root), palette)? },
                };
                Ok(vec![(Layer { region, op }, only)])
            }),
            "stroke" => stroke(root).and_then(|region| {
                let slot = swatch(material(root), palette)?;
                Ok(vec![(Layer { region, op: PaintOp::Paint { channel: Channel::Color, slot } }, None)])
            }),
            "facets" => facet_layers(program, objects, facets, root, palette),
            "strokes" => match root.get("file") {
                Some(Arg::Value(V::Str(f))) if crate::program::is_strokes_file(f) => {
                    let region = Region::Strokes { file: f.clone(), strokes: Default::default(), slots: Default::default() };
                    Ok(vec![(Layer { region, op: PaintOp::Paint { channel: Channel::Color, slot: 0 } }, None)])
                }
                _ => Err(format!("give it a strokes file: m.strokes(\"name.{}\")", crate::program::STROKES_EXTENSION)),
            },
            "erase" => region(program, objects, bodies, root, "region").map(|(region, only)| vec![(Layer { region, op: PaintOp::Erase { channel: Channel::Color } }, only)]),
            "support_block" => support_region(program, objects, bodies, root).map(|(region, only)| vec![(Layer { region, op: PaintOp::Paint { channel: Channel::SupportBlock, slot: 1 } }, only)]),
            "support_enforce" => support_region(program, objects, bodies, root).map(|(region, only)| vec![(Layer { region, op: PaintOp::Paint { channel: Channel::SupportEnforce, slot: 1 } }, only)]),
            "settings" => settings_volume(program, objects, root).and_then(|(region, only)| {
                let slot = settings_slot(region_settings(root)?, settings)?;
                Ok(vec![(Layer { region, op: PaintOp::Paint { channel: Channel::Settings, slot } }, only)])
            }),
            // A volume, never brushed: no strokes file.
            "no_trees" => region(program, objects, bodies, root, "region").map(|(region, only)| vec![(Layer { region, op: PaintOp::Paint { channel: Channel::NoTrees, slot: 1 } }, only)]),
            "paint_file" if !top => Err("a paint file reads no other paint file".into()),
            "paint_file" => match root.get("file") {
                Some(Arg::Value(V::Str(f))) if crate::program::is_paint_file(f) => match files(f) {
                    Some(text) => match lang::evaluate(&text, &Options { generated: &Default::default(), assets, images: &Default::default(), extension: "fab", libraries: &[], inputs: &lang::NO_INPUTS, unit: UNIT, roots: crate::program::PAINT_ROOTS, overrides: Default::default(), weave: None, solved: Default::default() }) {
                        Ok(p) => {
                            let mut inner = Vec::new();
                            statements(&p, &[], &Bodies::default(), assets, facets, files, false, palette, settings, layers, &mut inner);
                            match inner.first() {
                                None => continue,
                                Some(d) => Err(format!("{f}:{}:{}: {}", d.line, d.col, d.message)),
                            }
                        }
                        Err(d) => Err(format!("{f}:{}:{}: {}", d[0].line, d[0].col, d[0].message)),
                    },
                    None => Err(format!("no file '{f}' in the project")),
                },
                _ => Err(format!("give it a paint file: m.paint_file(\"name.{}\")", crate::program::PAINT_EXTENSION)),
            },
            _ => continue,
        };
        match r {
            Ok(l) => layers.extend(l),
            Err(message) => errors.push(diag(root.loc, format!("m.{}: {message}", root.kind))),
        }
    }
}

fn diag(loc: Option<(u32, u32)>, message: String) -> Diag {
    let (line, col) = loc.unwrap_or((1, 1));
    Diag { line, col, message }
}

fn material(root: &Root) -> Option<&V> {
    match root.get("material") {
        Some(Arg::Value(v)) => Some(v),
        _ => None,
    }
}

/// The material a statement paints in: its palette slot (an equal material shares one).
fn swatch(material: Option<&V>, palette: &mut Vec<Swatch>) -> Result<u8, String> {
    let Some(V::Obj(o)) = material else {
        return Err("give it a Material: Material{albedo: [0.8 0.05 0.05]}".into());
    };
    if o.proto.as_deref() != Some("Material") {
        return Err(format!("the material is a Material{{albedo: [r g b]}}, not {}", V::Obj(o.clone()).describe()));
    }
    let rgb = match o.get("albedo") {
        Some(V::Arr(a)) if a.len() == 3 && a.iter().all(|v| v.num().is_some()) => [a[0].num().unwrap() as f32, a[1].num().unwrap() as f32, a[2].num().unwrap() as f32],
        Some(V::Color(c)) => {
            let l = crate::program::color_linear(*c);
            [l[0], l[1], l[2]]
        }
        _ => return Err("the Material needs an albedo: [r g b] (0..1, linear) or #rrggbb".into()),
    };
    let name = match o.get("name") {
        None | Some(V::Nil) => None,
        Some(V::Str(n)) if !n.trim().is_empty() => Some(n.trim().to_string()),
        Some(v) => return Err(format!("a Material's name is the print colour it prints as, as text, not {}", v.describe())),
    };
    let s = Swatch { rgb, name };
    let i = match palette.iter().position(|p| *p == s) {
        Some(i) => i,
        None => {
            if palette.len() >= 254 {
                return Err("a script paints in at most 254 materials".into());
            }
            palette.push(s);
            palette.len() - 1
        }
    };
    Ok(i as u8 + 1)
}

/// A picture part's paint: a raster over all of it, its slots the filaments the build gives.
fn picture_layer(p: &Program, body: usize, arg: &lang::PictureArg, pictures: &crate::picture::Pictures) -> Result<Layer, String> {
    // the box's middle: the placements' moves over it (a picture lies flat, unturned)
    let mut centre = [0.0; 3];
    let mut node = p.solids[body].node;
    loop {
        match &p.graph.node(node).op {
            Op::Box { .. } => break,
            Op::Transform { source, at, rotate, scale } => {
                if rotate.iter().any(|r| *r != 0.0) || scale.iter().any(|s| *s != 1.0) {
                    return Err("a picture lies flat on the bed as it is made: place it with 'at' only, never turned or scaled".into());
                }
                for k in 0..3 {
                    centre[k] += at[k];
                }
                node = *source;
            }
            other => return Err(format!("a picture prints as its own part: it passes through {}", other.name())),
        }
    }
    let image = match &arg.image {
        Some(name) => Some((pictures.get(name).cloned().ok_or_else(|| format!("no picture '{name}' beside the script"))?, name.as_str())),
        None => None,
    };
    let mut first_guess = Vec::new();
    for (i, v) in arg.filaments.iter().enumerate() {
        let mut palette = Vec::new();
        swatch(Some(v), &mut palette)?;
        let w = palette.remove(0);
        first_guess.push((w.name.unwrap_or_else(|| format!("filament {}", i + 1)), w.rgb));
    }
    let spec = crate::picture::Spec::new(crate::picture::Arg::of(arg), image, first_guess, centre);
    let mix = fab_paint_field::Mix { field: fab_paint_field::MixField::Raster(fab_paint_field::raster::Raster::new(Arc::new(spec))), per_mm: 1.0, slots: Vec::new(), sharp: 1.0, balance: 0.0 };
    Ok(Layer { region: Region::All, op: PaintOp::Mix { channel: Channel::Color, mix: Arc::new(mix) } })
}

/// A `Mix{..}`'s paint: its materials' slots, its field reading the model frame in metres.
fn mix(m: &lang::MixArg, palette: &mut Vec<Swatch>) -> Result<PaintOp, String> {
    let slots = m.materials.iter().map(|v| swatch(Some(v), palette)).collect::<Result<Vec<u8>, String>>()?;
    let field = match &m.field {
        lang::MixField::Fn(kernel) => fab_paint_field::MixField::Fn(kernel.clone()),
        // grown on the part's surface when the part is built (`eval`)
        lang::MixField::Pattern(p) => {
            let spec = fab_paint_field::pattern::Spec::new(Some(p.preset), &p.values).map_err(|e| format!("{}: {e}", p.kind))?;
            fab_paint_field::MixField::Pattern(fab_paint_field::pattern::Grown::new(spec))
        }
    };
    let mix = fab_paint_field::Mix { field, per_mm: 1.0 / UNIT, slots, sharp: m.sharp, balance: m.balance };
    Ok(PaintOp::Mix { channel: Channel::Color, mix: Arc::new(mix) })
}

fn facet_layers(p: &Program, objects: &[Emitted], assets: &FacetAssets, root: &Root, palette: &mut Vec<Swatch>) -> Result<Vec<(Layer, Option<Only>)>, String> {
    let imported = "give it an imported solid: m.facets(cube, [red blue])";
    let Some(Arg::Solid(i)) = root.get("of") else { return Err(imported.into()) };
    let solid = &p.solids[*i];
    let name = solid.asset.as_ref().ok_or(imported)?;
    let facets = assets.get(name).ok_or_else(|| format!("the mesh '{name}' has no painted facets in the project"))?;
    let Some(Arg::Value(V::Arr(materials))) = root.get("materials") else {
        return Err("give it a list of materials: m.facets(cube, [red blue])".into());
    };
    let mut frames = Vec::new();
    let mut node = solid.node;
    loop {
        match &p.graph.node(node).op {
            Op::Mesh { .. } => break,
            Op::Transform { source, at, rotate, scale } => {
                if scale[0] != scale[1] || scale[1] != scale[2] || scale[0] <= 0.0 || !scale[0].is_finite() {
                    return Err("the import is scaled unevenly; scale it evenly".into());
                }
                frames.push((*at, *rotate, scale[0]));
                node = *source;
            }
            other => return Err(format!("the import passes through {}; only placements of a mesh have painted facets", other.name())),
        }
    }
    let owner = objects.iter().position(|o| o.bodies.iter().any(|b| within(p, *i, *b)));
    let mut layers = Vec::new();
    for state in facets.used().filter(|s| *s != 0) {
        let Some(material) = materials.get(state as usize - 1).filter(|m| !matches!(m, V::Nil)) else { continue };
        let slot = swatch(Some(material), palette)?;
        let region = Region::Facets { facets: facets.clone(), state, frames: frames.clone() };
        layers.push((Layer { region, op: PaintOp::Paint { channel: Channel::Color, slot } }, owner.map(Only::Object)));
    }
    Ok(layers)
}

fn p3(v: &V) -> Option<[f64; 3]> {
    match v {
        V::Arr(a) if a.len() == 3 => Some([a[0].num()? * UNIT, a[1].num()? * UNIT, a[2].num()? * UNIT]),
        _ => None,
    }
}

fn stroke(root: &Root) -> Result<Region, String> {
    let points: Vec<[f64; 3]> = match root.get("points") {
        Some(Arg::Value(V::Arr(a))) if !a.is_empty() => a.iter().map(p3).collect::<Option<_>>().ok_or("the points are [[x y z] [x y z] ...] with units")?,
        _ => return Err("give it the stroke's points: m.stroke([[0 6mm 0] [12mm 6mm 3mm]], 2mm, red)".into()),
    };
    let radius = match root.get("radius") {
        Some(Arg::Value(v)) => v.num().map(|r| r * UNIT).filter(|r| *r > 0.0 && r.is_finite()).ok_or("the radius is a length over 0")?,
        _ => return Err("give it a radius: m.stroke(points, 2mm, red)".into()),
    };
    if !points.iter().flatten().all(|v| v.is_finite()) {
        return Err("a point is not a finite length".into());
    }
    Ok(Region::Stroke { points, radius })
}

/// The one-time conversion of brushed paint files (`x.paint.splash` holding only `m.stroke`
/// statements and their materials, as the brush used to write them) into strokes files: each
/// becomes `x.strokes`, and the scripts' `m.paint_file("x.paint.splash")` reads
/// `m.strokes("x.strokes")` in its place. A paint file holding anything else stays as it is.
pub fn migrate_strokes(state: &mut crate::State) {
    let files: Vec<String> = state.scripts.keys().filter(|f| crate::program::is_paint_file(f)).cloned().collect();
    for f in files {
        let stem = f.strip_suffix(&format!(".{}", crate::program::PAINT_EXTENSION)).unwrap_or(&f).to_string();
        let target = format!("{stem}.{}", crate::program::STROKES_EXTENSION);
        if state.strokes.contains_key(&target) {
            continue;
        }
        let assets = Assets::default();
        let Ok(p) = lang::evaluate(&state.scripts[&f], &Options { generated: &Default::default(), assets: &assets, images: &Default::default(), extension: "fab", libraries: &[], inputs: &lang::NO_INPUTS, unit: UNIT, roots: crate::program::PAINT_ROOTS, overrides: Default::default(), weave: None, solved: Default::default() }) else { continue };
        if p.roots.is_empty() || p.roots.iter().any(|r| r.kind != "stroke") {
            continue;
        }
        let mut palette = Vec::new();
        let mut log = fab_paint_field::Strokes::new();
        let mut ok = true;
        for root in &p.roots {
            match (stroke(root), swatch(material(root), &mut palette)) {
                (Ok(Region::Stroke { points, radius }), Ok(slot)) => {
                    let w = &palette[slot as usize - 1];
                    let slot = log.slot_for(fab_paint_field::StrokeSwatch { rgb: w.rgb, filament: None });
                    log.push(fab_paint_field::StrokeOp::Paint { slot }, radius, 1.0, &points);
                }
                _ => ok = false,
            }
        }
        let call = format!("m.paint_file(\"{f}\")");
        let readers: Vec<String> = state.scripts.iter().filter(|(_, t)| t.contains(&call)).map(|(n, _)| n.clone()).collect();
        if !ok || readers.is_empty() {
            continue;
        }
        for r in readers {
            let t = state.scripts[&r].replace(&call, &format!("m.strokes(\"{target}\")"));
            state.scripts.insert(r, t);
        }
        state.scripts.remove(&f);
        Arc::make_mut(&mut state.strokes).insert(target, Arc::new(log));
    }
}

/// Solid `i` is `root` or part of it.
pub(crate) fn within(p: &Program, i: usize, root: usize) -> bool {
    let mut at = Some(i);
    while let Some(j) = at {
        if j == root {
            return true;
        }
        at = p.solids[j].parent;
    }
    false
}

/// How `m.settings` is written, for its messages.
const SETTINGS_HOW: &str = "m.settings(region, {sparse_infill_density: \"100%\" sparse_infill_pattern: \"gyroid\" wall_loops: 4})";

/// An `m.settings` statement's region: a volume, each solid's own (a feature of the object its
/// own solid, not the surface it makes; the object itself all of it); never a surface flood.
fn settings_volume(p: &Program, objects: &[Emitted], root: &Root) -> Result<(Region, Option<Only>), String> {
    let ids: Vec<usize> = match root.get("region") {
        Some(Arg::Solid(i)) => vec![*i],
        Some(Arg::Solids(ids)) if !ids.is_empty() => ids.clone(),
        Some(Arg::Value(V::Obj(o))) if o.proto.as_deref() == Some("Flood") => return Err("a region's settings hold in a volume: give it a solid (m.box({..}), m.sphere({..})), not a surface flood".into()),
        _ => return Err(format!("give it a volume: a solid (m.box({{..}}), m.sphere({{..}}), a feature of the object) or a list of them: {SETTINGS_HOW}")),
    };
    let (mut nodes, mut owners) = (Vec::new(), Vec::new());
    for i in ids {
        let solid = &p.solids[i];
        match solid_region(p, objects, i, Form::Meshed) {
            Ok((Region::All, _)) => return Ok((Region::All, Some(Only::Body(i)))),
            Ok((Region::Field { node, .. }, owner)) => {
                nodes.push(node);
                owners.push(owner.or_else(|| follows(p, objects, i)));
            }
            Ok(_) => return Err(format!("'{}' is no volume to set print settings in", if solid.id.is_empty() { solid.proto } else { &solid.id })),
            Err(why) => return Err(format!("'{}' has no spatial form to set print settings in: {why}", if solid.id.is_empty() { solid.proto } else { &solid.id })),
        }
    }
    // a region outside every object holds in each; features, and a volume that follows one, only
    // in their object
    let only = match owners.first() {
        Some(Some(oi)) if owners.iter().all(|o| *o == Some(*oi)) => Some(Only::Object(*oi)),
        _ => None,
    };
    let node = if nodes.len() == 1 { nodes.pop().unwrap() } else { SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: nodes } };
    Ok((Region::Field { node, grow: 0.0 }, only))
}

/// A script's print settings volume by name, as a region in its model frame: what its statement
/// holds in. A part's whole body is its own region.
pub fn volume_region(script: &crate::program::Script, name: &str) -> Option<Region> {
    let p = &script.program;
    let i = p.solids.iter().position(|s| s.id == name)?;
    match solid_region(p, &script.objects, i, Form::Meshed).ok()? {
        (Region::Field { node, grow }, _) => Some(Region::Field { node, grow }),
        _ => None,
    }
}

/// What the `m.settings` statements on solid `i` set, `(key, value)` as they read, key by key.
pub fn volume_settings(p: &Program, i: usize) -> Vec<(String, String)> {
    let on = |r: &&Root| r.kind == "settings" && matches!(r.get("region"), Some(Arg::Solid(j)) if *j == i);
    p.roots.iter().filter(on).filter_map(|r| region_settings(r).ok()).flatten().map(|(k, v)| (k.to_string(), v)).collect()
}

/// The object whose feature an offset solid follows: `m.offset(left_arm, 1mm)` holds in the object
/// `left_arm` is part of, as a feature does.
fn follows(p: &Program, objects: &[Emitted], i: usize) -> Option<usize> {
    let solid = &p.solids[i];
    if solid.proto != "Offset" {
        return None;
    }
    let (source, _) = crate::volumes::followed(p, i)?;
    let source = source.as_str();
    let of = p.solids.iter().position(|s| s.id == source)?;
    objects.iter().position(|o| o.bodies.iter().any(|b| within(p, of, *b)))
}

/// An `m.settings` statement's settings: `{key: value}` of [`fab_plate::REGION_SETTINGS`] keys,
/// each value as `place(..., settings: {...})` takes it (text or a number in the setting's
/// unit: `sparse_infill_density: "100%"` or `100`, `wall_loops: 4`), at least one. A pattern
/// that reads the layers round it or settings of its own is the whole part's to set.
fn region_settings(root: &Root) -> Result<RegionSettings, String> {
    let keys = || fab_plate::REGION_SETTINGS.join(", ");
    let o = match root.get("opts") {
        Some(Arg::Value(V::Obj(o))) if o.proto.is_none() => o,
        None | Some(Arg::Value(V::Nil)) => return Err(format!("give it the settings: {SETTINGS_HOW}")),
        Some(Arg::Value(v)) => return Err(format!("the settings are {{key: value}}, not {}: {SETTINGS_HOW}", v.describe())),
        Some(_) => return Err(format!("the settings are {{key: value}}: {SETTINGS_HOW}")),
    };
    let mut out = RegionSettings::new();
    for (k, v) in &o.fields {
        if !fab_plate::REGION_SETTINGS.contains(&k.as_str()) {
            let whole = if fab_plate::print_setting(k).is_some() { format!("; '{k}' is the whole part's: place(..., settings: {{{k}: ...}})") } else { String::new() };
            return Err(format!("a region sets {}, not '{k}'{whole}", keys()));
        }
        let text = crate::project::setting_text(k, v)?;
        if k == "sparse_infill_pattern" {
            use makepad_fab_toolpath::pattern::Sparse;
            if Sparse::parse(&text).is_some_and(|p| !region_pattern(p)) {
                let names = Sparse::ALL.iter().filter(|(_, p)| region_pattern(*p)).map(|(n, _)| *n).collect::<Vec<_>>().join(", ");
                return Err(format!("\"{text}\" reads the layers round it or settings of its own: set it as the whole part's pattern; a region takes {names}"));
            }
        }
        out.retain(|(key, _)| key != k);
        out.push((k.clone(), text));
    }
    if out.is_empty() {
        return Err(format!("give it a setting ({}): {SETTINGS_HOW}", keys()));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Whether a region may take the sparse pattern called `name` (`gyroid`): see [`region_pattern`].
pub fn region_takes_pattern(name: &str) -> bool {
    makepad_fab_toolpath::pattern::Sparse::parse(name).is_some_and(region_pattern)
}

/// Whether a region may take a sparse pattern: one each layer fills by itself from the part's
/// settings (lightning and the adaptive cubics read the layers round it, locked zag settings of
/// its own).
fn region_pattern(p: makepad_fab_toolpath::pattern::Sparse) -> bool {
    !p.cross_layer() && p != makepad_fab_toolpath::pattern::Sparse::LockedZag
}

/// The settings slot of `set` (an equal set shares one).
fn settings_slot(set: RegionSettings, all: &mut Vec<RegionSettings>) -> Result<u8, String> {
    let i = match all.iter().position(|s| *s == set) {
        Some(i) => i,
        None => {
            if all.len() >= 254 {
                return Err("a script gives at most 254 different region settings".into());
            }
            all.push(set);
            all.len() - 1
        }
    };
    Ok((i + 1) as u8)
}

/// A statement's region and, for a feature, the object it belongs to.
/// A support statement's region: any a fill takes, or a strokes file the brush paints
/// (`m.support_block("bracket.block.strokes")`), read as one layer where the statement stands.
fn support_region(p: &Program, objects: &[Emitted], bodies: &Bodies, root: &Root) -> Result<(Region, Option<Only>), String> {
    match root.get("region") {
        Some(Arg::Value(V::Str(f))) if crate::program::is_strokes_file(f) => Ok((Region::Strokes { file: f.clone(), strokes: Default::default(), slots: Default::default() }, None)),
        Some(Arg::Value(V::Str(f))) => Err(format!("'{f}' is no strokes file (.{}): give it a region, or the brush's strokes file", crate::program::STROKES_EXTENSION)),
        _ => region(p, objects, bodies, root, "region"),
    }
}

fn region(p: &Program, objects: &[Emitted], bodies: &Bodies, root: &Root, key: &str) -> Result<(Region, Option<Only>), String> {
    let i = match root.get(key) {
        Some(Arg::Solid(i)) => i,
        // a list of solids: their union, all features of the object or all regions
        Some(Arg::Solids(ids)) => {
            let (mut features, mut regions, mut owner) = (Vec::new(), Vec::new(), None);
            for i in ids {
                match one_region(p, objects, bodies, *i)? {
                    (Region::Operand(of), o) => {
                        features.extend(of.iter().cloned());
                        owner = owner.or(o);
                    }
                    (Region::Field { node, .. }, _) => regions.push(node),
                    _ => return Err("a list of solids fills by each one's own field; fill the object by its name".into()),
                }
            }
            return match (features.is_empty(), regions.is_empty()) {
                (false, true) => Ok((Region::Operand(Arc::new(features)), owner)),
                (true, false) => Ok((Region::Field { node: SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: regions }, grow: 0.0 }, None)),
                _ => Err("a list is all features of the object or all regions, not both".into()),
            };
        }
        // a smooth-region flood: resolved on each part's surface when the parts are built
        Some(Arg::Value(V::Obj(o))) if o.proto.as_deref() == Some("Flood") => {
            let seed = o.get("seed").and_then(p3).ok_or("a Flood's seed is a surface point [x y z] with units: Flood{seed: [0 20mm 21mm] angle: 30deg}")?;
            let angle = match o.get("angle") {
                None | Some(V::Nil) => DEFAULT_FLOOD_ANGLE.to_radians(),
                Some(v) => v.num().filter(|a| a.is_finite() && *a > 0.0 && *a <= std::f64::consts::PI).ok_or("a Flood's angle is over 0 and at most 180deg")?,
            };
            return Ok((Region::Flood { seed, angle }, None));
        }
        _ => return Err("give it a solid: a feature of the object (holes), the object itself, a region (m.box({..})), or a smooth-region flood (Flood{seed: [x y z] angle: 30deg})".into()),
    };
    one_region(p, objects, bodies, *i)
}

/// One solid's region and the parts it is limited to: a body all of its own part (a jointed
/// object's other bodies are parts of their own), a feature its object's.
fn one_region(p: &Program, objects: &[Emitted], bodies: &Bodies, i: usize) -> Result<(Region, Option<Only>), String> {
    match solid_region(p, objects, i, Form::Meshed) {
        Ok((Region::All, _)) => Ok((Region::All, Some(Only::Body(i)))),
        Ok((Region::Field { node, .. }, Some(oi))) => Ok((feature_region(p, bodies, &objects[oi], i, node), Some(Only::Object(oi)))),
        Ok((region, owner)) => Ok((region, owner.map(Only::Object))),
        Err(why) => {
            let solid = &p.solids[i];
            Err(format!("'{}' has no spatial form to fill by: {why}", if solid.id.is_empty() { solid.proto } else { &solid.id }))
        }
    }
}

/// Which distance a solid's region takes.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Form {
    /// Its ops' own (boxes, spheres, cylinders, tori, fields); a solid made otherwise has none.
    Exact,
    /// Those, and an op with none of its own (an extrusion, a revolve, a loft, a fillet, an
    /// exact body) takes the solid it produces: its mesh's.
    Meshed,
}

/// One solid's region and, for a feature, the object it belongs to: the object's own solid is
/// all of it ([`Region::All`]), a feature its own field grown by [`FEATURE_BAND`], a solid
/// outside every object its field. Else why it has no distance field.
pub(crate) fn solid_region(p: &Program, objects: &[Emitted], i: usize, form: Form) -> Result<(Region, Option<usize>), String> {
    if let Some(oi) = objects.iter().position(|o| o.bodies.contains(&i)) {
        return Ok((Region::All, Some(oi)));
    }
    let owner = objects.iter().position(|o| o.bodies.iter().any(|b| within(p, i, *b)));
    let node = sdf_of(&p.graph, p.solids[i].node, form)?;
    Ok(match owner {
        Some(oi) => (Region::Field { node, grow: FEATURE_BAND }, Some(oi)),
        None => (Region::Field { node, grow: 0.0 }, None),
    })
}

/// Feature `i` of `object` as a fill paints it ([`Operand`]): the surface it makes of the
/// object's, and round that surface where it is the nearest (a blend's fillet parts where its
/// operands are equally near, a carve's cutter paints the faces it cuts); where an op on its way
/// up the object has no distance of its own, its `own` surface's band.
fn feature_region(p: &Program, bodies: &Bodies, object: &Emitted, i: usize, own: SdfNode) -> Region {
    let operand = object.bodies.iter().find(|b| within(p, i, **b)).and_then(|&body| bodies.operand(p, body, i));
    Region::Operand(Arc::new(vec![operand.unwrap_or_else(|| Operand::band(own, FEATURE_BAND))]))
}

/// Each body's field with its features tagged ([`Body`]), built the first time one of its
/// features is asked for and shared by the rest: a compile's fills, the fill tool's features.
#[derive(Default)]
struct Bodies(RefCell<HashMap<usize, Option<Shared>>>);

/// A body's features' regions, and the graph nodes tagged in it.
struct Shared {
    body: Arc<Body>,
    tags: HashMap<NodeId, u32>,
}

impl Bodies {
    /// Feature `i` of `body`'s region. A feature whose name another solid of the body shares
    /// (or that has none) takes a body of its own, tagged alone.
    fn operand(&self, p: &Program, body: usize, i: usize) -> Option<Operand> {
        let tag = makepad_csg_sdf::part_id(&p.solids[i].id);
        let mut all = self.0.borrow_mut();
        let shared = all.entry(body).or_insert_with(|| {
            let mut names: HashMap<u32, Vec<NodeId>> = HashMap::new();
            for (j, s) in p.solids.iter().enumerate() {
                if j != body && !s.id.is_empty() && within(p, j, body) {
                    names.entry(makepad_csg_sdf::part_id(&s.id)).or_default().push(s.node);
                }
            }
            let tags: HashMap<NodeId, u32> = names.into_iter().filter(|(_, n)| n.len() == 1).map(|(t, n)| (n[0], t)).collect();
            let root = tagged_all(&p.graph, p.solids[body].node, &tags)?;
            Some(Shared { body: Arc::new(Body::new(&root)), tags })
        });
        // a body with an op of no distance in it has none for any of its features
        let s = shared.as_ref()?;
        if s.tags.get(&p.solids[i].node) == Some(&tag) {
            s.body.operand(tag)
        } else {
            Body::new(&tagged_sdf(&p.graph, p.solids[body].node, p.solids[i].node, tag)?).operand(tag)
        }
    }
}

/// The field of graph node `id` with each node of `tags` in it tagged ([`tagged_sdf`] of them
/// all at once); `None` where an op in it has no distance of its own.
fn tagged_all(g: &Graph, id: NodeId, tags: &HashMap<NodeId, u32>) -> Option<SdfNode> {
    let inner = match &g.node(id).op {
        Op::Transform { source, at, rotate, scale } if scale[0] == scale[1] && scale[1] == scale[2] && scale[0] > 0.0 => {
            SdfNode::Transform { at: *at, rotate: *rotate, scale: scale[0], of: Box::new(tagged_all(g, *source, tags)?) }
        }
        Op::Boolean { mode, a, b } => {
            let op = match mode {
                BoolMode::Union => SdfOp::Union,
                BoolMode::Difference => SdfOp::Difference,
                BoolMode::Intersection => SdfOp::Intersection,
            };
            let of = std::iter::once(*a).chain(b.iter().copied()).map(|x| tagged_all(g, x, tags)).collect::<Option<Vec<_>>>()?;
            SdfNode::Combine { op, blend: 0.0, of }
        }
        _ => sdf_of(g, id, Form::Exact).ok()?,
    };
    Some(match tags.get(&id) {
        Some(&tag) => SdfNode::Id { id: tag, of: Box::new(inner) },
        None => inner,
    })
}

/// The field of graph node `id` with node `target` in it tagged `tag` (a field part's operands
/// carry their tags already); `None` where an op on the way has no distance of its own.
fn tagged_sdf(g: &Graph, id: NodeId, target: NodeId, tag: u32) -> Option<SdfNode> {
    if id == target {
        return Some(SdfNode::Id { id: tag, of: Box::new(sdf_of(g, id, Form::Exact).ok()?) });
    }
    match &g.node(id).op {
        Op::Transform { source, at, rotate, scale } if scale[0] == scale[1] && scale[1] == scale[2] && scale[0] > 0.0 => {
            Some(SdfNode::Transform { at: *at, rotate: *rotate, scale: scale[0], of: Box::new(tagged_sdf(g, *source, target, tag)?) })
        }
        Op::Boolean { mode, a, b } => {
            let op = match mode {
                BoolMode::Union => SdfOp::Union,
                BoolMode::Difference => SdfOp::Difference,
                BoolMode::Intersection => SdfOp::Intersection,
            };
            let of = std::iter::once(*a).chain(b.iter().copied()).map(|x| tagged_sdf(g, x, target, tag)).collect::<Option<Vec<_>>>()?;
            Some(SdfNode::Combine { op, blend: 0.0, of })
        }
        _ => sdf_of(g, id, Form::Exact).ok(),
    }
}

/// A solid's distance field from its graph node, by `form`; else why it has none.
pub(crate) fn sdf_of(g: &Graph, id: NodeId, form: Form) -> Result<SdfNode, String> {
    Ok(match &g.node(id).op {
        Op::Box { size, radius } => SdfNode::Box { size: *size, radius: *radius },
        Op::Sphere { radius, .. } => SdfNode::Sphere { radius: *radius },
        Op::Cylinder { radius, top_radius, height, .. } => SdfNode::Cylinder { radius: *radius, top_radius: *top_radius, height: *height },
        Op::Torus { radius, tube, .. } => SdfNode::Torus { radius: *radius, tube: *tube },
        Op::Field { shape, .. } => shape.root.clone(),
        Op::Transform { source, at, rotate, scale } if scale[0] == scale[1] && scale[1] == scale[2] && scale[0] > 0.0 => {
            SdfNode::Transform { at: *at, rotate: *rotate, scale: scale[0], of: Box::new(sdf_of(g, *source, form)?) }
        }
        Op::Boolean { mode, a, b } => {
            let op = match mode {
                BoolMode::Union => SdfOp::Union,
                BoolMode::Difference => SdfOp::Difference,
                BoolMode::Intersection => SdfOp::Intersection,
            };
            let mut of = vec![sdf_of(g, *a, form)?];
            for x in b {
                of.push(sdf_of(g, *x, form)?);
            }
            SdfNode::Combine { op, blend: 0.0, of }
        }
        Op::Offset { source, distance, .. } => SdfNode::Round { radius: *distance, of: Box::new(sdf_of(g, *source, form)?) },
        other if form == Form::Exact => return Err(format!("it is made by {}, which has no distance of its own", other.name())),
        other => {
            // the solid the op produces, as the part's own build makes it
            let mut e = makepad_csg_graph::Evaluator::with_brep(makepad_csg_brep_model::engine());
            let mesh = e.mesh(g, id, None).map_err(|f| format!("its {} does not build: {}", other.name(), f.message))?;
            if mesh.triangles.is_empty() {
                return Err(format!("its {} builds no solid", other.name()));
            }
            SdfNode::Mesh(Arc::new(crate::mech::mesh_field(&mesh)))
        }
    })
}

/// A named feature of an object (a solid inside it with a `let`): its own region in the model
/// frame, what `m.fill(name, ..)` paints, and its box's volume.
#[derive(Clone, Debug)]
pub struct Feature {
    pub name: String,
    pub region: Region,
    pub volume: f64,
}

impl Feature {
    /// How far a point of the part is from the feature's own shape, blends aside. A fill's region
    /// says which surface it paints, which isn't this.
    pub fn own_distance(&self, q: [f64; 3]) -> f64 {
        match &self.region {
            Region::Operand(of) => of.iter().map(|o| makepad_csg_sdf::distance(&o.own, makepad_csg_math::Vec3d::new(q[0], q[1], q[2]))).fold(f64::INFINITY, f64::min),
            other => other.distance(q),
        }
    }
}

/// The feature a point inside the part is in: the smallest that holds it, else the nearest within
/// `reach` (a blend's fillet belongs to what it joins).
pub fn feature_at(features: &[Feature], q: [f64; 3], reach: f64) -> Option<&Feature> {
    let holds = features.iter().map(|f| (f, f.own_distance(q))).collect::<Vec<_>>();
    holds.iter().filter(|(_, d)| *d <= 0.0).min_by(|a, b| a.0.volume.total_cmp(&b.0.volume))
        .or_else(|| holds.iter().filter(|(_, d)| *d <= reach).min_by(|a, b| a.1.total_cmp(&b.1)))
        .map(|(f, _)| *f)
}

/// The features of an emitted object the fill tool can pick: its named solids with a spatial
/// form, the object's own solids left out (the whole part is its own pick).
pub fn features(p: &Program, object: &Emitted) -> Vec<Feature> {
    let bodies = Bodies::default();
    p.solids
        .iter()
        .enumerate()
        .filter(|(i, s)| s.named && !object.bodies.contains(i) && object.bodies.iter().any(|b| within(p, *i, *b)))
        .filter_map(|(i, s)| {
            let node = sdf_of(&p.graph, s.node, Form::Exact).ok()?;
            let (lo, hi) = makepad_csg_sdf::node_bounds(&node)?;
            let volume = (0..3).map(|k| (hi[k] - lo[k]).max(0.0)).product();
            Some(Feature { name: s.id.clone(), region: feature_region(p, &bodies, object, i, node), volume })
        })
        .collect()
}
