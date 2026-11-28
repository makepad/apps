//! The Force render mode in the plate view. The person looks at the real part, drawn as it always
//! is: [`Overlaid`] hands its shade the study's values and the loads' marks to read at the point it
//! hit (`mod.draw.PaintLookup`), and a click reads the point it hit on the part.
//!
//! Only the exaggerated deformation needs a mesh ([`AppState::force_deforms`]): the study's surface
//! in the part's place, coloured by the field the Force panel shows and deformed as far as it
//! asks (the part itself is not drawn under it, [`AppState::under_deformed`]). It goes into the
//! G-buffer as a print bead does, under its part's element and selection state (the composite
//! keeps its lit colour as is and outlines it with its part), and into the lit pass with its
//! colour ramp, picked regions striped in their kind's colour, under the parts' light rig. The
//! surface is in printer millimetres, Z up, as the study builds it; the shaders take it to the
//! passes' metres (and the lit pass's Y up) as the beads do.

use super::*;
use crate::ui::force::magnify;
use fab_force::display::Bins;
use fab_paint_field::bake::{Overlay, OverlayLattice, OVERLAY_RAMP};

script_mod! {
    use mod.prelude.fab.*
    use mod.math.*
    use mod.shader.*
    use mod.draw

    mod.fab_force_geom = {
        ForceVertex: #(force_vertex_pod(vm))
        ForceGeom: #(force_placeholder_geom(vm))
    }

    mod.draw.DrawFabForce = mod.std.set_type_default() do #(DrawFabForce::script_shader(vm)){
        alpha_blend: false
        depth_write: true
        cull_mode: @None
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_force_geom.ForceVertex, mod.fab_force_geom.ForceGeom)
        v_world: varying(vec3f)
        v_normal: varying(vec3f)
        v_view: varying(vec3f)
        v_vnormal: varying(vec3f)
        v_value: varying(float)
        v_tag: varying(float)
        // The parts' light rig, as the beads take it (`print::bind_light`, Z up).
        u_sun_dir: uniform(vec4(0.35, 0.0 - 0.45, 0.8, 1.0))
        u_sun_color: uniform(vec4(0.72, 0.72, 0.72, 1.0))
        u_sun_sky: uniform(vec4(0.28, 0.28, 0.28, 0.0))
        u_sun_ground: uniform(vec4(0.28, 0.28, 0.28, 0.0))

        // blue, cyan, green, yellow, red over 0..1
        ramp: fn(x: float) -> vec3f {
            let f = clamp(x, 0.0, 1.0) * 4.0
            return vec3(0.15, 0.30, 0.95).mix(vec3(0.10, 0.80, 0.85), clamp(f, 0.0, 1.0))
                .mix(vec3(0.20, 0.85, 0.25), clamp(f - 1.0, 0.0, 1.0))
                .mix(vec3(0.95, 0.85, 0.15), clamp(f - 2.0, 0.0, 1.0))
                .mix(vec3(0.95, 0.20, 0.15), clamp(f - 3.0, 0.0, 1.0))
        }

        // compression blue, nothing grey, tension red over -1..1
        diverge: fn(x: float) -> vec3f {
            let t = clamp(x, 0.0 - 1.0, 1.0)
            let mid = vec3(0.80, 0.80, 0.78)
            if t < 0.0 {
                return mid.mix(vec3(0.15, 0.35, 0.95), 0.0 - t)
            }
            return mid.mix(vec3(0.95, 0.20, 0.15), t)
        }

        kind_color: fn(tag: float) -> vec3f {
            if tag < 1.5 { return vec3(0.35, 0.55, 1.0) }
            if tag < 2.5 { return vec3(1.0, 0.35, 0.85) }
            if tag < 3.5 { return vec3(1.0, 0.65, 0.2) }
            return vec3(0.3, 1.0, 0.65)
        }

        vertex: fn() {
            let p = self.geom.pos + self.geom.disp * self.ctl.z
            self.v_world = p
            self.v_normal = self.geom.normal
            self.v_value = dot(self.geom.values, self.channel)
            self.v_tag = self.geom.tag
            // printer mm into the pass's metres; the lit pass is Y up
            var wp = p * 0.001
            var wn = self.geom.normal
            if self.space.x > 0.5 {
                wp = vec3(wp.x, wp.z, 0.0 - wp.y)
                wn = vec3(wn.x, wn.z, 0.0 - wn.y)
            }
            let view4 = self.draw_pass.camera_view * vec4(wp.x, wp.y, wp.z, 1.0)
            self.v_view = view4.xyz
            self.v_vnormal = (self.draw_pass.camera_view * vec4(wn.x, wn.y, wn.z, 0.0)).xyz
            self.vertex_pos = self.draw_pass.camera_projection * view4
        }

        // The field's colour (the key's display colour, as a reflectance) under the parts' light
        // rig, as a print bead is lit (`print::bind_light`): the heatmap reads as bright as the
        // parts around it in either lane.
        pixel: fn() -> vec4f {
            var n = normalize(self.v_normal)
            if dot(normalize(self.v_vnormal), self.v_view) > 0.0 {
                n = 0.0 - n
            }
            let t = self.v_value / max(self.ctl.x, 0.000001)
            var base = self.ramp(t)
            if self.ctl.y > 0.5 && self.ctl.y < 1.5 {
                base = self.diverge(t)
            } else if self.ctl.y > 1.5 && t > 1.0 {
                base = vec3(0.85, 0.10, 0.75)
            }
            if self.v_tag > 0.5 {
                let stripe = step(0.5, fract((self.v_world.x + self.v_world.y + self.v_world.z) * 0.6))
                // the focused load's region (its tag 4 up): solid, the others striped
                if self.v_tag > 4.5 {
                    base = base.mix(self.kind_color(self.v_tag - 4.0), 0.8 + 0.2 * stripe)
                } else {
                    base = base.mix(self.kind_color(self.v_tag), 0.35 + 0.4 * stripe)
                }
            }
            let albedo = pow(max(base, vec3(0.0, 0.0, 0.0)), vec3(2.2, 2.2, 2.2))
            let l = normalize(self.u_sun_dir.xyz)
            let hemi = mix(self.u_sun_ground.xyz, self.u_sun_sky.xyz, clamp(n.z * 0.5 + 0.5, 0.0, 1.0))
            let lit = albedo * (hemi + self.u_sun_color.xyz * max(dot(n, l), 0.0)) * self.u_sun_dir.w
            if self.u_sun_color.w < 0.5 {
                return vec4(lit, 1.0)
            }
            let mapped = clamp(
                (lit * (lit * 2.51 + vec3(0.03, 0.03, 0.03)))
                    / (lit * (lit * 2.43 + vec3(0.59, 0.59, 0.59)) + vec3(0.14, 0.14, 0.14)),
                vec3(0.0, 0.0, 0.0),
                vec3(1.0, 1.0, 1.0)
            )
            return vec4(pow(mapped, vec3(0.4545454, 0.4545454, 0.4545454)), 1.0)
        }

        fragment: fn() {
            self.fb0 = self.pixel()
        }
    }

    // The same surface into the G-buffer: view distance, octahedral view normal, and a print
    // bead's id under its part's code (the composite's `bead_code`).
    mod.draw.DrawFabForceAux = mod.std.set_type_default() do #(DrawFabForceAux::script_shader(vm)){
        ..mod.draw.DrawFabForce
        color_format: @Rgba32F

        oct_encode: fn(n: vec3f) -> vec2f {
            let l1 = abs(n.x) + abs(n.y) + abs(n.z)
            if l1 < 0.00000001 {
                return vec2(0.0, 0.0)
            }
            let e = n / l1
            if e.z >= 0.0 {
                return vec2(e.x, e.y)
            }
            let sx = step(0.0, e.x) * 2.0 - 1.0
            let sy = step(0.0, e.y) * 2.0 - 1.0
            return vec2((1.0 - abs(e.y)) * sx, (1.0 - abs(e.x)) * sy)
        }

        pixel: fn() -> vec4f {
            var n = normalize(self.v_vnormal)
            // toward the pixel's ray: from the eye, or down the view's axis orthographic
            let persp = clamp((self.draw_pass.camera_projection * vec4(0.0, 0.0, -1.0, 0.0)).w, 0.0, 1.0)
            if dot(n, self.v_view * persp + vec3(0.0, 0.0, persp - 1.0)) > 0.0 {
                n = 0.0 - n
            }
            let e = self.oct_encode(n)
            return vec4(length(self.v_view), e.x, e.y, 16000008.0 + 8.0 * floor(self.space.y + 0.5))
        }
    }
}

