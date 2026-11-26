//! The linked printer's live view, floating over the window ([`FabCameraFloat`], opened from
//! the title bar's camera button). The stream runs only while it is shown: the printer
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
    use mod.widgets.*
    mod.widgets.FabPrinterCameraBase = #(FabPrinterCamera::register_widget(vm))
    mod.widgets.FabPrinterCamera = set_type_default() do mod.widgets.FabPrinterCameraBase{
        width: Fill
        height: Fill
        draw_bg +: {color: #x101214}
        draw_nv12: mod.draw.DrawNv12{}
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
            cam_box := View{ width: Fill height: Fill camera := mod.widgets.FabPrinterCamera{} }
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
