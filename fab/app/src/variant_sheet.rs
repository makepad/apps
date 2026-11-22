//! `fab.variants` in the app: each variant drawn by the review renderer from the job's one camera
//! (with a fit, the survey of the plate as it is first, then the variants from the view it finds),
//! the captures read on the task pool ([`fab_ai::variants::answer`]) and laid out as sheets, the
//! variants across and their views down, each cell written with its name and how it compares.

use crate::model_host::{captured_views, HostAction};
use crate::review::{self, Frames, Overlay, PreparedReview};
use fab_ai::compare::Fit;
use fab_ai::variants::{self, VariantJob};
use makepad_widgets::makepad_platform::thread::{Class, TaskPool};
use makepad_widgets::*;
use std::sync::{Arc, Mutex};

/// A `fab.variants` call between its captures: the round out and what came back of it.
pub struct VariantRun {
    job: Arc<VariantJob>,
    /// The view the reference is seen from, once the survey found it.
    fit: Option<Fit>,
    /// The captures the round asked for.
    expect: usize,
    frames: Vec<Frames>,
}

/// A finished round: the survey (to fit), or every variant drawn (to answer).
pub struct Round {
    job: Arc<VariantJob>,
    fit: Option<Fit>,
    frames: Vec<Frames>,
    /// No round comes after it.
    pub last: bool,
}

impl VariantRun {
    pub fn new(job: Arc<VariantJob>, expect: usize) -> VariantRun {
        VariantRun { job, fit: None, expect, frames: Vec::new() }
    }

    /// One of the round's captures: the round, once its last is in.
    pub fn captured(&mut self, frames: Frames) -> Option<Round> {
        self.frames.push(frames);
        (self.frames.len() == self.expect).then(|| Round {
            job: self.job.clone(),
            fit: self.fit.clone(),
            frames: std::mem::take(&mut self.frames),
            last: self.fit.is_some() || self.job.survey().is_none(),
        })
    }

    /// Where its round is: whether it is the survey of the plate (else the variants), the
    /// captures back, and of how many.
    pub fn progress(&self) -> (bool, usize, usize) {
        (self.fit.is_none() && self.job.survey().is_some(), self.frames.len(), self.expect)
    }

    /// The survey found the view: the variants' captures follow.
    pub fn fitted(&mut self, fit: Fit, captures: usize) {
        self.fit = Some(fit);
        self.expect = captures;
    }
}

/// On the model worker: what a call's first round draws, the survey of the plate as it is when
/// it fits the reference's view, else each variant.
pub fn first(job: &VariantJob) -> Result<Vec<PreparedReview>, String> {
    match job.survey() {
        Some(survey) => Ok(vec![review::prepare(&survey.ev, &survey.views, &survey.objects, None, &survey.camera, &[])?]),
        None => drawn(job, None),
    }
}

/// Each variant from its rows' views, with the job's camera, each view one cell of a sheet of four
/// (the variants share the sheets).
fn drawn(job: &VariantJob, fit: Option<&Fit>) -> Result<Vec<PreparedReview>, String> {
    let rows = job.rows(fit);
    job.built.iter().map(|v| review::prepare_cells(&v.ev, &rows, &job.objects, &job.camera)).collect()
}

/// A finished round, read on the task pool at low priority: the survey searched for the view
/// ([`HostAction::VariantsFitted`]), or the variants answered with their sheets
/// ([`HostAction::Pictured`]).
pub fn read(pool: &TaskPool, id: u64, round: Round) {
    let work = move || {
        if round.fit.is_none() && round.job.survey().is_some() {
            let found = variants::fit(&round.job, &captured_views(&round.frames[0])).and_then(|fit| Ok((drawn(&round.job, Some(&fit))?, fit)));
            match found {
                Ok((prepared, fit)) => Cx::post_action(HostAction::VariantsFitted { id, round: Mutex::new(Some((fit, prepared))) }),
                Err(e) => Cx::post_action(HostAction::Pictured { id, result: Err(e) }),
            }
            return;
        }
        Cx::post_action(HostAction::Pictured { id, result: answer(&round) });
    };
    if let Err(e) = pool.submit_named(Class::Background, "fab variants", work).map(|task| task.detach()) {
        Cx::post_action(HostAction::Pictured { id, result: Err(format!("the variants could not be read: {e}")) });
    }
}

