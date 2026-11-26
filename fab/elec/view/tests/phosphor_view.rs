//! The R32F picture and its grading agree with a CPU recurrence on a real GPU.
#![cfg(not(target_arch = "wasm32"))]

use fab_elec_view::{
    instruments::{
        phosphor::{grade_x, Dirty, Hits, Persistence},
        phosphor_view::{graded, spot_ring, PhosphorLook, PhosphorView, Pushed},
    },
    live::{ramp, RampId},
};
use makepad_widgets_core::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const WIDTH: usize = 64;
const HEIGHT: usize = 16;
const WINDOW_WIDTH: usize = WIDTH * 3;
const DT: f64 = 1.0 / 60.0;
const WEDGE_FRAMES: u32 = 480 + 240;
const GHOST_FRAMES: u32 = 6;
/// Twelve rounds of a window swept in four quarters and a frame of whole waveforms.
const SWEEP_FRAMES: u32 = 60;
const GHOST_ROW: usize = 3;
const LIVE_ROW: usize = 11;
const SHARED_ROW: usize = 7;

#[derive(Clone, Copy, PartialEq)]
enum GhostMode {
    Kept,
    Cleared,
    Never,
}

#[derive(Clone, Copy)]
enum Case {
    Grade,
    Waterfall,
    Twin { enabled: bool },
    Wedge,
    Sweep,
    Ghost { light: bool, mode: GhostMode },
}

impl Case {
    fn frames(self) -> u32 {
        match self {
            Self::Grade => 1,
            Self::Waterfall => 6,
            Self::Twin { .. } => 4,
            Self::Wedge => WEDGE_FRAMES,
            Self::Sweep => SWEEP_FRAMES,
            Self::Ghost { mode: GhostMode::Never, .. } => GHOST_FRAMES,
            Self::Ghost { .. } => 2 * GHOST_FRAMES,
        }
    }

    fn look(self) -> PhosphorLook {
        PhosphorLook {
            tau: match self {
                Self::Grade | Self::Waterfall | Self::Twin { .. } => DT / std::f64::consts::LN_2,
                Self::Wedge => 10.0,
                Self::Sweep => 0.5,
                Self::Ghost { .. } => f64::INFINITY,
            },
            gain: 1000.0,
            bloom: 0.0,
            light: matches!(self, Self::Ghost { light: true, .. }),
        }
    }

    fn screen(self) -> [f32; 3] {
        if self.look().light {
            [1.0; 3]
        } else {
            [0.0; 3]
        }
    }

