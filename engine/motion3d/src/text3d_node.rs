//! The Text3D node: extruded 3D type with per-letter (word, line, piece)
//! animation. Owned by the 3D typography lane (T3); the rest of motion3d
//! calls it through `parse`, `eval` and `TextDef`.
//!
//! The solids come from [`build_text3d`] (vj_fx's glyph extruder): one mesh
//! per letter around its rest centre, split by part (front and back caps,
//! side walls, bevel band) so each part can wear its own material. A unit
//! (letter, word, line, the whole block, or a shard of a letter with
//! `pieces`) animates on its own clock `t − rank · stagger` and turns about
//! its own centre:
//!
//! ```text
//! world · place(centre) · T(each.position + spread·r + alternate·±1)
//!       · R(each.rotation + …) · S(each.scale · …) · T(piece pivot − centre)
//! ```
//!
//! `place` is the unit's rest centre, or with `path:` the point of the
//! curve at the unit's distance along the line (plus `path_offset`), turned
//! to the curve's tangent. `r` is a fixed random vector per unit in −1..1
//! (from `seed`), so `spread` fans units out on their own directions and a
//! `tween` of the spread assembles them.

use crate::doc::*;
use crate::anim::{Anim1, Anim3};
use crate::eval::{ranks, Walk};
use crate::frame::MaterialFrame;
use makepad_geom3d::*;
use makepad_text_mesh::letters::*;
use makepad_geom3d::math::*;
use makepad_geom3d::mesh::*;
use crate::scene::*;
use crate::script::*;
use makepad_widgets::*;
use crate::script::MediaRef;
use std::sync::Arc;

/// What moves as one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text3dUnit {
    Letter,
    Word,
    Line,
    Whole,
    /// A shard of a letter (`pieces`).
    Piece,
}

impl Text3dUnit {
    pub const NAMES: &'static [&'static str] = &["letter", "word", "line", "whole", "piece"];
}

/// The material slots of a letter's parts.
const FRONT: usize = 0;
const SIDE: usize = 1;
const BEVEL: usize = 2;
const BACK: usize = 3;
/// Per-word tiles (`tiles`).
const TILE: usize = 4;

/// One drawable piece: a letter, or a shard of one.
#[derive(Clone, Debug)]
pub struct Piece {
    pub letter: usize,
    /// Rest centre in text space; the meshes are relative to it.
    pub pivot: [f32; 3],
    /// Meshes by material slot (parts without their own material merged
    /// into the front slot).
    pub slots: Vec<(usize, SharedMesh)>,
}

/// Per-unit random (`spread`) or parity (`alternate`) offsets.
#[derive(Clone, Debug, Default)]
pub struct UnitOffsets {
    pub position: Option<Anim3>,
    pub rotation: Option<Anim3>,
    pub scale: Option<Anim1>,
}

#[derive(Clone, Debug)]
pub struct TextPath {
    pub arc: ArcTable,
    /// World units along the path added to every unit's place (keyable).
    pub offset: Anim1,
    /// Where the text's left end sits on the path, 0..1 of its length.
    pub start: f32,
    /// Turn the units to the tangent (else they keep facing +z).
    pub align: bool,
}

/// Where the letters stand: the laid-out block, or one of vj_fx's text
/// layouts in space (same shapes and defaults as `makepad_text_mesh::text_mesh`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpaceLayout {
    Block,
    /// The text bent round a vertical cylinder of `radius`, facing out.
    Ring { radius: f32 },
    /// The lines end to end on a coil about +Y, descending `pitch` per turn.
    Helix { radius: f32, pitch: f32 },
    /// The letters on a lattice lying in the XZ plane, facing +Y.
    Grid { cols: usize, pitch: f32 },
    /// Every word on its own ring round −Z, `pitch` apart, tops outward.
    Tunnel { radius: f32, pitch: f32 },
    /// The lines round a drum about X (line 0 facing +z, the next ones
    /// further down the drum): a slot-machine reel. Turn the node about X
    /// by −360·k/lines to bring line k to the front.
    Reel { radius: f32 },
    /// The lines along a circular arc in the text's own plane: over the top
    /// of a centre `radius` below (badge text), or with a negative radius
    /// under a centre above, still reading left to right.
    Arc { radius: f32 },
}

impl SpaceLayout {
    pub const NAMES: &'static [&'static str] = &["block", "ring", "helix", "grid", "tunnel", "reel", "arc"];
}

/// Voxel letters: one cube mesh drawn once per cell.
#[derive(Clone, Debug)]
pub struct VoxelDraw {
    pub cube: SharedMesh,
    /// (letter slot, rest centre) per cube.
    pub cubes: Vec<(usize, [f32; 3])>,
}

/// Particles sampled on the letters' faces that fly in (or out) on their
/// own clocks — type that forms from, or dissolves into, particles.
#[derive(Clone, Debug)]
pub struct ParticleDef {
    pub mesh: SharedMesh,
    /// (letter slot, rest position in text space) per particle.
    pub points: Vec<(usize, [f32; 3])>,
    pub size: Anim1,
    /// Offset amount along each particle's own random direction (keyable).
    pub spread: Anim3,
    pub opacity: Anim1,
    /// Seconds between the first and the last particle's clock.
    pub stagger: f32,
    pub material: Option<MaterialDef>,
}

#[derive(Clone, Debug)]
pub struct Karaoke {
    pub words: Vec<[f32; 2]>,
    pub ghost: [f32; 4],
    pub current: [f32; 4],
    pub sung: [f32; 4],
}

impl Karaoke {
    /// Word `w`'s face colour, glow share and opacity at `t`: the ghost
    /// before its time, the current colour (glowing) while it is sung,
    /// easing to the sung colour after.
    pub fn state(&self, w: usize, t: f32) -> ([f32; 4], f32, f32) {
        let Some(&[s, e]) = self.words.get(w) else { return (self.sung, 0.0, 1.0) };
        let mix = |a: [f32; 4], b: [f32; 4], k: f32| [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k, 1.0];
        if t < s {
            (self.ghost, 0.0, self.ghost[3])
        } else if t < e {
            let k = ((t - s) / 0.06).clamp(0.0, 1.0);
            (mix(self.ghost, self.current, k), k, self.ghost[3] + (1.0 - self.ghost[3]) * k)
        } else {
            let k = ((t - e) / 0.15).clamp(0.0, 1.0);
            (mix(self.current, self.sung, k), 1.0 - k, 1.0)
        }
    }
}

#[derive(Clone, Debug)]
pub struct TextDef {
    pub text: Arc<Text3d>,
    pub pieces: Vec<Piece>,
    /// With `pieces`: every letter whole again (index = letter), drawn in
    /// place of its shards once they have all landed — one mesh per letter,
    /// so the seams between shards cannot show.
    pub welded: Vec<Piece>,
    /// Front (and every part without its own), side, bevel, back.
    pub materials: [Option<MaterialDef>; 5],
    pub by: Text3dUnit,
    pub stagger: f32,
    pub order: Order,
    pub each: EachDef,
    /// `each: fn(i, n, t, edit) {…}`: a closure per unit (replaces the
    /// fields it returns: position, rotation, scale, color).
    pub each_fn: Option<u32>,
    pub spread: UnitOffsets,
    pub alternate: UnitOffsets,
    pub path: Option<TextPath>,
    pub layout: SpaceLayout,
    /// Per letter slot: its distance along the text (helix), from 0.
    pub arc: Vec<f32>,
    pub voxels: Option<VoxelDraw>,
    /// With `led`: the unlit dots of the board (static, behind).
    pub led_unlit: Option<(VoxelDraw, f32)>,
    pub particles: Option<ParticleDef>,
    /// Particles follow their letter's unit clock (a stamp's dust).
    pub particle_sync: bool,
    /// Share of letters that fail (flicker), and how fast.
    pub flicker: (f32, f32),
    /// Karaoke: per word (start, end) on the layer clock, and the ghost,
    /// current and sung colours (linear RGBA; the ghost's alpha is its
    /// opacity).
    pub karaoke: Option<Karaoke>,
    pub seed: u64,
    pub deform: DeformDef,
}