/// One vertex as the force shaders read it: 64 bytes, std140 and `repr(C)` alike (each `vec3`
/// followed by a float to fill its 16).
#[repr(C)]
#[derive(Clone, Script, ScriptHook)]
pub struct ForceVertex {
    #[live]
    pub pos: Vec3f,
    /// 0, or the kind of the load whose region it lies in (1 fixed, 2 force, 3 pressure, 4
    /// torque).
    #[live]
    pub tag: f32,
    #[live]
    pub normal: Vec3f,
    #[live]
    pub pad: f32,
    #[live]
    pub disp: Vec3f,
    #[live]
    pub pad2: f32,
    #[live]
    pub values: Vec4f,
}

const FORCE_VERTEX_FLOATS: usize = 16;
const _: () = assert!(std::mem::size_of::<ForceVertex>() == FORCE_VERTEX_FLOATS * 4);

pub(crate) fn force_vertex_pod(vm: &mut ScriptVm) -> ScriptValue {
    let v = ForceVertex::script_pod(vm).expect("ForceVertex pod");
    vm.heap_mut().pod_type_name_set(v, id_lut!(ForceVertex));
    v.into()
}

/// Fixes the vertex format for the shaders; the surface is bound per draw.
pub(crate) fn force_placeholder_geom(vm: &mut ScriptVm) -> ScriptValue {
    let id = vm.cx_mut().shared_geometry(id!(ForceGeom), |cx| {
        let g = Geometry::new(cx);
        let mut v = Vec::new();
        for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            v.extend_from_slice(&[p[0], p[1], p[2], 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        }
        g.update(cx, vec![0, 1, 2], v);
        g
    });
    Geometry::new_borrowed(id).into_script_handle(vm)
}

macro_rules! force_draw {
    ($name:ident) => {
        #[derive(Script, ScriptHook, Debug)]
        #[repr(C)]
        pub struct $name {
            #[deref]
            pub draw_vars: DrawVars,
            /// x = the colour scale's top, y = its kind (0 magnitude, 1 signed, 2
            /// utilisation), z = the displacement's magnification.
            #[live]
            pub ctl: Vec4f,
            /// Which of the vertex's values colours it (one-hot).
            #[live]
            pub channel: Vec4f,
            /// x = 1: the lit pass (Y up), y = the part's code in the G-buffer (1 + 4 x its
            /// element + its selection state).
            #[live]
            pub space: Vec4f,
        }

        impl $name {
            fn draw(&mut self, cx: &mut CxDraw, geometry_id: GeometryId) {
                self.draw_vars.geometry_id = Some(geometry_id);
                if self.draw_vars.can_instance() {
                    let new_area = cx.add_instance(&mut self.draw_vars);
                    self.draw_vars.area = cx.update_area_refs(self.draw_vars.area, new_area);
                }
            }
        }
    };
}
force_draw!(DrawFabForce);
force_draw!(DrawFabForceAux);

/// What a recording shows of the study: its scene and how it colours it, and the deformed mesh.
/// Part of the scene key: a change records the passes again.
#[derive(Clone, Debug, PartialEq)]
pub struct ForceFrame {
    pub revision: u64,
    /// The deformed mesh stands in for the part.
    pub deforms: bool,
    pub ctl: [f32; 4],
    pub channel: [f32; 4],
    /// The part's code in the G-buffer (its element and selection, as a bead's).
    pub code: f32,
}

/// The colour scale's top and whether it is signed, for the field shown.
fn scale_of(answer: &ForceAnswer, field: ForceField) -> (f32, bool) {
    match field {
        ForceField::Safety => (1.0, false),
        ForceField::Principal => (answer.display.scale[field.index()], true),
        _ => (answer.display.scale[field.index()], false),
    }
}

/// What the element lookup's pack follows of the study (`elements.rs`): a change packs it again.
pub(super) fn overlay_key(state: &AppState) -> Option<(u64, usize, bool)> {
    state.heatmap_part()?;
    Some((state.force.scene.as_ref()?.revision, state.force.field.index(), state.force_deforms()))
}

/// The study as the studied part's shade reads it ([`Overlay`]): the field shown over the colour
/// scale, once solved, and the marks.
pub(super) struct Overlaid {
    pub index: usize,
    values: Vec<f32>,
    scene: Arc<ForceScene>,
    palette: Vec<[f32; 4]>,
}

impl Overlaid {
    /// The part and the scene an overlay shows: none while nothing shows over the part, or while
    /// the deformed mesh stands in for it.
    fn shown(state: &AppState) -> Option<(usize, Arc<ForceScene>)> {
        let index = state.heatmap_part().filter(|_| !state.force_deforms())?;
        let scene = state.force.scene.clone()?;
        (scene.answer.is_some() || scene.marks.any).then_some((index, scene))
    }

    /// What [`Overlaid::of`]'s overlay holds, without making it: its part, then its values', its
    /// marks' and its palette's lengths (the element lookup sizes its texture by them).
    pub fn sizes(state: &AppState) -> Option<(usize, usize, usize, usize)> {
        let (index, scene) = Self::shown(state)?;
        let values = scene.answer.as_ref().map_or(0, |a| a.values.nodes.len());
        let marks = if scene.marks.any { scene.marks.marks.len() } else { 0 };
        Some((index, values, marks, palette(state.force.field).len()))
    }

    /// None: nothing shows over the part, or the deformed mesh stands in for it.
    pub fn of(state: &AppState) -> Option<Overlaid> {
        let (index, scene) = Self::shown(state)?;
        let field = state.force.field;
        let values = scene.answer.as_ref().map_or_else(Vec::new, |a| {
            let (top, signed) = scale_of(a, field);
            a.values.over_ramp(field.index(), top, signed)
        });
        Some(Overlaid { index, values, scene, palette: palette(field) })
    }

    pub fn overlay(&self) -> Overlay<'_> {
        let m = &self.scene.marks;
        let marks = m.any.then_some(OverlayLattice { origin: m.origin.map(|v| v as f32), h: m.h as f32, cells: m.cells, data: &m.marks });
        let values = self.scene.answer.as_ref().map(|a| OverlayLattice { origin: a.values.origin.map(|v| v as f32), h: a.values.h as f32, cells: a.values.cells, data: &self.values });
        Overlay { values, marks, palette: &self.palette }
    }
}