    fn fill(self, hits: &mut Hits, frame: u32) {
        hits.reset(WIDTH as u32, HEIGHT as u32);
        hits.waveforms = 100;
        match self {
            Self::Waterfall => {
                // Two old rows, an off frame, an idle frame to clear on re-enable, then two new rows.
                if frame == 3 {
                    hits.waveforms = 0;
                } else {
                    let col = if frame < 4 { 2 } else { frame as usize };
                    hits.data[col * HEIGHT] = 100.0;
                    hits.mark(col as u32, col as u32 + 1, 0, 1);
                }
            }
            Self::Grade | Self::Twin { .. } => {
                hits.data[10 * HEIGHT + 5] = 100.0;
                for row in 0..HEIGHT {
                    hits.data[20 * HEIGHT + row] = 10.0;
                }
                for col in 30..60 {
                    hits.data[col * HEIGHT + 12] = 1.0;
                }
                hits.mark(10, 60, 0, HEIGHT as u32);
            }
            Self::Wedge => {
                let bright = if frame < 480 { 1.05 } else { 1.0 };
                // Deliberately leave nonzero stale texels outside the uploaded rectangle.
                for col in 0..WIDTH {
                    let fraction = 10.0f64.powf(-4.0 * col as f64 / (WIDTH - 1) as f64);
                    hits.data[col * HEIGHT..(col + 1) * HEIGHT].fill((100.0 * bright * fraction) as f32);
                }
                let (c0, c1) = if frame >= 480 {
                    (0, WIDTH)
                } else if frame % 2 == 0 {
                    (0, WIDTH / 2)
                } else {
                    (WIDTH / 2, WIDTH)
                };
                hits.mark(c0 as u32, c1 as u32, 0, HEIGHT as u32);
                if frame < 480 && (frame + 1) % 8 == 0 {
                    hits.waveforms = 0;
                }
            }
            Self::Sweep => {
                // Each round: a window arrives a quarter of the columns at a time (completing in
                // the fourth), then two whole waveforms reach every column. Counts differ per part
                // and round, so the columns' pictures and weights drift apart.
                let (round, part) = (frame / 5, (frame % 5) as usize);
                let quarter = WIDTH / 4;
                let (c0, c1) = if part == 4 { (0, WIDTH) } else { (part * quarter, (part + 1) * quarter) };
                for col in c0..c1 {
                    let level = 10.0 + 7.0 * part as f32 + (round % 3) as f32 + col as f32 * 0.5;
                    hits.data[col * HEIGHT..(col + 1) * HEIGHT].fill(level);
                }
                hits.mark(c0 as u32, c1 as u32, 0, HEIGHT as u32);
                if part == 4 {
                    hits.waveforms = 2;
                } else {
                    hits.partial.resize(WIDTH, 0.0);
                    hits.partial[c0..c1].fill(1.0);
                    hits.waveforms = u64::from(part == 3);
                    hits.partial_done = hits.waveforms;
                }
            }
            Self::Ghost { mode, .. } => {
                let old = mode != GhostMode::Never && frame < GHOST_FRAMES;
                let row = if old { GHOST_ROW } else { LIVE_ROW };
                for col in 0..WIDTH {
                    let fraction = [1.0, 0.1, 0.01, 0.0005][col / (WIDTH / 4)];
                    hits.data[col * HEIGHT + row] = 100.0 * fraction;
                    hits.data[col * HEIGHT + SHARED_ROW] = if old { 10.0 } else { 100.0 };
                }
                hits.mark(0, WIDTH as u32, row.min(SHARED_ROW) as u32, row.max(SHARED_ROW) as u32 + 1);
            }
        }
    }
}

struct State {
    _window: WindowHandle,
    pass: DrawPass,
    list: DrawList2d,
    view: PhosphorView,
    read: DrawQuad,
    reuse: Option<Hits>,
    previous: (Dirty, u64),
    persistence: Persistence,
    warmup: u32,
    frames: u32,
    frozen: bool,
}

impl State {
    fn buffer(&mut self) -> Hits {
        let mut hits = self.reuse.take().unwrap_or_else(|| Hits::new(WIDTH as u32, HEIGHT as u32));
        hits.reset(WIDTH as u32, HEIGHT as u32);
        hits
    }

    fn push(&mut self, cx: &mut Cx, hits: Hits) {
        let dirty = hits.dirty;
        let expected_bytes = if dirty.is_empty() {
            0
        } else {
            (dirty.c1 - dirty.c0) as usize * (dirty.r1 - dirty.r0) as usize * 4
        };
        let previous = self.previous;
        self.previous = (dirty, hits.waveforms);
        match self.view.push_frame(cx, hits) {
            Pushed::Taken(old) => {
                if let Some(old) = &old {
                    assert_eq!(
                        (old.dirty, old.waveforms),
                        previous,
                        "returned buffers must retain their own dirty rectangle and waveform count"
                    );
                    assert_eq!(old.data.len(), WIDTH * HEIGHT, "returned buffers must retain their allocation");
                }
                self.reuse = old;
            }
            other => panic!("an available plot must take a matching frame: {other:?}"),
        }
        assert_eq!(self.view.uploaded_bytes(), expected_bytes, "only the dirty rectangle is submitted for upload");
    }
}