pub(crate) fn parse(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str) -> Option<NodeDef> {
    r.check_fields(o, path, "Text3D", &with_node_fields(&[
        "text", "font", "weight", "wdth", "slnt", "size", "depth", "bevel", "bevel_segments", "bevel_type", "detail", "tracking", "line_height", "align", "wrap",
        "material", "side_material", "bevel_material", "back_material", "by", "stagger", "order", "each", "spread", "alternate", "pieces",
        "path", "path_offset", "path_start", "path_align", "seed", "displace", "displace_frequency", "displace_speed", "twist", "bend",
        "layout", "layout_radius", "layout_pitch", "grid_cols", "voxels", "voxel_layers", "particles", "particle_size", "particle_spread",
        "particle_opacity", "particle_stagger", "particle_material", "tube", "tube_path", "tube_sides", "flicker", "flicker_speed", "led", "led_size",
        "led_unlit", "tiles", "tile_depth", "tile_radius", "tile_material", "particle_source", "particle_sync", "karaoke", "karaoke_ghost",
        "karaoke_current", "karaoke_sung", "wrap_radius", "ripple", "ripple_frequency", "ripple_speed", "explode", "curl", "curl_radius",
    ]));
    let text = r.string(o, "text").unwrap_or_default();
    if text.trim().is_empty() {
        let loc = r.loc(o);
        r.error(loc, &format!("{path}.text"), "Text3D needs `text`", Some("text: \"HELLO\" (or text: in(@headline))".into()));
        return None;
    }
    let font = parse_font(r, cx, o, path)?;
    let d = Text3dParams::default();
    let align = match r.id(o, "align").as_deref() {
        Some("left") => TextAlign::Left,
        Some("right") => TextAlign::Right,
        _ => TextAlign::Center,
    };
    let bevel_profile = match r.id(o, "bevel_type") {
        None => BevelProfile::Chamfer,
        Some(name) => BevelProfile::by_name(&name).unwrap_or_else(|| {
            let loc = r.loc(o);
            r.warn(loc, &format!("{path}.bevel_type"), format!("no bevel type `@{name}`"), Some(format!("one of {}", BevelProfile::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
            BevelProfile::Chamfer
        }),
    };
    // Curved profiles need rings to read as curves.
    let segments_default = match bevel_profile {
        BevelProfile::Round | BevelProfile::Cove | BevelProfile::Ogee => 4,
        BevelProfile::Step => 3,
        _ => d.bevel_segments,
    };
    let wrap = r.number(r.get(o, "wrap")).map(|w| w as f32);
    let params = Text3dParams {
        text,
        font,
        weight: r.number(r.get(o, "weight")).map(|w| w as f32),
        // Variable-font width and slant, read with the real outlines.
        axes: [(*b"wdth", "wdth"), (*b"slnt", "slnt")].iter().filter_map(|(tag, name)| r.number(r.get(o, name)).map(|v| (u32::from_be_bytes(*tag), v as f32))).collect(),
        size: r.f32(o, path, "size", d.size),
        depth: r.f32(o, path, "depth", d.depth),
        bevel: r.f32(o, path, "bevel", d.bevel),
        bevel_segments: r.u32(o, path, "bevel_segments", segments_default, 8),
        bevel_profile,
        detail: r.f32(o, path, "detail", d.detail),
        tracking: r.f32(o, path, "tracking", d.tracking),
        line_height: r.f32(o, path, "line_height", d.line_height),
        align,
        wrap,
        voxels: if r.has(o, "voxels") { Some((r.u32(o, path, "voxels", 12, 40).max(4), r.u32(o, path, "voxel_layers", 2, 12).max(1))) } else { None },
    };
    let (params_size, params_depth) = (params.size, params.depth);
    let text = match build_text3d(&params) {
        Ok(t) => t,
        Err(e) => {
            let loc = r.loc(o);
            let fonts: Vec<String> = BUNDLED_FONTS.iter().map(|n| format!("@{n}")).collect();
            r.error(loc, path, format!("the text did not build: {e}"), Some(format!("bundled fonts: {}", fonts.join(" "))));
            return None;
        }
    };
    for w in &text.warnings {
        let loc = r.loc(o);
        r.warn(loc, path, w.clone(), None);
    }
    let mut materials: [Option<MaterialDef>; 5] = [Some(parse_material(r, cx, r.get(o, "material"), &format!("{path}.material"))), None, None, None, None];
    for (slot, field) in [(SIDE, "side_material"), (BEVEL, "bevel_material"), (BACK, "back_material"), (TILE, "tile_material")] {
        if r.has(o, field) {
            materials[slot] = Some(parse_material(r, cx, r.get(o, field), &format!("{path}.{field}")));
        }
    }
    let by = match r.id(o, "by").as_deref() {
        None | Some("letter") => Text3dUnit::Letter,
        Some("word") => Text3dUnit::Word,
        Some("line") => Text3dUnit::Line,
        Some("whole") | Some("all") => Text3dUnit::Whole,
        Some("piece") | Some("shard") => Text3dUnit::Piece,
        Some(other) => {
            let loc = r.loc(o);
            r.warn(loc, &format!("{path}.by"), format!("`by: @{other}` is not one of {}", Text3dUnit::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" ")), None);
            Text3dUnit::Letter
        }
    };
    let seed = r.f32(o, path, "seed", 1.0) as u64;
    let pieces_per_letter = r.u32(o, path, "pieces", 1, 64).max(1) as usize;
    if by == Text3dUnit::Piece && pieces_per_letter < 2 && !r.has(o, "voxels") {
        let loc = r.loc(o);
        r.warn(loc, &format!("{path}.by"), "`by: @piece` moves the shards of `pieces: n` (n ≥ 2) or the cubes of `voxels`; otherwise it is `@letter`", Some("pieces: 6".into()));
    }
    let slot_of = |part: TextPart| -> usize {
        let slot = match part {
            TextPart::Front => FRONT,
            TextPart::Side => SIDE,
            TextPart::Bevel => BEVEL,
            TextPart::Back => BACK,
        };
        if materials[slot].is_some() { slot } else { FRONT }
    };
    let mut pieces = split_pieces(&text, pieces_per_letter, seed, &slot_of);
    let size = params_size;
    // Neon tubes: a round tube along each letter's centre lines (or its
    // outline) instead of the extruded solid.
    let tube = r.f32(o, path, "tube", 0.0) * size;
    if tube > 0.0 {
        let outline = matches!(r.id(o, "tube_path").as_deref(), Some("outline"));
        let sides = r.u32(o, path, "tube_sides", 10, 24);
        pieces = text
            .letters
            .iter()
            .enumerate()
            .map(|(li, l)| {
                let paths: Vec<(Vec<[f32; 2]>, bool)> = if outline {
                    outline_loops(l).into_iter().map(|lp| (lp, true)).collect()
                } else {
                    centre_lines(&ink_grid(l, 0.018 * size), 0.09 * size)
                };
                Piece { letter: li, pivot: l.pivot, slots: vec![(FRONT, SharedMesh::new(tube_mesh(&paths, tube, sides)))] }
            })
            .collect();
    }
    // Tiles: a rounded slab behind every word, moving with the word.
    let tiles = r.f32(o, path, "tiles", 0.0) * size;
    let mut tile_rects: Vec<(usize, [f32; 3], [f32; 2], f32)> = Vec::new();
    if tiles > 0.0 {
        let tile_depth = r.f32(o, path, "tile_depth", 0.25) * size;
        let radius = r.f32(o, path, "tile_radius", 0.12) * size;
        for word in 0..text.words.max(1) {
            let ls: Vec<(usize, &Letter)> = text.letters.iter().enumerate().filter(|(_, l)| l.word == word).collect();
            let Some(&(first, _)) = ls.first() else { continue };
            let (mut a, mut b) = ([f32::MAX; 2], [f32::MIN; 2]);
            for (_, l) in &ls {
                if let Some((mn, mx)) = l.mesh.bounds() {
                    a = [a[0].min(mn[0] + l.pivot[0]), a[1].min(mn[1] + l.pivot[1])];
                    b = [b[0].max(mx[0] + l.pivot[0]), b[1].max(mx[1] + l.pivot[1])];
                }
            }
            let dims = [b[0] - a[0] + 2.0 * tiles, b[1] - a[1] + 2.0 * tiles];
            let centre = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, -(params_depth * 0.5 + tile_depth * 0.5) + 0.01 * size];
            let slot = if materials[TILE].is_some() { TILE } else { FRONT };
            let mesh = BoxGeo { size: [dims[0], dims[1], tile_depth], radius: radius.min(dims[0].min(dims[1]) * 0.45), radius_segments: 3, ..BoxGeo::default() }.build();
            pieces.push(Piece { letter: first, pivot: centre, slots: vec![(slot, SharedMesh::new(mesh))] });
            tile_rects.push((first, centre, dims, tile_depth));
        }
    }
    let welded = if pieces_per_letter > 1 { split_pieces(&text, 1, seed, &slot_of) } else { Vec::new() };
    let text_path = if r.has(o, "path") {
        let curve = parse_curve(r, r.get(o, "path"), &format!("{path}.path"))?;
        Some(TextPath {
            arc: curve.arc(),
            offset: r.anim::<1>(o, path, "path_offset", [0.0], Semantic::Plain),
            start: r.f32(o, path, "path_start", 0.0).clamp(0.0, 1.0),
            align: r.bool(o, "path_align", true),
        })
    } else {
        None
    };
    let layout = parse_layout_kind(r, o, path, &text);
    let arc = arc_positions(&text);
    let voxels = if text.voxels.is_empty() {
        None
    } else {
        let slot = |glyph: usize| text.letters.iter().position(|l| l.index == glyph).unwrap_or(0);
        let h = text.voxel_half;
        Some(VoxelDraw {
            cube: SharedMesh::new(BoxGeo { size: [h[0] * 1.84, h[1] * 1.84, h[2] * 1.84], radius: h[0] * 0.12, radius_segments: 2, ..BoxGeo::default() }.build()),
            cubes: text.voxels.iter().map(|v| (slot(v.glyph), v.center)).collect(),
        })
    };
    // LED face: the letters as lit dots on a lattice; `led_unlit` also
    // draws the whole board's dark dots behind.
    let mut led_unlit = None;
    let voxels = if r.has(o, "led") {
        let pitch = r.f32(o, path, "led", 0.08).max(0.01) * params_size;
        let dot = r.f32(o, path, "led_size", 0.38).clamp(0.05, 0.7) * pitch;
        let mesh = SharedMesh::new(scale_mesh(SphereGeo { radius: 1.0, width_segments: 10, height_segments: 6, ..SphereGeo::default() }.build(), [dot, dot, dot * 0.45]));
        let mut cells = Vec::new();
        for (li, l) in text.letters.iter().enumerate() {
            for d in led_dots(l, pitch) {
                cells.push((li, [l.pivot[0] + d[0], l.pivot[1] + d[1], l.pivot[2]]));
            }
        }
        let unlit = r.f32(o, path, "led_unlit", 0.0);
        if unlit > 0.0 {
            // The board: every lattice point of the block (one line of
            // margin) at the letters' lattice phase.
            let (mn, mx) = text.bounds;
            let mut dots = Vec::new();
            let (x0, y0) = ((mn[0] / pitch).floor() - 1.0, (mn[1] / pitch).floor() - 1.0);
            let (x1, y1) = ((mx[0] / pitch).ceil() + 1.0, (mx[1] / pitch).ceil() + 1.0);
            let mut y = y0;
            while y <= y1 {
                let mut x = x0;
                while x <= x1 {
                    dots.push((0usize, [x * pitch, y * pitch, -dot * 0.6]));
                    x += 1.0;
                }
                y += 1.0;
            }
            led_unlit = Some((VoxelDraw { cube: mesh.clone(), cubes: dots }, unlit));
        }
        Some(VoxelDraw { cube: mesh, cubes: cells })
    } else {
        voxels
    };
    let particles = if r.has(o, "particles") {
        let count = r.u32(o, path, "particles", 400, 200_000).max(1) as usize;
        let material = if r.has(o, "particle_material") { Some(parse_material(r, cx, r.get(o, "particle_material"), &format!("{path}.particle_material"))) } else { None };
        Some(ParticleDef {
            mesh: SharedMesh::new(SphereGeo { radius: 1.0, width_segments: 6, height_segments: 4, ..SphereGeo::default() }.build()),
            points: if matches!(r.id(o, "particle_source").as_deref(), Some("tiles")) && !tile_rects.is_empty() {
                sample_tile_edges(&tile_rects, count, seed)
            } else {
                sample_faces(&text, count, seed)
            },
            size: r.anim::<1>(o, path, "particle_size", [0.02], Semantic::Plain),
            spread: r.anim::<3>(o, path, "particle_spread", [0.0; 3], Semantic::Plain),
            opacity: r.anim::<1>(o, path, "particle_opacity", [1.0], Semantic::Plain),
            stagger: r.f32(o, path, "particle_stagger", 0.5),
            material,
        })
    } else {
        None
    };
    let def = TextDef {
        text: Arc::new(text),
        pieces,
        welded,
        materials,
        by,
        stagger: r.f32(o, path, "stagger", 0.04),
        order: parse_order(r, o, path),
        each: parse_each(r, o, path),
        each_fn: r.closure(o, "each"),
        spread: parse_offsets(r, o, path, "spread"),
        alternate: parse_offsets(r, o, path, "alternate"),
        path: text_path,
        layout,
        arc,
        voxels,
        led_unlit,
        particles,
        particle_sync: r.bool(o, "particle_sync", false),
        flicker: (r.f32(o, path, "flicker", 0.0).clamp(0.0, 1.0), r.f32(o, path, "flicker_speed", 9.0).max(0.1)),
        karaoke: if r.has(o, "karaoke") {
            let words: Vec<[f32; 2]> = r.list(r.get(o, "karaoke")).into_iter().filter_map(|v| r.plain::<2>(v, Semantic::Plain)).collect();
            let colour = |r: &mut Reader, name: &str, d: [f32; 4]| r.plain::<4>(r.get(o, name), Semantic::Color).unwrap_or(d);
            let mut ghost = colour(r, "karaoke_ghost", [0.5, 0.5, 0.5, 1.0]);
            // A ghost without its own alpha is see-through.
            if !r.has(o, "karaoke_ghost") {
                ghost[3] = 0.3;
            }
            Some(Karaoke { words, ghost, current: colour(r, "karaoke_current", [1.0, 0.25, 0.02, 1.0]), sung: colour(r, "karaoke_sung", [1.0; 4]) })
        } else {
            None
        },
        seed,
        deform: parse_deform(r, o, path),
    };
    Some(base_node(r, cx, o, path, NodeKind::Text3D(Box::new(def))))
}

fn parse_font(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str) -> Option<FontSource> {
    let fv = r.get(o, "font");
    let load = |r: &mut Reader, cx: &mut LoadCx, m: MediaRef| match cx.resolver.load(&m) {
        Ok(b) => Some(FontSource::Bytes(b)),
        Err(e) => {
            let loc = r.loc(o);
            r.error(loc, &format!("{path}.font"), format!("the font did not load: {e}"), None);
            None
        }
    };
    if fv.is_nil() {
        Some(FontSource::Bundled("bold".into()))
    } else if let Some(name) = fv.as_id().map(|i| i.to_string()) {
        Some(FontSource::Bundled(name))
    } else if fv.is_string_like() {
        let s = r.string_of(fv).unwrap_or_default();
        if s.contains('.') || s.contains('/') {
            load(r, cx, MediaRef::File(s))
        } else {
            Some(FontSource::Bundled(s))
        }
    } else {
        let m = r.media(o, path, "font")?;
        load(r, cx, m)
    }
}

fn parse_offsets(r: &mut Reader, o: ScriptObject, path: &str, field: &str) -> UnitOffsets {
    let mut out = UnitOffsets::default();
    let Some(e) = r.get(o, field).as_object() else { return out };
    let p = format!("{path}.{field}");
    r.check_fields(e, &p, field, &["position", "rotation", "scale"]);
    if r.has(e, "position") {
        out.position = Some(r.anim::<3>(e, &p, "position", [0.0; 3], Semantic::Plain));
    }
    if r.has(e, "rotation") {
        out.rotation = Some(r.anim::<3>(e, &p, "rotation", [0.0; 3], Semantic::Plain));
    }
    if r.has(e, "scale") {
        out.scale = Some(r.anim::<1>(e, &p, "scale", [0.0], Semantic::Plain));
    }
    out
}

/// The pieces to draw: each letter whole, or cut into `n` shards (its
/// triangles grouped by the nearest of `n` seeded points in its outline
/// box), each shard around its own centre.
fn split_pieces(text: &Text3d, n: usize, seed: u64, slot_of: &dyn Fn(TextPart) -> usize) -> Vec<Piece> {
    let mut out = Vec::new();
    for (li, letter) in text.letters.iter().enumerate() {
        // Parts merged per material slot, relative to the letter's pivot.
        let mut slots: Vec<(usize, Mesh)> = Vec::new();
        for (part, mesh) in &letter.parts {
            let slot = slot_of(*part);
            match slots.iter_mut().find(|(s, _)| *s == slot) {
                Some((_, m)) => m.append(mesh),
                None => slots.push((slot, mesh.clone())),
            }
        }
        if n < 2 {
            out.push(Piece { letter: li, pivot: letter.pivot, slots: slots.into_iter().map(|(s, m)| (s, SharedMesh::new(m))).collect() });
            continue;
        }
        let Some((mn, mx)) = letter.mesh.bounds() else { continue };
        let seeds: Vec<[f32; 2]> = (0..n)
            .map(|k| {
                let h = (li as u64) << 16 | k as u64;
                [mn[0] + (mx[0] - mn[0]) * rand01(seed ^ 0x5eed, h * 2), mn[1] + (mx[1] - mn[1]) * rand01(seed ^ 0x5eed, h * 2 + 1)]
            })
            .collect();
        let nearest = |p: [f32; 2]| -> usize {
            let mut best = (f32::MAX, 0);
            for (k, s) in seeds.iter().enumerate() {
                let d = (p[0] - s[0]).powi(2) + (p[1] - s[1]).powi(2);
                if d < best.0 {
                    best = (d, k);
                }
            }
            best.1
        };
        // shard -> slot -> mesh
        let mut shards: Vec<Vec<(usize, Mesh)>> = vec![Vec::new(); n];
        for (slot, mesh) in &slots {
            let mut per: Vec<(Mesh, Vec<u32>)> = (0..n).map(|_| (Mesh::new(), vec![u32::MAX; mesh.positions.len()])).collect();
            for tri in mesh.indices.chunks_exact(3) {
                let c = tri.iter().fold([0.0f32; 2], |a, &i| {
                    let p = mesh.positions[i as usize];
                    [a[0] + p[0] / 3.0, a[1] + p[1] / 3.0]
                });
                let (m, remap) = &mut per[nearest(c)];
                let mut ids = [0u32; 3];
                for (j, &i) in tri.iter().enumerate() {
                    let i = i as usize;
                    if remap[i] == u32::MAX {
                        remap[i] = m.vertex(mesh.positions[i], mesh.normals[i], mesh.uvs.get(i).copied().unwrap_or([0.0; 2]));
                    }
                    ids[j] = remap[i];
                }
                m.tri(ids[0], ids[1], ids[2]);
            }
            for (k, (m, _)) in per.into_iter().enumerate() {
                if !m.is_empty() {
                    shards[k].push((*slot, m));
                }
            }
        }
        for mut shard in shards.into_iter().filter(|s| !s.is_empty()) {
            let (mut a, mut b) = ([f32::MAX; 3], [f32::MIN; 3]);
            for (_, m) in &shard {
                if let Some((p, q)) = m.bounds() {
                    for k in 0..3 {
                        a[k] = a[k].min(p[k]);
                        b[k] = b[k].max(q[k]);
                    }
                }
            }
            let c = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, 0.0];
            for (_, m) in &mut shard {
                m.positions.iter_mut().for_each(|p| *p = sub3(*p, c));
            }
            out.push(Piece { letter: li, pivot: add3(letter.pivot, c), slots: shard.into_iter().map(|(s, m)| (s, SharedMesh::new(m))).collect() });
        }
    }
    out
}