/// The variants' answer and its sheets: the cells in the answer's order, two variants to a sheet
/// with their views down, or four to a sheet with one view.
fn answer(round: &Round) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    let drawn: Vec<_> = round.frames.iter().map(captured_views).collect();
    let answer = variants::answer(&round.job, round.fit.as_ref(), &drawn)?;
    let rows = round.job.rows(round.fit.as_ref());
    let side = round.frames[0].side;
    let mut sheet = Frames { side, frames: Vec::new(), labels: Vec::new(), overlays: Vec::new(), cameras: Vec::new(), tiles: Vec::new() };
    let words = |row: usize, (_, top, name): &(usize, Vec<String>, String)| if row == 0 { top.clone() } else { vec![name.clone()] };
    let mut names: Vec<Vec<&str>> = Vec::new();
    if rows.len() == 1 {
        for group in answer.order.chunks(4) {
            for v in group {
                cell(&mut sheet, &round.frames[v.0], 0, &words(0, v));
            }
            names.push(group.iter().map(|v| v.2.as_str()).collect());
        }
    } else {
        for pair in answer.order.chunks(2) {
            for row in 0..rows.len() {
                for v in pair {
                    cell(&mut sheet, &round.frames[v.0], row, &words(row, v));
                }
                if pair.len() == 1 && row + 1 < rows.len() {
                    // the right of a lone variant's row stays empty, so its next view starts a row
                    let blank: Vec<u8> = [28u8, 23, 20, 255].repeat(side * side);
                    sheet.frames.push(blank);
                    sheet.overlays.push(Overlay::default());
                    sheet.cameras.push(round.frames[pair[0].0].cameras[row].clone());
                }
            }
            names.push(pair.iter().map(|v| v.2.as_str()).collect());
        }
    }
    sheet.labels = names
        .iter()
        .map(|n| match rows.len() {
            1 => format!("fab.variants: {} (left to right, top to bottom) from {}", n.join(", "), rows[0]),
            _ => format!("fab.variants: {} across, {} down", n.join(" | "), rows.join(", ")),
        })
        .collect();
    let sheets = review::encode_sheets(&sheet)?;
    Ok((answer.text, sheets.pictures))
}

