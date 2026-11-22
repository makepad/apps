//! The rows' eye: an LED's light as each frame shows it, a box over the frame's stretch of the run.

use fab_elec_sim::live::LiveValues;
use fab_elec_simhost::host::Eye;

/// The LED's forward current of a 1 kHz PWM, on 25 % of each period at 1 (the units the row reads).
const PERIOD: f64 = 1e-3;
const ON: f64 = 0.25;

/// Wall seconds between frames.
const FRAME: f64 = 1.0 / 60.0;

/// The PWM's integral from 0 to `t`.
fn charge(t: f64) -> f64 {
    let whole = (t / PERIOD).floor();
    whole * ON * PERIOD + (t - whole * PERIOD).min(ON * PERIOD)
}

/// One publication of the interval `[since, t]` with the LED's mean current over it.
fn values(t: f64, since: f64, ratio: f64, mean: f32) -> LiveValues {
    LiveValues { t, generation: 1, ratio, since, branch_i: vec![0.0], pin_mean: vec![mean], ..LiveValues::default() }
}

/// The glow after each of `n` frames, one publication a frame, the box stepping `ratio` simulated
/// seconds per real second.
fn glow(eye: &mut Eye, ratio: f64, n: usize) -> Vec<f32> {
    let mut since = 0.0;
    (1..=n)
        .map(|k| {
            let t = k as f64 * FRAME * ratio;
            eye.see(&values(t, since, ratio, ((charge(t) - charge(since)) / (t - since)) as f32));
            since = t;
            eye.pin()[0]
        })
        .collect()
}

fn spread(g: &[f32]) -> (f32, f32) {
    (g.iter().copied().fold(f32::INFINITY, f32::min), g.iter().copied().fold(f32::NEG_INFINITY, f32::max))
}

/// A frame shows its own stretch of the run, no tail: a 1 kHz PWM LED at 25 % glows steadily at
/// 0.25 where a frame spans many periods (1x, or as fast as the box goes), and blinks as the slowed
/// PWM does where it spans less than one; a hard switch is full on the first frame after its edge
/// and dark on the first after it goes off; a frame that missed publications shows the newest
/// light; a publication with no simulated time (the run held) changes nothing.
#[test]
fn each_frame_shows_its_own_light() {
    for ratio in [1.0, 3.0] {
        let (lo, hi) = spread(&glow(&mut Eye::default(), ratio, 600));
        assert!((lo - 0.25).abs() < 0.02 && (hi - 0.25).abs() < 0.02, "a 1 kHz 25 % PWM LED glows steadily at 0.25 stepping {ratio} of real time: {lo}..{hi}");
    }
    let (lo, hi) = spread(&glow(&mut Eye::default(), 0.01, 600));
    assert!(lo < 0.05 && hi > 0.95, "at 0.01 of real time a frame spans a sixth of a period: the LED blinks: {lo}..{hi}");

    // dark, then switched on 5 ms into a frame: that frame shows the part it was lit, the next full
    let mut eye = Eye::default();
    let mut t = 0.0;
    for _ in 0..30 {
        eye.see(&values(t + FRAME, t, 1.0, 0.0));
        t += FRAME;
    }
    let edge = t + 0.005;
    eye.see(&values(t + FRAME, t, 1.0, ((t + FRAME - edge) / FRAME) as f32));
    let part = eye.pin()[0];
    t += FRAME;
    eye.see(&values(t + FRAME, t, 1.0, 1.0));
    assert!((part - 0.7).abs() < 0.01 && eye.pin()[0] == 1.0, "a hard switch: the edge's frame shows its lit share ({part}), the next is full ({})", eye.pin()[0]);
    t += FRAME;
    eye.see(&values(t + FRAME, t, 1.0, 0.0));
    assert_eq!(eye.pin()[0], 0.0, "and dark on the first frame after it goes off");

    // frames miss a second of publications while the LED came on: the newest light shows
    let gap = t + 1.0;
    eye.see(&values(gap + FRAME, gap, 1.0, 1.0));
    assert_eq!(eye.pin()[0], 1.0, "after missed publications the frame shows the newest light");
    eye.see(&values(gap + FRAME, gap + FRAME, 0.0, 0.0));
    assert_eq!(eye.pin()[0], 1.0, "a held run's empty publication keeps the light");
}
