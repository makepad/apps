use makepad_story_cut::reframe::*;
use makepad_story_cut::Ease;

const FPS: f64 = 30.0;

/// A 4K desktop master reframed to 1080×1920: crops are 1080..1215 wide.
fn input<'a>(track: &'a [FocusSample], pinned: &'a [PinnedKey], safe: SafeZone) -> ReframeInput<'a> {
    ReframeInput { track, source: (3840.0, 2160.0), output: (1080.0, 1920.0), fps: FPS, frames: 0, safe, pinned, params: ReframeParams::default() }
}

/// A 100×100 focus rect centred at (x, y) on every frame of `frames`.
fn at(frames: std::ops::Range<u64>, x: f64, y: f64) -> Vec<FocusSample> {
    frames.map(|frame| FocusSample { frame, rects: vec![(Rect::new(x - 50.0, y - 50.0, 100.0, 100.0), 1.0)], typing: false }).collect()
}

fn cx(rect: &Rect) -> f64 {
    rect.centre().0
}

#[test]
fn never_upscales() {
    let track = at(0..10, 1900.0, 1000.0);
    let solution = solve(&input(&track, &[], SafeZone::None)).unwrap();
    assert!(!solution.upscaled);
    assert!(solution.frames.iter().all(|crop| crop.w == 1080.0 && crop.h == 1920.0), "{:?}", solution.frames[0]);
    // The whole window is the largest crop of the aspect.
    let wide = [FocusSample { frame: 0, rects: vec![(Rect::new(1800.0, 900.0, 50.0, 50.0), 1.0), (Rect::new(0.0, 0.0, 3840.0, 2160.0), 5.0)], typing: false }];
    let solution = solve(&input(&wide, &[], SafeZone::None)).unwrap();
    assert_eq!(solution.frames[0].w, 2160.0 * 9.0 / 16.0, "the strong wide rect overrides the weak one");
    // A source smaller than the output cannot avoid upscaling; it says so.
    let small = ReframeInput { source: (1280.0, 720.0), ..input(&track, &[], SafeZone::None) };
    let solution = solve(&small).unwrap();
    assert!(solution.upscaled);
    assert_eq!(solution.frames[0].w, 720.0 * 9.0 / 16.0);
}

#[test]
fn small_moves_pan_smoothly() {
    let mut track = at(0..60, 1000.0, 1080.0);
    track.extend(at(60..120, 1400.0, 1080.0));
    let solution = solve(&input(&track, &[], SafeZone::None)).unwrap();
    assert!(solution.cuts.is_empty());
    assert_eq!(cx(&solution.frames[59]), 1000.0);
    let steps = solution.frames.windows(2).map(|pair| (cx(&pair[1]) - cx(&pair[0])).abs()).fold(0.0, f64::max);
    assert!(steps < 120.0, "largest step {steps}");
    // Settled within 2% of the move 0.4 s after it started.
    assert!((cx(&solution.frames[72]) - 1400.0).abs() <= 8.0 + 1e-6, "{}", cx(&solution.frames[72]));
    assert!(cx(&solution.frames[66]) > 1100.0 && cx(&solution.frames[66]) < 1390.0);
    assert!((cx(&solution.frames[119]) - 1400.0).abs() < 0.5);
}

#[test]
fn jumps_cut() {
    let mut track = at(0..60, 1000.0, 1080.0);
    track.extend(at(60..90, 3000.0, 1080.0));
    let solution = solve(&input(&track, &[], SafeZone::None)).unwrap();
    assert_eq!(solution.cuts, [60]);
    assert_eq!((cx(&solution.frames[59]), cx(&solution.frames[60])), (1000.0, 3000.0));
    let cut_key = solution.keys.iter().find(|key| key.t == 2.0).expect("a key on the cut");
    assert_eq!(cut_key.ease, Ease::Hold);
}

#[test]
fn shots_last_at_least_min_shot() {
    let mut track = at(0..30, 1000.0, 1080.0);
    track.extend(at(30..36, 3000.0, 1080.0));
    track.extend(at(36..90, 1000.0, 1080.0));
    let solution = solve(&input(&track, &[], SafeZone::None)).unwrap();
    // The jump back at frame 36 waits for 0.8 s (24 frames) of shot.
    assert_eq!(solution.cuts, [30, 54]);
    assert_eq!(cx(&solution.frames[53]), 3000.0);
    assert_eq!(cx(&solution.frames[54]), 1000.0);
}