/// One variant's view on the sheet, its words (the top row's lines, or its name) below one another
/// from its top left.
fn cell(sheet: &mut Frames, frames: &Frames, row: usize, words: &[String]) {
    let mut overlay = Overlay { bar: frames.overlays.get(row).and_then(|o| o.bar.clone()), ..Default::default() };
    for (k, line) in words.iter().enumerate() {
        overlay.marks.push((10, 14 + (k * sheet.side / 10) as i32, line.clone()));
    }
    sheet.frames.push(frames.frames[row].clone());
    sheet.overlays.push(overlay);
    sheet.cameras.push(frames.cameras[row].clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_ai::camera::{named_view, Frame};

    /// A session whose one block is `w` wide, and its fab.variants answer to `call`.
    fn job(call: &str) -> Arc<VariantJob> {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
        let source = format!("{}let w = param(20mm, min: 5mm, max: 60mm)\nlet b = m.box({{size: [w 20mm 20mm]}})\nobject(b)\n", fab_model::program::EMPTY);
        let (reply, _) = session.set_source("t.cad.splash", &source);
        assert!(reply.ok, "{}", reply.text);
        let r = session.call(variants::TOOL, &makepad_model::json::parse(call.as_bytes()).unwrap(), None);
        assert!(r.ok, "{}", r.text);
        let Some(fab_ai::RenderRequest::Variants(job)) = r.render else { panic!("fab.variants asks for views") };
        job
    }

    /// What the review hands back for a capture of `views`: a flat colour a view, `side` square.
    fn frames(job: &VariantJob, views: &[String], side: usize, shade: u8) -> Frames {
        let cameras = views.iter().map(|v| {
            let view = named_view(v).unwrap();
            (v.clone(), Frame::of_render([[0.0; 3], [0.1; 3]], std::slice::from_ref(&view), &job.camera).camera(&view, side))
        }).collect();
        Frames { side, frames: views.iter().map(|_| [shade, shade, shade, 255].repeat(side * side)).collect(), labels: vec![String::new(); views.len().div_ceil(4)], overlays: views.iter().map(|_| Overlay::default()).collect(), cameras, tiles: views.iter().map(|_| Vec::new()).collect() }
    }

    /// Three variants from two views: two sheets, two variants across and their views down (the
    /// last one alone on its sheet), within the picture budget, and the answer names each.
    #[test]
    fn variants_come_two_to_a_sheet_with_their_views_down() {
        let job = job(r#"{"variants": [{"name": "a", "edits": "set(w, 20mm)"}, {"name": "b", "edits": "set(w, 30mm)"}, {"name": "c", "edits": "set(w, 40mm)"}], "views": ["front", "right"]}"#);
        let rows = job.rows(None);
        let mut run = VariantRun::new(job.clone(), 3);
        assert!(run.captured(frames(&job, &rows, 96, 60)).is_none() && run.captured(frames(&job, &rows, 96, 90)).is_none());
        let round = run.captured(frames(&job, &rows, 96, 120)).expect("the round's last capture");
        assert!(round.last, "no fit: one round");
        let (text, pictures) = answer(&round).unwrap();
        assert!(text.contains("apply:") && ["\"a\"", "\"b\"", "\"c\""].iter().all(|n| text.contains(n)), "{text}");
        assert_eq!(pictures.len(), 2, "{:?}", pictures.iter().map(|(l, _)| l).collect::<Vec<_>>());
        assert!(pictures[0].0.starts_with("fab.variants: a | b across, front, right down"), "{}", pictures[0].0);
        let sheet = makepad_ai_services::pictures::decode_png(&pictures[0].1).unwrap();
        assert_eq!(sheet.width, sheet.height, "two across, two down");
        let bytes: usize = pictures.iter().map(|(_, png)| makepad_ai_services::pictures::base64_len(png.len())).sum();
        assert!(bytes <= makepad_ai_services::wire::MAX_RESULT_IMAGE_BYTES);
    }

    /// Each variant's cell is written with its rank and its name, which read back from the sheet
    /// as written, whatever characters the name has.
    #[test]
    fn a_variant_s_cell_reads_back_its_rank_and_name() {
        let job = job(r#"{"variants": [{"name": "Tall (A1)", "edits": "set(w, 20mm)"}, {"name": "wide-b: 40mm?", "edits": "set(w, 40mm)"}], "views": ["front"]}"#);
        let rows = job.rows(None);
        let side = 256;
        let mut run = VariantRun::new(job.clone(), 2);
        assert!(run.captured(frames(&job, &rows, side, 60)).is_none());
        let round = run.captured(frames(&job, &rows, side, 90)).expect("the round's last capture");
        let (_, pictures) = answer(&round).unwrap();
        let sheet = makepad_model::RgbaImage::from_png(&pictures[0].1, &makepad_model::Limits::default()).unwrap();
        // two across, one down; each cell's first line where `cell` marks it, in the cell's pixels
        let cell = sheet.width as i32 / 2;
        assert_eq!(sheet.height as i32, cell, "{}", pictures[0].0);
        let (x, y) = (10 * cell / side as i32, 14 * cell / side as i32);
        for (k, words) in ["1 Tall (A1)", "2 wide-b: 40mm?"].into_iter().enumerate() {
            assert_eq!(review::read_words(&sheet, (k as i32 * cell, 0), x + 7, y - 7, words.len()), words, "cell {k}");
        }
    }

    /// Six variants from one view, each drawn as the review draws it: two sheets (four and two
    /// variants), each no more than the chat takes a side.
    #[test]
    fn six_variants_come_back_within_the_chat_s_picture_side() {
        let variants: Vec<String> = (0..6).map(|i| format!(r#"{{"name": "v{i}", "edits": "set(w, {}mm)"}}"#, 10 + 5 * i)).collect();
        let job = job(&format!(r#"{{"variants": [{}], "views": ["front"]}}"#, variants.join(", ")));
        let rows = job.rows(None);
        let side = drawn(&job, None).unwrap()[0].side;
        let mut run = VariantRun::new(job.clone(), 6);
        let rounds: Vec<Option<Round>> = (0..6).map(|i| run.captured(frames(&job, &rows, side, 40 + 30 * i as u8))).collect();
        let round = rounds.into_iter().last().flatten().expect("the round's last capture");
        let (_, pictures) = answer(&round).unwrap();
        assert_eq!(pictures.len(), 2, "four variants to a sheet");
        let sizes = crate::chat::as_the_provider_takes(pictures).unwrap();
        assert!(sizes.iter().all(|s| s[0].max(s[1]) <= makepad_ai_hub::providers::provider::MAX_TOOL_IMAGE_SIDE), "{side} px views: {sizes:?}");
    }

    /// With a fit the survey of the plate as it is goes first, then the variants from its view.
    #[test]
    fn a_fit_surveys_first_then_draws_the_variants_from_its_view() {
        let picture = std::env::temp_dir().join(format!("fab-variant-sheet-{}.png", std::process::id()));
        let pixels = (0..64 * 64).flat_map(|i| if (16..48).contains(&(i % 64)) && (8..56).contains(&(i / 64)) { [90, 60, 40, 255] } else { [250, 250, 250, 255] }).collect();
        std::fs::write(&picture, makepad_model::RgbaImage { width: 64, height: 64, pixels }.to_png(&makepad_model::Limits::default()).unwrap()).unwrap();
        let job = job(&format!(r#"{{"variants": [{{"name": "a", "edits": "set(w, 20mm)"}}, {{"name": "b", "edits": "set(w, 30mm)"}}], "compare": {{"picture": {:?}, "fit": true}}}}"#, picture.display().to_string()));
        let survey = job.survey().expect("a fit surveys the plate as it is");
        assert_eq!(first(&job).unwrap().len(), 1, "one capture: the survey");
        let mut run = VariantRun::new(job.clone(), 1);
        let round = run.captured(frames(&job, &survey.views, 64, 50)).expect("the survey");
        assert!(!round.last, "the variants come after it");
        let fit = Fit { yaw: 30.0, pitch: -10.0, iou: 0.9, runner_up: None, missed: Vec::new() };
        assert_eq!(job.rows(Some(&fit)), ["30,-10"]);
        run.fitted(fit, 2);
        let rows = job.rows(run.fit.as_ref());
        assert!(run.captured(frames(&job, &rows, 64, 70)).is_none());
        assert!(run.captured(frames(&job, &rows, 64, 70)).expect("the variants").last);
        let _ = std::fs::remove_file(&picture);
    }
}