#[test]
fn hits_accumulate_and_grade_on_the_gpu() {
    std::env::set_var("MAKEPAD_HAL_BACKEND", "offscreen");
    let (width, rgba, _) = run(Case::Grade);
    let look = Case::Grade.look();
    let mut hits = Hits::new(WIDTH as u32, HEIGHT as u32);
    Case::Grade.fill(&mut hits, 0);
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as i32;
    let mut worst = (0, String::new());
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            // Constant frames give c/n immediately, including the first frame after clear.
            let (c, r) = (col as u32, row as u32);
            let want = graded(hits.at(c, r) / 100.0, spot_ring(&hits, c, r) / 100.0, &look, [0.0; 3]).map(byte);
            let pixel = &rgba[(row * width + col) * 4..(row * width + col + 1) * 4];
            let difference = (0..3).map(|i| (pixel[i] as i32 - want[i]).abs()).max().unwrap();
            if difference > worst.0 {
                worst = (difference, format!("pixel ({col}, {row}): gpu {pixel:?}, cpu {want:?}"));
            }
            assert_eq!(pixel[3], 255, "graded pixel ({col}, {row}) must be opaque");
        }
    }
    println!("phosphor grading: worst difference {} levels ({})", worst.0, worst.1);
    assert!(worst.0 <= 3, "phosphor grading differs by {} levels: {}", worst.0, worst.1);
    let lit = |col: usize, row: usize| rgba[(row * width + col) * 4..(row * width + col) * 4 + 3].iter().any(|&v| v > 0);
    assert!(
        lit(9, 4) && lit(11, 6) && !lit(12, 5) && !lit(10, 7),
        "a lone hit's beam spot lights its eight neighbours and nothing further"
    );

    // An fp16 picture stalls about 2.4% away here; reading float bits avoids an 8-bit grading tolerance.
    let worst = float_error(Case::Wedge, "wedge");
    assert!(worst.0 <= 0.01, "R32F persistence differs by {:.6}%: {}", 100.0 * worst.0, worst.1);
    // A window swept in parts: every column blends with its own weight, on the GPU as in the model.
    let worst = float_error(Case::Sweep, "sweep");
    assert!(worst.0 <= 0.01, "per-column persistence differs by {:.6}%: {}", 100.0 * worst.0, worst.1);

    for light in [false, true] {
        check_ghost(light);
    }
    check_live_twin();
    let (width, waterfall, _) = run(Case::Waterfall);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let pixel = (y * width + WIDTH + x) * 4;
            let lit = waterfall[pixel..pixel + 3].iter().any(|&v| v != 0);
            // The ring is half the plot height and is graded newest first.
            let expected = (y / 2 == 0 && x == 5) || (y / 2 == 1 && x == 4);
            assert_eq!(lit, expected, "only the two fresh waterfall rows may show at ({x}, {y})");
        }
    }
}

/// The worst relative error of `case`'s float picture against the CPU model, every texel lit.
fn float_error(case: Case, name: &str) -> (f64, String) {
    let (width, rgba, expected) = run(case);
    let mut worst = (0.0f64, String::new());
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let low = (row * width + WIDTH + col) * 4;
            let high = (row * width + 2 * WIDTH + col) * 4;
            let value = f32::from_le_bytes([rgba[low], rgba[low + 1], rgba[low + 2], rgba[high]]) as f64;
            let reference = expected[col * HEIGHT + row];
            assert!(
                value.is_finite() && reference > 0.0,
                "{name} texel ({col}, {row}) must be finite and positive: {value}, {reference}"
            );
            let error = (value / reference - 1.0).abs();
            if error > worst.0 {
                worst = (error, format!("texel ({col}, {row}): gpu {value}, cpu {reference}"));
            }
        }
    }
    println!("phosphor {name}: worst relative error {:.6}% ({})", 100.0 * worst.0, worst.1);
    worst
}

fn check_live_twin() {
    let (width, twin, _) = run(Case::Twin { enabled: true });
    let (baseline_width, single, _) = run(Case::Twin { enabled: false });
    assert_eq!(width, baseline_width, "twin and single captures use the same pixel layout");
    for row in 0..HEIGHT {
        let start = (row * width + WIDTH) * 4;
        let end = start + WIDTH * 2 * 4;
        assert_eq!(&twin[start..end], &single[start..end], "live twin uploads and reversible swaps leave main float pixels unchanged on row {row}");
    }
    let at = (6 * width + 3) * 4;
    assert_ne!(&twin[at..at + 3], &single[at..at + 3], "the last secondary frame appears live in its new row");
}

