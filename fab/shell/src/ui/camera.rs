//! The linked printer's camera, floating over the window ([`FabCameraFloat`], opened from
//! the title bar's camera button). The stream runs only while it is shown: the printer
//! serves one live viewer, so a view that stops drawing lets its feed tear
//! the session down within a few seconds.
//!
//! Every view shares the real feed's latest upload or the virtual job's last camera pass.

use crate::api::{AppState, PlateInfo};
use crate::print::{PrinterChoice, PrintPreview};
use crate::viewport::print::{draw_print_eye, CameraMoment, DrawFabFar, DrawFabHead, DrawFabStrip, FarTiers, PrintFrame, PrintPalette};
use fab_plate::ColourMode;
use fab_printer_link::{ConnectionStatus, Snapshot};
use makepad_video::{DecodedFrame, StreamVideoCodec, VideoColor, VideoStreamDecoder};
use makepad_widgets_core::makepad_draw::shader::draw_nv12::DrawNv12;
use makepad_widgets_core::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// Fit the bed and the job's entire extrusion height, in the forward shaders' Y-up metres.
pub fn camera_eye(bed_min: [f64; 2], bed_max: [f64; 2], height: f64, top: f64, aspect: f64) -> (Mat4f, Mat4f) {
    use crate::api::{Aabb, Camera};
    use crate::{tools::transform::mm_to_world, viewport::pack::to_render};
    let room = |p| to_render(mm_to_world(p));
    let [left, front] = bed_min;
    let [right, back] = bed_max;
    let bounds = Aabb { min: room([left, back, 0.0]), max: room([right, front, top]) };
    let mut camera = Camera {
        eye: room([left, front, height]),
        target: room([(left + right) * 0.5, (front + back) * 0.5, height * 0.25]),
        up: vec3f(0.0, 1.0, 0.0),
        ..Camera::default()
    };
    let (forward, across, up) = (camera.forward(), camera.right(), camera.true_up());
    let centre = (bounds.min + bounds.max) * 0.5;
    // Leave 2.5% at either edge. For a corner at (x, z) in eye axes the right
    // plane requires distance + horizontal_shift / slope >= x / slope - z.
    // The opposite plane gives the other bound. Solve both axes at all eight corners.
    let slope_y = (camera.fov_y_deg.to_radians() * 0.5).tan() * 0.95;
    let slope_x = slope_y * aspect as f32;
    let mut edges = [f32::NEG_INFINITY; 4];
    for x in [left, right] {
        for y in [front, back] {
            for z in [0.0, top] {
                let offset = room([x, y, z]) - centre;
                let h = offset.dot(across) / slope_x;
                let v = offset.dot(up) / slope_y;
                for (edge, side) in edges.iter_mut().zip([h, -h, v, -v]) {
                    *edge = edge.max(side - offset.dot(forward));
                }
            }
        }
    }
    let [r, l, u, d] = edges;
    let distance = (r + l).max(u + d) * 0.5;
    camera.target = centre + across * ((r - l) * slope_x * 0.5) + up * ((u - d) * slope_y * 0.5);
    camera.eye = camera.target - forward * distance;
    camera.fit_clip_planes(&bounds, aspect as f32);
    (camera.view(), camera.projection(aspect as f32))
}

#[derive(Default, PartialEq, Eq)]
enum CameraSource {
    #[default]
    None,
    Virtual(PrinterChoice),
    Real { config: String },
}

/// The camera presented by both the Monitor panel and the plate's picture-in-picture.
#[derive(Default)]
pub struct PrinterCamera {
    source: CameraSource,
    live: Option<LiveView>,
    virtual_view: Option<VirtualView>,
    #[cfg(test)]
    fake_open: Option<Box<dyn Fn(&str, Arc<Feed>)>>,
}

impl PrinterCamera {
    /// Called by the owner of the link on a report or unlink, even when both views are hidden.
    pub fn update_source(&mut self, picked: &PrinterChoice, held_real: bool, snapshot: Option<&Snapshot>) {
        let source = match (picked, snapshot) {
            (_, Some(p)) if p.virtual_printer => CameraSource::Virtual(picked.clone()),
            (PrinterChoice::Real(config), Some(p)) if held_real && p.status == ConnectionStatus::Connected && p.state.camera => {
                CameraSource::Real { config: config.clone() }
            }
            _ => CameraSource::None,
        };
        if self.source != source {
            self.live = None;
            self.virtual_view = None;
            self.source = source;
        }
    }
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*
    use mod.math.*
    use mod.shader.*
    mod.widgets.FabPrinterCameraBase = #(FabPrinterCamera::register_widget(vm))
    mod.widgets.FabPrinterCamera = set_type_default() do mod.widgets.FabPrinterCameraBase{
        width: Fill
        height: Fill
        draw_bg +: {color: #x101214}
        draw_nv12: mod.draw.DrawNv12{}
        draw_path: mod.draw.DrawFabStrip{}
        draw_far: mod.draw.DrawFabFar{}
        draw_head: mod.draw.DrawFabHead{}
        draw_picture: mod.draw.DrawQuad{
            picture: texture_2d(float)
            pixel: fn() { return self.picture.sample(self.pos) }
        }
        draw_bed: mod.draw.DrawQuad{
            depth_write: true
            backface_culling: false
            alpha_blend: false
            vertex: fn() {
                let mm = self.rect_pos + self.geom.pos * self.rect_size
                let world = vec4(mm.x * 0.001, 0.0, -mm.y * 0.001, 1.0)
                self.vertex_pos = self.draw_pass.camera_projection * (self.draw_pass.camera_view * world)
            }
            fragment: fn() { self.fb0 = #x303438 }
        }
        draw_badge: mod.draw.DrawText{
            text_style: theme.font_regular{ font_size: 18 }
            color: fab.color_text
            // Stock glyph coverage in the camera texture's pixels, over the 3D scene.
            glyph_vertex: mod.draw.DrawText.vertex
            vertex: fn() {
                self.glyph_vertex()
                let pos = self.world.xy / self.slug_viewport_px
                self.vertex_pos = vec4(2.0 * pos.x - 1.0, 1.0 - 2.0 * pos.y, 0.0, 1.0)
            }
        }
        // its rounded corners, drawn over the picture in the ground around it (`ground`)
        draw_mask: mod.draw.DrawQuad{
            ground: uniform(fab.color_editor)
            radius: uniform(fab.radius_lg)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, self.radius)
                sdf.fill(vec4(1.0, 1.0, 1.0, 1.0))
                let outside = 1.0 - sdf.result.w
                return vec4(self.ground.xyz * outside, outside)
            }
        }
        label: FabLabelDim{ text: "" }
    }

