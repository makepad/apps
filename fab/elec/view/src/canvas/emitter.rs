//! An LED's own colour, from the part's emitter data (`fab_elec_parts::Light`), for every view
//! that draws a lit LED: the canvas's glow ([`emitter_ix`], `Common::live_ix[3]`) and the 3D
//! view's emission ([`emitter_rgb`]). One conversion, so a green LED is the same green everywhere.

use fab_colour::oklab::srgb8_of_linear;
use fab_colour::spectrum::{cie_xyz, xyz_to_rgb};
use fab_elec_parts::Light;

/// A peak wavelength's line width at half power, in nm: a typical indicator LED die.
const LINE_FWHM_NM: f32 = 30.0;

/// The emitter's linear sRGB colour at unit brightness (the brightest channel 1), black for no
/// light. A peak wavelength is an LED line [`LINE_FWHM_NM`] wide, a spectrum its samples; both go
/// through the CIE 1931 observer to D65-balanced linear sRGB. A colour outside sRGB (a narrow
/// line) is desaturated toward white just until no channel is negative, which keeps its hue.
pub fn emitter_rgb(light: &Light) -> [f32; 3] {
    let mut c = match light {
        Light::Rgb(c) => *c,
        Light::Peak(nm) => {
            let (peak, sigma) = (*nm as f32, LINE_FWHM_NM / 2.3548);
            let (mut xyz, mut weight) = ([0.0f32; 3], 0.0f32);
            let mut l = (peak - 3.0 * sigma).floor();
            while l <= peak + 3.0 * sigma {
                let t = (l - peak) / sigma;
                let w = (-0.5 * t * t).exp();
                let c = cie_xyz(l);
                for k in 0..3 { xyz[k] += w * c[k]; }
                weight += w;
                l += 1.0;
            }
            visible(xyz, weight)
        }
        Light::Spectrum(samples) => {
            // Each sample stands for half the gap to each neighbour (trapezoid weights).
            let (mut xyz, mut weight) = ([0.0f32; 3], 0.0f32);
            for (i, &(nm, power)) in samples.iter().enumerate() {
                let before = if i > 0 { samples[i - 1].0 } else { nm };
                let after = samples.get(i + 1).map_or(nm, |s| s.0);
                let w = (power * 0.5 * (after - before).abs()) as f32;
                let c = cie_xyz(nm as f32);
                for k in 0..3 { xyz[k] += w * c[k]; }
                weight += w.abs();
            }
            visible(xyz, weight)
        }
    };
    if c.iter().any(|v| !v.is_finite()) { return [0.0; 3]; }
    let lo = c[0].min(c[1]).min(c[2]);
    if lo < 0.0 {
        for v in &mut c { *v -= lo; }
    }
    let hi = c[0].max(c[1]).max(c[2]);
    if !(hi > 0.0) { return [0.0; 3]; }
    c.map(|v| v / hi)
}

/// Linear sRGB of a light's integrated XYZ, or black when the eye sees next to none of its power
/// (`weight`: the power integrated): normalising a vanishing colour would only amplify rounding.
fn visible(xyz: [f32; 3], weight: f32) -> [f32; 3] {
    if !(xyz[0] + xyz[1] + xyz[2] > 1e-6 * weight) { return [0.0; 3]; }
    xyz_to_rgb(xyz)
}

/// The emitter's colour as the canvas carries it in `Common::live_ix[3]`: display sRGB packed
/// 0xRRGGBB into a float (exact below 2^24), at unit brightness; 0 for no light. The shader glows
/// a part in it by the part row's LED light output (y) instead of the dissipation ramp.
pub fn emitter_ix(light: &Light) -> f32 {
    let [r, g, b] = srgb8_of_linear(emitter_rgb(light));
    ((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn led_colours_keep_their_hue_at_full_brightness() {
        let unpack = |ix: f32| { let v = ix as u32; [(v >> 16) as u8, (v >> 8) as u8, v as u8] };
        let green = unpack(emitter_ix(&Light::Peak(525.0)));
        assert!(green[1] == 255 && green[0] < 128, "green: a 525 nm die is green ({green:?})");
        // The parts' generic names glow as named (`Light::of_led_color`): "green" is a true green die, "yellow-green"
        // the older ~570 nm kind. A line at 570 nm sits just outside sRGB: lifted to the gamut it lights red and green
        // nearly equally with almost no blue; at 525 nm red is the negative channel and blue stays well above it.
        let generic = unpack(emitter_ix(&Light::of_led_color("green").expect("green is a generic LED")));
        assert!(generic[1] == 255 && generic[0] < 128, "generic green: an LED named green glows green ({generic:?})");
        let yellow_green = unpack(emitter_ix(&Light::of_led_color("yellow-green").expect("yellow-green is a generic LED")));
        assert!(yellow_green[0] >= 160 && yellow_green[1] >= 160 && yellow_green[2] < 64,
            "yellow-green: an LED named yellow-green lights red and green, not blue ({yellow_green:?})");
        let red = unpack(emitter_ix(&Light::Peak(630.0)));
        assert!(red[0] == 255 && red[1] < 128 && red[2] < 64, "red: 630 nm is red ({red:?})");
        let blue = unpack(emitter_ix(&Light::Peak(470.0)));
        assert!(blue[2] == 255 && blue[0] < 128, "blue: 470 nm is blue ({blue:?})");
        assert_eq!(emitter_rgb(&Light::Rgb([0.5, 0.5, 0.5])), [1.0; 3], "white: an RGB light is scaled to unit brightness");
        let line = emitter_rgb(&Light::Spectrum(vec![(560.0, 0.0), (570.0, 1.0), (580.0, 0.0)]));
        let peak = emitter_rgb(&Light::Peak(570.0));
        assert!(line.iter().all(|&v| v <= 1.0) && line.contains(&1.0) && (line[0] - peak[0]).abs() < 0.2, "spectrum: a narrow line near 570 nm is the peak's colour ({line:?} vs {peak:?})");
        assert_eq!(emitter_ix(&Light::Peak(100.0)), 0.0, "no light: a wavelength nobody sees is black, packed 0");
    }
}
