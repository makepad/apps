//! The `spectrum` module draws a track's sound: a 1 kHz tone's band is the
//! tallest spectrum bar, the ring and the oscilloscope draw, and waveform
//! bars are the tone's size.

use makepad_motion::*;

fn tone(freq: f64, amplitude: f64, seconds: f64) -> AudioFeatures {
    let rate = 48_000.0;
    let samples: Vec<f32> = (0..(seconds * rate) as usize).map(|i| (amplitude * (std::f64::consts::TAU * freq * i as f64 / rate).sin()) as f32).collect();
    let mut features = AudioFeatures::default();
    features.music.push(("m".into(), MusicFeatures { signal: Some(AudioSignal::new(0.0, rate, samples.into())), ..Default::default() }));
    features
}

fn list(frame: &MotionFrame, index: usize) -> std::sync::Arc<draw2d::Draw2dList> {
    match &frame.items[index].kind {
        ItemKind::Draw2d { picture, .. } => picture.list.clone(),
        other => panic!("item {index} is not a Draw2D: {other:?}"),
    }
}

/// Each filled shape's box `[x0, y0, x1, y1]`.
fn boxes(list: &draw2d::Draw2dList) -> Vec<[f64; 4]> {
    list.ops.iter().filter_map(|op| match op {
        draw2d::Op::Fill { contours, .. } => contours.iter().flatten().fold(None, |b: Option<[f64; 4]>, p| Some(match b {
            None => [p[0], p[1], p[0], p[1]],
            Some(b) => [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])],
        })),
        _ => None,
    }).collect()
}

#[test]
fn the_spectrum_module_draws_a_tone() {
    let source = r#"use mod.motion.*
use mod.motion.modules.spectrum.*
Motion{size: vec2(960, 540) duration: 2.0 layers: [
    Draw2D{name: "bars" size: vec2(960, 300) draw: fn(c, t, edit) { spectrum_bars(c, music(@m).spectrum(edit, 24), {w: 960 h: 300 color: #ff4d12 color2: #ffd27a}) }}
    Draw2D{name: "ring" size: vec2(540, 540) draw: fn(c, t, edit) { spectrum_ring(c, music(@m).spectrum(edit, 32), {x: 270 y: 270 r: 100 len: 150}) }}
    Draw2D{name: "scope" size: vec2(960, 200) draw: fn(c, t, edit) { wave_line(c, music(@m).wave(edit, 200, 0.01), {w: 960 h: 200}) }}
    Draw2D{name: "wave" size: vec2(960, 200) draw: fn(c, t, edit) { wave_bars(c, music(@m).wave(edit, 40, 0.05), {w: 960 h: 200}) }}
]}"#;
    let doc = MotionDoc::load_with(source, &[], MotionContext::analyzed(tone(1000.0, 0.5, 2.0), None, 0.0)).unwrap_or_else(|d| panic!("{d:?}"));
    let frame = doc.eval(1.0);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    // Bars: the tallest is the band holding 1 kHz (13 of 24: 30 Hz × (16000/30)^(13/24) = 902 Hz up to 1176 Hz).
    let bars = boxes(&list(&frame, 0));
    assert!(!bars.is_empty() && bars.len() <= 24);
    let tallest = bars.iter().max_by(|a, b| (a[3] - a[1]).total_cmp(&(b[3] - b[1]))).unwrap();
    assert_eq!(((tallest[0] + tallest[2]) * 0.5 / 40.0).floor(), 13.0, "{tallest:?}");
    assert!((tallest[3] - 300.0).abs() < 1e-6, "bars stand on the box's bottom");
    // The ring and the scope stroke.
    assert!(!list(&frame, 1).segs.is_empty());
    assert!(list(&frame, 2).segs.len() >= 100);
    // Waveform bars: 50 ms holds 50 cycles, every slot peaks at the tone's 0.5.
    let wave = boxes(&list(&frame, 3));
    assert_eq!(wave.len(), 40);
    assert!(wave.iter().all(|b| ((b[3] - b[1]) - 100.0).abs() < 2.0 && ((b[1] + b[3]) * 0.5 - 100.0).abs() < 1e-6), "{wave:?}");
    // Past the sound: nothing to draw.
    let after = MotionDoc::load_with(source, &[], MotionContext::analyzed(tone(1000.0, 0.5, 0.5), None, 0.0)).unwrap();
    let frame = after.eval(1.0);
    assert!(boxes(&list(&frame, 0)).is_empty() && boxes(&list(&frame, 3)).is_empty());
}
