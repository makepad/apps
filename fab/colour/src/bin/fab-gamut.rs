//! `fab-gamut`: how much colour filament sets print as a picture tile, from first guesses of
//! typical spool colours.
//!
//! ```text
//! fab-gamut [--step 0.02] [--layer 0.16] [--layers 4] [--slots 9]
//! ```
//! For CMYK, CMYW and CMYKW: the OKLab volume of the colours the tile's stacks mix (its first
//! two layers dithered, the rest the set's white or, without one, its second layer again), its
//! share of the sRGB cube's, and the volume each candidate filament adds loaded beside the set.
//! Then CMYKW filled to `--slots` filaments, each time with the candidate that adds the most.

use fab_colour::gamut::{candidates, report, spool};
use fab_colour::palette::Filament;
use fab_colour::tile::{Axis, Face, TileSpec};
use fab_colour::{FilamentSet, Rules};

fn arg(args: &[String], k: &str) -> Option<f64> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok())
}

fn set(names: &str) -> FilamentSet {
    FilamentSet::new(names, names.chars().filter_map(|c| spool(&c.to_string())).collect())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let step = arg(&args, "--step").unwrap_or(0.02) as f32;
    let layer = arg(&args, "--layer").unwrap_or(0.16);
    let layers = arg(&args, "--layers").unwrap_or(4.0) as usize;
    let slots = arg(&args, "--slots").unwrap_or(9.0) as usize;
    let spec = TileSpec { cell: 0.42, size: [1, 1], layers, layer, face: Face::Down, depth: 2.min(layers), rules: Rules { min_run: 2, switch: 0.0, lightness: 1.0 }, first_lines: Axis::X, background: [0.8; 3], viewing: 0.0 };
    let pool = candidates();
    println!("OKLab volume of a {layers}-layer tile's colours ({layer} mm layers, the first two dithered), cubes of {step}:");
    for names in ["CMYK", "CMYW", "CMYKW"] {
        let s = set(names);
        let extra: Vec<Filament> = pool.iter().filter(|f| !s.filaments.iter().any(|g| g.name == f.name)).cloned().collect();
        let r = report(&s, &spec.model(&s), &extra, step);
        let adds: Vec<String> = r.adds.iter().map(|(n, v)| format!("{n} +{v:.4} ({:+.0}%)", 100.0 * v / r.volume)).collect();
        println!("{names:6} {:.4} ({:.1}% of sRGB {:.4}); adding: {}", r.volume, 100.0 * r.volume / r.srgb, r.srgb, adds.join(", "));
    }
    let mut s = set("CMYKW");
    let mut line = format!("CMYKW {:.4}", report(&s, &spec.model(&s), &[], step).volume);
    while s.filaments.len() < slots {
        let extra: Vec<Filament> = pool.iter().filter(|f| !s.filaments.iter().any(|g| g.name == f.name)).cloned().collect();
        let model = spec.model(&s);
        let r = report(&s, &model, &extra, step);
        let Some((best, add)) = r.adds.iter().cloned().max_by(|a, b| a.1.total_cmp(&b.1)) else { break };
        s.filaments.push(extra.iter().find(|f| f.name == best).unwrap().clone());
        line += &format!(", + {best} {:.4} (+{add:.4})", r.volume + add);
    }
    println!("filled to {slots} slots, each the largest gain: {line}");
}
