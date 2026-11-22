//! The plate scene's GPU uploads and shared field data.

use super::{board, casters, force, glass, paint, sdf, see_through, stage, tile_light};
use crate::api::*;
use fab_sdf_tiles::draw::TileLight;
use makepad_render::material_surface::{device_texture_from_source, PixelSemantic, PreparedTexture};
use makepad_render::{ModelInstance, Renderer};
use makepad_widgets_core::*;
use makepad_widgets_core::makepad_platform::thread::{checkpoint, Class, TaskHandle};
use std::sync::Arc;

pub struct SceneGpu {
    /// The host owns uploads, environments and material lanes; secondary panes fork its scene.
    pub renderer: Renderer,
    /// Held by the first drawing pane until it parks or releases its slot.
    pub(crate) host: Option<PaneId>,
    /// Hover-out from another pane must not clear a hit this pane just published.
    pub(crate) hovered_pane: Option<PaneId>,
    /// Whether the composites tint the part under the pointer of the hovered pane: not under the
    /// Device view's, where a tint brightened the whole board and moving the pointer over its
    /// parts made it flicker. Its live controls show a ring instead (`draw_control_ring`).
    pub(crate) hover_tints: bool,
    /// The last camera gesture owns motion; the other pane must not keep a competing glide.
    pub(crate) navigation_pane: Option<PaneId>,
    /// That pane's navigator moves the camera pane 0 and the linked panes share (a drag, a glide,
    /// inertia): those panes hold their still average until it stops, when all draw again.
    pub(crate) camera_moving: bool,
    /// `Cx::drawing_id()`, shared by all draw contexts in one Draw event (not redraw_id).
    synced_frame: Option<(u64, SceneSync)>,
    /// Field publication also runs while XR freezes desktop passes; keep its own stamp.
    fields_frame: Option<(u64, bool, bool, bool)>,
    /// Aux-pass geometry: one per `Scene::batches`, the 48-byte stream as-is.
    pub(super) batch_geometries: Vec<Geometry>,
    pub(super) uploaded_generation: u64,
    /// Name the merged `StaticModel` is resident under (`load_model_parsed`
    /// early-returns on a repeat id, so a re-pack needs a fresh one).
    pub(super) model_id: String,
    pub(super) model_revision: u32,
    /// Shared with the headset view (`AppState::sdf`).
    pub(crate) sdf: std::rc::Rc<std::cell::RefCell<sdf::SdfLayer>>,
    /// Every pane shares the same field and electronics casters and their revisions.
    pub(super) casters: std::rc::Rc<std::cell::RefCell<casters::SceneCasters>>,
    pub(super) sdf_revision: u64,
    pub(super) sdf_busy: bool,
    /// The tracer woke the view, or a look it owes fell due: the next draw records the scene.
    pub(super) sdf_look: bool,
    /// Drawn field parts, collected once per draw into retained storage.
    pub(super) sdf_drawn: Vec<ElementId>,
    /// View › See-Through's grids, and the box each is drawn in.
    pub(super) see: see_through::SeeThroughLayer,
    pub(super) force_layer: force::ForceLayer,
    pub(super) board_layer: board::BoardLayer,
    /// Bound light-pipe measurements and their lazy simulation-row texture, shared by panes.
    pub(super) part_glow: super::part_glow::PartGlowLayer,
    /// Mesh glass over each pane's finished opaque composite, ordered for that pane's camera.
    pub(super) glass_layer: glass::GlassLayer,
    /// Created only while at least one pane is Traced. CPU snapshots are shared;
    /// each pane owns its camera's GPU accumulation.
    pub(crate) product_scene: Option<super::product_scene::ProductSceneCache>,
    /// The plate face's element index, keyed by scene generation.
    pub(super) plate_index: Option<(u64, f32)>,
    pub(super) star_map_asked: bool,
    pub(super) star_map_task: Option<TaskHandle<Result<Option<Arc<PreparedTexture>>, String>>>,
    /// One lazy bake per studio, retained across workspace and lighting changes.
    studios: [StudioSlot; 2],
    /// The log has said the studio's lighting reached the view.
    pub(super) studio_lit: bool,
    /// A completed host lighting input invalidates every pane's retained picture.
    pub(super) lighting_revision: u64,
    /// Panes on the stage's poses and on a set were drawn in one frame (logged once a time).
    mixed_poses: bool,
}