#[test]
fn holds_while_the_target_stays_inside() {
    let track = (0..60).map(|frame| {
        let jitter = if frame % 2 == 0 { 20.0 } else { -20.0 };
        FocusSample { frame, rects: vec![(Rect::new(1850.0 + jitter, 1030.0 - jitter, 100.0, 100.0), 1.0)], typing: false }
    }).collect::<Vec<_>>();
    let solution = solve(&input(&track, &[], SafeZone::None)).unwrap();
    assert!(solution.frames.iter().all(|crop| *crop == solution.frames[0]));
    assert_eq!(solution.keys.len(), 2, "a still shot is two keys");
}

#[test]
fn holds_while_typing() {
    // The caret line grows rightwards from the text field's left edge.
    let typing = |typing: bool| (0..60).map(|frame| FocusSample {
        frame, rects: vec![(Rect::new(1700.0, 1000.0, 100.0 + frame as f64 * 8.0, 60.0), 1.0)], typing,
    }).collect::<Vec<_>>();
    let track = typing(true);
    let solution = solve(&input(&track, &[], SafeZone::None)).unwrap();
    assert!(solution.frames.iter().all(|crop| *crop == solution.frames[0]), "typing inside the framing holds");
    let track = typing(false);
    let solution = solve(&input(&track, &[], SafeZone::None)).unwrap();
    assert!(solution.frames.iter().any(|crop| *crop != solution.frames[0]), "the same growth without typing follows");
}

#[test]
fn pinned_keys_are_met_exactly() {
    let track = at(0..90, 1000.0, 1080.0);
    let pin = PinnedKey { frame: 45, crop: Rect::new(700.0, 120.0, 1080.0, 1920.0) };
    let solution = solve(&input(&track, &[pin], SafeZone::None)).unwrap();
    assert_eq!(solution.frames[45], pin.crop);
    assert_eq!(solution.frames[32].x, 460.0, "outside the blend window the solve is untouched");
    assert!(solution.frames[40].x > 460.0 && solution.frames[40].x < 700.0);
    assert!(solution.cuts.is_empty());
    let keys = to_keys(&solution.keys);
    assert_eq!(keys.sample(1.5), pin.crop.to_array(), "the reduced keys keep the pin");

    // A pin far away from the solved framing is reached by a cut.
    let far = PinnedKey { frame: 45, crop: Rect::new(2600.0, 120.0, 1080.0, 1920.0) };
    let solution = solve(&input(&track, &[far], SafeZone::None)).unwrap();
    assert_eq!(solution.frames[45], far.crop);
    assert!(solution.cuts.contains(&45));
}

#[test]
fn focus_stays_out_of_safe_zone_bands() {
    let track = at(0..30, 1900.0, 1080.0);
    for safe in [SafeZone::Reels, SafeZone::TikTok, SafeZone::Shorts, SafeZone::None] {
        let solution = solve(&input(&track, &[], safe)).unwrap();
        let target = track[0].rects[0].0;
        for crop in &solution.frames {
            assert!(safe.safe_rect(crop).contains(&target), "{safe:?}: {crop:?}");
        }
    }
    let bands = SafeZone::Reels.bands();
    assert_eq!((bands.top, bands.bottom), (0.14, 0.35));
    assert_eq!(SafeZone::from_id("tiktok"), Some(SafeZone::TikTok));
    // With the bottom band the target sits above the frame's centre.
    let solution = solve(&input(&track, &[], SafeZone::Reels)).unwrap();
    assert!(solution.frames[0].centre().1 > 1080.0);
}

