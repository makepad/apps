//! The manufacturing model the writers render and the validator compares against: what a board
//! (with its fills) comes to per layer, as objects with attributes, drills, poses and the BOM
//! references. `plot` (board input) builds it; everything here is plain data.

use crate::drill::{write_excellon, DrillOpts};
use crate::error::CamError;
use crate::gerber_write::{write_gerber, GerberOpts};
use crate::image::{DrillHole, FileAttr, GItem};
use crate::profile::{FileKind, GerberVariant, OutFile, ResolvedProfile};
use crate::text::{csv_cell};
use fab_elec_core::{Angle, LayerId, LayerKind, LayerSide, Side, P};
use std::collections::BTreeMap;

#[derive(Clone, PartialEq, Debug)]
pub struct Pose {
    pub refdes: String,
    pub at: P,
    pub rot: Angle,
    pub side: Side,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct CamModel {
    pub copper_layers: u8,
    pub layers: BTreeMap<LayerId, Vec<GItem>>,
    /// The fill lattice the pours were tiled by (the validator names tiles and checks seams on it);
    /// `None` for untiled pours.
    pub lattice: Option<fab_elec_board::FillLattice>,
    /// Drilled holes with their plating.
    pub holes: Vec<(bool, DrillHole)>,
    pub poses: Vec<Pose>,
    /// References the BOM must cover (every part that is not left out).
    pub bom_refs: Vec<String>,
}

/// The `TF.FileFunction` and `TF.FilePolarity` of a layer file (X2 file attributes).
pub fn gerber_file_attrs(layer: LayerId, copper_layers: u8) -> Vec<FileAttr> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let top = layer.side() == LayerSide::Front;
    let side = if top { "Top" } else { "Bot" };
    let (func, pol) = match layer.kind() {
        LayerKind::Copper => {
            let (n, pos) = match layer.0 {
                0 => (1u32, "Top"),
                127 => (copper_layers.max(2) as u32, "Bot"),
                k => (k as u32 + 1, "Inr"),
            };
            (vec!["Copper".to_string(), format!("L{n}"), pos.to_string()], "Positive")
        }
        LayerKind::Mask => (s(&["Soldermask", side]), "Negative"),
        LayerKind::Paste => (s(&["Paste", side]), "Positive"),
        LayerKind::Silk => (s(&["Legend", side]), "Positive"),
        LayerKind::Edge => (s(&["Profile", "NP"]), "Positive"),
        _ => (vec!["Other".to_string(), layer.name()], "Positive"),
    };
    vec![("FileFunction".into(), func), ("FilePolarity".into(), s(&[pol])), ("SameCoordinates".into(), s(&["Original"]))]
}

pub fn drill_file_attrs(plated: Option<bool>, copper_layers: u8) -> Vec<FileAttr> {
    let n = copper_layers.max(2).to_string();
    let func = match plated {
        Some(true) => vec!["Plated".into(), "1".into(), n, "PTH".into()],
        Some(false) => vec!["NonPlated".into(), "1".into(), n, "NPTH".into()],
        None => vec!["MixedPlating".into(), "1".into(), n],
    };
    vec![("FileFunction".into(), func)]
}

pub fn drill_opts(p: &ResolvedProfile) -> DrillOpts {
    DrillOpts { inch: p.drill.inch, zeros: p.drill.zeros, int_digits: p.drill.int_digits, frac_digits: p.drill.frac_digits }
}

pub fn csv_text(header: &[String], rows: impl Iterator<Item = Vec<String>>, delim: char) -> String {
    let line = |cells: &[String]| cells.iter().map(|c| csv_cell(c, delim)).collect::<Vec<_>>().join(&delim.to_string());
    let mut s = line(header);
    s.push('\n');
    for r in rows {
        s.push_str(&line(&r));
        s.push('\n');
    }
    s
}

/// Writes every file of the profile from the model.
pub fn render(model: &CamModel, profile: &ResolvedProfile) -> Result<Vec<OutFile>, CamError> {
    let mut out = Vec::with_capacity(profile.files.len());
    for f in &profile.files {
        let bytes = match &f.kind {
            FileKind::Gerber { layer, variant } => {
                let items = model.layers.get(layer).map(|v| v.as_slice()).unwrap_or(&[]);
                let x2 = *variant == GerberVariant::X2;
                let attrs = gerber_file_attrs(*layer, model.copper_layers);
                write_gerber(items, &attrs, &GerberOpts { decimals: profile.gerber.decimals, x2, ..GerberOpts::default() }).map_err(|e| match e {
                    CamError::Geometry(m) => CamError::Geometry(format!("{} (layer {}): {m}", f.name, layer.name())),
                    e => e,
                })?
            }
            FileKind::Drill { plated } => {
                let holes: Vec<DrillHole> = model.holes.iter().filter(|(p, _)| plated.map_or(true, |w| w == *p)).map(|(_, h)| h.clone()).collect();
                write_excellon(&holes, &drill_file_attrs(*plated, model.copper_layers), &drill_opts(profile))
            }
            FileKind::Bom => csv_text(&profile.bom.header, profile.bom.rows.iter().map(|r| r.cells.clone()), profile.bom.delimiter).into_bytes(),
            FileKind::Placement => csv_text(&profile.placement.header, profile.placement.rows.iter().map(|r| r.cells.clone()), profile.placement.delimiter).into_bytes(),
            FileKind::Readme => profile.readme.clone().into_bytes(),
        };
        out.push(OutFile { name: f.name.clone(), bytes });
    }
    Ok(out)
}