fn upload_twin(cx: &mut Cx, view: &mut PhosphorView, frame: u32) {
    let mut hits = Hits::new(WIDTH as u32, HEIGHT as u32);
    let row = 3 + frame;
    hits.waveforms = 100;
    for col in 2..6 { hits.data[col * HEIGHT + row as usize] = 100.0; }
    hits.mark(2, 6, row, row + 1);
    let main = view.picture().map(Texture::texture_id);
    let bytes = view.uploaded_bytes();
    let hits = match view.set_ghost_hits(cx, hits) {
        Pushed::Taken(Some(hits)) => hits,
        other => panic!("each drawn secondary frame accepts the next live upload and returns its buffer: {other:?}"),
    };
    assert_eq!(view.picture().map(Texture::texture_id), main, "secondary upload leaves the main picture handle unchanged");
    assert_eq!(view.uploaded_bytes(), bytes, "secondary upload leaves the primary upload metadata unchanged");
    assert_eq!(view.ghost_uploaded_bytes(), 4 * 4, "secondary upload covers only its transposed dirty rectangle");
    let texture = view.ghost_hits_texture().expect("live secondary has an upload texture");
    match texture.get_format(cx) {
        TextureFormat::VecRf32 { width, height, data: Some(data), .. } => {
            assert_eq!((*width, *height), (HEIGHT, WIDTH), "secondary CPU upload rows use the primary's transposed layout");
            for col in 2..6 {
                assert_eq!(data[col * HEIGHT + row as usize], 100.0, "secondary CPU row contains frame {frame}'s hits in column {col}");
            }
        }
        _ => panic!("secondary upload retains CPU R32F rows"),
    }
    assert!(matches!(view.set_ghost_hits(cx, hits), Pushed::Busy(_)), "a pending secondary frame preserves backpressure");
}

fn check_ghost(light: bool) {
    let case = Case::Ghost { light, mode: GhostMode::Kept };
    let look = case.look();
    let screen = case.screen();
    let (width, kept, _) = run(case);
    let (_, cleared, _) = run(Case::Ghost {
        light,
        mode: GhostMode::Cleared,
    });
    let (_, never, _) = run(Case::Ghost { light, mode: GhostMode::Never });
    assert_eq!(
        cleared, never,
        "clearing the ghost must match never freezing, including live float bits, light={light}"
    );
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as i32;
    let screen_bytes = screen.map(byte);
    let mut a = Hits::new(WIDTH as u32, HEIGHT as u32);
    let mut b = Hits::new(WIDTH as u32, HEIGHT as u32);
    case.fill(&mut a, 0);
    case.fill(&mut b, GHOST_FRAMES);
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let at = (row * width + col) * 4;
            let pixel = &kept[at..at + 4];
            let p = a.at(col as u32, row as u32) / 100.0;
            let x = grade_x(p * look.gain, look.gain);
            let grade = ramp(if light { RampId::PhosphorLight } else { RampId::PhosphorDark }, x);
            let luma = 0.2126 * grade.x + 0.7152 * grade.y + 0.0722 * grade.z;
            let alpha = (8.0 * x).clamp(0.0, 1.0) * 0.45;
            let color = [grade.x, grade.y, grade.z];
            let ghost = std::array::from_fn(|i| screen[i] + (color[i] * 0.35 + luma * 0.65 - screen[i]) * alpha);
            let (c, r) = (col as u32, row as u32);
            let want = graded(b.at(c, r) / 100.0, spot_ring(&b, c, r) / 100.0, &look, ghost).map(byte);
            let property = match row {
                GHOST_ROW => "A's rows show the muted ghost grade",
                LIVE_ROW => "B's rows show the live grade",
                SHARED_ROW => "the live grade wins where both pictures are lit",
                _ if !light && (row.abs_diff(LIVE_ROW) == 1 || row.abs_diff(SHARED_ROW) == 1) => {
                    "a dark screen's beam spot lights the rows beside B's"
                }
                _ => "rows lit by neither picture keep the screen colour",
            };
            let difference = (0..3).map(|i| (pixel[i] as i32 - want[i]).abs()).max().unwrap();
            assert!(difference <= 3, "{property}, light={light}, pixel ({col}, {row}): gpu {pixel:?}, cpu {want:?}");
            assert_eq!(pixel[3], 255, "ghost compositing keeps the screen opaque, light={light}, pixel ({col}, {row})");
            if row == GHOST_ROW {
                let live_at = (LIVE_ROW * width + col) * 4;
                let contrast = |rgb: &[u8]| (0..3).map(|i| (rgb[i] as i32 - screen_bytes[i]).abs()).sum::<i32>();
                let ghost_contrast = contrast(pixel);
                assert!(ghost_contrast > 0, "A's ghost remains visible, light={light}, column {col}");
                assert!(
                    ghost_contrast < contrast(&kept[live_at..live_at + 4]),
                    "A's ghost is dimmer than an equally intense live trace, light={light}, column {col}"
                );
                assert_eq!(
                    [cleared[at] as i32, cleared[at + 1] as i32, cleared[at + 2] as i32],
                    screen_bytes,
                    "clearing the ghost returns A's rows to the screen colour, light={light}, column {col}"
                );
            }
        }
    }
}

