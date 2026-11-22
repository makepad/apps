//! The Camera panel: the linked printer's live view (the Monitor workspace,
//! or the plate view's corner). The stream runs only while it is shown: the printer
//! serves one live viewer, so a panel that stops drawing lets its feed tear
//! the session down within a few seconds.
//!
//! Main-loop cost: a mutex check per signal and one NV12 texture upload per
//! new picture (about 3 MB at 1080p, at the camera's rate); nothing while the
//! panel is not shown.

use crate::api::AppState;
use makepad_video::{DecodedFrame, StreamVideoCodec, VideoColor, VideoStreamDecoder};
use makepad_widgets_core::makepad_draw::shader::draw_nv12::DrawNv12;
use makepad_widgets_core::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    mod.widgets.FabPrinterCameraBase = #(FabPrinterCamera::register_widget(vm))
    mod.widgets.FabPrinterCamera = set_type_default() do mod.widgets.FabPrinterCameraBase{
        width: Fill
        height: Fill
        draw_bg +: {color: #x101214}
        draw_nv12: mod.draw.DrawNv12{}
        label: FabLabelDim{ text: "" }
    }

    mod.widgets.FabCameraPanelBase = #(FabCameraPanel::register_widget(vm))
    mod.widgets.FabCameraPanel = set_type_default() do mod.widgets.FabCameraPanelBase{
        width: Fill
        height: Fill
        flow: Overlay
        show_bg: true
        draw_bg +: { color: #x101214 }
        cam_box := View{ width: Fill height: Fill camera := mod.widgets.FabPrinterCamera{} }
        moved := View{
            visible: false
            width: Fill height: Fill align: Align{x: 0.5 y: 0.5}
            mod.widgets.FabLabelDim{ text: "The camera is showing in the corner of the plate view." }
        }
        head := View{
            width: Fill height: Fit flow: Right padding: 6
            Filler{}
            mod.widgets.FabTip{ text: "Show the camera in the corner of the plate view"
                pop := mod.widgets.FabButton{ text: "In the view" }
            }
        }
    }
}

/// The Camera panel: the live view, and a button that moves it into the
/// plate view's corner (the printer serves one live viewer, so the panel's
/// own feed rests while the corner one runs).
#[derive(Script, ScriptHook, Widget)]
pub struct FabCameraPanel {
    #[deref]
    view: View,
}

impl Widget for FabCameraPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Camera");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(head.pop)).clicked(actions) {
                cx.action(crate::ui::stage::StageAction::CameraPip(true));
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let pip = scope.data.get::<AppState>().is_some_and(|s| s.ui.camera_pip);
        self.view.view(cx, ids!(cam_box)).set_visible(cx, !pip);
        self.view.view(cx, ids!(moved)).set_visible(cx, pip);
        self.view.view(cx, ids!(head)).set_visible(cx, !pip);
        self.view.draw_walk(cx, scope, walk)
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
        !feed.stop.load(Ordering::Relaxed) && epoch.elapsed().saturating_sub(seen) < IDLE_STOP
    };
    let say = |feed: &Feed, text: String| { *feed.status.lock().unwrap_or_else(|e| e.into_inner()) = text; makepad_widgets_core::makepad_platform::SignalToUI::set_ui_signal(); };
    while shown(&feed) {
        say(&feed, "Connecting to the printer camera…".into());
        let result = (|| -> Result<(), String> {
            let mut rtsp = fab_printer_link::camera::open(&conf).map_err(|e| e.message())?;
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
    #[live]
    label: WidgetRef,
    #[rust]
    feed: Option<(String, Arc<Feed>)>,
    #[rust]
    epoch: Option<Instant>,
    #[rust]
    texture: Option<(Texture, [u32; 2], VideoColor)>,
}

impl Drop for FabPrinterCamera {
    fn drop(&mut self) {
        if let Some((_, feed)) = &self.feed { feed.stop.store(true, Ordering::Relaxed); }
    }
}

impl FabPrinterCamera {
    fn ensure_feed(&mut self, conf: &str) {
        let epoch = *self.epoch.get_or_insert_with(Instant::now);
        let running = self.feed.as_ref().is_some_and(|(name, feed)| {
            name == conf && !feed.stop.load(Ordering::Relaxed) && Arc::strong_count(feed) > 1
        });
        if let Some((_, feed)) = &self.feed { feed.seen_ms.store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed); }
        if running { return; }
        if let Some((_, old)) = self.feed.take() { old.stop.store(true, Ordering::Relaxed); }
        let feed = Arc::new(Feed::default());
        feed.seen_ms.store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
        let (thread_feed, name) = (feed.clone(), conf.to_string());
        if std::thread::Builder::new().name("fab-camera".into()).spawn(move || run_feed(name, thread_feed, epoch)).is_ok() {
            self.feed = Some((conf.to_string(), feed));
        }
    }
}

impl Widget for FabPrinterCamera {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Image;
        makepad_platform::a11y::set_string(&mut node.name, "Printer camera");
        let status = self.feed.as_ref().map(|(_, f)| f.status.lock().unwrap_or_else(|e| e.into_inner()).clone()).unwrap_or_default();
        makepad_platform::a11y::set_string(&mut node.value, if self.texture.is_some() && status.is_empty() { "live" } else { &status });
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Event::Signal = event {
            let Some((_, feed)) = &self.feed else { return };
            let frame = feed.frame.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(frame) = frame {
                let size = [frame.width, frame.height];
                match &self.texture {
                    Some((t, s, _)) if *s == size => { let _ = t.take_vec_u8(cx); t.put_back_vec_u8(cx, frame.nv12, None); }
                    _ => self.texture = Some((Texture::new_nv12(cx, frame.width, frame.height, frame.nv12), size, frame.color)),
                }
                if let Some(t) = self.texture.as_mut() { t.2 = frame.color; }
            }
            self.draw_bg.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.draw_abs(cx, rect);
        let printer = scope.data.get::<AppState>().and_then(|s| s.print.printer.clone());
        let text = match &printer {
            None => "No printer is linked. Add one in ~/.makepad/fab/printers.".to_string(),
            Some(p) if p.status != fab_printer_link::ConnectionStatus::Connected => format!("{}: {}", p.name, p.status.label()),
            Some(p) if !p.state.camera => format!("{} offers no live view (check the camera setting on the printer).", p.name),
            Some(p) => {
                self.ensure_feed(&p.name);
                self.feed.as_ref().map(|(_, f)| f.status.lock().unwrap_or_else(|e| e.into_inner()).clone()).unwrap_or_default()
            }
        };
        if let Some((tex, [w, h], color)) = &self.texture {
            // Letterboxed, aspect kept.
            let scale = (rect.size.x / *w as f64).min(rect.size.y / *h as f64);
            let size = dvec2(*w as f64 * scale, *h as f64 * scale);
            let pos = rect.pos + (rect.size - size) * 0.5;
            self.draw_nv12.set_nv12(tex, *w, *h, *color, None);
            self.draw_nv12.draw_abs(cx, Rect { pos, size });
        }
        if !text.is_empty() {
            self.label.set_text(cx, &text);
            // It wraps short of the panel's button at the top right.
            let inner = Rect { pos: rect.pos + dvec2(16.0, 16.0), size: dvec2((rect.size.x - 32.0 - 100.0).max(0.0), 40.0) };
            self.label.draw_walk_all(cx, scope, Walk::abs_rect(inner));
        }
        DrawStep::done()
    }
}
