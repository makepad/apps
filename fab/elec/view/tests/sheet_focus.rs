//! The sheet's line shader on the GPU, through a window's pixels:
//! - An emphasised net (a hovered pin's) keeps its voltage's colour: the canvas draws the emphasis
//!   the way it draws an emphasised item, its focus accent as a rim just outside the stroke and a
//!   quarter tint inside, so a 5 V net never reads as the ramp's 0 V blue (the board instead draws
//!   the net in its own accent, `live::net_color`). Drawn beside the same net without emphasis.
//! - A pressed button draws closed: the lines whose pane-state row has y = 1 move their ends by
//!   their press offsets, while another item's lines with the same offsets stay put.
#![cfg(not(target_arch = "wasm32"))]

use fab_elec_view::canvas::draw::{CanvasLayer, CanvasLive, CanvasUniforms, Common, DrawCanvasLine, LineInst, LineTemplate};
use fab_elec_view::canvas::looks::DARK;
use fab_elec_view::live::*;
use fab_elec_view::ItemStates;
use makepad_widgets_core::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const WIDTH: usize = 64;
const HEIGHT: usize = 36;
/// The wires' centre lines, in points from the top: net 0 emphasised, net 1 not, both at 5 V.
const WIRES: [f32; 2] = [6.0, 18.0];
/// The wires' width in points.
const WIDE: f32 = 4.0;
/// Two actuator lines at rest, in points from the top: the pressed item's left (x 4..28), another
/// item's right (x 36..60); both move up by `RISE` when their item is pressed.
const REST: f32 = 30.0;
const RISE: f32 = 6.0;
const PRESSED_ITEM: f32 = 3.0;