impl Default for SceneGpu {
    fn default() -> Self {
        let sdf = std::rc::Rc::new(std::cell::RefCell::new(sdf::SdfLayer::default()));
        let board_layer = board::BoardLayer::default();
        let casters = std::rc::Rc::new(std::cell::RefCell::new(casters::SceneCasters::new(sdf.clone(), board_layer.casters.clone())));
        Self {
            renderer: Renderer::default(), host: None, hovered_pane: None, hover_tints: true, navigation_pane: Some(PaneId::PLATE_ZERO), camera_moving: false,
            synced_frame: None, fields_frame: None, batch_geometries: Vec::new(), uploaded_generation: 0,
            model_id: String::new(), model_revision: 0, sdf, casters, sdf_revision: 0, sdf_busy: false,
            sdf_look: false, sdf_drawn: Vec::new(), see: Default::default(), force_layer: Default::default(),
            board_layer, part_glow: Default::default(), glass_layer: Default::default(), product_scene: None, plate_index: None,
            star_map_asked: false, star_map_task: None, studios: Default::default(),
            studio_lit: false, lighting_revision: 0, mixed_poses: false,
        }
    }
}

#[derive(Default)]
struct StudioSlot {
    /// Asked for already: never submitted again, whether in flight, finished or refused.
    requested: bool,
    /// The bake in flight, its duration and when it was requested.
    task: Option<(TaskHandle<(makepad_render::env_map::GeneratedEnvMap, Option<Arc<makepad_raytrace::EnvironmentLight>>, f64)>, std::time::Instant)>,
    keys: Option<stage::StudioKeys>,
    /// The retained map and sampling tables for the traced pane.
    light: Option<Arc<makepad_raytrace::EnvironmentLight>>,
}

#[derive(Clone, Copy)]
pub struct SceneSync {
    pub board_changed: bool,
    /// Every pane sees the same tracer wake; the first pane must not consume it alone.
    pub sdf_record: bool,
}

impl SceneGpu {
    /// True only on the draw that acquires the renderer.
    pub(crate) fn take_host(&mut self, pane: PaneId) -> bool {
        if self.host.is_some() { return false; }
        self.host = Some(pane);
        true
    }

