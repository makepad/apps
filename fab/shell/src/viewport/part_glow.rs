//! Light carried from a board emitter through a posed printed part.
//! Geometry is measured only on input changes; live levels remain in the simulation texture.

use super::{bodies::report_dark_emitter, elements::ElementLut, set_texture_named, BodyLiveKeys, DrawGeometry};
use crate::api::*;
use crate::model::state::{Pose, POSE_IDENTITY};
use fab_elec_bench::models::LED_RATED_A;
use fab_elec_core::{CompRef, Sel};
use fab_elec_mech::scene::{ElecScene, SceneBody};
use fab_elec_view::{canvas::emitter::emitter_rgb, ElecLive};
use makepad_widgets_core::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

pub(super) const PIPE_TRANSMISSION: f32 = 0.5;
pub(super) const SIDE_BOTTOM: f32 = 0.1;
pub(super) const SIDE_TOP: f32 = 0.4;
pub(super) const TOP_NORMAL_Z: f32 = 0.7;

/// The studio key's illuminance in lux: the view's photometric anchor. A white face under
/// it has luminance KEY_LUX / pi, so an emitter of luminance L reads L / (KEY_LUX / pi)
/// display whites. This exposure choice sets how hot every lit LED reads.
pub(super) const KEY_LUX: f32 = 2000.0;

/// Full rated-current emission in display whites, for the projected emitting area in m².
/// Rec. 709 luminance normalizes the colour; the stated flux alone sets the brightness.
pub(super) fn emitter_whites(body: &SceneBody, area_m2: f32) -> [f32; 3] {
    let Some(emitter) = &body.emitter else { return [0.0; 3] };
    let Some(flux_per_a) = emitter.flux_per_a else {
        report_dark_emitter(body, "an emitter that states no flux_per_a");
        return [0.0; 3];
    };
    let rgb = emitter_rgb(&emitter.light);
    let luminance = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    if !(area_m2 > 0.0) || !(luminance > 0.0) { return [0.0; 3]; }
    let whites = (flux_per_a * LED_RATED_A) as f32 / (area_m2 * KEY_LUX);
    rgb.map(|channel| channel / luminance * whites)
}

pub(super) fn side_share(t: f32) -> f32 {
    SIDE_BOTTOM + (SIDE_TOP - SIDE_BOTTOM) * t
}

#[derive(Clone, Debug)]
pub(super) struct PipeGlow {
    pub element: ElementId,
    pub comp: CompRef,
    pub emit: [f32; 3],
    glass: bool,
    /// Fab Z-up metres, exactly the aux shader's `v_world` frame.
    pub z_bottom: f32,
    pub z_top: f32,
}

impl PipeGlow {
    pub fn height(&self, z: f32) -> f32 {
        if self.z_top > self.z_bottom {
            ((z - self.z_bottom) / (self.z_top - self.z_bottom)).clamp(0.0, 1.0)
        } else { 0.0 }
    }
}

/// An element's transform under `poses`, column-major, in the aux shader's frame.
pub(super) fn scene_pose_under(state: &AppState, poses: PanePoses, element: ElementId) -> Mat4f {
    state.pose_under(poses, element).map_or_else(Mat4f::identity, pose_matrix)
}

fn pose_matrix(p: Pose) -> Mat4f {
    Mat4f { v: [
        p[0][0], p[1][0], p[2][0], 0.0,
        p[0][1], p[1][1], p[2][1], 0.0,
        p[0][2], p[1][2], p[2][2], 0.0,
        p[0][3], p[1][3], p[2][3], 1.0,
    ] }
}

/// Stored normals determine the facing, as in aux, independently of triangle winding.
pub(super) fn posed_triangle(vertices: &[f32], tri: &[u32], pose: &Mat4f) -> ([Vec3f; 3], Vec3f) {
    let mut normal = vec3(0.0, 0.0, 0.0);
    let points = std::array::from_fn(|i| {
        let v = &vertices[tri[i] as usize * VERTEX_STRIDE..];
        let p = pose.transform_vec4(vec4(v[0], v[1], v[2], 1.0));
        let n = pose.transform_vec4(vec4(v[4], v[5], v[6], 0.0));
        normal += vec3(n.x, n.y, n.z);
        vec3(p.x, p.y, p.z)
    });
    if normal.length() > 0.0 { normal = normal.normalize(); }
    (points, normal)
}

