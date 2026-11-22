//! Board-clock conversion for boot-stage.md's RWDT stage-0 hold.
use crate::{image::refusal, runtime::SLEEP_CAP_TICKS, Refusal};

/// Describes measured board timing from the probe or the machine's TimeProfile.
pub struct BoardTime {
    /// Gives the measured RTC_SLOW frequency as a nonzero numerator/denominator in Hz.
    pub rc_slow_hz: (u64, u64),
    /// Gives the board's two-bit eFuse WDT_DELAY_SEL value (facts-machine.md §5).
    pub wdt_delay_sel: u8,
}

/// Returns a nonzero record hold covering 2 * (sleep cap + gap_ns).
/// The factor of two covers RC clock spread and the instruction-bound estimate.
/// Both conversions round up: nanoseconds to RTC_SLOW cycles, then cycles to
/// HOLD. Facts-machine.md §5 specifies HOLD << (WDT_DELAY_SEL + 1) and the
/// 32-bit expiry counter; boot-stage.md, Record, reserves zero for RWDT off.
///
/// The runtime feeds only once a quarter of this timeout has passed since its last feed
/// ([`feed_quarter_per_hold`]), and two feed points are at most S + G apart (S the sleep cap,
/// G the gap). So with T = 2 (S + G), feeds come at most T/4 + (S + G) = 1.5 (S + G) apart,
/// against the T = 2 (S + G) the hold allows: the margin for an RC slow clock running fast drops
/// from 2x to about 1.33x, and a masked span the gap does not count may take the rest, about 90
/// times the largest radio gate (0.55 ms). The tests assert the inequality.
pub fn wdt_hold(gap_ns: u64, t: &BoardTime) -> Result<u32, Refusal> {
    if t.wdt_delay_sel > 3 {
        return Err(refusal(format!("RWDT WDT_DELAY_SEL {} exceeds the two-bit limit 3", t.wdt_delay_sel)));
    }
    let (num, den) = t.rc_slow_hz;
    if num == 0 || den == 0 {
        return Err(refusal("RWDT rc_slow_hz numerator and denominator must both be nonzero"));
    }
    // facts-machine.md §1: SYSTIMER averages 16 MHz, or 125/2 ns per tick.
    let sleep_ns = (u128::from(SLEEP_CAP_TICKS) * 125).div_ceil(2);
    let ns = 2 * (sleep_ns + u128::from(gap_ns));
    let denominator = u128::from(den) * 1_000_000_000;
    // Even the largest denominator times u32::MAX fits u128. Thus a product
    // overflowing u128 necessarily exceeds the counter's limit after division.
    let product = ns.checked_mul(u128::from(num)).ok_or_else(|| refusal(format!(
        "RWDT scaled hold for {ns} ns at {num}/{den} Hz exceeds the 32-bit limit {} RTC_SLOW cycles", u32::MAX)))?;
    let cycles = product.div_ceil(denominator);
    let scale = 1u128 << (t.wdt_delay_sel + 1);
    let hold = cycles.div_ceil(scale).max(1);
    let scaled = hold * scale;
    if scaled > u128::from(u32::MAX) {
        return Err(refusal(format!("RWDT hold {hold} scales to {scaled} RTC_SLOW cycles; 32-bit limit is {}", u32::MAX)));
    }
    Ok(hold as u32)
}

/// CPU cycles per stage-0 HOLD unit, divided by four: the RWDT counts HOLD << (WDT_DELAY_SEL + 1)
/// RTC_SLOW cycles (facts-machine.md §5), here at the board's RTC_SLOW rate, in cycles at CPU_HZ.
/// The runtime multiplies the hold the boot stage programmed by it, so it feeds once a quarter
/// of the timeout the record set has passed, whichever hold the record carries. Rounded down, so
/// the quarter is never longer; zero (a board slow clock past CPU_HZ) feeds at every feed point.
pub fn feed_quarter_per_hold(t: &BoardTime) -> u32 {
    let (num, den) = t.rc_slow_hz;
    if num == 0 { return 0; }
    let scale = 1u128 << (u32::from(t.wdt_delay_sel.min(3)) + 1);
    let per_hold = scale * u128::from(crate::CPU_HZ) * u128::from(den) / (4 * u128::from(num));
    u32::try_from(per_hold).unwrap_or(u32::MAX)
}