/// The colour a utilisation over 1 (the part fails there) shows, as the deformed mesh's shader
/// has it, and the marks' kinds' colours.
const OVER_ONE: [f32; 3] = [0.85, 0.10, 0.75];
const KINDS: [[f32; 3]; 4] = [[0.35, 0.55, 1.0], [1.0, 0.35, 0.85], [1.0, 0.65, 0.2], [0.3, 1.0, 0.65]];

/// A display colour as a reflectance.
fn linear(c: [f32; 3], shown: f32) -> [f32; 4] {
    [c[0].powf(2.2), c[1].powf(2.2), c[2].powf(2.2), shown]
}

/// The field's colours as [`Overlay::palette`] has them: the key's ramp, what shows over 1, the kinds.
fn palette(field: ForceField) -> Vec<[f32; 4]> {
    let mut p = Vec::with_capacity(OVERLAY_RAMP + 5);
    for k in 0..OVERLAY_RAMP {
        let t = k as f64 / (OVERLAY_RAMP - 1) as f64;
        let c = match field {
            ForceField::Principal => crate::ui::force::diverge(t * 2.0 - 1.0),
            _ => crate::ui::force::ramp(t),
        };
        p.push(linear([c.x, c.y, c.z], 1.0));
    }
    p.push(linear(OVER_ONE, if field == ForceField::Safety { 1.0 } else { 0.0 }));
    p.extend(KINDS.map(|c| linear(c, 1.0)));
    p
}

