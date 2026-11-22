//! Ghost answers share the live scale and digits; value brackets need only two levels.
use fab_elec_view::instruments::{
    scope::{ghost_suffix, lane_answers, Domain, Measurement},
    span::{value_bracket_end_at, value_bracket_label, End},
};

#[test]
fn ghost_answers_and_value_brackets() {
    let rows = [("freq", 10_000.0, "Hz"), ("duty", 51.3, "%"), ("Vpp", 0.01234, "V"), ("rise", 2e-9, "s")]
        .map(|(name, value, unit)| Measurement::exact(0, name, value, unit));
    let mut ghost = rows.clone();
    for (row, value) in ghost.iter_mut().zip([9_980.0, 20.0, 0.00998, 2e-9]) { row.value = value; }
    assert_eq!(ghost_suffix(&rows[1], &ghost[1]).as_deref(), Some(" (was 20.0)"), "duty retains the live decimal place without repeating percent");
    assert_eq!(ghost_suffix(&rows[0], &ghost[0]).as_deref(), Some(" (was 9.98)"), "frequency uses the live kHz scale");
    assert_eq!(ghost_suffix(&rows[2], &ghost[2]).as_deref(), Some(" (was 9.98)"), "peak-to-peak voltage uses the live mV scale across a decimal decade");
    assert_eq!(ghost_suffix(&rows[3], &ghost[3]), None, "an equal rise time has no ghost suffix");
    let mut near = rows[0].clone();
    near.value += 0.4;
    assert_eq!(ghost_suffix(&rows[0], &near), None, "a sub-digit frequency change has equal shown text");
    near.value = 980.0;
    assert_eq!(ghost_suffix(&rows[0], &near).as_deref(), Some(" (was 0.98)"), "a ghost across an SI boundary stays in the live kHz scale");
    let mut other = ghost[2].clone();
    other.unit = "A".into();
    assert_eq!(ghost_suffix(&rows[2], &other), None, "a unit mismatch never compares volts with amps");
    other = ghost[1].clone();
    other.name = "mean".into();
    assert_eq!(ghost_suffix(&rows[1], &other), None, "a name mismatch is not the same measurement");
    other = ghost[1].clone();
    other.lane = 1;
    assert_eq!(ghost_suffix(&rows[1], &other), None, "a different lane cannot supply the old value");

    let answers = lane_answers(&rows, &ghost, 0, Domain::Time, false, false, false);
    assert_eq!(answers.items[0].value, "10.00", "live frequency keeps the same decimal places as its ghost");
    assert_eq!(answers.items[0].unit, "kHz (was 9.98)", "frequency suffix belongs to the complete measured unit text");
    assert_eq!(answers.items[1].unit, "% high (was 20.0)", "duty suffix follows the high qualifier");
    assert_eq!(answers.items[2].unit, "mV pp (was 9.98)", "voltage suffix follows the peak-to-peak qualifier");
    assert_eq!(answers.items[3].unit, "ns", "an equal row adds no suffix to its answer");
    assert_eq!(answers.text, "10.00\u{2009}kHz (was 9.98)   51.3\u{2009}% high (was 20.0)   12.34\u{2009}mV pp (was 9.98)   rise 2\u{2009}ns", "accessible text contains the same complete answers and ghost words");
    let missing = lane_answers(&rows, &ghost[..1], 0, Domain::Time, false, false, false);
    assert_eq!(missing.items[1].unit, "% high", "a missing ghost row adds no suffix");
    ghost[2].unit = "A".into();
    let mismatch = lane_answers(&rows, &ghost, 0, Domain::Time, false, false, false);
    assert_eq!(mismatch.items[2].unit, "mV pp", "answer building also rejects a mismatched ghost unit");
    let empty = lane_answers(&rows, &[], 0, Domain::Time, false, false, false);
    assert!(!empty.text.contains("was"), "no ghost rows leave all accessible answers unchanged");
    assert_eq!(empty.items[0].value, "10", "without a comparison the ordinary answer trims trailing zeroes");
    let mut uncertain = rows.clone();
    uncertain[0].step = 200.0;
    let dim = lane_answers(&uncertain, &ghost, 0, Domain::Time, false, false, false);
    assert_eq!(dim.items[0].dim, vec![4..5], "comparison padding still dims digits finer than the source resolution");

    assert_eq!(value_bracket_label(0.0, 3.3, "V"), "dV 3.3 V", "value bracket uses existing SI answer formatting");
    assert_eq!(value_bracket_label(3.3, 0.0, "V"), "dV 3.3 V", "reversing the two levels keeps a positive difference");
    assert_eq!(value_bracket_label(-0.002, 0.001, "A"), "dV 3 mA", "difference spans signed levels in the lane's own unit");
    assert_eq!(value_bracket_label(1.5, 1.5, "V"), "dV 0 V", "coincident levels label the zero span");
    assert_eq!(value_bracket_end_at(34.0, 30.0, 10.0), Some(End::Start), "the lower screen level includes four-point slop");
    assert_eq!(value_bracket_end_at(6.0, 30.0, 10.0), Some(End::Finish), "the upper screen level includes four-point slop");
    assert_eq!(value_bracket_end_at(34.01, 30.0, 10.0), None, "lower level cannot be grabbed beyond four points");
    assert_eq!(value_bracket_end_at(5.99, 30.0, 10.0), None, "upper level cannot be grabbed beyond four points");
    assert_eq!(value_bracket_end_at(20.0, 30.0, 10.0), None, "the bracket interior leaves pan available");
    assert_eq!(value_bracket_end_at(12.0, 10.0, 14.0), Some(End::Start), "overlapping level slop resolves ties deterministically");
    assert_eq!(value_bracket_end_at(13.0, 10.0, 14.0), Some(End::Finish), "nearest level wins when hit regions overlap");
    assert_eq!(value_bracket_end_at(10.0, 10.0, 10.0), Some(End::Start), "coincident level lines remain draggable");
    assert_eq!(value_bracket_end_at(f64::NAN, 10.0, 20.0), None, "a nonfinite pointer never grabs a level");
}
