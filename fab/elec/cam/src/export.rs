//! C7: plot a board through a resolved profile, validate the result, package it.
//!
//! Plotting never refills (fills come in) and never calls the language (the profile's tables
//! are already resolved). Cost: one pass over the board's items per layer (O(items)), then
//! the writers (O(objects)); the validator reads every file back and runs one polygon
//! exclusive-or per layer (O(n log n) in the vertices). Export work: not per frame.

use crate::error::CamError;
use crate::model::render;
use crate::package::package;
use crate::plot::build_model;
use crate::drill::read_excellon;
use crate::model::CamModel;
use std::sync::Arc;
use crate::gerber_read::read_gerber;
use crate::image::{DrillImage, GerberImage};
use crate::profile::{FileKind, OutFile, ResolvedProfile, Validation};
use crate::text::csv_rows;
use crate::validate::validate;
use fab_elec_board::{Board, ZoneFill};
use fab_elec_circuit::Circuit;

/// The files of the profile for this board, in the profile's order.
pub fn plot(board: &Board, circuit: &Circuit, fills: &[ZoneFill], profile: &ResolvedProfile) -> Result<Vec<OutFile>, CamError> {
    render(&build_model(board, circuit, fills), profile)
}

/// Reads `files` back and compares them with the board (`validate` against a fresh model).
pub fn validate_export(board: &Board, circuit: &Circuit, fills: &[ZoneFill], profile: &ResolvedProfile, files: &[OutFile]) -> Validation {
    validate(files, profile, &build_model(board, circuit, fills))
}

/// One file as read back by our own readers: what the proof view draws (never the model).
#[derive(Clone, PartialEq, Debug)]
pub enum ReadBack {
    Gerber(GerberImage),
    Drill(DrillImage),
    /// A CSV table (BOM or placement), rows of cells, the header first.
    Table(Vec<Vec<String>>),
    /// Text (the readme).
    Text(String),
}

/// What an export made.
#[derive(Clone, PartialEq, Debug)]
pub struct Export {
    pub files: Vec<OutFile>,
    /// Each file of the profile, read back, in the profile's order: `(file name, kind, image)`.
    /// A file that did not read has no entry (the validation says why).
    pub read_back: Vec<(String, FileKind, ReadBack)>,
    /// What the files were made from (the proof view's ghost: the design as the model draws it).
    pub model: Arc<CamModel>,
    /// The zip when the profile names one.
    pub zip: Option<OutFile>,
    pub validation: Validation,
}

/// Reads every file of the profile back with the readers.
pub fn read_back(files: &[OutFile], profile: &ResolvedProfile) -> Vec<(String, FileKind, ReadBack)> {
    let mut out = Vec::new();
    for spec in &profile.files {
        let Some(f) = files.iter().find(|f| f.name == spec.name) else { continue };
        let rb = match &spec.kind {
            FileKind::Gerber { .. } => read_gerber(&f.bytes).ok().map(ReadBack::Gerber),
            FileKind::Drill { .. } => read_excellon(&f.bytes).ok().map(ReadBack::Drill),
            FileKind::Bom => Some(ReadBack::Table(csv_rows(&String::from_utf8_lossy(&f.bytes), profile.bom.delimiter))),
            FileKind::Placement => Some(ReadBack::Table(csv_rows(&String::from_utf8_lossy(&f.bytes), profile.placement.delimiter))),
            FileKind::Readme => Some(ReadBack::Text(String::from_utf8_lossy(&f.bytes).into_owned())),
        };
        if let Some(rb) = rb {
            out.push((spec.name.clone(), spec.kind.clone(), rb));
        }
    }
    out
}

/// Plot, validate and package in one go; the zip is made only when the files validate.
pub fn export(board: &Board, circuit: &Circuit, fills: &[ZoneFill], profile: &ResolvedProfile) -> Result<Export, CamError> {
    let model = build_model(board, circuit, fills);
    let files = render(&model, profile)?;
    let validation = validate(&files, profile, &model);
    let zip = match (&profile.zip, validation.ok()) {
        (Some(name), true) => Some(package(&files, name)?),
        _ => None,
    };
    let read_back = read_back(&files, profile);
    Ok(Export { files, read_back, model: Arc::new(model), zip, validation })
}