impl ForceFrame {
    /// What the plate view shows of the study now, if the Force render mode shows one.
    pub fn of(state: &AppState) -> Option<ForceFrame> {
        use super::elements::{STATE_ACTIVE, STATE_SELECTED, STATE_VISIBLE};
        let index = state.heatmap_part()?;
        let f = &state.force;
        let s = f.scene.as_ref()?;
        let k = f.field.index();
        let mode = match f.field {
            ForceField::Principal => 1.0,
            ForceField::Safety => 2.0,
            _ => 0.0,
        };
        let top = s.answer.as_ref().map_or(1.0, |a| scale_of(a, f.field).0);
        let m = magnify(s, f.deform) as f32;
        let mut channel = [0.0f32; 4];
        channel[k] = 1.0;
        let selection = &state.scene_state.selection;
        let code = match state.plate.parts.get(index).and_then(|p| p.element) {
            Some(e) => {
                let st = if selection.active == Some(e) { STATE_ACTIVE } else if selection.contains(e) { STATE_SELECTED } else { STATE_VISIBLE };
                1.0 + 4.0 * e.0 as f32 + st
            }
            None => 0.0,
        };
        Some(ForceFrame { revision: s.revision, deforms: state.force_deforms(), ctl: [top, mode, m, 0.0], channel, code })
    }
}