    /// Upload once per `Scene::generation`. Never per frame — see the Metal
    /// upload hazard.
    pub(super) fn ensure_uploaded(&mut self, cx: &mut Cx, state: &AppState) {
        if self.uploaded_generation == state.scene.generation {
            return;
        }
        self.uploaded_generation = state.scene.generation;
        self.batch_geometries.clear();
        for batch in &state.scene.batches {
            let g = Geometry::new(cx);
            g.update(cx, batch.indices.clone(), batch.vertices.clone());
            self.batch_geometries.push(g);
        }

        // The lit model was packed and prepared where the scene was built: here it is only
        // uploaded (no packing, encoding, decoding or mipping on the UI thread).
        let Some(prepared) = state.scene.prepared.as_ref().filter(|_| !state.scene.is_empty()) else {
            if !self.model_id.is_empty() {
                self.renderer.unload_model(&self.model_id);
            }
            self.model_id.clear();
            self.renderer.set_models(Vec::new());
            return;
        };
        if !self.model_id.is_empty() {
            self.renderer.unload_model(&self.model_id);
        }
        self.model_revision = self.model_revision.wrapping_add(1);
        self.model_id = format!("fab/{}-{}", state.scene.generation, self.model_revision);
        let started = std::time::Instant::now();
        if let Some(tris) = self.renderer.install_static_preview(cx, &self.model_id, prepared.0.clone(), false) {
            // Every resident material layer is static CSM geometry; no lightmap chart is
            // required.
            self.renderer.set_model_casts_shadow(&self.model_id, true);
            log!("fab viewport: uploaded {} ({} triangles) in {:.1} ms", self.model_id, tris, started.elapsed().as_secs_f64() * 1000.0);
        }
        // The instance changes with the upload, never with a camera. Forks inherit it even
        // when they record before the host pane; no model vector/string per moving frame.
        self.renderer.set_models(vec![ModelInstance {
            model: self.model_id.clone(),
            transform: Mat4f::identity(),
            tint: vec4(1.0, 1.0, 1.0, 1.0),
            color_adjust: vec4(0.0, 1.0, 1.0, 0.0),
            // Upload-time registration owns the CSM caster list. Keep
            // the plate static so it never enters the live-mover or
            // analytic-lamp lanes merely to cast a shadow.
            dynamic: false,
            depth_order: 0.0,
            part_poses: Vec::new(),
            custom_material: None,
            layer_materials: Vec::new(),
            lamps: Default::default(),
        }]);
    }

    /// Only a sky asks for the cached star panorama, once. Studio never reads it; analytic
    /// point stars remain until the background read, decode and mip preparation finish.
    pub(super) fn start_star_map(&mut self, cx: &mut Cx, lighting: Lighting) {
        if self.star_map_asked || lighting.is_studio() {
            return;
        }
        self.star_map_asked = true;
        let pool = cx.task_pool();
        match pool.clone().submit_named(Class::Background, "fab-star-map", move || {
            let Some(path) = makepad_render::star_map_path() else {
                return Ok(None);
            };
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if checkpoint().is_err() {
                return Ok(None);
            }
            let prepared = device_texture_from_source(&bytes, PixelSemantic::Color, || {
                let image = ImageBuffer::from_png(&bytes).map_err(|e| format!("png decode failed: {e:?}"))?;
                checkpoint().map_err(|_| "star map cancelled".to_string())?;
                Ok(PreparedTexture::prepare(image, PixelSemantic::Color))
            });
            // Cancellation inside the cache's decode callback is an empty result too.
            if checkpoint().is_err() {
                return Ok(None);
            }
            let (prepared, later) = prepared?;
            // The stored compressed form (the next start's) is encoded on a job of its own, so
            // the stars do not wait for it.
            if let Some(later) = later {
                let encoder = pool.clone();
                if let Err(error) = pool.submit(Class::Background, move || later.run(&makepad_render::texture_compression::pool_executor(&encoder))).map(|task| task.detach()) {
                    log!("star map: store encode not submitted: {error:?}");
                }
            }
            Ok(Some(Arc::new(prepared)))
        }) {
            Ok(task) => self.star_map_task = Some(task),
            Err(error) => log!("star map: {error}"),
        }
    }

    pub(super) fn poll_star_map(&mut self, cx: &mut Cx, force_record: &mut bool, area: Area) {
        let Some(result) = self.star_map_task.as_mut().and_then(|task| task.try_take()) else {
            return;
        };
        self.star_map_task = None;
        match result.map_err(|e| e.to_string()).and_then(|result| result) {
            Ok(Some(prepared)) => match Arc::try_unwrap(prepared) {
                Ok(prepared) => {
                    self.renderer.set_star_map_texture(prepared.upload(cx));
                    self.lighting_revision = self.lighting_revision.wrapping_add(1);
                    *force_record = true;
                    area.redraw(cx);
                }
                Err(_) => log!("star map: prepared texture retained an unexpected owner"),
            },
            Ok(None) => {}
            Err(error) => log!("star map: {error}"),
        }
    }