/// A signed random in −1..1 per unit and axis.
fn signed(seed: u64, unit: usize, axis: u64) -> f32 {
    rand01(seed ^ 0x9e37_79b9, unit as u64 * 4 + axis) * 2.0 - 1.0
}

fn parse_layout_kind(r: &mut Reader, o: ScriptObject, path: &str, text: &Text3d) -> SpaceLayout {
    let (w, _) = ((text.bounds.1[0] - text.bounds.0[0]).max(0.1), ());
    let radius = if r.has(o, "layout_radius") { Some(r.f32(o, path, "layout_radius", 1.0).max(0.05)) } else { None };
    let pitch = if r.has(o, "layout_pitch") { Some(r.f32(o, path, "layout_pitch", 1.0)) } else { None };
    let n = text.letters.len().max(1);
    match r.id(o, "layout").as_deref() {
        None | Some("block") | Some("line") => SpaceLayout::Block,
        // vj_fx: R = row width / τ · 1.05, so the ring closes with a gap.
        Some("ring") => SpaceLayout::Ring { radius: radius.unwrap_or(w / std::f32::consts::TAU * 1.05) },
        // vj_fx: about three turns, 1.6 cap heights per turn.
        Some("helix") => {
            let len = arc_positions(text).last().copied().unwrap_or(w) + 1.0;
            SpaceLayout::Helix { radius: radius.unwrap_or((len / (3.0 * std::f32::consts::TAU)).max(1.5)), pitch: pitch.unwrap_or(1.6) }
        }
        // vj_fx: ⌈√N⌉ columns, 1.5 cap heights apart.
        Some("grid") => {
            let cols = if r.has(o, "grid_cols") { r.u32(o, path, "grid_cols", 4, 1000).max(1) as usize } else { (n as f32).sqrt().ceil() as usize };
            SpaceLayout::Grid { cols, pitch: pitch.unwrap_or(1.5) }
        }
        // vj_fx: the longest word closes its ring, never under 2 cap heights.
        Some("tunnel") => {
            let mut longest = 0.0f32;
            for word in 0..text.words.max(1) {
                let xs: Vec<&Letter> = text.letters.iter().filter(|l| l.word == word).collect();
                if let (Some(a), Some(b)) = (xs.first(), xs.last()) {
                    longest = longest.max(b.pivot[0] - a.pivot[0] + 0.6);
                }
            }
            SpaceLayout::Tunnel { radius: radius.unwrap_or((longest / std::f32::consts::TAU * 1.05).max(2.0)), pitch: pitch.unwrap_or(4.0) }
        }
        // A drum just wide enough for its lines.
        // By default the arc spans about 120°.
        Some("arc") => SpaceLayout::Arc { radius: radius.unwrap_or((w / (std::f32::consts::TAU / 3.0)).max(1.0)) },
        Some("reel") => SpaceLayout::Reel { radius: radius.unwrap_or((text.lines.max(1) as f32 * 1.35 / std::f32::consts::TAU).max(0.6)) },
        Some(other) => {
            let loc = r.loc(o);
            r.warn(loc, &format!("{path}.layout"), format!("no layout `@{other}`"), Some(format!("one of {}", SpaceLayout::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
            SpaceLayout::Block
        }
    }
}

/// Per letter slot: its distance along the whole text, lines end to end
/// with half a cap height between them.
fn arc_positions(text: &Text3d) -> Vec<f32> {
    let mut out = Vec::with_capacity(text.letters.len());
    let mut offset = 0.0;
    let mut line = usize::MAX;
    let mut line_left = 0.0;
    let mut line_right = 0.0f32;
    for l in &text.letters {
        if l.line != line {
            if line != usize::MAX {
                offset += line_right - line_left + 0.5;
            }
            line = l.line;
            line_left = l.pivot[0];
            line_right = l.pivot[0];
        }
        line_right = line_right.max(l.pivot[0]);
        out.push(offset + l.pivot[0] - line_left);
    }
    out
}

/// `m` scaled per axis (normals follow the inverse scale).
fn scale_mesh(mut m: Mesh, s: [f32; 3]) -> Mesh {
    for p in &mut m.positions {
        *p = [p[0] * s[0], p[1] * s[1], p[2] * s[2]];
    }
    for n in &mut m.normals {
        *n = normalize_or_up([n[0] / s[0], n[1] / s[1], n[2] / s[2]]);
    }
    m
}

/// `count` points along the bottom and side edges of the word tiles, at
/// their front face (where a stamped tile kicks up its dust).
fn sample_tile_edges(tiles: &[(usize, [f32; 3], [f32; 2], f32)], count: usize, seed: u64) -> Vec<(usize, [f32; 3])> {
    (0..count)
        .map(|j| {
            let (li, c, dims, depth) = tiles[j % tiles.len()];
            let u = rand01(seed ^ 0xd057, j as u64 * 2);
            let v = rand01(seed ^ 0xd057, j as u64 * 2 + 1);
            let perim = dims[0] + dims[1];
            let d = u * perim;
            let (x, y) = if d < dims[0] { (d - dims[0] * 0.5, -dims[1] * 0.5) } else if v < 0.5 { (-dims[0] * 0.5, (d - dims[0]) / dims[1] * dims[1] * 0.6 - dims[1] * 0.5) } else { (dims[0] * 0.5, (d - dims[0]) / dims[1] * dims[1] * 0.6 - dims[1] * 0.5) };
            (li, [c[0] + x, c[1] + y, c[2] - depth * 0.5 + v * depth])
        })
        .collect()
}

/// `count` points on the letters' front caps, spread by area (seeded).
fn sample_faces(text: &Text3d, count: usize, seed: u64) -> Vec<(usize, [f32; 3])> {
    let mut tris: Vec<(usize, [[f32; 3]; 3], f32)> = Vec::new();
    for (li, l) in text.letters.iter().enumerate() {
        for (part, m) in &l.parts {
            if *part != TextPart::Front {
                continue;
            }
            for t in m.indices.chunks_exact(3) {
                let p = [m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]].map(|q| add3(q, l.pivot));
                let area = len3(cross3(sub3(p[1], p[0]), sub3(p[2], p[0]))) * 0.5;
                if area > 0.0 {
                    tris.push((li, p, area));
                }
            }
        }
    }
    let total: f32 = tris.iter().map(|t| t.2).sum();
    if tris.is_empty() || total <= 0.0 {
        return Vec::new();
    }
    let mut cumulative = Vec::with_capacity(tris.len());
    let mut acc = 0.0;
    for t in &tris {
        acc += t.2 / total;
        cumulative.push(acc);
    }
    (0..count)
        .map(|j| {
            let pick = rand01(seed ^ 0x7a11, j as u64 * 3);
            let k = cumulative.partition_point(|c| *c < pick).min(tris.len() - 1);
            let (li, p, _) = tris[k];
            let (mut a, mut b) = (rand01(seed ^ 0x7a11, j as u64 * 3 + 1), rand01(seed ^ 0x7a11, j as u64 * 3 + 2));
            if a + b > 1.0 {
                a = 1.0 - a;
                b = 1.0 - b;
            }
            let q = add3(p[0], add3(scale3(sub3(p[1], p[0]), a), scale3(sub3(p[2], p[0]), b)));
            (li, q)
        })
        .collect()
}

/// A letter's frame in text space: where its rest centre stands and how it
/// is turned, for the layout (or the path).
fn letter_frame(td: &TextDef, li: usize, path_offset: f32) -> Mat4f {
    let text = &td.text;
    let l = &text.letters[li];
    let deg = 180.0 / std::f32::consts::PI;
    if let Some(p) = &td.path {
        let len = p.arc.length().max(1e-6);
        let s = p.start * len + (l.pivot[0] - text.bounds.0[0]) + path_offset;
        let closed = p.arc.tangent_at(0.0) == p.arc.tangent_at(1.0) && len > 0.0;
        let uu = if closed { (s / len).rem_euclid(1.0) } else { (s / len).clamp(0.0, 1.0) };
        let pos = p.arc.point_at(uu);
        let q = if p.align {
            let tangent = p.arc.tangent_at(uu);
            let up0 = if tangent[1].abs() > 0.99 { [0.0, 0.0, 1.0] } else { [0.0, 1.0, 0.0] };
            let z = normalize_or_up(cross3(tangent, up0));
            let y = normalize_or_up(cross3(z, tangent));
            look_rotation(z, y)
        } else {
            [0.0, 0.0, 0.0, 1.0]
        };
        // The letter's height above the line rides on the path's up.
        let lift = transform_dir(&compose([0.0; 3], q, [1.0; 3]), [0.0, l.pivot[1], l.pivot[2]]);
        return compose(add3(pos, lift), q, [1.0; 3]);
    }
    let line_y = text.line_centers.get(l.line).map(|c| c[1]).unwrap_or(0.0);
    match td.layout {
        SpaceLayout::Block => compose(l.pivot, [0.0, 0.0, 0.0, 1.0], [1.0; 3]),
        SpaceLayout::Ring { radius } => {
            let th = l.pivot[0] / radius;
            compose([radius * th.sin(), l.pivot[1], radius * th.cos()], quat_from_euler_deg([0.0, th * deg, 0.0]), [1.0; 3])
        }
        SpaceLayout::Helix { radius, pitch } => {
            let total = td.arc.last().copied().unwrap_or(0.0);
            let th = (td.arc.get(li).copied().unwrap_or(0.0) - total * 0.5) / radius;
            let y = l.pivot[1] - line_y - pitch * th / std::f32::consts::TAU;
            compose([radius * th.sin(), y, radius * th.cos()], quat_from_euler_deg([0.0, th * deg, 0.0]), [1.0; 3])
        }
        SpaceLayout::Grid { cols, pitch } => {
            let n = text.letters.len();
            let rows = n.div_ceil(cols.max(1)).max(1);
            let (row, col) = (li / cols.max(1), li % cols.max(1));
            let x = (col as f32 - (cols.min(n) as f32 - 1.0) * 0.5) * pitch;
            let z = (row as f32 - (rows as f32 - 1.0) * 0.5) * pitch;
            compose([x, 0.0, z], quat_from_euler_deg([-90.0, 0.0, 0.0]), [1.0; 3])
        }
        SpaceLayout::Arc { radius } => {
            let (r0, up) = (radius.abs().max(0.05), radius >= 0.0);
            let lift = l.pivot[1] - line_y;
            let th = l.pivot[0] / r0;
            if up {
                let r = r0 + lift;
                compose([r * th.sin(), r * th.cos() - r0, l.pivot[2]], quat_from_euler_deg([0.0, 0.0, -th * deg]), [1.0; 3])
            } else {
                let r = r0 - lift;
                compose([r * th.sin(), -r * th.cos() + r0, l.pivot[2]], quat_from_euler_deg([0.0, 0.0, th * deg]), [1.0; 3])
            }
        }
        SpaceLayout::Reel { radius } => {
            let lines = text.lines.max(1) as f32;
            let th = l.line as f32 / lines * std::f32::consts::TAU;
            let dy = l.pivot[1] - line_y;
            let r = radius + l.pivot[2];
            compose([l.pivot[0], -r * th.sin() + dy * th.cos(), r * th.cos() + dy * th.sin()], quat_from_euler_deg([th * deg, 0.0, 0.0]), [1.0; 3])
        }
        SpaceLayout::Tunnel { radius, pitch } => {
            let word_x = text.word_centers.get(l.word).map(|c| c[0]).unwrap_or(0.0);
            let phi = l.word as f32 * 2.39996 - (l.pivot[0] - word_x) / radius;
            let r = radius + (l.pivot[1] - line_y);
            compose([r * phi.cos(), r * phi.sin(), -(l.word as f32) * pitch], quat_from_euler_deg([0.0, 0.0, phi * deg - 90.0]), [1.0; 3])
        }
    }
}

pub(crate) fn eval(w: &mut Walk, n: &NodeDef, td: &TextDef, world: &Mat4f, opacity: f32) {
    let t = w.t;
    let text = &td.text;
    let letters = &text.letters;
    let block = td.layout == SpaceLayout::Block && td.path.is_none();
    let block_centre = [(text.bounds.0[0] + text.bounds.1[0]) * 0.5, (text.bounds.0[1] + text.bounds.1[1]) * 0.5, 0.0];
    // What is drawn: letters or their shards, or voxel cubes. (letter slot,
    // rest centre) per item.
    let items: Vec<(usize, [f32; 3])> = match &td.voxels {
        Some(v) => v.cubes.clone(),
        None => td.pieces.iter().map(|p| (p.letter, p.pivot)).collect(),
    };
    let unit_of = |k: usize| -> usize {
        let l = &letters[items[k].0];
        match td.by {
            Text3dUnit::Letter => l.index,
            Text3dUnit::Word => l.word,
            Text3dUnit::Line => l.line,
            Text3dUnit::Whole => 0,
            Text3dUnit::Piece => k,
        }
    };
    // A unit turns about its own centre; off the block (layouts, paths)
    // letters, words and lines turn about each letter's own centre.
    let centre_of = |k: usize| -> [f32; 3] {
        let (li, pivot) = items[k];
        let l = &letters[li];
        match td.by {
            Text3dUnit::Piece => pivot,
            _ if !block => l.pivot,
            Text3dUnit::Letter => l.pivot,
            Text3dUnit::Word => text.word_centers.get(l.word).copied().unwrap_or(l.pivot),
            Text3dUnit::Line => text.line_centers.get(l.line).copied().unwrap_or(l.pivot),
            Text3dUnit::Whole => block_centre,
        }
    };
    let units = (0..items.len()).map(unit_of).max().map(|m| m + 1).unwrap_or(0);
    let distance = if td.order == Order::Distance {
        let mut d = vec![0.0; units];
        for k in 0..items.len() {
            d[unit_of(k)] = len3(sub3(centre_of(k), block_centre));
        }
        Some(d)
    } else {
        None
    };
    let rank = ranks(td.order, units, td.seed, distance.as_deref());
    let mats: Vec<Option<MaterialFrame>> = td.materials.iter().map(|m| m.as_ref().map(|m| w.material(m, opacity))).collect();
    let deform = w.deform(&td.deform, &td.pieces.first().and_then(|p| p.slots.first()).map(|(_, m)| m.mesh.clone()).unwrap_or_default());
    // `each.emissive` is a colour; the face material's `emissive_intensity`
    // (default 1) makes it as bright as the material's own glow.
    let glow = td.materials[FRONT].as_ref().map(|m| w.a1(&m.emissive_intensity, t)).unwrap_or(1.0);
    let path_offset = td.path.as_ref().map(|p| w.a1(&p.offset, t)).unwrap_or(0.0);
    let frames: Vec<Mat4f> = (0..letters.len()).map(|li| letter_frame(td, li, path_offset)).collect();
    struct Job {
        k: usize,
        m: Mat4f,
        o: f32,
        lt: f32,
        fn_color: Option<[f32; 4]>,
        rest: bool,
    }
    let mut jobs: Vec<Job> = Vec::with_capacity(items.len());
    for k in 0..items.len() {
        let (li, pivot) = items[k];
        let u = unit_of(k);
        let centre = centre_of(k);
        let lt = t - rank.get(u).copied().unwrap_or(0.0) * td.stagger;
        let mut dp = td.each.position.as_ref().map(|a| w.a3(a, lt)).unwrap_or([0.0; 3]);
        let mut dr = td.each.rotation.as_ref().map(|a| w.a3(a, lt)).unwrap_or([0.0; 3]);
        let mut ds = td.each.scale.as_ref().map(|a| w.a3(a, lt)).unwrap_or([1.0; 3]);
        let o = td.each.opacity.as_ref().map(|a| w.a1(a, lt)).unwrap_or(1.0).clamp(0.0, 1.0);
        let rnd = [signed(td.seed, u, 0), signed(td.seed, u, 1), signed(td.seed, u, 2)];
        if let Some(a) = &td.spread.position {
            let s = w.a3(a, lt);
            dp = add3(dp, [s[0] * rnd[0], s[1] * rnd[1], s[2] * rnd[2]]);
        }
        if let Some(a) = &td.spread.rotation {
            let s = w.a3(a, lt);
            dr = add3(dr, [s[0] * rnd[1], s[1] * rnd[2], s[2] * rnd[0]]);
        }
        if let Some(a) = &td.spread.scale {
            let s = 1.0 + w.a1(a, lt) * signed(td.seed, u, 3);
            ds = scale3(ds, s.max(0.0));
        }
        let mut fn_color = None;
        if let Some(f) = td.each_fn {
            let out = w.script.eval_each(f, u, units, lt, lt + w.edit_offset);
            if let Some(p) = out.position {
                dp = p;
            }
            if let Some(r) = out.rotation {
                dr = r;
            }
            if let Some(s) = out.scale {
                ds = s;
            }
            fn_color = out.color;
        }
        let sign = if u % 2 == 0 { 1.0 } else { -1.0 };
        if let Some(a) = &td.alternate.position {
            dp = add3(dp, scale3(w.a3(a, lt), sign));
        }
        if let Some(a) = &td.alternate.rotation {
            dr = add3(dr, scale3(w.a3(a, lt), sign));
        }
        if let Some(a) = &td.alternate.scale {
            ds = scale3(ds, (1.0 + w.a1(a, lt) * sign).max(0.0));
        }
        if o <= 0.0 || ds.iter().all(|s| s.abs() < 1e-6) {
            continue;
        }
        let l = &letters[li];
        let unit_frame = mul(&frames[li], &compose(sub3(centre, l.pivot), [0.0, 0.0, 0.0, 1.0], [1.0; 3]));
        let m = mul(world, &mul(&unit_frame, &compose(dp, quat_from_euler_deg(dr), ds)));
        let m = mul(&m, &compose(sub3(pivot, centre), [0.0, 0.0, 0.0, 1.0], [1.0; 3]));
        let rest = dp == [0.0; 3] && dr == [0.0; 3] && ds == [1.0; 3] && o >= 1.0;
        jobs.push(Job { k, m, o, lt, fn_color, rest });
    }
    // The face's colour and glow at a unit's time.
    // A failing letter (`flicker`): out for random steps of 1/speed s.
    let failing = |li: usize| -> bool {
        let (share, speed) = td.flicker;
        if share <= 0.0 || rand01(td.seed ^ 0xf11c, li as u64) >= share {
            return false;
        }
        let step = (t * speed).floor().max(0.0) as u64;
        rand01(td.seed ^ 0xf11d ^ (li as u64) << 20, step) < 0.3
    };
    let face = |w: &mut Walk, base: &MaterialFrame, lt: f32, fn_color: Option<[f32; 4]>, li: usize| -> MaterialFrame {
        let mut mat = base.clone();
        if failing(li) {
            mat.emissive = [0.0; 3];
            mat.color = [mat.color[0] * 0.3, mat.color[1] * 0.3, mat.color[2] * 0.3, mat.color[3]];
            return mat;
        }
        if let Some(c) = &td.each.color {
            mat.color = w.a4(c, lt);
        }
        if let Some(c) = fn_color {
            mat.color = c;
        }
        if let Some(e) = &td.each.emissive {
            let e = w.a4(e, lt);
            mat.emissive = [e[0] * glow, e[1] * glow, e[2] * glow];
        }
        mat
    };
    if let Some(v) = &td.voxels {
        // Every cube in one draw; colour as a multiplier of the face's.
        let Some(base) = mats[FRONT].clone() else { return };
        let mut inst = Vec::with_capacity(jobs.len());
        let mut mat = base.clone();
        for j in &jobs {
            let f = face(w, &base, j.lt, j.fn_color, items[j.k].0);
            let c = [f.color[0] / base.color[0].max(1e-4), f.color[1] / base.color[1].max(1e-4), f.color[2] / base.color[2].max(1e-4), 1.0];
            // The face's emission, per cube: as strong as its brightest
            // channel, in the cube's own colour.
            let glow = f.emissive[0].max(f.emissive[1]).max(f.emissive[2]);
            if j.o < 1.0 {
                mat.transparent = true;
            }
            inst.push((j.m, c, j.o, glow));
        }
        if !inst.is_empty() {
            w.push_mesh(n, v.cube.mesh.clone(), v.cube.key, false, mat, deform, &inst);
        }
        if let Some((board, level)) = &td.led_unlit {
            // The dark dots: the face colour at `level`, unlit, static.
            let mut dim = base.clone();
            dim.color = [base.color[0] * level, base.color[1] * level, base.color[2] * level, 1.0];
            dim.emissive = [base.color[0] * level * 0.5, base.color[1] * level * 0.5, base.color[2] * level * 0.5];
            let inst: Vec<_> = board.cubes.iter().map(|(_, c)| (mul(world, &compose(*c, [0.0, 0.0, 0.0, 1.0], [1.0; 3])), [1.0; 4], 1.0, 0.0)).collect();
            w.push_mesh(n, board.cube.mesh.clone(), board.cube.key, false, dim, deform, &inst);
        }
    } else {
        // Letters whose shards have all landed are drawn whole.
        let mut landed = vec![!td.welded.is_empty(); letters.len()];
        let mut seen = vec![0usize; letters.len()];
        for j in &jobs {
            let l = td.pieces[j.k].letter;
            seen[l] += 1;
            landed[l] &= j.rest;
        }
        for (l, count) in seen.iter().enumerate() {
            let total = td.pieces.iter().filter(|p| p.letter == l).count();
            landed[l] &= *count == total && total > 0;
        }
        let mut drawn = vec![false; letters.len()];
        for j in jobs {
            let piece = &td.pieces[j.k];
            let (piece, m) = if landed[piece.letter] {
                if std::mem::replace(&mut drawn[piece.letter], true) {
                    continue;
                }
                let whole = &td.welded[piece.letter];
                (whole, mul(world, &frames[piece.letter]))
            } else {
                (piece, j.m)
            };
            for (slot, shared) in &piece.slots {
                let Some(base) = mats.get(*slot).and_then(|m| m.as_ref()).or(mats[FRONT].as_ref()) else { continue };
                // Colour and glow animate the face (the front slot).
                let mut mat = if *slot == FRONT { face(w, base, j.lt, j.fn_color, piece.letter) } else { base.clone() };
                mat.opacity *= j.o;
                if let Some(k) = &td.karaoke {
                    // The word's karaoke state: the face takes its colour
                    // and glow, every part its opacity.
                    let (c, glow_k, o) = k.state(letters[piece.letter].word, t);
                    if *slot == FRONT {
                        mat.color = c;
                        mat.emissive = [c[0] * glow_k * 0.8, c[1] * glow_k * 0.8, c[2] * glow_k * 0.8];
                    }
                    mat.opacity *= o;
                    if o < 1.0 {
                        mat.transparent = true;
                    }
                }
                w.push_mesh(n, shared.mesh.clone(), shared.key, false, mat, deform, &[(m, [1.0; 4], 1.0, 0.0)]);
            }
        }
    }
    // Each letter's unit rank (for synced particles).
    let letter_rank: Vec<f32> = (0..letters.len())
        .map(|li| {
            let l = &letters[li];
            let u = match td.by {
                Text3dUnit::Letter | Text3dUnit::Piece => l.index,
                Text3dUnit::Word => l.word,
                Text3dUnit::Line => l.line,
                Text3dUnit::Whole => 0,
            };
            rank.get(u).copied().unwrap_or(0.0)
        })
        .collect();
    if let Some(p) = &td.particles {
        let base = match &p.material {
            Some(m) => w.material(m, opacity),
            None => match mats[FRONT].clone() {
                Some(m) => m,
                None => return,
            },
        };
        let mut mat = base;
        mat.transparent = true;
        let mut inst = Vec::with_capacity(p.points.len());
        for (j, (li, q)) in p.points.iter().enumerate() {
            // Each particle on its own clock, in a seeded order.
            // Synced: on its letter's unit clock (a stamped word's dust),
            // spread by the particle stagger.
            let unit_t = if td.particle_sync { letter_rank.get(*li).copied().unwrap_or(0.0) * td.stagger } else { 0.0 };
            let lt = t - unit_t - rand01(td.seed ^ 0x9a27, j as u64) * p.stagger;
            let o = w.a1(&p.opacity, lt).clamp(0.0, 1.0);
            let size = w.a1(&p.size, lt);
            if o <= 0.0 || size <= 0.0 {
                continue;
            }
            let amount = w.a3(&p.spread, lt);
            let dir = [signed(td.seed ^ 0x51, j, 0), signed(td.seed ^ 0x51, j, 1), signed(td.seed ^ 0x51, j, 2)];
            let off = [amount[0] * dir[0], amount[1] * dir[1], amount[2] * dir[2]];
            let l = &letters[*li];
            let m = mul(world, &mul(&frames[*li], &compose(add3(sub3(*q, l.pivot), off), [0.0, 0.0, 0.0, 1.0], [size; 3])));
            inst.push((m, [1.0; 4], o, 0.0));
        }
        if !inst.is_empty() {
            w.push_mesh(n, p.mesh.mesh.clone(), p.mesh.key, false, mat, crate::eval::Deform::NONE, &inst);
        }
    }
}

fn transform_dir(m: &Mat4f, v: [f32; 3]) -> [f32; 3] {
    let p = transform_point(m, v);
    let o = transform_point(m, [0.0; 3]);
    sub3(p, o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Text3d {
        build_text3d(&Text3dParams { text: s.into(), bevel: 0.03, ..Default::default() }).unwrap()
    }

    #[test]
    fn shards_cover_their_letter() {
        let t = text("AB");
        let pieces = split_pieces(&t, 5, 3, &|_| FRONT);
        assert!(pieces.len() >= 6 && pieces.len() <= 10, "{}", pieces.len());
        for li in 0..2 {
            let tris: usize = pieces.iter().filter(|p| p.letter == li).flat_map(|p| p.slots.iter()).map(|(_, m)| m.mesh.triangle_count()).sum();
            assert_eq!(tris, t.letters[li].mesh.triangle_count());
        }
        // Shards put back at their pivots rebuild the letter's positions.
        let mut rebuilt: Vec<[i32; 3]> = pieces.iter().filter(|p| p.letter == 0).flat_map(|p| {
            let off = sub3(p.pivot, t.letters[0].pivot);
            p.slots.iter().flat_map(move |(_, m)| m.mesh.positions.iter().map(move |q| add3(*q, off)).collect::<Vec<_>>())
        }).map(|q| [(q[0] * 1e3).round() as i32, (q[1] * 1e3).round() as i32, (q[2] * 1e3).round() as i32]).collect();
        let mut orig: Vec<[i32; 3]> = t.letters[0].mesh.positions.iter().map(|q| [(q[0] * 1e3).round() as i32, (q[1] * 1e3).round() as i32, (q[2] * 1e3).round() as i32]).collect();
        rebuilt.sort();
        rebuilt.dedup();
        orig.sort();
        orig.dedup();
        assert_eq!(rebuilt, orig);
        // Deterministic.
        let again = split_pieces(&t, 5, 3, &|_| FRONT);
        assert_eq!(again.iter().map(|p| p.slots[0].1.key).collect::<Vec<_>>(), pieces.iter().map(|p| p.slots[0].1.key).collect::<Vec<_>>());
    }

    #[test]
    fn slots_follow_the_materials() {
        let t = text("H");
        let one = split_pieces(&t, 1, 1, &|_| FRONT);
        assert_eq!(one[0].slots.len(), 1);
        let split = split_pieces(&t, 1, 1, &|p| if p == TextPart::Side { SIDE } else { FRONT });
        assert_eq!(split[0].slots.iter().map(|s| s.0).collect::<Vec<_>>(), vec![FRONT, SIDE]);
    }

    #[test]
    fn each_closures_place_units() {
        use crate::anim::SyntheticMusic;
        use crate::doc::{FileResolver, LoadOptions, Scene3dDoc};
        use crate::eval::{EvalCache, EvalInput};
        let src = "use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"AB\" depth: 0.1 each: fn(i, n, t, edit) { return {position: vec3(0.0, i * 5.0 + t, 0.0)} }}] }";
        let resolver = FileResolver { base: std::path::PathBuf::from("/"), assets: Default::default(), library: None };
        let mut doc = Scene3dDoc::load(src, LoadOptions { name: "t".into(), inputs: Default::default(), resolver: &resolver, first_line: 1, origin_file: None, song: None }).map_err(|d| format!("{d:?}")).unwrap();
        let music = SyntheticMusic::default();
        let frame = doc.eval(&EvalInput { t: 1.0, aspect: 1.0, audio: &music, edit_offset: 0.0 }, &mut EvalCache::default());
        // Instance matrices' y translation (column-major: element 13).
        let mut ys: Vec<f32> = frame.meshes.iter().map(|m| m.instances[0].transform.v[13]).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(ys.len(), 2);
        assert!((ys[1] - ys[0] - 5.0).abs() < 0.3, "{ys:?}");
    }

    #[test]
    fn landed_shards_draw_as_whole_letters() {
        use crate::anim::SyntheticMusic;
        use crate::doc::{FileResolver, LoadOptions, Scene3dDoc};
        use crate::eval::{EvalCache, EvalInput};
        let src = "use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"AB\" pieces: 5 by: @piece stagger: 0.01 spread: {position: tween(vec3(3, 3, 3), vec3(0, 0, 0), 0, 1)}}] }";
        let resolver = FileResolver { base: std::path::PathBuf::from("/"), assets: Default::default(), library: None };
        let mut doc = Scene3dDoc::load(src, LoadOptions { name: "t".into(), inputs: Default::default(), resolver: &resolver, first_line: 1, origin_file: None, song: None }).map_err(|d| format!("{d:?}")).unwrap();
        let music = SyntheticMusic::default();
        let mut draws = |t: f32| doc.eval(&EvalInput { t, aspect: 1.0, audio: &music, edit_offset: 0.0 }, &mut EvalCache::default()).meshes.len();
        assert!(draws(0.5) >= 8, "flying shards draw apart");
        assert_eq!(draws(2.0), 2, "landed: one mesh per letter");
    }

    fn frame_of(src: &str, t: f32) -> crate::frame::SceneFrame {
        use crate::anim::SyntheticMusic;
        use crate::doc::{FileResolver, LoadOptions, Scene3dDoc};
        use crate::eval::{EvalCache, EvalInput};
        let resolver = FileResolver { base: std::path::PathBuf::from("/"), assets: Default::default(), library: None };
        let mut doc = Scene3dDoc::load(src, LoadOptions { name: "t".into(), inputs: Default::default(), resolver: &resolver, first_line: 1, origin_file: None, song: None }).map_err(|d| format!("{d:?}")).unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings.iter().map(|d| d.message.clone()).collect::<Vec<_>>());
        let music = SyntheticMusic::default();
        doc.eval(&EvalInput { t, aspect: 1.0, audio: &music, edit_offset: 0.0 }, &mut EvalCache::default())
    }

    #[test]
    fn space_layouts_place_letters() {
        let ring = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"ROUND AND ROUND\" layout: @ring layout_radius: 3}] }", 0.0);
        for m in &ring.meshes {
            let (x, z) = (m.instances[0].transform.v[12], m.instances[0].transform.v[14]);
            assert!(((x * x + z * z).sqrt() - 3.0).abs() < 0.05, "{x} {z}");
        }
        let grid = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"ABCDEFGHI\" layout: @grid}] }", 0.0);
        assert_eq!(grid.meshes.len(), 9);
        assert!(grid.meshes.iter().all(|m| m.instances[0].transform.v[13].abs() < 1e-4));
        let tunnel = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"ONE TWO\" layout: @tunnel}] }", 0.0);
        let zs: Vec<f32> = tunnel.meshes.iter().map(|m| m.instances[0].transform.v[14]).collect();
        assert!(zs.iter().any(|z| z.abs() < 1e-4) && zs.iter().any(|z| (*z + 4.0).abs() < 1e-3), "{zs:?}");
        let helix = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"UP AND AROUND THE COIL\" layout: @helix}] }", 0.0);
        let ys: Vec<f32> = helix.meshes.iter().map(|m| m.instances[0].transform.v[13]).collect();
        assert!(ys.first().unwrap() > ys.last().unwrap(), "the coil descends as it reads");
    }

    #[test]
    fn voxels_and_particles_draw_instanced() {
        let v = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"HI\" voxels: 10 by: @piece stagger: 0.001 each: {scale: tween(vec3(0, 0, 0), vec3(1, 1, 1), 0, 0.5)}}] }", 2.0);
        assert_eq!(v.meshes.len(), 1, "every cube in one draw");
        assert!(v.meshes[0].instance_count() > 40);
        let p = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"HI\" particles: 500 particle_spread: tween(vec3(3, 3, 3), vec3(0, 0, 0), 0, 1)}] }", 0.5);
        let dots = p.meshes.iter().find(|m| m.instance_count() == 500).expect("500 particles in one draw");
        assert!(dots.material.transparent);
        // Deterministic: the same frame twice.
        let q = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"HI\" particles: 500 particle_spread: tween(vec3(3, 3, 3), vec3(0, 0, 0), 0, 1)}] }", 0.5);
        let a: Vec<u32> = makepad_render::TransformTint::floats(&dots.instances).iter().map(|f| f.to_bits()).collect();
        let b: Vec<u32> = makepad_render::TransformTint::floats(&q.meshes.iter().find(|m| m.instance_count() == 500).unwrap().instances).iter().map(|f| f.to_bits()).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn tubes_tiles_leds_and_reels() {
        // Neon tubes: one mesh per letter, no extrusion parts.
        let t = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"OK\" font: @inter tube: 0.035 flicker: 1 flicker_speed: 10}] }", 0.35);
        assert_eq!(t.meshes.len(), 2);
        // Flicker: some step of the failing letters is dark, deterministic.
        let lit = |f: &crate::frame::SceneFrame| f.meshes.iter().filter(|m| m.material.color[0] > 0.5).count();
        let states: Vec<usize> = (0..20).map(|k| lit(&frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"OK\" font: @inter tube: 0.035 flicker: 1 flicker_speed: 10}] }", k as f32 * 0.1))).collect();
        assert!(states.iter().any(|s| *s < 2) && states.iter().any(|s| *s == 2), "{states:?}");
        // Tiles: one per word, drawn with the word.
        let tiles = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"S W E\" tiles: 0.15 by: @word tile_material: #333333 particles: 90 particle_source: @tiles particle_sync: true}] }", 0.5);
        assert_eq!(tiles.meshes.iter().filter(|m| m.material.color[0] < 0.1).count(), 3, "three tiles");
        // LED: one draw of lit dots, one of the board.
        let led = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"57.9\" led: 0.08 led_unlit: 0.12}] }", 0.0);
        assert_eq!(led.meshes.len(), 2);
        assert!(led.meshes[1].instance_count() > led.meshes[0].instance_count());
        // Reel: line k of n sits k/n of a turn down the drum.
        let reel = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"0\\n1\\n2\\n3\" layout: @reel layout_radius: 1}] }", 0.0);
        let front = reel.meshes.iter().filter(|m| (m.instances[0].transform.v[14] - 1.0).abs() < 0.05).count();
        assert_eq!(front, 1, "only line 0 faces the camera");
    }

    #[test]
    fn karaoke_and_arc() {
        let k = Karaoke { words: vec![[1.0, 2.0]], ghost: [0.5, 0.5, 0.5, 0.3], current: [1.0, 0.3, 0.0, 1.0], sung: [1.0; 4] };
        assert_eq!(k.state(0, 0.5), ([0.5, 0.5, 0.5, 0.3], 0.0, 0.3));
        assert_eq!(k.state(0, 1.5).0, [1.0, 0.3, 0.0, 1.0]);
        assert_eq!(k.state(0, 3.0).0, [1.0; 4]);
        assert_eq!(k.state(5, 0.0).0, [1.0; 4], "a word without a time is sung");
        let src = |t: f32| frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"ONE TWO\" by: @word karaoke: [vec2(0, 1), vec2(1, 2)] karaoke_current: #ff5a00}] }", t);
        let f = src(0.5);
        let opaque = f.meshes.iter().filter(|m| m.material.opacity >= 0.999).count();
        let ghost = f.meshes.iter().filter(|m| m.material.opacity < 0.5).count();
        assert!(opaque > 0 && ghost > 0, "word one sung, word two a ghost: {opaque} {ghost}");
        // Arc: letters on a circle about (0, -R), turned to it.
        let arc = frame_of("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Text3D{text: \"BADGE TEXT\" layout: @arc layout_radius: 3}] }", 0.0);
        for m in &arc.meshes {
            let (x, y) = (m.instances[0].transform.v[12], m.instances[0].transform.v[13] + 3.0);
            let r = (x * x + y * y).sqrt();
            assert!((r - 3.0).abs() < 0.8, "{r}");
        }
    }
}