#[test]
fn the_sheet_rims_an_emphasised_net_in_its_voltage_and_closes_a_pressed_button() {
    std::env::set_var("MAKEPAD_HAL_BACKEND", "offscreen");
    struct State {
        _window: WindowHandle,
        pass: DrawPass,
        list: DrawList2d,
        line: DrawCanvasLine,
        layer: CanvasLayer,
        rows: LiveRows,
        pane: ItemStates,
        live: Option<CanvasLive>,
        ready_frames: u32,
    }
    let look = LiveLook::dark();
    let mut state: Option<State> = None;
    let done = Rc::new(Cell::new(false));
    let done_in_draw = done.clone();
    let frames = Rc::new(Cell::new((0u32, 0u32)));
    let frames_in_draw = frames.clone();
    let dpi_seen = Rc::new(Cell::new(1.0f64));
    let dpi_in_draw = dpi_seen.clone();
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
        if matches!(event, Event::Startup) {
            let line = cx.with_vm(|vm| {
                makepad_platform::script::script_mod(vm);
                makepad_draw::script_mod(vm);
                makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
                fab_elec_view::live::script_mod(vm);
                fab_elec_view::canvas::draw::script_mod(vm);
                let v = vm.eval(script! {
                    use mod.draw.*
                    DrawCanvasLine{}
                });
                DrawCanvasLine::script_from_value(vm, v)
            });
            let mut window = WindowHandle::new(cx);
            window.configure_window(cx, dvec2(WIDTH as f64, HEIGHT as f64), dvec2(0.0, 0.0), false, "Sheet focus".into());
            let pass = DrawPass::new(cx);
            window.set_pass(cx, &pass);
            pass.set_window_clear_color(cx, vec4(0.0, 0.0, 0.0, 1.0));
            let list = DrawList2d::new(cx);
            let mut rows = LiveRows::new();
            rows.resize(2);
            rows.set(0, net_row(Some(5.0), 0.0, 1.0, &look));
            rows.set(1, net_row(Some(5.0), 0.0, 0.0, &look));
            let mut pane = ItemStates::new();
            pane.write(PRESSED_ITEM as usize + 2, vec![(PRESSED_ITEM as u32, [0.0, 1.0, 0.0, 0.0])]);
            state = Some(State { _window: window, pass, list, line, layer: CanvasLayer::default(), rows, pane, live: None, ready_frames: 0 });
            cx.redraw_all();
        }
        if matches!(event, Event::NextFrame(_)) {
            cx.redraw_all();
        }
        if let Event::Draw(event) = event {
            let s = state.as_mut().unwrap();
            let ready = s.line.draw_vars.draw_shader_id.is_some_and(|id| cx.draw_shader_ready(id, true));
            if ready && s.layer.count() == 0 {
                let template = LineTemplate::capture(cx, &s.line).expect("the line shader's instance layout");
                let mut out = Vec::new();
                for (net, y) in WIRES.iter().enumerate() {
                    let mut c = Common::at([0.0, 0.0, 0.0], [0.5, 0.5, 0.5, 1.0]);
                    c.live_ix = [net as f32, -1.0, -1.0, 0.0];
                    template.append(&mut out, &LineInst { c, seg: [4.0, -y, WIDTH as f32 - 4.0, -y], width: [WIDE, 0.0], ..LineInst::default() });
                }
                for (item, x) in [(PRESSED_ITEM, 4.0), (PRESSED_ITEM + 1.0, 36.0)] {
                    let mut c = Common::at([0.0, 0.0, 0.0], [0.5, 0.5, 0.5, 1.0]);
                    c.item = item;
                    template.append(&mut out, &LineInst {
                        c, seg: [x, -REST, x + 24.0, -REST], width: [WIDE, 0.0], press: [0.0, RISE, 0.0, RISE],
                        ..LineInst::default()
                    });
                }
                s.layer.set(template.slots(), out);
            }
            if s.live.is_none() {
                s.live = Some(CanvasLive::off(cx).with_pane(s.pane.texture(cx)));
            }
            let mut live = s.live.clone().unwrap();
            live.nets = s.rows.texture(cx).clone();
            let mut draw = CxDraw::new(cx, event);
            let mut cx = Cx2d::new(&mut draw);
            cx.begin_pass(&s.pass, None);
            s.list.begin_always(&mut cx);
            let dpi = cx.current_dpi_factor();
            dpi_in_draw.set(dpi);
            cx.begin_root_turtle(dvec2(WIDTH as f64 / dpi, HEIGHT as f64 / dpi), Layout::flow_overlay());
            let uniforms = CanvasUniforms {
                cam: [0.0, 0.0, 1.0, dpi as f32],
                motion_t: 0.0,
                hover_item: -1.0,
                hover_mix: 0.0,
                drag_item: -1.0,
                drag_offset: [0.0; 2],
                layer_alpha: 1.0,
                time: 0.0,
                live_time: 0.0,
                light: false,
                clip: [-1e20, -1e20, 1e20, 1e20],
            };
            uniforms.with(&DARK, |values| s.layer.draw(&mut cx, &mut s.line.draw_vars, values, &live));
            cx.end_pass_sized_turtle();
            s.list.end(&mut cx);
            cx.end_pass(&s.pass);
            s.ready_frames = if ready && s.layer.count() > 0 { s.ready_frames + 1 } else { 0 };
            done_in_draw.set(s.ready_frames >= 3);
            frames_in_draw.set((frames_in_draw.get().0 + 1, s.ready_frames));
            if !done_in_draw.get() {
                let _ = cx.new_next_frame();
            }
        }
    }))));
    cx.borrow_mut().init_cx_os();
    let (w, rgba) = capture(&cx, &done, &frames);
    let dpi = dpi_seen.get() as f32;
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as i32;
    // the middle of the wires, inside the window's own size (its buffer may be larger)
    let at = |col: usize, row: usize| -> [i32; 3] {
        let i = (row * w + col) * 4;
        [rgba[i] as i32, rgba[i + 1] as i32, rgba[i + 2] as i32]
    };
    let px = |row: usize| at(WIDTH / 2, row);
    // the device row whose centre lies just inside a wire's centre line, and the first one past its edge
    let core = |y: f32| (y * dpi) as usize;
    let rim = |y: f32| ((y + WIDE / 2.0) * dpi) as usize;
    let accent = [DARK.selection[0], DARK.selection[1], DARK.selection[2]];
    let volts = net_color(net_row(Some(5.0), 0.0, 0.0, &look), [0.5; 3], false, accent);
    let tinted = [0, 1, 2].map(|k| volts[k] * 0.75 + accent[k] * 0.25);
    let broken: Vec<String> = [
        ("the emphasised net's stroke keeps its voltage's colour, tinted a quarter toward the accent", px(core(WIRES[0])), tinted),
        ("just outside the emphasised net, the focus rim", px(rim(WIRES[0])), accent),
        ("the same net without emphasis: its voltage's colour", px(core(WIRES[1])), volts),
        ("and no rim", px(rim(WIRES[1])), [0.0; 3]),
    ]
    .into_iter()
    .filter(|(_, got, want)| (0..3).any(|k| (got[k] - byte(want[k])).abs() > 2))
    .map(|(what, got, want)| format!("{what}: gpu {got:?}, expected {:?}", want.map(byte)))
    .chain(
        [
            ("the pressed item's line at its pressed place", at(16, core(REST - RISE)), true),
            ("and gone from its rest", at(16, core(REST)), false),
            ("another item's line with the same offsets at its rest", at(48, core(REST)), true),
            ("and not at the pressed place", at(48, core(REST - RISE)), false),
        ]
        .into_iter()
        .filter(|(_, got, lit)| got.iter().any(|&v| v > 60) != *lit)
        .map(|(what, got, lit)| format!("{what}: gpu {got:?}, expected {}", if lit { "the line" } else { "background" })),
    )
    .collect();
    assert!(broken.is_empty(), "(dpi {dpi}) {}", broken.join("; "));
}