    /// Bake the selected studio on first use. A retained or in-flight request is a no-op.
    pub(super) fn start_studio(&mut self, cx: &mut Cx, lighting: Lighting) {
        let Some(set) = stage::StudioSet::from_lighting(lighting) else { return };
        let slot = &mut self.studios[set as usize];
        if slot.requested { return }
        // Asked once: the pool refuses only when it is closed, which no later frame changes.
        slot.requested = true;
        let rows = cx.task_pool();
        let asked = std::time::Instant::now();
        match cx.task_pool().submit_named(Class::Visible, "fab-studio", move || {
            let started = std::time::Instant::now();
            let params = stage::studio_params(set);
            let baked = stage::bake_studio(&params, |n, f| rows.fan_out(n, f));
            let map = &baked.map;
            let light = makepad_raytrace::EnvironmentLight::new(makepad_raytrace::EnvironmentMap {
                width: map.width, height: map.height, texels: map.data.iter().map(|t| [t[0], t[1], t[2]]).collect(),
            }).map(Arc::new);
            (baked, light, started.elapsed().as_secs_f64() * 1000.0)
        }) {
            Ok(task) => slot.task = Some((task, asked)),
            Err(error) => log!("studio lighting ({set:?}): {error}"),
        }
    }

    pub(super) fn poll_studio(&mut self, cx: &mut Cx, force_record: &mut bool, area: Area) {
        for set in [stage::StudioSet::Dark, stage::StudioSet::Bright] {
            let slot = &mut self.studios[set as usize];
            let Some(result) = slot.task.as_mut().and_then(|(task, _)| task.try_take()) else {
                continue;
            };
            let Some((_, asked)) = slot.task.take() else { continue };
            match result {
                Ok((baked, light, bake_ms)) => {
                    log!("fab studio: {set:?} baked in {bake_ms:.0} ms on the pool, registered {:.0} ms after it was asked for", asked.elapsed().as_secs_f64() * 1000.0);
                    self.renderer.register_generated_environment(set.environment(), Arc::new(baked.map), baked.keyless.map(Arc::new));
                    slot.keys = Some(stage::StudioKeys { sun: baked.sun, daylight: baked.daylight });
                    slot.light = light;
                    self.lighting_revision = self.lighting_revision.wrapping_add(1);
                    *force_record = true;
                    area.redraw(cx);
                }
                Err(error) => log!("studio lighting ({set:?}): {error}"),
            }
        }
    }

    /// Lighting the view asked for is still being prepared on the pool (a studio's bake or the
    /// star map): until it lands the view draws the lighting it had, so it is not settled. O(1).
    pub(super) fn lighting_pending(&self) -> bool {
        self.star_map_task.is_some() || self.studios.iter().any(|slot| slot.task.is_some())
    }

    /// Only the selected studio's keys, once its own bake has landed.
    pub(super) fn studio_keys(&self, lighting: Lighting) -> Option<stage::StudioKeys> {
        let set = stage::StudioSet::from_lighting(lighting)?;
        self.studios[set as usize].keys
    }

    /// The selected studio's retained traced light; cloning its Arc allocates nothing.
    pub(super) fn studio_light(&self, lighting: Lighting) -> Option<Arc<makepad_raytrace::EnvironmentLight>> {
        let set = stage::StudioSet::from_lighting(lighting)?;
        self.studios[set as usize].light.clone()
    }

    /// The studio map's own key in the map's frame, once prepared (the studio turns it to
    /// the camera).
    pub(super) fn studio_map_key(&self) -> Option<makepad_scene::EnvSun> {
        self.renderer.ibl_sun()
    }

    pub(super) fn sdf_light(&self, world: &makepad_scene::World, hdr: bool) -> TileLight {
        tile_light(&self.renderer, world, hdr)
    }