#[test]
fn reduced_keys_reproduce_the_path() {
    let mut track = at(0..60, 1000.0, 1080.0);
    track.extend(at(60..120, 1400.0, 900.0));
    track.extend(at(120..150, 3200.0, 1080.0));
    let solution = solve(&input(&track, &[], SafeZone::None)).unwrap();
    assert!(solution.keys.len() < solution.frames.len() / 3, "{} keys", solution.keys.len());
    let keys = to_keys(&solution.keys);
    for (frame, crop) in solution.frames.iter().enumerate() {
        let sampled = keys.sample(frame as f64 / FPS);
        let error = (0..4).map(|i| (sampled[i] - crop.to_array()[i]).abs()).fold(0.0, f64::max);
        assert!(error <= 1.0 + 1e-6, "frame {frame}: {error}");
    }
}

#[test]
fn keys_are_written_as_splash_and_retimed() {
    let keys = [
        CropKey { t: 0.0, crop: Rect::new(10.2, 20.0, 1080.0, 1920.0), ease: Ease::Linear, pinned: false },
        CropKey { t: 1.0 / 3.0, crop: Rect::new(30.0, 20.0, 1080.0, 1920.0), ease: Ease::Linear, pinned: false },
        CropKey { t: 2.0, crop: Rect::new(900.0, 20.0, 1080.0, 1920.0), ease: Ease::Hold, pinned: false },
    ];
    assert_eq!(keys_splash(&keys), "keys([0.0, [10, 20, 1080, 1920]], [0.333333, [30, 20, 1080, 1920]], [2.0, [900, 20, 1080, 1920], @hold])");
    let pinned = [CropKey { pinned: true, ..keys[2] }, CropKey { pinned: true, ..keys[0] }];
    assert_eq!(keys_splash(&pinned), "keys([2.0, [900, 20, 1080, 1920], @hold, @pin], [0.0, [10, 20, 1080, 1920], @pin])");
    let retimed = retime(&keys, 1.0, 2.0);
    assert_eq!(retimed.iter().map(|key| key.t).collect::<Vec<_>>(), [(1.0 / 3.0 - 1.0) / 2.0, 0.5]);
}

/// solve → `keys_splash` → a cut's `crop:` → load → sampled at every
/// output frame `n / fps` reproduces the solved crops, a cut on a frame
/// whose time is not a whole millisecond included.
#[test]
fn written_keys_round_trip_through_an_edit() {
    use makepad_story_cut::{DrawKind, CutDoc, CutEval};
    let fps = 60.0;
    // A cut on frame 61 (1.01666… s), then a pan.
    let mut track = at(0..61, 1000.0, 1080.0);
    track.extend(at(61..150, 3000.0, 1080.0));
    track.extend(at(150..240, 3400.0, 900.0));
    let solution = solve(&ReframeInput { fps, ..input(&track, &[], SafeZone::None) }).unwrap();
    assert_eq!(solution.cuts, [61]);
    let crop = keys_splash(&solution.keys);
    let source = format!(
        "use mod.flow.*\nuse mod.cut.*\nlet a = Clip{{src: asset(\"ast_yqzlg4xa4mbgpzs6e2qsuqwhsu\", \"arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f\")}}\nlet r = Reframe{{input: a.out(@frame) mode: @keys crop: {crop}}}\nlet out = CutOutput{{frame: r.out(@frame)}}\nFlow{{\n    kind: @cut\n    output: {{width: 1080 height: 1920 fps: 60}}\n    duration: 4.0\n    a, r, out\n}}\n"
    );
    let doc = CutDoc::load(&source).unwrap_or_else(|d| panic!("{d:#?}"));
    let mut eval = CutEval::new(&doc);
    let makepad_story_cut::CutNodeKind::Clip { src, .. } = &doc.nodes[0].kind else { panic!() };
    eval.set_source_size(src.clone(), 3840, 2160);
    for (frame, solved) in solution.frames.iter().enumerate() {
        let plan = eval.frame_plan(frame as f64 / fps);
        let DrawKind::Reframe { crop, .. } = &plan.ops[1].kind else { panic!() };
        let error = (0..4).map(|i| (crop.to_array()[i] - solved.to_array()[i]).abs()).fold(0.0, f64::max);
        assert!(error <= 1.5, "frame {frame}: {crop:?} vs {solved:?}");
    }
}

