//! The live overlay contract (`fab_elec_view::live`): values become texture rows by its rules, the
//! dash phase stays continuous across a current change and a zoom, every view draws the same
//! instant of a shared cursor, and the shader functions draw what their CPU twins compute (on the
//! GPU, through a window's pixels).
#![cfg(not(target_arch = "wasm32"))]

use fab_elec_design::{CursorSource, TimeCursor};
use fab_elec_view::live::*;
use makepad_widgets_core::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

fn luminance(c: Vec4f) -> f32 {
    let lin = |v: f32| v.powf(2.2);
    0.2126 * lin(c.x) + 0.7152 * lin(c.y) + 0.0722 * lin(c.z)
}

#[test]
fn values_become_attributes() {
    let look = LiveLook::dark();

    // ramps: the dark-canvas ramps brighten monotonically, so "more" always reads lighter
    for id in [RampId::VoltDark, RampId::MagDark, RampId::PhosphorDark] {
        let mut last = -1.0;
        for k in 0..=64 {
            let l = luminance(ramp(id, k as f32 / 64.0));
            assert!(l >= last - 1e-3, "ramp {id:?} darkens at x = {}: {l} after {last}", k as f32 / 64.0);
            last = l;
        }
    }
    assert_eq!(volt_color(look.v_range.0, &look), ramp(RampId::VoltDark, 0.0), "volt_color: the low end of the span is the ramp's start");
    assert_eq!(volt_color(99.0, &look), ramp(RampId::VoltDark, 1.0), "volt_color: above the span clamps to the ramp's end");
    assert_eq!(volt_color(f32::NAN, &look), ramp(RampId::VoltDark, 0.0), "volt_color: not a number draws as the low end");

    // flow: off below a thousandth of the reference, direction from the sign, faster and denser with more current
    assert_eq!(flow_attr(look.i_ref * 0.0009, &look), [0.0, 0.0], "flow: below the threshold nothing moves");
    assert_eq!(flow_attr(f32::NAN, &look), [0.0, 0.0], "flow: not a number draws nothing");
    let (a, b) = (flow_attr(0.002, &look), flow_attr(-0.02, &look));
    assert!(a[0] > 0.0 && b[0] < 0.0, "flow: the speed's sign is the current's direction ({a:?}, {b:?})");
    assert!(b[0].abs() > a[0].abs() && b[1] > a[1], "flow: more current moves faster and denser ({a:?}, {b:?})");
    assert_eq!(flow_attr(10.0, &look), flow_attr(look.i_ref, &look), "flow: full at the reference and above");

    let rating = 0.25;
    // glow: off below the threshold, rising, full at the reference
    assert_eq!(glow_attr(rating * 0.049, rating), 0.0, "glow: below the threshold");
    assert!(glow_attr(0.02, rating) < glow_attr(0.1, rating), "glow: rises with dissipation");
    assert!((glow_attr(rating * 0.1, rating) - (1.0 - 1.0 / 1.3)).abs() < 1e-6, "glow: one decade below the rating");
    assert_eq!(glow_attr(rating, rating), 1.0, "glow: full at the reference");
    assert_eq!(glow_color(rating, rating, &look).w, 1.0, "glow_color: alpha is the intensity");
    assert!(glow_attr(0.03, 0.1) > glow_attr(0.03, 0.5), "the same watts tint a smaller package more");
    assert_eq!(glow_peak(1.0, false), 0.35, "a full dissipation cue tints at most 0.35");
    assert!((glow_peak(0.15, true) - 0.8 * 0.15f32.sqrt()).abs() < 1e-6 && glow_peak(1.0, true) == 0.8, "an LED's light reads as seen: 3 mA of 20 mA at {}", glow_peak(0.15, true));
    assert_eq!(glow_alpha(0.35, -1.0, 0.5), 0.35, "soft fill inside the outline");
    assert_eq!(glow_alpha(0.35, 0.25, 0.5), 0.175, "half coverage halfway through the margin");
    assert_eq!(glow_alpha(0.35, 0.5, 0.5), 0.0, "glow ends at the world-space margin");
    assert_eq!(flow_density(flow_attr(0.002, &look)[0]), flow_attr(0.002, &look)[1], "flow: the shader's density from the speed is the CPU's");
    assert_eq!((emission_attr(0.0, 0.02), emission_attr(-0.01, 0.02)), (0.0, 0.0), "emission: an LED without forward current is dark");
    assert_eq!((emission_attr(0.01, 0.02), emission_attr(0.05, 0.02)), (0.5, 1.0), "emission: linear in current, full at the rated current");

    // segment texels: phase at the epoch, signed speed, current level, reserved zero
    let seg_clock = LiveClock::new(100.25);
    let mut flow = FlowPhase::default();
    for (now, i) in [(100.0, 0.002), (101.3, -0.02)] {
        flow.set(now, i, &look);
        let row = seg_row(&flow, &seg_clock, i, &look);
        let phase = flow.phase_at(seg_clock.epoch()).rem_euclid(1.0) as f32;
        assert!((row[0] - phase).abs() < 1e-6, "seg_row: x is the phase at the clock's epoch for current {i}");
        assert_eq!(row[1], flow_attr(i, &look)[0], "seg_row: y keeps the flow speed for current {i}");
        assert_eq!(row[1].signum(), i.signum(), "seg_row: speed has the current's sign");
        assert_eq!(row[2], current_level(i, &look), "seg_row: z is the current level");
        assert_eq!(row[3], 0.0, "seg_row: w is reserved zero");
    }
    let mut quiet_flow = FlowPhase::default();
    let quiet_i = look.i_ref * 0.0009;
    quiet_flow.set(seg_clock.epoch(), quiet_i, &look);
    assert_eq!(seg_row(&quiet_flow, &seg_clock, quiet_i, &look), SEG_OFF, "seg_row: a fresh flow below the threshold is off");

    // part texels: glow, optional LED light, signed watts, clamped emphasis
    let part = part_row(0.1, rating, Some((0.01, 0.02)), 0.4);
    assert_eq!(part[0], glow_attr(0.1, rating), "part_row: x follows the dissipation's glow");
    assert_eq!(part[1], emission_attr(0.01, 0.02), "part_row: y is the LED's light output");
    assert_eq!(part[2], 0.1, "part_row: z keeps raw watts");
    assert_eq!(part[3], 0.4, "part_row: w keeps emphasis within range");
    assert_eq!(part_row(rating * 0.049, rating, None, 0.0)[0], 0.0, "part_row: glow is zero below the threshold");
    assert_eq!(part_row(0.1, rating, None, 0.0)[1], 0.0, "part_row: no LED means zero light output");
    assert_eq!(part_row(-0.1, rating, None, 0.0)[2], -0.1, "part_row: raw watts keep their sign");
    assert_eq!(part_row(f32::NAN, rating, None, 0.0)[2], 0.0, "part_row: watts that are not a number become zero");
    for (emphasis, expected) in [(-1.0, 0.0), (2.0, 1.0), (f32::NAN, 0.0), (f32::INFINITY, 0.0)] {
        assert_eq!(part_row(0.1, rating, None, emphasis)[3], expected, "part_row: emphasis is clamped or zero when not finite");
    }
    assert_eq!(part_row(0.0, rating, None, 0.0), PART_OFF, "part_row: no dissipation, LED or emphasis is off");

    // Pin texels: firmware mode, instantaneous level, PWM duty and independent emphasis.
    for (mode, code) in [
        (PinLiveMode::Input, 0.0), (PinLiveMode::PullUp, 1.0), (PinLiveMode::Output, 2.0),
        (PinLiveMode::Pwm, 3.0), (PinLiveMode::Peripheral, 4.0),
    ] {
        assert_eq!(pin_row(Some(mode), None, 0.0, 0.0)[0], code, "pin_row: {mode:?} has its contract mode code");
        if mode != PinLiveMode::Pwm {
            assert_eq!(pin_row(Some(mode), Some(true), 0.7, 0.0)[2], 0.0, "pin_row: {mode:?} drops non-PWM duty");
        }
    }
    for (level, expected) in [(Some(true), 1.0), (Some(false), 0.0), (None, -1.0)] {
        assert_eq!(pin_row(Some(PinLiveMode::Input), level, 0.0, 0.0)[1], expected, "pin_row: level {level:?} keeps high, low or unknown");
    }
    assert_eq!(pin_row(Some(PinLiveMode::Pwm), Some(true), 0.3, 0.4), [3.0, 1.0, 0.3, 0.4], "pin_row: PWM keeps duty and emphasis in lane order");
    for (value, expected) in [(-1.0, 0.0), (2.0, 1.0), (f32::NAN, 0.0), (f32::INFINITY, 0.0), (f32::NEG_INFINITY, 0.0)] {
        let row = pin_row(Some(PinLiveMode::Pwm), None, value, value);
        assert_eq!(row[2], expected, "pin_row: PWM duty {value} is clamped or zero when non-finite");
        assert_eq!(row[3], expected, "pin_row: emphasis {value} is clamped or zero when non-finite");
        assert_eq!(pin_row(None, Some(true), value, value), [-1.0, -1.0, 0.0, expected], "pin_row: absent mode keeps only sanitized emphasis");
    }
    assert_eq!(pin_row(None, Some(true), 0.7, 0.0), PIN_OFF, "pin_row: absent mode is PIN_OFF regardless of level and duty");
    assert_eq!(pin_row(None, Some(false), 0.7, 0.4), [-1.0, -1.0, 0.0, 0.4], "pin_row: absent mode retains in-range emphasis");

    // net texels: voltage, volts, the current level carried, emphasis; off without a value
    assert_eq!(current_level(look.i_ref * 0.0009, &look), 0.0, "current level: below the threshold");
    assert!(current_level(-0.001, &look) < current_level(0.01, &look), "current level: rises with |current|, either direction");
    assert_eq!(current_level(look.i_ref, &look), 1.0, "current level: full at the reference");
    assert_eq!(net_row(Some(2.5), 0.01, 0.4, &look), [volt_x(2.5, &look), 2.5, current_level(0.01, &look), 0.4], "net_row: lanes in contract order");
    assert_eq!(net_row(None, 0.01, 2.0, &look), [-1.0, 0.0, 0.0, 1.0], "net_row: no value is off, emphasis clamped");
    assert_eq!(net_row(Some(f32::NAN), 0.01, 0.0, &look), NET_OFF, "net_row: a voltage that is not a number is off");
    assert_eq!(net_color(NET_OFF, [0.3, 0.4, 0.5], false, ACCENT), [0.3, 0.4, 0.5], "net_color: an off net keeps its ink");
    let volts = net_row(Some(2.5), 0.0, 0.0, &look);
    let v = ramp(look.volt_ramp, volts[0]);
    let [r, g, b] = [v.x, v.y, v.z];
    assert_eq!(net_color(volts, [0.3, 0.4, 0.5], false, ACCENT), [r, g, b], "net_color: a net with a value is its voltage's colour");
    for (row, what) in [(net_row(Some(2.5), 0.0, 1.0, &look), "with a value"), (net_row(None, 0.0, 1.0, &look), "without a value")] {
        for light in [false, true] {
            assert_eq!(net_color(row, [0.3, 0.4, 0.5], light, ACCENT), ACCENT, "net_color: an emphasised net {what} is drawn in the focus accent (light {light})");
        }
    }
    let half = net_color(net_row(Some(2.5), 0.0, 0.5, &look), [0.3, 0.4, 0.5], false, ACCENT);
    assert!((0..3).all(|k| (half[k] - ([r, g, b][k] * 0.5 + ACCENT[k] * 0.5)).abs() < 1e-6), "net_color: half the emphasis is halfway to the accent: {half:?}");
    let mut rows = LiveRows::new();
    rows.resize(300);
    assert_eq!((rows.nets(), rows.row(299)), (300, NET_OFF), "rows: new nets are off");
    rows.set(257, net_row(Some(1.0), 0.0, 0.0, &look));
    assert_eq!(rows.row(257), net_row(Some(1.0), 0.0, 0.0, &look), "rows: a texel reads back");
    rows.resize(100);
    assert_eq!(rows.row(257), NET_OFF, "rows: a dropped net is off");
    rows.set(3, net_row(Some(1.0), 0.0, 0.0, &look));
    rows.clear();
    assert_eq!(rows.row(3), NET_OFF, "rows: clear turns every net off");
    let mut parts = LiveRows::with_off(PART_OFF);
    parts.resize(2);
    parts.set(1, part_row(0.1, rating, None, 0.0));
    parts.resize(1);
    assert_eq!(parts.row(7), PART_OFF, "rows: a part row out of range is the part's off texel, not a net's");
    parts.resize(2);
    assert_eq!(parts.row(1), PART_OFF, "rows: a dropped part row comes back as the part's off texel");

    // the phase is continuous across a change of current, and the shader's segment row reads the same phase
    let mut clock = LiveClock::new(100.0);
    let mut f = FlowPhase::default();
    f.set(100.0, 0.004, &look);
    let before = f.phase_at(101.3);
    f.set(101.3, -0.03, &look);
    let after = f.phase_at(101.3);
    assert!((before - after).rem_euclid(1.0).min((after - before).rem_euclid(1.0)) < 1e-9, "flow phase jumps at a change of current: {before} -> {after}");
    let (t, moved) = clock.time(102.0);
    assert!(!moved, "the clock moves its epoch only after {LIVE_REBASE_S} s");
    let [phase, speed] = f.attr(&clock);
    let shader_phase = (phase + speed * t) as f64;
    assert!(
        (shader_phase - f.phase_at(102.0)).rem_euclid(1.0) < 1e-4 || (f.phase_at(102.0) - shader_phase).rem_euclid(1.0) < 1e-4,
        "the segment row's phase differs from the flow's"
    );
    let (_, moved) = clock.time(100.0 + LIVE_REBASE_S + 1.0);
    assert!(moved, "the clock rebases past {LIVE_REBASE_S} s");

    // zooming never makes the dashes jump: coverage at a fixed point is continuous in the zoom
    let mut last = flow_alpha(2.0, 37.0, 0.25, 0.3, 1.5);
    let mut worst = 0.0f32;
    for k in 1..=4000 {
        let px = 0.25 * (64.0f32).powf(k as f32 / 4000.0);
        let a = flow_alpha(2.0, 37.0, px, 0.3, 1.5);
        worst = worst.max((a - last).abs());
        last = a;
    }
    assert!(worst < 0.05, "flow: coverage jumps by {worst} while zooming (an octave switch that is not cross-faded)");

    // two views that heard the same playing cursor draw the same instant; a scrub pauses there for both
    let playing = TimeCursor { t: 0.004, playing: true, rate: 0.5, source: CursorSource::Sim, fw: None };
    let (mut v1, mut v2) = (TimeCursorView::heard(playing, 10.0), TimeCursorView::heard(playing, 10.0));
    assert_eq!(v1.at(10.2), v2.at(10.2), "two views of one cursor disagree");
    assert!((v1.at(10.2) - (0.004 + 0.1)).abs() < 1e-12, "a playing cursor advances by its rate");
    let sent = v1.scrub_to(0.0021);
    v2.hear(sent, 10.3);
    assert_eq!((v1.at(11.0), v2.at(11.0)), (0.0021, 0.0021), "a scrub shows the same instant in every view");
    v1.release(11.0);
    assert_eq!(v1.at(12.0), 0.0021, "a released scrub stays where it was put");
}