    pub fn sync_fields(&mut self, cx: &mut Cx, state: &mut AppState) -> sdf::SdfSync {
        let frame = cx.drawing_id();
        if let Some((stamp, changed, record, busy)) = self.fields_frame {
            if stamp == frame {
                return sdf::SdfSync { changed, record, busy };
            }
        }
        // A sculpt stroke being drawn shows in its part's field (the bricks it reaches build).
        self.sdf.borrow_mut().set_sculpt(state);
        let sdf_sync = self.sdf.borrow_mut().sync(cx, state);
        self.sdf.borrow().scan.set_blocked(state.print.slice_state().0 == "Slicing", &cx.task_pool());
        // The stroke being drawn updates the traced field at once.
        let stroke = crate::tools::paint::live_stroke();
        let mut fill = if state.ui.paint_mode != crate::api::PaintMode::Brush && crate::tools::paint::active(state) { crate::tools::paint::live_fill(state) } else { None };
        let live = match (&stroke, &mut fill) {
            (Some((o, p, r, c, e)), _) => Some(paint::LivePaint::Stroke { object: o.as_str(), points: p.as_slice(), radius: *r, rgb: *c, erase: *e }),
            (None, Some((o, regions, names))) => Some(paint::LivePaint::Fill { object: o.as_str(), regions: std::mem::take(regions), names: std::mem::take(names), rgb: state.ui.paint_color }),
            (None, None) => None,
        };
        self.sdf.borrow_mut().paint.set_live(state, live);
        if sdf_sync.changed {
            self.sdf_revision += 1;
        }
        self.sdf_busy = sdf_sync.busy;
        self.fields_frame = Some((frame, sdf_sync.changed, sdf_sync.record, sdf_sync.busy));
        sdf_sync
    }

    /// Shared uploads and layers, once per draw event, whichever pane records first.
    /// Each widget updates its own element lookup after this call.
    pub fn sync(&mut self, cx: &mut Cx, state: &mut AppState) -> SceneSync {
        let frame = cx.drawing_id();
        if let Some((stamp, sync)) = self.synced_frame {
            if stamp == frame {
                return sync;
            }
        }
        self.ensure_uploaded(cx, state);
        // the shared layers follow one pose source: say so once when panes on two are drawn
        let mixed = {
            let mut live = state.view.panes.iter().flatten().filter(|p| !p.parked);
            let set = |p: &&PaneState| matches!(p.poses, PanePoses::Set(_));
            live.clone().any(|p| set(&p)) && live.any(|p| !set(&p))
        };
        if mixed && !self.mixed_poses {
            log!("fab viewport: panes on the stage's poses and on a pose set draw together; the shared layers show the set's");
        }
        self.mixed_poses = mixed;
        // a field the GPU cannot draw: the note says so (the look stays the person's)
        if let Some(why) = self.sdf.borrow_mut().take_unavailable() {
            Cx::post_action(ShellAction::FieldUnavailable { why });
        }
        // See-Through's grids: the parts it draws leave the other passes
        self.see.sync(cx, state, &self.sdf.borrow().paint);
        let see_drawn = self.see.drawn_elements();
        if state.view.see.drawn != see_drawn {
            state.view.see.drawn = see_drawn;
        }
        let mut sdf_drawn = std::mem::take(&mut self.sdf_drawn);
        sdf_drawn.clear();
        sdf_drawn.extend(self.sdf.borrow().drawn_elements());
        self.glass_layer.sync(state, &self.batch_geometries);
        let board_changed = self.board_layer.sync(cx, state);
        // the case parts whose boards the board layer draws leave the plate passes
        if state.drawn_by_link != self.board_layer.drawn_parts() {
            state.drawn_by_link = self.board_layer.drawn_parts().to_vec();
        }
        self.force_layer.sync(cx, state);
        self.sdf_drawn = sdf_drawn;
        let sdf_record = std::mem::take(&mut self.sdf_look);
        let sync = SceneSync { board_changed, sdf_record };
        self.synced_frame = Some((frame, sync));
        sync
    }
}