fn run(case: Case) -> (usize, Vec<u8>, Vec<f64>) {
    let mut state: Option<State> = None;
    let done = Rc::new(Cell::new(false));
    let done_in_draw = done.clone();
    let frames = Rc::new(Cell::new((0u32, 0u32)));
    let frames_in_draw = frames.clone();
    let reference = Rc::new(RefCell::new(vec![0.0; WIDTH * HEIGHT]));
    let reference_in_draw = reference.clone();
    let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
        if done_in_draw.get() {
            return;
        }
        if matches!(event, Event::Startup) {
            let (mut view, read) = cx.with_vm(|vm| {
                makepad_platform::script::script_mod(vm);
                makepad_draw::script_mod(vm);
                makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
                fab_elec_view::live::script_mod(vm);
                fab_elec_view::instruments::phosphor_view::script_mod(vm);
                let value = vm.eval(script! {
                    use mod.prelude.widgets_internal.*
                    use mod.widgets.*
                    PhosphorView { width: Fill height: Fill screen: #000 }
                });
                let view = PhosphorView::script_from_value(vm, value);
                let value = vm.eval(script! {
                    use mod.pod.*
                    use mod.math.*
                    use mod.shader.*
                    use mod.draw.*
                    DrawQuad {
                        color_format: @Bgra8NoBlend
                        image: texture_2d(float)
                        pixel: fn() {
                            let p = floor(self.pos * vec2(128.0, 16.0))
                            let x = p.x - 64.0 * step(64.0, p.x)
                            let b = asuint(self.image.sample_nearest((vec2(x, p.y) + vec2(0.5, 0.5)) / vec2(64.0, 16.0)).x)
                            if p.x >= 64.0 {
                                return vec4(f32(b >> 24u) / 255.0, 0.0, 0.0, 1.0)
                            }
                            return vec4(f32(b & 255u) / 255.0, f32((b >> 8u) & 255u) / 255.0, f32((b >> 16u) & 255u) / 255.0, 1.0)
                        }
                    }
                });
                (view, DrawQuad::script_from_value(vm, value))
            });
            let screen = case.screen();
            view.screen = vec4(screen[0], screen[1], screen[2], 1.0);
            view.set_look(cx, case.look());
            view.set_waterfall(matches!(case, Case::Waterfall));
            view.set_frame_interval(Some(DT));
            let mut window = WindowHandle::new(cx);
            window.configure_window(cx, dvec2(WINDOW_WIDTH as f64, HEIGHT as f64), dvec2(0.0, 0.0), false, "Phosphor pixels".into());
            let pass = DrawPass::new(cx);
            window.set_pass(cx, &pass);
            pass.set_window_clear_color(cx, vec4(0.0, 0.0, 0.0, 1.0));
            state = Some(State {
                _window: window,
                pass,
                list: DrawList2d::new(cx),
                view,
                read,
                reuse: None,
                previous: (Dirty::default(), 0),
                persistence: Persistence::default(),
                warmup: 0,
                frames: 0,
                frozen: false,
            });
            cx.redraw_all();
        }
        if let Some(s) = &mut state {
            s.view.handle_event(cx, event, &mut Scope::empty());
        }
        if matches!(event, Event::NextFrame(_)) {
            cx.redraw_all();
        }
        if let Event::Draw(event) = event {
            let s = state.as_mut().unwrap();
            let pending = cx.pipelines_pending();
            // The first-frame gate can request another Draw before it submits the first one.
            let waiting_for_start = s.warmup == 0 && (cx.frame_submission_serial() == 0 || pending != 0);
            if !waiting_for_start {
                assert_eq!(pending, 0, "no pipeline compiles during the measured frames, so no frame is held");
            }
            let mut blended = None;
            let mut finished = false;
            let mut freezing = false;
            if waiting_for_start {
                s.view.clear(cx);
            } else if s.warmup < 5 {
                match s.warmup {
                    0 => {
                        let wrong = Hits::new(1, 1);
                        match s.view.push_frame(cx, wrong.clone()) {
                            Pushed::WrongSize(returned) => assert_eq!(returned, wrong, "wrong-size submission returns the buffer untouched"),
                            other => panic!("wrong-size submission must be rejected: {other:?}"),
                        }
                        if matches!(case, Case::Twin { enabled: true } | Case::Ghost { .. }) {
                            upload_twin(cx, &mut s.view, 0);
                        }
                        s.view.clear(cx);
                    }
                    1 => {
                        let mut hits = s.buffer();
                        hits.data.fill(f32::NAN);
                        hits.waveforms = 1;
                        hits.mark(0, WIDTH as u32, 0, HEIGHT as u32);
                        s.push(cx, hits);
                    }
                    2 => s.view.clear(cx),
                    4 => {
                        if matches!(case, Case::Ghost { .. }) {
                            assert!(s.view.has_ghost(), "a live twin has rendered data before it is removed");
                            s.view.clear_twin(cx);
                            assert!(s.view.ghost_hits_texture().is_none(), "clearing the twin releases its upload rows before the freeze scenario");
                            assert!(!s.view.has_ghost(), "clearing a twin without a frozen image hides the ghost");
                        }
                        s.view.clear(cx);
                    }
                    3 => {
                        if matches!(case, Case::Ghost { .. }) { upload_twin(cx, &mut s.view, 0); }
                        let mut hits = s.buffer();
                        hits.waveforms = 10;
                        s.push(cx, hits);
                        blended = Some(true);
                    }
                    _ => unreachable!(),
                }
                s.warmup += 1;
            } else if matches!(
                case,
                Case::Ghost {
                    mode: GhostMode::Kept | GhostMode::Cleared,
                    ..
                }
            ) && s.frames == GHOST_FRAMES
                && !s.frozen
            {
                assert!(!s.view.has_ghost(), "a plot has no visible ghost before its first freeze");
                assert!(s.view.ghost_picture().is_none(), "the ghost texture is allocated only on demand");
                let mut queued = s.buffer();
                case.fill(&mut queued, 0);
                s.push(cx, queued);
                s.view.freeze_ghost(cx);
                s.view.freeze_ghost(cx);
                assert!(!s.view.has_ghost(), "a pending freeze becomes visible only when its copy is drawn");
                s.persistence.clear();
                reference_in_draw.borrow_mut().fill(0.0);
                s.frozen = true;
                freezing = true;
            } else if s.frames < case.frames() {
                if matches!(case, Case::Waterfall) { s.view.set_waterfall(s.frames != 2); }
                if matches!(case, Case::Twin { enabled: true }) { upload_twin(cx, &mut s.view, s.frames); }
                let mut hits = s.buffer();
                case.fill(&mut hits, s.frames);
                // The CPU model: each column blends with its own keep and scale.
                let mut blends = Vec::new();
                let swept = s.persistence.frame(WIDTH, hits.whole(), &hits.partial, DT, case.look().tau, &mut blends);
                blended = Some(swept);
                if swept {
                    let d = hits.dirty;
                    let mut reference = reference_in_draw.borrow_mut();
                    for col in 0..WIDTH {
                        let blend = blends[col];
                        for row in 0..HEIGHT {
                            let index = col * HEIGHT + row;
                            let inside = col >= d.c0 as usize && col < d.c1 as usize && row >= d.r0 as usize && row < d.r1 as usize;
                            let count = if inside { hits.data[index] as f64 } else { 0.0 };
                            reference[index] = blend.keep as f64 * reference[index] + blend.scale as f64 * count;
                        }
                    }
                }
                s.push(cx, hits);
                if s.frames == 0 {
                    let mut busy = Hits::new(WIDTH as u32, HEIGHT as u32);
                    busy.data[0] = 7.0;
                    busy.waveforms = 9;
                    busy.mark(0, 1, 0, 1);
                    match s.view.push_frame(cx, busy.clone()) {
                        Pushed::Busy(returned) => assert_eq!(returned, busy, "busy submission returns the buffer untouched"),
                        other => panic!("a pending display frame must reject a second submission: {other:?}"),
                    }
                }
                s.frames += 1;
            } else {
                if matches!(case, Case::Twin { enabled: true }) {
                    assert!(s.view.has_ghost(), "the live secondary stays visible after its last frame");
                    let main = s.view.picture().map(Texture::texture_id);
                    let other = s.view.ghost_picture().map(Texture::texture_id);
                    s.view.swap_twin(cx);
                    assert_eq!(s.view.picture().map(Texture::texture_id), other, "swap promotes the retained secondary texture without acquisition");
                    assert_eq!(s.view.ghost_picture().map(Texture::texture_id), main, "swap demotes the retained primary texture to ghost");
                    s.view.swap_twin(cx);
                    assert_eq!(s.view.picture().map(Texture::texture_id), main, "a second swap restores the original main texture");
                }
                if matches!(case, Case::Ghost { mode: GhostMode::Cleared, .. }) {
                    let ghost = s.view.ghost_picture().map(Texture::texture_id);
                    assert!(s.view.has_ghost() && ghost.is_some(), "the captured ghost remains present while B accumulates");
                    s.view.clear_ghost(cx);
                    assert!(!s.view.has_ghost(), "clearing the ghost hides it immediately");
                    assert_eq!(
                        s.view.ghost_picture().map(Texture::texture_id),
                        ghost,
                        "clearing retains the ghost texture for reuse"
                    );
                }
                // A draw with no pending frame must keep the same picture and do no accumulation.
                assert!(!s.persistence.frame(WIDTH, 0, &[], DT, case.look().tau, &mut Vec::new()), "idle reference holds its picture");
                blended = Some(false);
                finished = true;
            }
            let before = s.view.picture().map(Texture::texture_id);
            let mut draw = CxDraw::new(cx, event);
            let mut cx = Cx2d::new(&mut draw);
            cx.begin_pass(&s.pass, None);
            s.list.begin_always(&mut cx);
            let dpi = cx.current_dpi_factor();
            cx.begin_root_turtle(dvec2(WINDOW_WIDTH as f64 / dpi, HEIGHT as f64 / dpi), Layout::flow_overlay());
            let walk = Walk {
                width: Size::Fixed(WIDTH as f64 / dpi),
                height: Size::Fixed(HEIGHT as f64 / dpi),
                ..Walk::default()
            };
            let _ = s.view.draw_walk(&mut cx, &mut Scope::empty(), walk);
            if freezing {
                assert!(s.view.has_ghost(), "a ready freeze makes the copied ghost visible");
                assert!(s.view.ghost_picture().is_some(), "a ready freeze exposes its separate picture");
                assert_ne!(
                    before,
                    s.view.picture().map(Texture::texture_id),
                    "a ready freeze switches the live picture to the cleared buffer"
                );
                assert_ne!(
                    s.view.ghost_picture().map(Texture::texture_id),
                    s.view.picture().map(Texture::texture_id),
                    "the ghost must not alias the live picture"
                );
            }
            assert_eq!(s.view.plot_px(), Some((WIDTH as u32, HEIGHT as u32)), "the plot must use device pixels");
            let picture = s.view.picture().expect("a drawn plot has a picture");
            if let Some(blended) = blended {
                assert_eq!(
                    before != Some(picture.texture_id()),
                    blended,
                    "only a waveform frame swaps pictures, even when its dirty rectangle is empty (no step: {:?})",
                    s.view.why_no_step()
                );
            }
            s.read.draw_vars.set_texture(0, picture);
            s.read.draw_abs(
                &mut cx,
                Rect {
                    pos: dvec2(WIDTH as f64 / dpi, 0.0),
                    size: dvec2((WIDTH * 2) as f64 / dpi, HEIGHT as f64 / dpi),
                },
            );
            if matches!(case, Case::Grade | Case::Waterfall) {
                s.view.draw_waterfall(&mut cx, Rect {
                    pos: dvec2(WIDTH as f64 / dpi, 0.0),
                    size: dvec2(WIDTH as f64 / dpi, HEIGHT as f64 / dpi),
                });
                let pass = &cx.passes[s.view.waterfall_pass().unwrap()];
                if matches!(case, Case::Grade) {
                    assert!(s.view.draw_waterfall_row.draw_vars.area.is_empty(), "a disabled waterfall clear records no row draw item");
                    assert!(s.view.draw_waterfall_grade.draw_vars.area.is_empty(), "a disabled waterfall records no grading draw item");
                    assert_eq!(pass.painted_serial, 0, "the disabled waterfall pass never paints");
                    assert!(pass.main_draw_list_id.is_none(), "the disabled waterfall has no pass to paint this frame");
                } else if s.frames == 3 {
                    assert!(pass.main_draw_list_id.is_none(), "turning the waterfall off withdraws its previous pass");
                } else if s.frames == 4 {
                    assert!(pass.main_draw_list_id.is_some(), "re-enabling the waterfall records its pending clear");
                    assert_eq!(s.view.draw_waterfall_row.rect_size, vec2f(WIDTH as f32, (HEIGHT / 2) as f32), "the clear covers the whole ring before the new rows");
                }
            }
            cx.end_pass_sized_turtle();
            s.list.end(&mut cx);
            cx.end_pass(&s.pass);
            done_in_draw.set(finished);
            frames_in_draw.set((frames_in_draw.get().0 + 1, s.frames));
            if !finished {
                let _ = cx.new_next_frame();
            }
        }
    }))));
    cx.borrow_mut().init_cx_os();
    let (width, rgba) = capture(&cx, &done, &frames, case.frames());
    // A window is never smaller than the platform's minimum, so the drawn area is its top-left corner.
    assert!(
        width >= WINDOW_WIDTH && rgba.len() >= width * HEIGHT * 4,
        "the capture covers the drawn {WINDOW_WIDTH} x {HEIGHT} corner: {width} wide, {} bytes",
        rgba.len()
    );
    let expected = std::mem::take(&mut *reference.borrow_mut());
    (width, rgba, expected)
}