#[test]
fn pin_cuts_respect_min_shot_and_pins_survive_reduction() {
    let mut track = at(0..60, 1000.0, 1080.0);
    track.extend(at(60..120, 3000.0, 1080.0));
    // A far pin 6 frames after the solver's cut blends instead of cutting.
    let far = PinnedKey { frame: 66, crop: Rect::new(200.0, 120.0, 1080.0, 1920.0) };
    let solution = solve(&input(&track, &[far], SafeZone::None)).unwrap();
    assert_eq!(solution.cuts, [60]);
    assert_eq!(solution.frames[66], far.crop);
    // A pin 0.6 px off the solved path keeps its own key.
    let still = at(0..90, 1000.0, 1080.0);
    let solved = solve(&input(&still, &[], SafeZone::None)).unwrap();
    let near = PinnedKey { frame: 45, crop: Rect { x: solved.frames[45].x + 0.6, ..solved.frames[45] } };
    let solution = solve(&input(&still, &[near], SafeZone::None)).unwrap();
    assert!(solution.keys.iter().any(|key| key.t == 45.0 / FPS && key.crop == near.crop && key.pinned), "{:?}", solution.keys);
    assert_eq!(solution.keys.iter().filter(|key| key.pinned).count(), 1, "only the pin is marked");
    assert!(keys_splash(&solution.keys).contains("@pin"));
}

#[test]
fn bad_input_is_refused() {
    let track = at(0..10, 1000.0, 1080.0);
    assert_eq!(solve(&ReframeInput { fps: 0.0, ..input(&track, &[], SafeZone::None) }), Err(ReframeError::Fps(0.0)));
    assert!(matches!(solve(&ReframeInput { source: (0.0, 2160.0), ..input(&track, &[], SafeZone::None) }), Err(ReframeError::Size { .. })));
    assert!(matches!(solve(&ReframeInput { output: (1080.0, f64::NAN), ..input(&track, &[], SafeZone::None) }), Err(ReframeError::Size { .. })));
    let params = ReframeParams { pad: f64::NAN, ..ReframeParams::default() };
    assert_eq!(solve(&ReframeInput { params, ..input(&track, &[], SafeZone::None) }), Err(ReframeError::Param("pad")));
}

#[test]
fn long_settles_reduce_without_deep_recursion() {
    // 10 minutes at 60 fps of one slow monotone settle, on a small stack.
    let frames = (0..36_000).map(|f| Rect::new(1000.0 * (1.0 - (-(f as f64) / 5000.0).exp()), 0.0, 1080.0, 1920.0)).collect::<Vec<_>>();
    let keys = std::thread::Builder::new().stack_size(2 << 20)
        .spawn(move || reduce(&frames, &[], 60.0, 0.01))
        .unwrap().join().unwrap();
    assert!(keys.len() > 2 && keys.len() < 36_000);
}

/// A pin survives a solve → write → load → `pinned_keys` → re-solve cycle.
#[test]
fn pins_round_trip_through_the_source() {
    use makepad_story_cut::{CutDoc, CutEval};
    let track = at(0..90, 1000.0, 1080.0);
    let pin = PinnedKey { frame: 45, crop: Rect::new(700.0, 120.0, 1080.0, 1920.0) };
    let solution = solve(&input(&track, &[pin], SafeZone::None)).unwrap();
    let source = format!(
        "use mod.flow.*\nuse mod.cut.*\nlet a = Clip{{src: asset(\"ast_yqzlg4xa4mbgpzs6e2qsuqwhsu\", \"arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f\")}}\nlet r = Reframe{{input: a.out(@frame) crop: {}}}\nlet out = CutOutput{{frame: r.out(@frame)}}\nFlow{{\n    kind: @cut\n    output: {{width: 1080 height: 1920 fps: 30}}\n    duration: 3.0\n    a, r, out\n}}\n",
        keys_splash(&solution.keys),
    );
    let doc = CutDoc::load(&source).unwrap_or_else(|d| panic!("{d:#?}"));
    let pins = CutEval::new(&doc).pinned_keys("r", FPS).unwrap();
    assert_eq!(pins, [pin]);
    let again = solve(&input(&track, &pins, SafeZone::None)).unwrap();
    assert_eq!(again.frames[45], pin.crop);
}