/// The deformed mesh's GPU surface and the ray buckets its picks and readouts go through.
#[derive(Default)]
pub struct ForceLayer {
    geometry: Option<Geometry>,
    /// The scene revision the geometry holds.
    uploaded: Option<u64>,
    /// Ray buckets over the surface as drawn, for the scene revision and magnification they
    /// were made at, with the vertices where they show.
    bins: Option<(u64, u32, Bins, Vec<[f32; 3]>)>,
}

/// The interleaved vertices and the triangles of a scene's surface.
fn mesh(scene: &ForceScene, d: &fab_force::display::Display) -> (Vec<f32>, Vec<u32>) {
    let mut v = Vec::with_capacity(d.pos.len() * FORCE_VERTEX_FLOATS);
    for i in 0..d.pos.len() {
        let (p, n, u, x) = (d.pos[i], d.normal[i], d.disp[i], d.values[i]);
        let tag = scene.marks.at(p.map(|v| v as f64), n.map(|v| v as f64)) as f32;
        v.extend_from_slice(&[p[0], p[1], p[2], tag, n[0], n[1], n[2], 0.0, u[0], u[1], u[2], 0.0, x[0], x[1], x[2], x[3]]);
    }
    let mut idx = Vec::with_capacity(d.quads.len() * 6);
    for q in &d.quads {
        idx.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
    }
    (v, idx)
}

impl ForceLayer {
    /// The deformed mesh onto the GPU when it changed, while it shows.
    pub fn sync(&mut self, cx: &mut Cx, state: &AppState) {
        let Some(s) = state.force.scene.as_ref().filter(|_| state.force_deforms()) else { return };
        let Some(answer) = &s.answer else { return };
        if self.uploaded != Some(s.revision) {
            let (v, idx) = mesh(s, &answer.display);
            let g = self.geometry.get_or_insert_with(|| Geometry::new(cx));
            g.update(cx, idx, v);
            self.uploaded = Some(s.revision);
        }
    }