/// Captures the completed window as top-first RGBA rows.
fn capture(cx: &Rc<RefCell<Cx>>, done: &Cell<bool>, frames: &Cell<(u32, u32)>, frame_count: u32) -> (usize, Vec<u8>) {
    use makepad_platform::makepad_hal::{self as hal_api, open_flags, Hal, OpenReq};
    let hal = Hal::global();
    let mut req = OpenReq::from_env(0, open_flags::OFFSCREEN_WINDOWS, 1.0);
    req.flags |= open_flags::OFFSCREEN_WINDOWS | open_flags::RETAIN_WINDOWS;
    let info = hal.open(&req);
    assert_ne!(info.backend, hal_api::backend_id::HEADLESS, "pixel comparisons require a GPU");
    cx.borrow_mut().hal_attach_session(info);
    // A hang guard counted in frames, not time: once every pipeline is built the acquisitions run one
    // a frame, back to back, and a busy box only makes those frames slower.
    const GUARD_FRAMES: u32 = 120;
    let mut built_at: Option<u32> = None;
    let pixels = loop {
        let (drawn, acquired) = frames.get();
        if cx.borrow().pipelines_pending() > 0 {
            built_at = None;
        } else {
            let since = *built_at.get_or_insert(drawn);
            assert!(
                drawn - since < frame_count + GUARD_FRAMES,
                "capture never completed: {drawn} draws, {acquired}/{frame_count} acquisitions, {} draws with every pipeline built",
                drawn - since
            );
        }
        let mut cx = cx.borrow_mut();
        assert!(cx.hal_session_turn(1_000_000, true), "the GPU session must stay open until capture");
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
