//! The achieved rate from a scripted sequence of publications: no clock, only what `LiveValues`
//! carries.

use fab_elec_sim::live::LiveValues;
use fab_elec_simhost::rate::{RateMeter, KEEP_UP, RATE_WINDOW};
use fab_elec_simhost::{Limit, Split};

/// A publication of generation `g` covering `[since, t]`, stepped at `ratio` with the device's and
/// the sinks' shares of the stepping time, `(co, sink)`.
fn lv(g: u64, since: f64, t: f64, ratio: f64, (co, sink): (f32, f32)) -> LiveValues {
    LiveValues { generation: g, since, t, ratio, co_share: co, sink_share: sink, ..LiveValues::default() }
}

/// One publication 0.15 s of wall time after the previous one (whose time `t` is), stepped at
/// `ratio`.
fn publish(meter: &mut RateMeter, t: &mut f64, g: u64, ratio: f64, shares: (f32, f32)) {
    let since = *t;
    *t += 0.15 * ratio;
    meter.see(&lv(g, since, *t, ratio, shares));
}

/// The meter equals simulated over wall time across the intervals that cover the window; the
/// oldest leave as newer ones fill it, and a run reads out a rate only once they cover it; a
/// publication at rest, an empty interval and the first of a new lowering do not count, and the one
/// that ends a run's start empties the window; the limit is the pacing while the run keeps up,
/// else the largest of the solver's, the scopes' and the emulator's shares of the stepping time.
#[test]
fn the_achieved_rate_is_simulated_over_wall_time_of_the_newest_publications() {
    let mut meter = RateMeter::default();
    assert_eq!(meter.achieved(), None, "no rate before an interval");
    let mut t = 0.0;
    publish(&mut meter, &mut t, 1, 1e-2, (0.2, 0.0));
    assert!((meter.achieved().unwrap() - 1e-2).abs() < 1e-15, "one interval: its own ratio: {:?}", meter.achieved());
    assert_eq!(meter.reached(), None, "no rate is read out before the window is full");
    for _ in 0..3 {
        publish(&mut meter, &mut t, 1, 2e-2, (0.2, 0.0));
    }
    let want = (0.15 * 1e-2 + 3.0 * 0.15 * 2e-2) / 0.6;
    assert!((meter.achieved().unwrap() - want).abs() < 1e-15, "0.6 s of intervals: sum(span) / sum(wall) = {want}: {:?}", meter.achieved());
    assert_eq!(meter.reached(), meter.achieved(), "a full window reads out its rate");
    publish(&mut meter, &mut t, 1, 2e-2, (0.2, 0.0));
    assert!((meter.achieved().unwrap() - 2e-2).abs() < 1e-15, "the oldest left once the newer cover {RATE_WINDOW} s: {:?}", meter.achieved());

    let before = meter.achieved();
    meter.see(&lv(1, t, t + 1e-3, 0.0, (0.0, 0.0)));
    meter.see(&lv(1, t, t, 1.0, (0.0, 0.0)));
    assert_eq!(meter.achieved(), before, "a publication at rest (ratio 0) and an empty interval do not count");
    publish(&mut meter, &mut t, 2, 1.0, (0.9, 0.0));
    assert_eq!(meter.achieved(), before, "the first publication of a new lowering does not count");

    assert_eq!(meter.limited_by(2e-2, true), Some(Limit::Paced), "keeping up with a finite asked rate: paced");
    assert_eq!(meter.limited_by(2e-2 / KEEP_UP * 1.01, true), Some(Limit::Solver), "falling behind with the device at 20 %: the solver");
    assert_eq!(meter.limited_by(2e-2, false), Some(Limit::Solver), "a firmware step is not paced");
    for _ in 0..6 {
        publish(&mut meter, &mut t, 2, 1e-3, (0.8, 0.0));
    }
    assert_eq!(meter.limited_by(f64::INFINITY, true), Some(Limit::Emulator), "the device took most of the stepping time: the emulator");
    let near = |split: Split, want: Split| (split.solver - want.solver).abs() < 1e-6 && (split.scope - want.scope).abs() < 1e-6 && (split.emulator - want.emulator).abs() < 1e-6;
    assert!(meter.split().is_some_and(|s| near(s, Split { solver: 0.2, scope: 0.0, emulator: 0.8 })), "the shares over the window: {:?}", meter.split());
    for _ in 0..6 {
        publish(&mut meter, &mut t, 2, 1e-3, (0.06, 0.84));
    }
    assert_eq!(meter.limited_by(f64::INFINITY, true), Some(Limit::Scope), "the scopes' sinks took most of the stepping time: the scope");
    assert!(meter.split().is_some_and(|s| near(s, Split { solver: 0.1, scope: 0.84, emulator: 0.06 })), "the solver has the rest: {:?}", meter.split());
    for _ in 0..6 {
        publish(&mut meter, &mut t, 2, 1e-3, (0.4, 0.3));
    }
    assert_eq!(meter.limited_by(f64::INFINITY, true), Some(Limit::Emulator), "the largest share names the limit, under half too");
    meter.see(&LiveValues { started: true, ..lv(2, t, t + 1e-3, 1e-4, (0.0, 0.0)) });
    assert_eq!((meter.achieved(), meter.reached()), (None, None), "the publication that ends a run's start empties the window: its set-up and first plan are no measure");
    publish(&mut meter, &mut t, 2, 1.0, (0.2, 0.0));
    assert!(meter.achieved() == Some(1.0) && meter.reached().is_none(), "the run is measured from there, read out once the window is full again: {:?}", meter.achieved());
    meter.clear();
    assert_eq!((meter.achieved(), meter.limited_by(1.0, true)), (None, None), "a cleared meter has no rate");
}