/// The window (device pixels): nine rows of two pixels, one per shader function.
const WIDTH: usize = 128;
const HEIGHT: usize = 18;
const ROWS: usize = 9;

/// The net texel the GPU test sets for net `i`: voltages across the ramp, every fifth net
/// emphasised, every eleventh without a value.
fn test_row(i: usize) -> [f32; 4] {
    let v = (i % 11 != 0).then(|| (i % 37) as f32 / 36.0 * 5.0);
    net_row(v, 0.0, if i % 5 == 0 { 1.0 } else if i % 7 == 0 { 0.5 } else { 0.0 }, &LiveLook::dark())
}

/// The focus accent the net colour tests draw emphasis in (the GPU test's literal matches it).
const ACCENT: [f32; 3] = [0.30, 0.55, 1.0];

/// The net the GPU test's row of net colours reads at pixel column `col`: 44 to 298, so the reads
/// cross from the texture's first row into its second.
fn test_net(col: usize) -> usize {
    col * 2 + 44
}

#[test]
fn shader_fns_match_their_cpu_twins() {
    std::env::set_var("MAKEPAD_HAL_BACKEND", "offscreen");
    struct State {
        _window: WindowHandle,
        pass: DrawPass,
        list: DrawList2d,
        quad: DrawQuad,
        rows: LiveRows,
        /// The nets of the texture's second row have been set (a partial upload after the first frames).
        changed: bool,
        frames_after: u32,
        ready_frames: u32,
    }
    let mut state: Option<State> = None;
    let done = Rc::new(Cell::new(false));
    let done_in_draw = done.clone();
    let frames = Rc::new(Cell::new((0u32, 0u32)));
    let frames_in_draw = frames.clone();
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
        if matches!(event, Event::Startup) {
            let quad = cx.with_vm(|vm| {
                makepad_platform::script::script_mod(vm);
                makepad_draw::script_mod(vm);
                fab_elec_view::live::script_mod(vm);
                let v = vm.eval(script! {
                    use mod.pod.*
                    use mod.math.*
                    use mod.shader.*
                    use mod.draw.*
                    use mod.elec_live.*
                    DrawQuad{
                        live_rows: texture_2d(float)
                        pixel: fn() {
                            let x = self.pos.x
                            let row = floor(self.pos.y * 9.0)
                            var c = Live.volt(x, 0.0)
                            if row > 0.5 {
                                c = Live.volt(x, 1.0)
                            }
                            if row > 1.5 {
                                c = Live.mag(x, 0.0)
                            }
                            if row > 2.5 {
                                c = Live.mag(x, 1.0)
                            }
                            if row > 3.5 {
                                c = Live.grade(x, 0.0)
                            }
                            if row > 4.5 {
                                c = Live.grade(x, 1.0)
                            }
                            if row > 5.5 {
                                let a = Live.flow(1.25, x * 40.0, 1.0, 0.3, 2.0)
                                c = vec3(a, a, a)
                            }
                            if row > 6.5 {
                                let g = Live.glow_alpha(0.7, x - 0.25, 0.5)
                                c = Live.lit(vec3(0.2, 0.4, 0.6), g, 0.0)
                            }
                            if row > 7.5 {
                                let n = floor(x * 128.0) * 2.0 + 44.0
                                let r = self.live_rows.sample_nearest(Live.net_uv(n, self.live_rows.size()))
                                c = Live.net_color(r, vec3(0.3, 0.3, 0.3), 0.0, vec3(0.30, 0.55, 1.0))
                            }
                            return vec4(c.x, c.y, c.z, 1.0)
                        }
                    }
                });
                DrawQuad::script_from_value(vm, v)
            });
            let mut window = WindowHandle::new(cx);
            window.configure_window(cx, dvec2(WIDTH as f64, HEIGHT as f64), dvec2(0.0, 0.0), false, "Live shader fns".into());
            let pass = DrawPass::new(cx);
            window.set_pass(cx, &pass);
            pass.set_window_clear_color(cx, vec4(0.0, 0.0, 0.0, 1.0));
            let list = DrawList2d::new(cx);
            // the first texture row now; the second (nets 256 on) first wrong, then set after a few frames
            let mut rows = LiveRows::new();
            rows.resize(300);
            for i in 0..300 {
                rows.set(i, if i < 256 { test_row(i) } else { net_row(Some(5.0), 0.0, 1.0, &LiveLook::dark()) });
            }
            state = Some(State { _window: window, pass, list, quad, rows, changed: false, frames_after: 0, ready_frames: 0 });
            cx.redraw_all();
        }
        if matches!(event, Event::NextFrame(_)) {
            cx.redraw_all();
        }
        if let Event::Draw(event) = event {
            let s = state.as_mut().unwrap();
            let ready = s.quad.draw_vars.draw_shader_id.is_some_and(|id| cx.draw_shader_ready(id, true));
            if !s.changed && s.ready_frames >= 2 {
                for i in 256..300 {
                    s.rows.set(i, test_row(i));
                }
                s.changed = true;
            }
            let texture = s.rows.texture(cx);
            s.quad.draw_vars.set_texture(0, texture);
            let mut draw = CxDraw::new(cx, event);
            let mut cx = Cx2d::new(&mut draw);
            cx.begin_pass(&s.pass, None);
            s.list.begin_always(&mut cx);
            let dpi = cx.current_dpi_factor();
            let size = dvec2(WIDTH as f64 / dpi, HEIGHT as f64 / dpi);
            cx.begin_root_turtle(size, Layout::flow_overlay());
            s.quad.draw_abs(&mut cx, Rect { pos: dvec2(0.0, 0.0), size });
            cx.end_pass_sized_turtle();
            s.list.end(&mut cx);
            cx.end_pass(&s.pass);
            s.ready_frames = if ready { s.ready_frames + 1 } else { 0 };
            s.frames_after += s.changed as u32;
            done_in_draw.set(s.ready_frames >= 3 && s.frames_after >= 3);
            frames_in_draw.set((frames_in_draw.get().0 + 1, s.ready_frames));
            if !done_in_draw.get() {
                let _ = cx.new_next_frame();
            }
        }
    }))));
    cx.borrow_mut().init_cx_os();
    let (w, rgba) = capture(&cx, &done, &frames);
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as i32;
    let mut worst = (0i32, String::new());
    for row in 0..ROWS {
        // every column of the net row (each reads its own net), every fifth of the others
        for col in (0..WIDTH).step_by(if row == 8 { 1 } else { 5 }) {
            let x = (col as f32 + 0.5) / WIDTH as f32;
            let rgb = |v: Vec4f| [v.x, v.y, v.z];
            let want: [f32; 3] = rgb(match row {
                0 => ramp(RampId::VoltDark, x),
                1 => ramp(RampId::VoltLight, x),
                2 => ramp(RampId::MagDark, x),
                3 => ramp(RampId::MagLight, x),
                4 => ramp(RampId::PhosphorDark, x),
                5 => ramp(RampId::PhosphorLight, x),
                6 => {
                    let a = flow_alpha(1.25, x * 40.0, 1.0, 0.3, 2.0);
                    vec4(a, a, a, 1.0)
                }
                8 => {
                    let c = net_color(test_row(test_net(col)), [0.3, 0.3, 0.3], false, ACCENT);
                    vec4(c[0], c[1], c[2], 1.0)
                }
                _ => {
                    let l = lit([0.2, 0.4, 0.6], glow_alpha(0.7, x - 0.25, 0.5), false);
                    vec4(l[0], l[1], l[2], 1.0)
                }
            });
            let y = row * HEIGHT / ROWS + HEIGHT / ROWS / 2;
            let p = &rgba[(y * w + col) * 4..(y * w + col) * 4 + 3];
            let d = (0..3).map(|k| (p[k] as i32 - byte(want[k])).abs()).max().unwrap();
            if d > worst.0 {
                worst = (d, format!("row {row} col {col}: gpu {p:?}, cpu {:?}", want.map(byte)));
            }
        }
    }
    println!("live shader fns: worst difference {} levels ({})", worst.0, worst.1);
    assert!(worst.0 <= 2, "a shader fn differs from its CPU twin by {} levels: {}", worst.0, worst.1);
    // an emphasised net's pixel on the GPU is the focus accent itself, in both texture rows
    let y = 8 * HEIGHT / ROWS + HEIGHT / ROWS / 2;
    let accented: Vec<usize> = (0..WIDTH).filter(|&col| test_row(test_net(col))[3] == 1.0).collect();
    assert!(accented.iter().any(|&c| test_net(c) < 256) && accented.iter().any(|&c| test_net(c) >= 256), "emphasised nets in both texture rows: {accented:?}");
    for col in accented {
        let p = &rgba[(y * w + col) * 4..(y * w + col) * 4 + 3];
        let d = (0..3).map(|k| (p[k] as i32 - byte(ACCENT[k])).abs()).max().unwrap();
        assert!(d <= 2, "the emphasised net at column {col} draws {p:?} on the GPU, not the accent {:?}", ACCENT.map(byte));
    }
}

/// The window's pixels once `done` holds and the frame that drew it finished: its width and RGBA
/// rows, top first (as draw/tests/linear_light.rs reads them).
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