/// The window's pixels once `done` holds and the frame that drew it finished: its width and RGBA
/// rows, top first (as tests/live.rs reads them).
fn capture(cx: &Rc<RefCell<Cx>>, done: &Cell<bool>, frames: &Cell<(u32, u32)>) -> (usize, Vec<u8>) {
    use makepad_platform::makepad_hal::{self as hal_api, open_flags, Hal, OpenReq};
    let hal = Hal::global();
    let mut req = OpenReq::from_env(0, open_flags::OFFSCREEN_WINDOWS, 1.0);
    req.flags |= open_flags::OFFSCREEN_WINDOWS | open_flags::RETAIN_WINDOWS;
    let info = hal.open(&req);
    assert_ne!(info.backend, hal_api::backend_id::HEADLESS, "pixel comparisons require a GPU");
    cx.borrow_mut().hal_attach_session(info);
    // A hang guard counted in frames, not time: once every pipeline is built the picture completes
    // within a few frames, and a busy box only makes those frames slower.
    const GUARD_FRAMES: u32 = 120;
    let mut built_at: Option<u32> = None;
    let pixels = loop {
        let (drawn, ready) = frames.get();
        if cx.borrow().pipelines_pending() > 0 {
            built_at = None;
        } else {
            let since = *built_at.get_or_insert(drawn);
            assert!(
                drawn - since < GUARD_FRAMES,
                "never painted whole: {drawn} frames drawn, the last {ready} ready, {GUARD_FRAMES} of them with every pipeline built"
            );
        }
        let mut cx = cx.borrow_mut();
        assert!(cx.hal_session_turn(1_000_000, true));
        let submitted = cx.frame_submission_serial();
        if done.get() && submitted != 0 && cx.frame_completion_serial() == submitted && cx.pipelines_pending() == 0 {
            if let Some(pixels) = hal.window_pixels(info.session, None) {
                break pixels;
            }
        }
    };
    hal.close(info.session);
    (pixels.w as usize, pixels.rgba)
}