fn report_dark_pipe(element: ElementId, why: &'static str) {
    static REPORTED: OnceLock<Mutex<HashSet<(ElementId, &'static str)>>> = OnceLock::new();
    if REPORTED.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()).insert((element, why)) {
        log!("light pipe: element {}: {why}: it does not glow", element.0);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PipeSurface {
    Hidden,
    Opaque,
    Glass,
}

impl PipeSurface {
    fn of_under(state: &AppState, poses: PanePoses, element: ElementId) -> Self {
        if state.is_glass_under(poses, element) {
            Self::Glass
        } else if state.is_drawn(element) {
            Self::Opaque
        } else {
            Self::Hidden
        }
    }
}

struct Inputs {
    bindings: Arc<[PartGlow]>,
    scene: Arc<Scene>,
    electronics: Option<Arc<ElecScene>>,
    surfaces: Vec<PipeSurface>,
}

impl Inputs {
    fn matches(&self, state: &AppState) -> bool {
        let same_electronics = match (&self.electronics, &state.elec_scene) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !Arc::ptr_eq(&self.bindings, &state.part_glows) || !Arc::ptr_eq(&self.scene, &state.scene) || !same_electronics {
            return false;
        }
        let poses = state.shared_poses();
        self.bindings.iter().zip(&self.surfaces).all(|(binding, surface)| {
            *surface == PipeSurface::of_under(state, poses, binding.element)
        })
    }

    fn build(&self) -> Arc<[PipeGlow]> {
        let bodies: HashMap<_, _> = self.electronics.iter().flat_map(|scene| &scene.boards)
            .flat_map(|board| &board.bodies).filter_map(|body| match &body.sel {
                Sel::Comp(comp) => Some((*comp, body)), _ => None,
            }).collect();
        self.bindings.iter().zip(&self.surfaces).filter_map(|(binding, surface)| {
            if *surface == PipeSurface::Hidden { return None; }
            let &PartGlow { element, comp, seat } = binding;
            let Some(body) = bodies.get(&comp) else {
                report_dark_pipe(element, "no LED body");
                return None;
            };
            if body.emitter.is_none() {
                report_dark_pipe(element, "no emitter");
                return None;
            }
            let pose = pose_matrix(seat);
            let mut area = 0.0f64;
            let (mut z_bottom, mut z_top) = (f32::INFINITY, f32::NEG_INFINITY);
            if let Some(part) = self.scene.elements.get(element.index()) {
                for &(batch, first, count) in &part.ranges {
                    let batch = &self.scene.batches[batch as usize];
                    for tri in batch.indices[first as usize..(first + count) as usize].chunks_exact(3) {
                        let (points, normal) = posed_triangle(&batch.vertices, tri, &pose);
                        for p in points { z_bottom = z_bottom.min(p.z); z_top = z_top.max(p.z); }
                        if normal.z >= TOP_NORMAL_Z {
                            let [a, b, c] = points;
                            area += 0.5 * Vec3f::cross(b - a, c - a).z.abs() as f64;
                        }
                    }
                }
            }
            if !(area > 0.0) {
                report_dark_pipe(element, "zero top area");
                return None;
            }
            let emit = emitter_whites(body, area as f32).map(|v| v * PIPE_TRANSMISSION);
            if !emit.iter().any(|v| *v > 0.0) { return None; }
            Some(PipeGlow { element, comp, emit, z_bottom, z_top, glass: *surface == PipeSurface::Glass })
        }).collect::<Vec<_>>().into()
    }
}

/// Shared by all panes. Empty bindings allocate neither a list nor a texture.
#[derive(Default)]
pub(super) struct PartGlowLayer {
    inputs: Option<Inputs>,
    logged_off_seat: bool,
    pub pipes: Option<Arc<[PipeGlow]>>,
    /// The scene batches holding a glowing element: the only geometry the glow draws.
    batches: Vec<usize>,
    revision: u64,
    texture: Option<Texture>,
    uploaded: Option<(u64, Option<(Option<TextureId>, u64)>)>,
}

impl PartGlowLayer {
    pub fn active(&self) -> bool { self.inputs.is_some() }

    pub fn key(&self) -> Option<u64> { self.pipes.as_ref().map(|_| self.revision) }

    /// A pipe lights only where it sits on its LED. The pane draws glow when all bound pipes are seated.
    pub fn seated_under(&self, state: &AppState, poses: PanePoses) -> bool {
        state.part_glows.iter().all(|pipe| {
            let pose = state.pose_under(poses, pipe.element).unwrap_or(POSE_IDENTITY);
            pose.iter().flatten().zip(pipe.seat.iter().flatten()).all(|(at, seat)| (at - seat).abs() <= 1e-5)
        })
    }

    /// Rebuild the seated measurements when the bindings, scene, electronics or pipe surfaces change.
    pub fn sync(&mut self, state: &AppState) {
        if state.part_glows.is_empty() {
            self.inputs = None;
            self.pipes = None;
            self.batches.clear();
            self.texture = None;
            self.uploaded = None;
            return;
        }
        if self.inputs.as_ref().is_none_or(|inputs| !Arc::ptr_eq(&inputs.bindings, &state.part_glows)) {
            self.logged_off_seat = false;
        }
        if !self.logged_off_seat && !self.seated_under(state, state.shared_poses()) {
            log!("pipe glow: bound pipes are off their seat; they stay dark until seated");
            self.logged_off_seat = true;
        }
        if self.inputs.as_ref().is_some_and(|inputs| inputs.matches(state)) { return; }
        let poses = state.shared_poses();
        let inputs = Inputs {
            bindings: state.part_glows.clone(), scene: state.scene.clone(), electronics: state.elec_scene.clone(),
            surfaces: state.part_glows.iter().map(|p| PipeSurface::of_under(state, poses, p.element)).collect(),
        };
        let pipes = inputs.build();
        self.batches = pipes.iter().filter_map(|pipe| state.scene.elements.get(pipe.element.index()))
            .flat_map(|element| element.ranges.iter().map(|&(batch, _, _)| batch as usize)).collect();
        self.batches.sort_unstable();
        self.batches.dedup();
        self.pipes = (!pipes.is_empty()).then_some(pipes);
        self.inputs = Some(inputs);
        self.revision = self.revision.wrapping_add(1);
        self.uploaded = None;
        if self.pipes.is_none() { self.texture = None; }
    }

    /// Re-resolve component rows only on a new pipe list or simulation row map/texture.
    fn resolve(&mut self, cx: &mut Cx) -> Option<Texture> {
        let pipes = self.pipes.as_ref()?;
        let sim = &cx.get_global_ref::<ElecLive>()?.sim;
        let parts = sim.parts.clone()?;
        let key = (self.revision, BodyLiveKeys::of_sim(Some(sim)).rows);
        if self.uploaded != Some(key) {
            let texels = self.inputs.as_ref()?.scene.elements.len() * 2;
            let width = texels.min(super::elements::LUT_WIDTH).max(2);
            let height = texels.div_ceil(width);
            let mut data = vec![0.0; width * height * 4];
            for pair in data.chunks_exact_mut(8) { pair[3] = -1.0; }
            for pipe in pipes.iter() {
                let at = pipe.element.index() * 8;
                data[at..at + 3].copy_from_slice(&pipe.emit);
                data[at + 3] = sim.index.comp(pipe.comp).map_or(-1.0, |row| row as f32);
                data[at + 4] = pipe.z_bottom;
                data[at + 5] = pipe.z_top;
                data[at + 6] = if pipe.glass { 1.0 } else { 0.0 };
            }
            self.texture = Some(Texture::new_with_format(cx, TextureFormat::VecRGBAf32 {
                width, height, data: Some(data), updated: TextureUpdated::Full,
            }));
            self.uploaded = Some(key);
        }
        Some(parts)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, cx: &mut Cx3d, geometries: &[Geometry], elements: &ElementLut,
        planes: &[[f32; 4]], cut: Option<[f32; 4]>, aux: &Texture, gain: f32, draw: &mut DrawFabPartGlow) -> u32 {
        let Some(parts) = self.resolve(cx.cx) else { return 0; };
        let Some(elements_texture) = elements.texture() else { return 0; };
        let vars = &mut draw.draw_vars;
        set_texture_named(vars, cx.cx, id!(elem_map), elements_texture);
        set_texture_named(vars, cx.cx, id!(pipe_glows), self.texture.as_ref().unwrap());
        set_texture_named(vars, cx.cx, id!(live_parts), &parts);
        set_texture_named(vars, cx.cx, id!(aux), aux);
        let (width, height) = elements.size();
        vars.set_uniform(cx.cx, id!(elem_ctl), &[width, height, super::elements::STRIDE as f32, 1.0]);
        vars.set_uniform(cx.cx, id!(ghost_ctl), &[0.0; 4]);
        vars.set_uniform(cx.cx, id!(admit_hidden), &[1.0]);
        vars.set_uniform(cx.cx, id!(clip_ctl), &[planes.len() as f32, 0.0, 0.0, 0.0]);
        for (i, name) in [id!(clip0), id!(clip1), id!(clip2), id!(clip3), id!(clip4), id!(clip5)].iter().enumerate() {
            vars.set_uniform(cx.cx, *name, &planes.get(i).copied().unwrap_or([0.0; 4]));
        }
        vars.set_uniform(cx.cx, id!(clip_cut), &cut.unwrap_or([0.0; 4]));
        vars.set_uniform(cx.cx, id!(live_on), &[1.0]);
        vars.set_uniform(cx.cx, id!(glow_gain), &[gain]);
        draw.transform = Mat4f::identity();
        let mut calls = 0;
        for geometry in self.batches.iter().filter_map(|&batch| geometries.get(batch)) {
            draw.draw_geometry(cx.cx, geometry.geometry_id());
            calls += 1;
        }
        calls
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawFabPartGlow {
    #[deref]
    draw_vars: DrawVars,
    #[live]
    transform: Mat4f,
}

impl DrawGeometry for DrawFabPartGlow {
    fn vars(&mut self) -> &mut DrawVars { &mut self.draw_vars }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::{BoardId, CompId};

    #[test]
    fn pipe_glow_follows_seating_without_rebuilding_on_pose_changes() {
        let mut state = AppState::default();
        state.scene = Scene::from_model(crate::model::fixture::parts(), &mut |_| {}).into();
        let element = state.scene.elements[0].id;
        let comp = CompRef { board: BoardId(0), comp: CompId(0) };
        let mut seat = POSE_IDENTITY;
        seat[2][3] = 0.02;
        state.part_glows = Arc::new([PartGlow { element, comp, seat }]);
        let device = PanePoses::Set(PoseSetId::DEVICE);
        state.set_pane_poses(PaneId::PRIMARY, device);
        state.pose_set_mut(PoseSetId::DEVICE).set(element, seat);

        let mut layer = PartGlowLayer::default();
        layer.sync(&state);
        let revision = layer.revision;
        assert_eq!(state.shared_poses(), device, "sync sees the device pane's poses");
        assert!(layer.seated_under(&state, device), "the device holds the pipe at its seat");

        let mut near = seat;
        near[2][3] += 0.000005;
        state.pose_set_mut(PoseSetId::DEVICE).set(element, near);
        assert!(layer.seated_under(&state, device), "a translation within tolerance stays seated");

        let mut turned = seat;
        turned[0][0] = -1.0;
        turned[1][1] = -1.0;
        state.pose_set_mut(PoseSetId::DEVICE).set(element, turned);
        layer.sync(&state);
        assert!(!layer.seated_under(&state, device), "a pipe turned at its seat stays dark");
        assert!(!layer.seated_under(&state, PanePoses::Stage), "the pipe at rest is below its seat");
        assert_eq!(layer.revision, revision, "moving the pipe reuses its seated measurements");
    }
}