    // The camera floating over the whole window (the title bar's camera button): a card dragged
    // by its top, kept inside the window, with Hide.
    mod.widgets.FabCameraFloatBase = #(FabCameraFloat::register_widget(vm))
    mod.widgets.FabCameraFloat = set_type_default() do mod.widgets.FabCameraFloatBase{
        width: Fill
        height: Fill
        card := mod.widgets.FabFloat{
            width: 320
            height: 206
            flow: Overlay
            padding: 3
            cam_box := View{ width: Fill height: Fill camera := mod.widgets.FabPrinterCamera{ draw_mask +: { ground: uniform(fab.color_float) } } }
            head := View{
                width: Fill height: Fit flow: Right padding: 4
                cursor: MouseCursor.Move
                Filler{}
                mod.widgets.FabTip{ text: "Hide the camera"
                    close := mod.widgets.FabIconButton{ a11y: {label: "Hide camera"} draw_bg +: { color: #x000000a0 } draw_icon +: { svg: crate_resource("self://resources/icons/close.svg") color: #xffffffd0 } }
                }
            }
        }
    }

}

/// The printer camera floating over the window while [`crate::api::UiState::camera_pip`] is on
/// (the title bar's camera button): its card
/// where it was dragged (at first the window's bottom right), kept inside the window.
#[derive(Script, ScriptHook, Widget)]
pub struct FabCameraFloat {
    #[deref]
    view: View,
    /// The card's top left, once dragged (window points).
    #[rust]
    at: Option<DVec2>,
    /// A drag by its top: the pointer's offset in the card.
    #[rust]
    grab: Option<DVec2>,
    /// The window's rect as last drawn, that the card stays in.
    #[rust]
    room: Rect,
}

/// Its gap from the window's bottom right at first, clear of the plate view's player.
const FLOAT_INSET: f64 = 16.0;
const FLOAT_BOTTOM: f64 = 96.0;

impl FabCameraFloat {
    fn shown(scope: &mut Scope) -> bool {
        scope.data.get::<AppState>().is_some_and(|s| s.ui.camera_pip)
    }

    fn clamp(&self, at: DVec2, size: DVec2) -> DVec2 {
        let lo = self.room.pos;
        let hi = self.room.pos + self.room.size - size;
        dvec2(at.x.clamp(lo.x, hi.x.max(lo.x)), at.y.clamp(lo.y, hi.y.max(lo.y)))
    }
}

impl Widget for FabCameraFloat {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !Self::shown(scope) {
            self.grab = None;
            return;
        }
        let card = self.view.widget(cx, ids!(card));
        let rect = card.area().rect(cx);
        let on_button = |cx: &Cx, p: DVec2| self.view.widget(cx, ids!(close)).area().rect(cx).contains(p);
        match event {
            // a press on its top (not a button) drags it; any press on it is its own, not the
            // dock's under it
            Event::MouseDown(e) if rect.contains(e.abs) => {
                let head = self.view.widget(cx, ids!(head)).area().rect(cx);
                if e.button.is_primary() && head.contains(e.abs) && !on_button(cx, e.abs) {
                    self.grab = Some(e.abs - rect.pos);
                }
                if !on_button(cx, e.abs) {
                    e.handled.set(card.area());
                }
            }
            Event::MouseMove(e) => {
                if let Some(grab) = self.grab {
                    self.at = Some(self.clamp(e.abs - grab, rect.size));
                    self.view.redraw(cx);
                }
            }
            Event::MouseUp(_) => self.grab = None,
            _ => {}
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(close)).clicked(actions) {
                cx.action(crate::ui::stage::StageAction::CameraPip(false));
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if !Self::shown(scope) {
            return DrawStep::done();
        }
        cx.begin_turtle(walk, Layout::default());
        self.room = cx.turtle().rect();
        let card = self.view.widget(cx, ids!(card));
        let mut card_walk = card.walk(cx);
        let fixed = |s: Size, or: f64| if let Size::Fixed(v) = s { v } else { or };
        let size = dvec2(fixed(card_walk.width, 320.0), fixed(card_walk.height, 206.0));
        let first = self.room.pos + self.room.size - size - dvec2(FLOAT_INSET, FLOAT_BOTTOM);
        let at = self.clamp(self.at.unwrap_or(first), size);
        card_walk = card_walk.with_abs_pos(at);
        card.draw_walk_all(cx, scope, card_walk);
        cx.end_turtle();
        DrawStep::done()
    }
}

/// What the feed thread shares with the widget.
#[derive(Default)]
struct Feed {
    frame: Mutex<Option<DecodedFrame>>,
    status: Mutex<String>,
    /// Milliseconds since `epoch` of the panel's last draw: the thread stops when it goes stale.
    seen_ms: AtomicU64,
    stop: AtomicBool,
    pictures: AtomicU64,
}

const IDLE_STOP: Duration = Duration::from_secs(3);

fn run_feed(conf: String, feed: Arc<Feed>, epoch: Instant) {
    let shown = |feed: &Feed| {
        let seen = Duration::from_millis(feed.seen_ms.load(Ordering::Relaxed));
        !feed.stop.load(Ordering::Acquire) && epoch.elapsed().saturating_sub(seen) < IDLE_STOP
    };
    let say = |feed: &Feed, text: String| { *feed.status.lock().unwrap_or_else(|e| e.into_inner()) = text; makepad_widgets_core::makepad_platform::SignalToUI::set_ui_signal(); };
    while shown(&feed) {
        say(&feed, "Connecting to the printer camera…".into());
        let result = (|| -> Result<(), String> {
            if !shown(&feed) { return Ok(()); }
            let mut rtsp = fab_printer_link::camera::open(&conf).map_err(|e| e.message())?;
            if !shown(&feed) { return Ok(()); }
            let track = rtsp.start().map_err(|e| e.message())?;
            let mut depacketizer = fab_printer_link::camera::Depacketizer::new(track.parameter_sets.clone());
            let mut decoder = VideoStreamDecoder::new(StreamVideoCodec::H264).map_err(|e| format!("camera decoder: {e:?}"))?;
            say(&feed, String::new());
            let mut keepalive = Instant::now();
            let mut started = false;
            let result = loop {
                if !shown(&feed) { break Ok(()); }
                if keepalive.elapsed() >= fab_printer_link::camera::KEEPALIVE {
                    if let Err(e) = rtsp.keepalive() { break Err(e.message()); }
                    keepalive = Instant::now();
                }
                let packet = match rtsp.next() {
                    Ok(fab_printer_link::camera::Incoming::Rtp { channel: 0, packet }) => packet,
                    Ok(_) => continue,
                    Err(e) => break Err(e.message()),
                };
                for unit in depacketizer.push(&packet) {
                    // Decoding starts at a keyframe.
                    started |= unit.keyframe;
                    if !started { continue; }
                    let pts = unit.timestamp as i64 * 1000 / 9; // 90 kHz -> 100 ns
                    match decoder.push_packet(&unit.annex_b, pts) {
                        Ok(frames) => if let Some(frame) = frames.into_iter().last() {
                            *feed.frame.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
                            feed.pictures.fetch_add(1, Ordering::Relaxed);
                            makepad_widgets_core::makepad_platform::SignalToUI::set_ui_signal();
                        },
                        Err(e) => { started = false; log!("fab camera: decode: {e:?}"); }
                    }
                }
            };
            rtsp.teardown().shutdown();
            result
        })();
        match result {
            Ok(()) => break,
            Err(e) => {
                say(&feed, format!("Camera: {e}"));
                // Retry while shown; one viewer at a time, so do not hammer the printer.
                let until = Instant::now() + Duration::from_secs(5);
                while Instant::now() < until && shown(&feed) { std::thread::sleep(Duration::from_millis(200)); }
            }
        }
    }
    say(&feed, String::new());
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabPrinterCamera {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_nv12: DrawNv12,
    #[live] draw_path: DrawFabStrip,
    #[live] draw_far: DrawFabFar,
    #[live] draw_head: DrawFabHead,
    #[live] draw_picture: DrawQuad,
    #[live] draw_bed: DrawQuad,
    #[live] draw_badge: DrawText,
    /// The rounded corners over the picture.
    #[live] draw_mask: DrawQuad,
    #[live]
    label: WidgetRef,
    /// Accessibility can read the status of the displayed slot without keeping a retired feed alive.
    #[rust]
    displayed: Weak<Feed>,
    #[rust] virtual_status: Weak<&'static str>,
}

const CAMERA_SIZE: [usize; 2] = [640, 360];
const NO_JOB: &str = "No print on this printer yet";
const GETTING_PREVIEW: &str = "Getting the job's toolpaths ready";
const JOB_PICTURE: &str = "3D view of the job";

#[derive(Clone, Copy, PartialEq)]
struct CameraKey {
    preview: u64,
    cut: [u32; 3],
    whole_layer: f32,
    active_head: Option<usize>,
    colours: ColourMode,
    bed_min: [f64; 2],
    bed_max: [f64; 2],
    height: f64,
    size: [usize; 2],
}

/// One recording per drawing. Only waiters seen in the preceding drawing have priority,
/// so a hidden or removed camera cannot hold the slot indefinitely.
#[derive(Default)]
pub struct CameraTurns {
    drawing: u64,
    taken: bool,
    next_ticket: u64,
    previous_oldest: Option<u64>,
    oldest_refused: Option<u64>,
}

impl CameraTurns {
    fn take(&mut self, drawing: u64, ticket: &mut Option<u64>) -> bool {
        if self.drawing != drawing {
            self.drawing = drawing;
            self.taken = false;
            self.previous_oldest = self.oldest_refused.take();
        }
        let id = *ticket.get_or_insert_with(|| {
            let id = self.next_ticket;
            self.next_ticket += 1;
            id
        });
        if !self.taken && self.previous_oldest.is_none_or(|oldest| id <= oldest) {
            self.taken = true;
            *ticket = None;
            true
        } else {
            self.oldest_refused = Some(self.oldest_refused.map_or(id, |oldest| oldest.min(id)));
            false
        }
    }
}

pub struct VirtualView {
    pass: DrawPass,
    list: DrawList2d,
    colour: Texture,
    frame: Option<PrintFrame>,
    palette: PrintPalette,
    tiers: FarTiers,
    key: Option<CameraKey>,
    moment: Option<(Arc<PrintPreview>, CameraMoment)>,
    ticket: Option<u64>,
    send: u64,
    recordings: u64,
    status: Arc<&'static str>,
}

impl VirtualView {
    pub fn new(cx: &mut Cx, background: Vec4f) -> Self {
        let [width, height] = CAMERA_SIZE;
        let size = TextureSize::Fixed { width, height };
        let colour = Texture::new_with_format(cx, TextureFormat::RenderBGRAu8 { size: size.clone(), initial: true });
        let depth = Texture::new_with_format(cx, TextureFormat::DepthD32 { size, initial: true });
        let pass = DrawPass::new(cx);
        pass.set_keep_camera_matrix(cx, true);
        pass.set_size(cx, dvec2(width as f64, height as f64));
        pass.set_color_texture(cx, &colour, DrawPassClearColor::ClearWith(background));
        pass.set_depth_texture(cx, &depth, DrawPassClearDepth::ClearWith(1.0));
        Self { pass, list: DrawList2d::new(cx), colour, frame: None, palette: PrintPalette::default(),
            tiers: FarTiers::default(), key: None, moment: None, ticket: None, send: 0, recordings: 0, status: Arc::new(NO_JOB) }
    }

    pub fn pass(&self) -> &DrawPass { &self.pass }

    pub fn recordings(&self) -> u64 { self.recordings }

    /// Present one visible camera. A new report samples the timeline once, even if both hosts draw.
    pub fn draw(&mut self, cx: &mut Cx2d, turns: &mut CameraTurns, camera: &mut FabPrinterCamera, rect: Rect,
        preview: &Arc<PrintPreview>, time: f64, colours: ColourMode, plate: &PlateInfo) -> bool {
        if self.moment.as_ref().is_none_or(|(p, m)| p.id != preview.id || m.time != time) {
            self.moment = Some((preview.clone(), CameraMoment::at(preview, time)));
        }
        let (_, moment) = self.moment.as_ref().unwrap();
        let key = CameraKey { preview: preview.id, cut: moment.cut.0, whole_layer: moment.layers[3],
            active_head: moment.visible_head, colours, bed_min: plate.bed_min, bed_max: plate.bed_max,
            height: plate.max_height, size: CAMERA_SIZE };
        let stale = self.key != Some(key);
        let record = stale && turns.take(cx.drawing_id(), &mut self.ticket);
        if stale && !record {
            let host = cx.get_current_draw_list_id().unwrap();
            cx.redraw_list(host);
            if self.frame.is_none() { return true }
        }
        cx.make_child_pass(&self.pass);
        if record {
            self.key = Some(key);
            self.frame = PrintFrame::camera_at(preview.clone(), moment, colours, self.frame.take());
            self.record(cx, camera, key);
        }
        if self.frame.is_none() { return false }
        let picture = letterbox(rect, key.size[0] as f64, key.size[1] as f64);
        camera.draw_picture.draw_vars.set_texture(0, &self.colour);
        camera.draw_picture.draw_abs(cx, picture);
        camera.draw_picture.draw_vars.empty_texture(0);
        true
    }

    fn record(&mut self, cx: &mut Cx2d, camera: &mut FabPrinterCamera, key: CameraKey) {
        let Some(frame) = &self.frame else { return };
        let size = dvec2(key.size[0] as f64, key.size[1] as f64);
        let (eye, projection) = camera_eye(key.bed_min, key.bed_max, key.height, frame.preview.bounds[1][2] as f64, size.x / size.y);
        cx.begin_pass(&self.pass, Some(1.0));
        self.pass.set_camera(cx, eye, projection);
        self.list.begin_always(cx);
        camera.draw_bed.rect_pos = vec2f(key.bed_min[0] as f32, key.bed_min[1] as f32);
        camera.draw_bed.rect_size = vec2f((key.bed_max[0] - key.bed_min[0]) as f32, (key.bed_max[1] - key.bed_min[1]) as f32);
        cx.add_instance(&mut camera.draw_bed.draw_vars);
        let mut scene = Cx3d::new(cx.cx);
        draw_print_eye(&mut scene, frame, &mut self.palette, (&eye, &projection, size),
            &fab_sdf_tiles::draw::TileLight::default(), (&mut camera.draw_path, &mut camera.draw_far, &mut self.tiers), &mut camera.draw_head);
        // The list now owns its bindings. A hidden host can release the retired job's palette.
        for vars in [&mut camera.draw_path.draw_vars, &mut camera.draw_far.draw_vars, &mut camera.draw_head.draw_vars] {
            vars.texture_slots.fill(None);
        }
        cx.begin_root_turtle(size, Layout::flow_overlay());
        camera.draw_badge.draw_abs(cx, dvec2(10.0, size.y - 32.0), key.colours.label());
        cx.end_pass_sized_turtle();
        self.list.end(cx);
        cx.end_pass(&self.pass);
        self.recordings += 1;
    }
}

fn letterbox(rect: Rect, width: f64, height: f64) -> Rect {
    let scale = (rect.size.x / width).min(rect.size.y / height);
    let size = dvec2(width * scale, height * scale);
    Rect { pos: rect.pos + (rect.size - size) * 0.5, size }
}

/// A fresh slot for each opening, including a return to the same config. A retiring
/// worker can only publish to its own slot. Only this view can upload a slot's frames.
struct LiveView {
    feed: Arc<Feed>,
    epoch: Instant,
    texture: Option<(Texture, [u32; 2], VideoColor)>,
}

impl Drop for LiveView {
    fn drop(&mut self) {
        self.feed.stop.store(true, Ordering::Release);
    }
}

impl PrinterCamera {
    fn open_feed(&self, cx: &Cx, config: &str, feed: Arc<Feed>, epoch: Instant) -> bool {
        #[cfg(test)]
        if let Some(open) = &self.fake_open {
            open(config, feed);
            return true;
        }
        let name = config.to_owned();
        cx.spawn_worker_with(makepad_platform::thread::ThreadOptions {
            name: Some("fab-camera".into()), ..Default::default()
        }, move || run_feed(name, feed, epoch)).map(|worker| worker.detach()).is_ok()
    }

    fn show(&mut self, cx: &mut Cx) -> Option<&LiveView> {
        let CameraSource::Real { config } = &self.source else { return None };
        // The worker drops its reference when both views have been hidden long enough.
        if self.live.as_ref().is_none_or(|v| Arc::strong_count(&v.feed) == 1) {
            let feed = Arc::new(Feed::default());
            let epoch = Instant::now();
            if !self.open_feed(cx, config, feed.clone(), epoch) { return None; }
            self.live = Some(LiveView { feed, epoch, texture: None });
        }
        let live = self.live.as_mut()?;
        live.feed.seen_ms.store(live.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
        let frame = live.feed.frame.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(frame) = frame {
            let size = [frame.width, frame.height];
            match &mut live.texture {
                Some((t, s, color)) if *s == size => {
                    let _ = t.take_vec_u8(cx);
                    t.put_back_vec_u8(cx, frame.nv12, None);
                    *color = frame.color;
                }
                _ => live.texture = Some((Texture::new_nv12(cx, frame.width, frame.height, frame.nv12), size, frame.color)),
            }
        }
        Some(live)
    }
}

impl FabPrinterCamera {
    fn draw_virtual(&mut self, cx: &mut Cx2d, rect: Rect, state: &mut AppState) -> &'static str {
        let print = &mut state.print;
        let snapshot = print.shown_printer().cloned();
        let report = snapshot.as_deref();
        let view = print.camera.virtual_view.get_or_insert_with(|| VirtualView::new(cx, self.draw_bg.color));
        let send = report.map_or(0, |p| p.job_sends);
        // A whole new print may have run while both hosts were hidden.
        if view.send != send {
            view.send = send;
            view.frame = None;
            view.key = None;
            view.moment = None;
        }
        let mut status = NO_JOB;
        let input = match &print.follow.job {
            Some(job) if job.id.is_some() => {
                status = GETTING_PREVIEW;
                if job.preview.is_none() {
                    view.key = None;
                    view.frame = None;
                    view.moment = None;
                }
                job.preview.as_ref().map(|p| (p.clone(), job.time))
            }
            Some(_) => None,
            None => view.moment.as_ref().map(|(preview, moment)| {
                // Follow releases the running job on its terminal report. Finish deposits
                // everything; a stopped print stays at the time the printer reached.
                let time = match report.map(|p| &p.state) {
                    Some(s) if s.gcode_state.as_deref() == Some("FINISH") => preview.total,
                    Some(s) if s.gcode_state.as_deref() == Some("FAILED") => s.print_seconds.unwrap_or(moment.time),
                    _ => moment.time,
                };
                (preview.clone(), time)
            }),
        };
        if let Some((preview, time)) = input {
            if view.draw(cx, &mut print.camera_turns, self, rect, &preview, time, state.view.colours, &state.plate) {
                status = JOB_PICTURE;
            }
        } else {
            cx.make_child_pass(&view.pass);
        }
        if *view.status != status { view.status = Arc::new(status); }
        self.virtual_status = Arc::downgrade(&view.status);
        if status == JOB_PICTURE { "" } else { status }
    }
}

impl Widget for FabPrinterCamera {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Image;
        makepad_platform::a11y::set_string(&mut node.name, "Printer camera");
        let status = self.displayed.upgrade().filter(|f| !f.stop.load(Ordering::Acquire)).map(|f| {
            let status = f.status.lock().unwrap_or_else(|e| e.into_inner());
            if status.is_empty() && f.pictures.load(Ordering::Relaxed) > 0 { "live".into() } else { status.clone() }
        }).or_else(|| self.virtual_status.upgrade().map(|s| (*s).into())).unwrap_or_default();
        makepad_platform::a11y::set_string(&mut node.value, &status);
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if matches!(event, Event::Signal) && self.displayed.strong_count() > 0 {
            self.draw_bg.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        let state = scope.data.get_mut::<AppState>();
        if state.as_ref().is_some_and(|s| matches!(s.print.camera.source, CameraSource::Virtual(_))) {
            let clipped = rect.clip(cx.turtle_ancestor_clip());
            if !cx.turtle().rect_is_visible(rect) || clipped.size.x <= 0.0 || clipped.size.y <= 0.0 {
                return DrawStep::done();
            }
        }
        self.draw_bg.draw_abs(cx, rect);
        let mut displayed = Weak::new();
        self.virtual_status = Weak::new();
        let text = match state {
            Some(s) if matches!(s.print.camera.source, CameraSource::Virtual(_)) => self.draw_virtual(cx, rect, s).into(),
            Some(s) if matches!(s.print.camera.source, CameraSource::Real { .. }) => {
                if let Some(live) = s.print.camera.show(cx) {
                    displayed = Arc::downgrade(&live.feed);
                    if let Some((tex, [w, h], color)) = &live.texture {
                        self.draw_nv12.set_nv12(tex, *w, *h, *color, None);
                        self.draw_nv12.draw_abs(cx, letterbox(rect, *w as f64, *h as f64));
                        // The submitted draw holds the texture for this frame. A hidden widget
                        // must not retain another reference after its source is retired.
                        self.draw_nv12.draw_vars.empty_texture(0);
                        self.draw_nv12.draw_vars.empty_texture(1);
                    }
                    live.feed.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
                } else { String::new() }
            }
            _ => match state.and_then(|s| s.print.shown_printer()) {
                None => "No printer is linked. Pick a virtual printer in the Printer panel, or add yours in ~/.makepad/fab/printers.".into(),
                Some(p) if p.status != ConnectionStatus::Connected => p.status.label().into(),
                Some(_) => "This printer offers no live view (check the camera setting on the printer).".into(),
            },
        };
        self.displayed = displayed;
        self.draw_mask.draw_abs(cx, rect);
        self.label.set_text(cx, &text);
        if !text.is_empty() {
            // It wraps short of the panel's button at the top right.
            let inner = Rect { pos: rect.pos + dvec2(16.0, 16.0), size: dvec2((rect.size.x - 32.0 - 100.0).max(0.0), 40.0) };
            self.label.draw_walk_all(cx, scope, Walk::abs_rect(inner));
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_printer_eye_fits_the_bed_and_job_from_the_high_front_left() {
        for (min, max, height) in [
            ([-100.0, -80.0], [100.0, 120.0], 200.0),
            ([0.0, 0.0], [350.0, 320.0], 325.0),
        ] {
            for aspect in [16.0 / 9.0, 9.0 / 16.0] {
                for top in [1.0, height] {
                    let (view, projection) = camera_eye(min, max, height, top, aspect);
                    let project = |x: f64, y: f64, z: f64| {
                        let room = vec4(x as f32 * 0.001, z as f32 * 0.001, -y as f32 * 0.001, 1.0);
                        projection.transform_vec4(view.transform_vec4(room))
                    };
                    let mut lo = [f32::INFINITY; 2];
                    let mut hi = [f32::NEG_INFINITY; 2];
                    for x in [min[0], max[0]] {
                        for y in [min[1], max[1]] {
                            for z in [0.0, top] {
                                let p = project(x, y, z);
                                assert!(p.w > 0.0 && p.x.abs() < p.w && p.y.abs() < p.w, "corner {x}, {y}, {z} fits at aspect {aspect}, job height {top}");
                                assert!(0.0 < p.z && p.z < p.w, "corner {x}, {y}, {z} is within clip depth at aspect {aspect}, job height {top}");
                                for (axis, coord) in [p.x / p.w, p.y / p.w].into_iter().enumerate() {
                                    lo[axis] = lo[axis].min(coord);
                                    hi[axis] = hi[axis].max(coord);
                                }
                            }
                        }
                    }
                    let share = (hi[0] - lo[0]).max(hi[1] - lo[1]) * 0.5;
                    assert!(share >= 0.94, "the limiting axis fills at least 94% (95% fit with rounding room), got {share} at aspect {aspect}, job height {top}");
                    assert!(project(min[0], min[1], 0.0).w < project(max[0], max[1], 0.0).w,
                        "the front-left bed corner is nearer than the back-right at aspect {aspect}, job height {top}");
                }
            }
        }
    }

    #[test]
    fn only_a_connected_real_link_with_a_camera_can_supply_a_config() {
        let pick = PrinterChoice::Real("fixture".into());
        let mut report = Snapshot { status: ConnectionStatus::Connected, ..Default::default() };
        Arc::make_mut(&mut report.state).camera = true;
        let mut camera = PrinterCamera::default();
        camera.update_source(&pick, false, Some(&report));
        assert!(matches!(camera.source, CameraSource::None), "the owner must hold a real link");
        camera.update_source(&PrinterChoice::Virtual("fixture".into()), true, Some(&report));
        assert!(matches!(camera.source, CameraSource::None), "a standalone choice has no config");
        Arc::make_mut(&mut report.state).camera = false;
        camera.update_source(&pick, true, Some(&report));
        assert!(matches!(camera.source, CameraSource::None), "the printer must report a camera");
        Arc::make_mut(&mut report.state).camera = true;
        report.status = ConnectionStatus::Connecting;
        camera.update_source(&pick, true, Some(&report));
        assert!(matches!(camera.source, CameraSource::None), "the printer must be connected");
    }

    #[test]
    fn dock_and_pip_share_only_the_current_printers_feed() {
        use crate::print::{Bead, BeadStreams, PREVIEW_SLOTS};
        use makepad_widgets_core::makepad_draw::cx_draw::CxDraw;
        use makepad_widgets_core::makepad_platform::retained_instances::RetainedInstances;
        use std::{cell::RefCell, rc::Rc};

        let mut cx = Cx::new(Box::new(|_, _| {}));
        let root = cx.with_vm(|vm| {
            makepad_widgets_core::script_mod_with(vm, WindowFamilies::default(), &[makepad_widgets_fab::fab_mod]);
            makepad_xr_stream::serve_script_mod(vm);
            crate::theme::script_mod(vm);
            crate::ui::script_mod_kit(vm);
            makepad_render::script_mod(vm);
            crate::viewport::print::script_mod(vm);
            vm.capture_errors();
            super::script_mod(vm);
            let value = vm.eval(script! {
                use mod.prelude.widgets_internal.*
                use mod.widgets.*
                View{width: Fill height: Fill flow: Overlay
                    dock := View{width: Fill height: Fill cam_box := View{width: Fill height: Fill camera := FabPrinterCamera{}}}
                    pip := View{width: 320 height: 206 cam_box := View{width: Fill height: Fill camera := FabPrinterCamera{}}}}
            });
            let root = WidgetRef::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty(), "two camera hosts must instantiate");
            root
        });
        let views = [root.widget(&cx, ids!(dock.cam_box.camera)),
            root.widget(&cx, ids!(pip.cam_box.camera))];
        let pass = DrawPass::new(&mut cx);
        let mut list = DrawList2d::new(&mut cx);
        let offscreen = std::cell::Cell::new(false);
        let draw_other_first = std::cell::Cell::new(false);
        let retry_only = std::cell::Cell::new(false);
        let mut other_view = None;
        let mut draw = |cx: &mut Cx, state: &mut AppState| {
            let size = dvec2(320.0, 206.0);
            pass.set_size(cx, size);
            if !retry_only.replace(false) { cx.redraw_all(); }
            let event = std::mem::take(&mut cx.new_draw_event);
            {
                let mut draw = CxDraw::new(cx, &event);
                let mut frame = Cx2d::new(&mut draw);
                frame.begin_pass(&pass, Some(1.0));
                list.begin_always(&mut frame);
                frame.begin_root_turtle(size, Layout::default());
                if draw_other_first.replace(false) {
                    std::mem::swap(&mut other_view, &mut state.print.camera.virtual_view);
                    views[1].borrow_mut::<FabPrinterCamera>().unwrap()
                        .draw_virtual(&mut frame, Rect { pos: dvec2(0.0, 0.0), size }, state);
                    std::mem::swap(&mut other_view, &mut state.print.camera.virtual_view);
                }
                if offscreen.get() {
                    // A child of a card whose own turtle is entirely past the viewport edge.
                    let card = Walk::abs_rect(Rect { pos: dvec2(size.x + 10.0, 0.0), size });
                    frame.begin_turtle(card, Layout::default());
                    views[0].draw_all(&mut frame, &mut Scope::with_data(state));
                    frame.end_turtle();
                } else {
                    root.draw_all(&mut frame, &mut Scope::with_data(state));
                }
                frame.end_pass_sized_turtle();
                list.end(&mut frame);
                frame.end_pass(&pass);
            }
            views.each_ref().map(|v| v.borrow::<FabPrinterCamera>().unwrap().draw_nv12.draw_vars.area.is_valid(cx))
        };
        let status = |cx: &Cx| views.each_ref().map(|v| {
            let mut node = a11y_tree::Node::default();
            v.borrow::<FabPrinterCamera>().unwrap().semantics(cx, &mut node);
            node.value
        });
        let mut state = AppState::default();
        let opens = Rc::new(RefCell::new(Vec::new()));
        let worker = opens.clone();
        state.print.camera.fake_open = Some(Box::new(move |key, slot| worker.borrow_mut().push((key.to_owned(), slot))));
        let before = fab_printer_link::entry_counts().camera_open;
        let config = "fixture";
        state.print.links.picked = PrinterChoice::Real(config.into());
        let report = |state: &mut AppState, held_real, virtual_printer, display: &str| {
            let mut snapshot = Snapshot { name: display.into(), status: ConnectionStatus::Connected, virtual_printer, ..Default::default() };
            Arc::make_mut(&mut snapshot.state).camera = true;
            state.print.camera.update_source(&state.print.links.picked, held_real, Some(&snapshot));
            state.print.printers.set(&state.print.links.picked, Arc::new(snapshot));
        };
        report(&mut state, false, true, config);
        assert_eq!(draw(&mut cx, &mut state), [false; 2]);
        // Even a report inconsistent with the held link must never open a real feed.
        report(&mut state, true, true, config);
        assert_eq!(draw(&mut cx, &mut state), [false; 2]);
        assert!(opens.borrow().is_empty(), "a virtual snapshot opened a camera feed");
        assert_eq!(fab_printer_link::entry_counts().camera_open, before);

        let pip = root.view(&cx, ids!(pip));
        pip.set_visible(&mut cx, false);
        assert_eq!(status(&cx), [NO_JOB; 2], "a new virtual printer has no picture");
        state.print.follow.job = Some(crate::print::follow::PrinterJob { id: Some("job".into()), ..Default::default() });
        draw(&mut cx, &mut state);
        assert_eq!(status(&cx)[0], GETTING_PREVIEW, "a matched job waits for its own toolpaths");
        state.plate.bed_max = [350.0, 320.0];
        state.plate.max_height = 325.0;
        struct ToolChange(AtomicU64);
        impl crate::print::PrintTimeline for ToolChange {
            fn now(&self, t: f64) -> crate::print::PrintNow {
                self.0.fetch_add(1, Ordering::Relaxed);
                crate::print::PrintNow { active: usize::from(t >= 24.0), ..Default::default() }
            }
        }
        let timeline = Arc::new(ToolChange(AtomicU64::new(0)));
        let mut preview = crate::print::follow::tests::preview_of(3);
        let fixture = Arc::get_mut(&mut preview).unwrap();
        fixture.timeline = timeline.clone();
        // Put each surface bead at its layer's height so the deposited top rises at each cut.
        let beads: Vec<_> = fixture.beads.each().zip(&fixture.layers).flat_map(|(slots, layer)| {
            let mut bead = Bead::of(slots);
            bead.a[2] = layer.z;
            bead.b[2] = layer.z;
            bead.pack()
        }).collect();
        fixture.beads = BeadStreams::surface(RetainedInstances::new(PREVIEW_SLOTS, beads.into()).unwrap());
        fixture.bounds[1][2] = fixture.layers.last().unwrap().z;
        // The first layer's last move finishes after the second layer's first bead starts.
        fixture.layers[0].t_end = 22.0;
        let job = state.print.follow.job.as_mut().unwrap();
        job.preview = Some(preview.clone());
        job.time = 15.0;
        let count = |s: &AppState| s.print.camera.virtual_view.as_ref().unwrap().recordings;
        let dock = || views[0].borrow::<FabPrinterCamera>().unwrap();
        draw_other_first.set(true);
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 0, "the dock records nothing while its first picture waits");
        assert_eq!(dock().label.text(), "", "the dock label stays empty while its first picture waits");
        assert_eq!(status(&cx)[0], JOB_PICTURE, "the dock reports the job picture during its first wait");
        assert!(!dock().draw_picture.draw_vars.area.is_valid(&cx), "the waiting dock draws no picture quad");
        let waiting_rect = dock().draw_bg.area().rect(&cx);
        assert!(waiting_rect.size.x > 0.0 && waiting_rect.size.y > 0.0, "the waiting dock occupies a visible tile");
        retry_only.set(true);
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 1, "the dock records its first picture on the next drawing");
        assert!(dock().draw_picture.draw_vars.area.is_valid(&cx), "the dock presents its first picture on the next drawing");
        assert_eq!(dock().draw_bg.area().rect(&cx), waiting_rect, "the dock keeps its tile rect when the first picture arrives");
        assert_eq!(dock().label.text(), "", "the dock label stays empty when its first picture arrives");
        assert_eq!(status(&cx)[0], JOB_PICTURE, "the dock describes the job picture");
        let camera_pass = state.print.camera.virtual_view.as_ref().unwrap().pass.draw_pass_id();
        let pass_camera = |cx: &Cx| {
            let u = &cx.passes[camera_pass].pass_uniforms;
            (u.camera_view, u.camera_projection)
        };
        let first_camera = pass_camera(&cx);
        assert!(cx.passes[camera_pass].paint_dirty, "a fresh recording requests a paint");
        // This harness has no GPU consumer. Model the flag the backend clears after painting.
        cx.passes[camera_pass].paint_dirty = false;
        let retained = cx.passes[camera_pass].main_draw_list_id.expect("the camera recorded its list");
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 1, "the same key reuses the camera recording");
        assert_eq!(timeline.0.load(Ordering::Relaxed), 2, "each camera samples the reported time once across the wait, retry and cached draw");
        assert!(!cx.passes[camera_pass].paint_dirty, "a cached camera does not request a paint");
        assert_eq!(cx.passes[camera_pass].main_draw_list_id, Some(retained), "a cached draw never begins the camera pass");
        assert!(!cx.pass_attachment_is_stale(camera_pass), "a cached draw reattaches the camera pass");
        state.print.follow.job.as_mut().unwrap().time = 17.0;
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 1, "a new time inside the same bead keeps the picture");
        state.print.follow.job.as_mut().unwrap().time = 21.0;
        offscreen.set(true);
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 1, "a card outside the viewport does not record an advanced cut");
        offscreen.set(false);
        draw_other_first.set(true);
        draw(&mut cx, &mut state);
        let waiting = state.print.camera.virtual_view.as_ref().unwrap();
        assert_eq!(waiting.recordings, 1, "the dock waits when another camera took this drawing");
        assert_eq!(waiting.frame.as_ref().unwrap().cut, preview.cut_at(17.0), "a refused dock retains the frame its picture shows");
        assert!(!cx.passes[camera_pass].paint_dirty, "presenting the previous picture requests no camera paint");
        let picture = views[0].borrow::<FabPrinterCamera>().unwrap().draw_picture.draw_vars.area;
        assert!(picture.is_valid(&cx) && status(&cx)[0] == JOB_PICTURE, "a refused dock presents its picture without a status label");
        assert!(cx.new_draw_event.draw_lists.contains(&picture.draw_list_id().unwrap()), "the refused dock requests its own next drawing");
        retry_only.set(true);
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 2, "the refused dock records on its requested drawing without another report");
        assert_eq!(state.print.camera.virtual_view.as_ref().unwrap().frame.as_ref().unwrap().cut, preview.cut_at(21.0),
            "the retry records the visible report that crossed a bead start");
        assert_eq!(pass_camera(&cx), first_camera, "the view and projection stay fixed when the next layer starts");
        state.print.follow.job.as_mut().unwrap().time = 23.0;
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 3, "finishing a whole layer changes the picture at the same bead cut");
        state.print.follow.job.as_mut().unwrap().time = 25.0;
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 4, "switching the active head changes the picture at the same bead cut");
        let samples = timeline.0.load(Ordering::Relaxed);
        state.view.colours = ColourMode::Material;
        pip.set_visible(&mut cx, true);
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 5, "both widgets share one recording when the colour mode changes");
        assert_eq!(timeline.0.load(Ordering::Relaxed), samples, "changing colours and drawing both hosts reuses the reported-time sample");
        assert_eq!(status(&cx), [JOB_PICTURE; 2], "both widgets present the shared picture");
        // the camera moved to the other host: this one hides
        state.ui.camera_pip = true;
        let dock_host = root.view(&cx, ids!(dock));
        dock_host.set_visible(&mut cx, false);
        pip.set_visible(&mut cx, false);
        state.print.follow.job.as_mut().unwrap().time = 31.0;
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 5, "two hidden cameras do no recording");
        pip.set_visible(&mut cx, true);
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 6, "PiP alone records the newest time");
        state.print.follow.job = None;
        Arc::make_mut(&mut Arc::make_mut(state.print.target_printer_mut().unwrap()).state).gcode_state = Some("FINISH".into());
        draw(&mut cx, &mut state);
        let view = state.print.camera.virtual_view.as_ref().unwrap();
        assert_eq!(view.moment.as_ref().unwrap().1.time, preview.total, "a finish takes the clock to the end");
        assert!(view.frame.as_ref().unwrap().heads.is_empty(), "the heads disappear when deposition finishes");
        assert_eq!(view.frame.as_ref().unwrap().cut, preview.cut_at(preview.total), "a terminal report records the final cut");
        assert_eq!(pass_camera(&cx), first_camera, "the final cut keeps the first recording's view and projection");
        let [w, h] = CAMERA_SIZE;
        assert_eq!(first_camera, camera_eye(state.plate.bed_min, state.plate.bed_max, state.plate.max_height,
            preview.bounds[1][2] as f64, w as f64 / h as f64), "the first recording frames the job's full extrusion height");
        draw(&mut cx, &mut state);
        assert_eq!(count(&state), 7, "the final picture stays cached");
        Arc::make_mut(state.print.target_printer_mut().unwrap()).job_sends += 1;
        draw(&mut cx, &mut state);
        assert_eq!(status(&cx)[1], NO_JOB, "a newer send with no matched preview cannot inherit the previous print's picture");
        assert_eq!(fab_printer_link::entry_counts().camera_open, before, "virtual draws never open the camera feed");
        state.ui.camera_pip = false;
        dock_host.set_visible(&mut cx, true);

        report(&mut state, true, false, "display");
        assert!(state.print.camera.virtual_view.is_none(), "changing source retires the virtual target");
        assert_eq!(draw(&mut cx, &mut state), [false; 2]);
        assert_eq!(opens.borrow().len(), 1, "dock and PiP must share the one real viewer");
        let (key, old) = opens.borrow()[0].clone();
        assert!(key == config, "the feed must open under the picked config");
        let publish = |slot: &Feed, value| {
            *slot.frame.lock().unwrap() = Some(DecodedFrame {
                width: 2, height: 2, nv12: vec![value; 6], pts_100ns: 0, gpu: None, color: VideoColor::default(),
            });
            *slot.status.lock().unwrap() = "Receiving video".into();
            slot.pictures.fetch_add(1, Ordering::Relaxed);
        };
        publish(&old, 81);
        report(&mut state, true, false, "another display");
        assert_eq!(draw(&mut cx, &mut state), [true; 2], "both views drew the shared NV12 frame");
        let texture = &state.print.camera.live.as_ref().unwrap().texture.as_ref().unwrap().0;
        let bytes = texture.take_vec_u8(&mut cx);
        assert_eq!(bytes, [81; 6]);
        texture.put_back_vec_u8(&mut cx, bytes, None);
        assert_eq!(status(&cx), ["Receiving video"; 2]);

        report(&mut state, false, true, config);
        assert!(old.stop.load(Ordering::Acquire), "the switch stops the worker before either view draws");
        assert!(state.print.camera.live.is_none(), "the old frame slot and texture are detached");
        assert_eq!(status(&cx), [""; 2], "both views released the old stream's status");
        publish(&old, 99);
        assert_eq!(draw(&mut cx, &mut state), [false; 2], "a late real frame was drawn in Virtual");
        assert_eq!(count(&state), 0, "returning to Virtual creates a fresh target");
        assert_eq!(opens.borrow().len(), 1);
        report(&mut state, true, false, "display");
        assert_eq!(draw(&mut cx, &mut state), [false; 2], "a reconnect reused an old frame");
        assert_eq!(opens.borrow().len(), 2, "reconnecting opens exactly one new feed");
        let (key, new) = opens.borrow()[1].clone();
        assert!(key == config && !Arc::ptr_eq(&old, &new));
        assert_eq!(status(&cx), [""; 2]);
        assert_eq!(fab_printer_link::entry_counts().camera_open, before, "the fake never enters RTSP");
    }
}
