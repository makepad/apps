//! `fab-tile`: a picture as a colour tile in filament sets, seen four ways, with how far it looks
//! from the picture.
//!
//! ```text
//! fab-tile PICTURE --set C,M,Y,K [--set C,M,Y,K,W ..] [--width 100] [--nozzle 0.4] [--layer 0.16]
//!          [--layers 4] [--min-run 2] [--smooth 0.3] [--lightness 2] [--viewing 300] [--inks] [--out DIR]
//! ```
//! Each set is filament names or letters (C M Y K W R O G B V), first guesses of typical spools.
//! For each: `DIR/tile-<set>.png` (the picture as the cells take it, the face cell by cell, the
//! face seen from 30 cm and its ΔE map against the picture seen so: black 0, red 10, yellow 20,
//! white 30), and the ΔE seen from 30 cm (mean and 95th percentile): the whole, the gamut's part
//! and the dither's; then each dithered layer's runs per filament.

use fab_colour::gamut::spool;
use fab_colour::tile::{self, Axis, Face, TileSpec};
use fab_colour::{view, FilamentSet, Image, Rules};

fn value<'a>(args: &'a [String], k: &str) -> Option<&'a str> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn num(args: &[String], k: &str, default: f64) -> Result<f64, String> {
    value(args, k).map_or(Ok(default), |v| v.parse().map_err(|_| format!("{k} {v}: not a number")))
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let picture = args.first().filter(|a| !a.starts_with("--")).ok_or("usage: fab-tile PICTURE --set C,M,Y,K [--set ..] [--width 100] [--nozzle 0.4] [--layer 0.16] [--layers 4] [--min-run 2] [--smooth 0.3] [--out DIR]")?;
    let image = Image::decode(&std::fs::read(picture).map_err(|e| format!("{picture}: {e}"))?)?;
    let width = num(&args, "--width", 100.0)?;
    let nozzle = num(&args, "--nozzle", 0.4)?;
    let layer = num(&args, "--layer", 0.16)?;
    let layers = num(&args, "--layers", 4.0)? as usize;
    let min_run = num(&args, "--min-run", 2.0)? as usize;
    let smooth = num(&args, "--smooth", 0.3)?;
    let lightness = num(&args, "--lightness", 2.0)? as f32;
    let viewing = num(&args, "--viewing", view::VIEWING)?;
    let out = value(&args, "--out").unwrap_or(".");
    // the bead and its spacing as a picture part prints them
    let cell = nozzle * 1.05 - layer * (1.0 - std::f64::consts::PI / 4.0);
    let nx = (width / cell).floor() as usize;
    let ny = ((width * image.height as f64 / image.width as f64) / cell).floor() as usize;
    let sets: Vec<&str> = args.iter().enumerate().filter(|(i, a)| *i > 0 && args[i - 1] == "--set" && !a.is_empty()).map(|(_, a)| a.as_str()).collect();
    if sets.is_empty() {
        return Err("give at least one --set".into());
    }
    for names in sets {
        let filaments = names.split(',').map(|n| spool(n.trim()).ok_or(format!("no typical spool '{n}'"))).collect::<Result<Vec<_>, _>>()?;
        let label: String = names.split(',').map(str::trim).collect();
        let set = FilamentSet::new(&label, filaments);
        let spec = TileSpec { cell, size: [nx, ny], layers, layer, face: Face::Down, depth: 2.min(layers), rules: Rules { min_run, switch: (smooth * 0.1) as f32, lightness }, first_lines: Axis::X, background: [0.8; 3], viewing };
        let palette = fab_colour::Palette::new(&set, &spec.model(&set));
        let (floor, top) = fab_colour::gamut::Gamut::of(&palette).range();
        if args.iter().any(|a| a == "--inks") {
            let mut inks: Vec<_> = palette.inks.iter().collect();
            inks.sort_by(|a, b| a.lab[0].total_cmp(&b.lab[0]));
            for ink in inks {
                let names: Vec<&str> = ink.stack[..palette.depth].iter().map(|&f| set.filaments[f as usize].name.as_str()).collect();
                println!("{label} ink {:24} L {:.3} a {:+.3} b {:+.3}", names.join("/"), ink.lab[0], ink.lab[1], ink.lab[2]);
            }
        }
        let started = std::time::Instant::now();
        let t = tile::make(&image, &set, &spec);
        let made = started.elapsed().as_secs_f64();
        let c = view::compare(&Image { width: nx, height: ny, pixels: image.cells(nx, ny) }, &t.as_seen(Face::Down, &t.wanted), &t.face(Face::Down), cell, 3);
        let file = format!("{out}/tile-{label}.png");
        std::fs::write(&file, c.image.png()?).map_err(|e| format!("{file}: {e}"))?;
        let runs: Vec<String> = (0..spec.depth)
            .map(|l| {
                let r = t.runs(l, spec.lines(l), set.filaments.len());
                format!("layer {}: {}", l + 1, set.filaments.iter().zip(&r).map(|(f, n)| format!("{} {n}", f.name)).collect::<Vec<_>>().join(", "))
            })
            .collect();
        // the mean colour (in linear light, as the cells mix) the dither aimed at and the face's
        let mean = |cells: &[[f32; 3]]| fab_colour::oklab::oklab(std::array::from_fn(|k| cells.iter().map(|c| c[k] as f64).sum::<f64>() as f32 / cells.len().max(1) as f32));
        let (aim, got) = (mean(&t.wanted), mean(&t.shown));
        println!(
            "{label}: {nx} x {ny} cells of {cell:.3} mm in {made:.2} s, greys L {floor:.2} to {top:.2}; ΔE at 30 cm (mean / p95): {:.1} / {:.1} (gamut {:.1} / {:.1}, dither {:.1} / {:.1}) -> {file}\n{label} mean aimed at L {:.3} a {:+.3} b {:+.3}, face L {:.3} a {:+.3} b {:+.3}\n{label} runs {}",
            c.total.0, c.total.1, c.gamut.0, c.gamut.1, c.dither.0, c.dither.1, aim[0], aim[1], aim[2], got[0], got[1], got[2], runs.join("; ")
        );
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("fab-tile: {e}");
        std::process::exit(1);
    }
}