    /// Records the deformed mesh into the aux pass (`lit` None) or the lit pass under the parts'
    /// light rig, as the beads take it; the draw calls.
    pub fn draw(&mut self, cx: &mut Cx3d, frame: &ForceFrame, lit: Option<&TileLight>, aux: &mut DrawFabForceAux, shade: &mut DrawFabForce) -> u32 {
        let Some(g) = &self.geometry else { return 0 };
        if !frame.deforms || self.uploaded != Some(frame.revision) {
            return 0;
        }
        let id = g.geometry_id();
        let [a, b, c, d] = frame.ctl;
        let [e, f, g2, h] = frame.channel;
        let space = vec4(if lit.is_some() { 1.0 } else { 0.0 }, frame.code, 0.0, 0.0);
        if let Some(light) = lit {
            shade.ctl = vec4(a, b, c, d);
            shade.channel = vec4(e, f, g2, h);
            shade.space = space;
            super::print::bind_light(&mut shade.draw_vars, cx.cx, light);
            shade.draw(cx.cx, id);
        } else {
            aux.ctl = vec4(a, b, c, d);
            aux.channel = vec4(e, f, g2, h);
            aux.space = space;
            aux.draw(cx.cx, id);
        }
        1
    }

    /// Where `ray` (the plate view's, metres) meets the study's part as drawn now (printer mm,
    /// undeformed) and the fields there: the exact hit on the real part, else on the deformed mesh
    /// while it stands in for the part.
    fn under(&mut self, state: &AppState, pane: PaneId, scene: &ForceScene, ray: &Ray) -> Option<(Option<[f32; 4]>, [f64; 3])> {
        let answer = scene.answer.as_ref().filter(|_| state.force_deforms());
        let Some(answer) = answer else {
            let hit = super::pick_ray(state, pane, ray).filter(|h| state.plate.part_of(h.element) == state.heatmap_part())?;
            let p = [hit.point.x as f64 * 1e3, hit.point.y as f64 * 1e3, hit.point.z as f64 * 1e3];
            return Some((scene.answer.as_ref().and_then(|a| a.values.at(p)), p));
        };
        let m = magnify(scene, state.force.deform) as f32;
        if self.bins.as_ref().is_none_or(|(r, mb, _, _)| *r != scene.revision || *mb != m.to_bits()) {
            let shown = answer.display.shown(m);
            let bins = Bins::new(&shown, &answer.display.quads);
            self.bins = Some((scene.revision, m.to_bits(), bins, shown));
        }
        let (_, _, bins, shown) = self.bins.as_ref()?;
        let o = [ray.origin.x as f64 * 1e3, ray.origin.y as f64 * 1e3, ray.origin.z as f64 * 1e3];
        let d = [ray.dir.x as f64, ray.dir.y as f64, ray.dir.z as f64];
        let (q, p) = bins.hit(shown, &answer.display.quads, o, d)?;
        let (v, rest) = answer.display.at(shown, q, p);
        Some((Some(v), rest))
    }

    /// Where on the part `ray` meets it: what a click places a load on.
    pub fn hit(&mut self, state: &AppState, pane: PaneId, scene: &ForceScene, ray: &Ray) -> Option<[f64; 3]> {
        self.under(state, pane, scene, ray).map(|(_, rest)| rest)
    }

    /// The fields at the point under `ray`, for the hover tip.
    pub fn readout(&mut self, state: &AppState, pane: PaneId, scene: &ForceScene, ray: &Ray) -> Option<String> {
        let (v, rest) = self.under(state, pane, scene, ray)?;
        let at = format!("x {:.1}  y {:.1}  z {:.1} mm", rest[0], rest[1], rest[2]);
        Some(match (v, &scene.answer) {
            (Some(v), Some(_)) => {
                let sf = if v[2] > 0.0 { format!("{:.2}", 1.0 / v[2]) } else { "no load".into() };
                format!("Von Mises {:.2} MPa\nPrincipal {:.2} MPa\nSafety factor {sf}\nMoves {:.4} mm\n{at}", v[0], v[1], v[3])
            }
            // the cells did not reach this bit of the part
            (None, Some(_)) => format!("Not analysed here\n{at}"),
            _ => at,
        })
    }
}
