//! The instrument tokens keep their promises: one lightness per theme, channels that stay apart
//! (the first four also under the two common colour-vision deficiencies), and text that reads.

use fab_elec_view::instruments::look::{InstrumentLook, CHANNEL_LIGHTNESS};

fn linear(c: f32) -> f64 {
    let c = f64::from(c);
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// OKLab from linear sRGB.
fn oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

/// Dichromat simulation in linear sRGB (Machado, Oliveira and Fernandes 2009, severity 1).
const PROTANOPIA: [[f64; 3]; 3] = [[0.152286, 1.052583, -0.204868], [0.114503, 0.786281, 0.099216], [-0.003882, -0.048116, 1.051998]];
const DEUTERANOPIA: [[f64; 3]; 3] = [[0.367322, 0.860646, -0.227968], [0.280085, 0.672501, 0.047413], [-0.011820, 0.042940, 0.968881]];

fn seen(c: [f32; 4], vision: Option<&[[f64; 3]; 3]>) -> [f64; 3] {
    let rgb = [linear(c[0]), linear(c[1]), linear(c[2])];
    let rgb = match vision {
        None => rgb,
        Some(m) => std::array::from_fn(|i| (m[i][0] * rgb[0] + m[i][1] * rgb[1] + m[i][2] * rgb[2]).clamp(0.0, 1.0)),
    };
    oklab(rgb)
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn luminance(c: [f32; 4]) -> f64 {
    0.2126 * linear(c[0]) + 0.7152 * linear(c[1]) + 0.0722 * linear(c[2])
}

fn contrast(a: [f32; 4], b: [f32; 4]) -> f64 {
    let (a, b) = (luminance(a) + 0.05, luminance(b) + 0.05);
    a.max(b) / a.min(b)
}

#[test]
fn channel_hues_and_text_levels() {
    for (dark, lightness) in [(true, CHANNEL_LIGHTNESS.0), (false, CHANNEL_LIGHTNESS.1)] {
        let look = InstrumentLook::for_theme(dark);
        let canvas = look.canvas.canvas;
        for (i, &c) in look.channels.iter().enumerate() {
            let l = seen(c, None)[0];
            assert!((l - f64::from(lightness)).abs() < 0.01, "dark={dark} channel {i}: OKLab lightness {l}, expected {lightness}: density, not hue, carries brightness");
            assert!(contrast(c, canvas) >= 4.5, "dark={dark} channel {i}: contrast {} against the canvas; a channel's numbers are text in its hue", contrast(c, canvas));
        }
        for (vision, name) in [(None, "normal"), (Some(&PROTANOPIA), "protanopia"), (Some(&DEUTERANOPIA), "deuteranopia")] {
            for a in 0..4 {
                for b in a + 1..4 {
                    let d = distance(seen(look.channels[a], vision), seen(look.channels[b], vision));
                    assert!(d >= 0.06, "dark={dark} {name}: channels {a} and {b} are {d} apart in OKLab; the first four must stay distinct");
                }
            }
        }
        for a in 0..8 {
            for b in a + 1..8 {
                let d = distance(seen(look.channels[a], None), seen(look.channels[b], None));
                assert!(d >= 0.05, "dark={dark}: channels {a} and {b} are {d} apart in OKLab under normal vision");
            }
        }
        assert!(contrast(look.text, canvas) >= 7.0, "dark={dark}: primary text contrast {}", contrast(look.text, canvas));
        assert!(contrast(look.text_secondary, canvas) >= 4.5, "dark={dark}: secondary text contrast {}", contrast(look.text_secondary, canvas));
        assert!(contrast(look.text_tertiary, canvas) >= 3.0, "dark={dark}: tertiary text contrast {}", contrast(look.text_tertiary, canvas));
        assert!(look.hairline[3] < look.zero_line[3], "dark={dark}: the zero line must read over the hairlines");
    }
}
