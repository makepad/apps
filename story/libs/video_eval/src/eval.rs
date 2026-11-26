//! One case end to end: generate (optional), load the cut, measure it,
//! take its pictures, judge them, and fold everything into a row.

use crate::bench::Case;
use crate::contract::{expand_command, GenRequest, GenResult};
use crate::judge::{self, Rubric, Verdict};
use crate::metrics::{self, Check, CutFacts, Exposure, InspectSummary, MotionStats, Timing};
use crate::tools::{self, CutTools};
use makepad_micro_serde::*;
use makepad_story_cut::CutDoc;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Everything measured for one generated video.
#[derive(Clone, Debug, Default, SerJson, DeJson)]
pub struct CaseResult {
    pub case: String,
    pub category: String,
    pub format: String,
    /// 0-based repeat of the case in this run.
    pub repeat: u32,
    /// The case's directory, relative to the run directory.
    pub dir: String,
    pub success: bool,
    pub error: Option<String>,
    pub iterations: Option<u32>,
    pub wall_s: Option<f64>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub model_used: Option<String>,
    pub cut: Option<String>,
    /// `CutDoc::load` diagnostics when the cut does not load.
    pub load_errors: Vec<String>,
    pub facts: Option<CutFacts>,
    pub inspect: Option<InspectSummary>,
    pub inspect_error: Option<String>,
    pub render_s: Option<f64>,
    pub render_error: Option<String>,
    pub motion: MotionStats,
    /// The contact sheet's picture numbers (clipping, black, mean luma).
    pub exposure: Option<Exposure>,
    pub conformance: Vec<Check>,
    /// Pictures beside the case: `(label, file name)`.
    pub images: Vec<Picture>,
    pub picture_errors: Vec<String>,
    pub verdict: Option<Verdict>,
    pub judge_error: Option<String>,
    /// 0..100, per the rubric's formula.
    pub overall: f64,
    /// The separate visual-richness score (1..5; never part of `overall`
    /// or the visual mean, so runs from before it stay comparable).
    pub richness: Option<f64>,
    pub richness_note: Option<String>,
    pub richness_error: Option<String>,
    /// Acceptance-set cases only: the checklist (measured items and the
    /// judge's items), all of which must pass.
    pub acceptance: Option<Vec<Check>>,
    pub acceptance_error: Option<String>,
}

#[derive(Clone, Debug, Default, SerJson, DeJson)]
pub struct Picture {
    pub label: String,
    pub file: String,
}

impl CaseResult {
    pub fn judged(&self) -> bool {
        self.verdict.is_some()
    }
    pub fn failed_checks(&self) -> usize {
        self.conformance.iter().filter(|c| !c.pass).count()
    }
}

/// What the harness knows of a generation before measuring it.
#[derive(Default)]
pub struct GenInfo {
    pub result: GenResult,
    pub wall_s: Option<f64>,
    pub error: Option<String>,
}

pub struct Ctx {
    pub tools: Option<CutTools>,
    pub judge: Option<String>,
    pub rubric: Option<Rubric>,
    pub judge_timeout: Duration,
    pub render: bool,
    pub strip_count: u32,
}

/// The rubric's overall score (see the rubric's "Overall").
pub fn overall(r: &CaseResult) -> f64 {
    if !r.success || r.facts.is_none() {
        return 0.0;
    }
    let visual = r.verdict.as_ref().map_or(60.0, |v| (v.mean - 1.0) / 4.0 * 100.0);
    let (errors, zone) = r.inspect.as_ref().map_or((0, 0), |i| (i.errors, i.safe_zone + i.overflow));
    let penalty = 5.0 * errors as f64 + 2.0 * zone as f64 + 5.0 * r.failed_checks() as f64;
    (visual - penalty).clamp(0.0, 100.0)
}

/// Run the generator for one case into `dir`, from the working directory
/// `cwd` (the checkout: Stage finds its resources relative to it).
#[allow(clippy::too_many_arguments)]
pub fn generate(template: &str, case: &Case, model: &str, max_iterations: u32, label: &str, dir: &Path, cwd: &Path, timeout: Duration, song: Option<&(PathBuf, PathBuf)>) -> GenInfo {
    if case.song && song.is_none() {
        return GenInfo { error: Some(format!("{} is made from a song: pass --song FILE --lyrics FILE", case.id)), ..Default::default() };
    }
    let dir_s = dir.display().to_string();
    // Fresh, empty per-case state folders for generators that keep any
    // (see `--generator stage`).
    // A rerun into the same case folder starts empty too.
    for state in [".drafts", ".boards"] {
        let state = dir.join(state);
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::create_dir_all(&state);
    }
    let mut req = GenRequest::new(case, model, max_iterations, &dir_s, label);
    if case.song {
        if let Some((audio, lyrics)) = song {
            req.song = Some(audio.display().to_string());
            req.lyrics = Some(lyrics.display().to_string());
        }
    }
    let request_path = dir.join("request.json");
    if let Err(e) = std::fs::write(&request_path, req.serialize_json_pretty()) {
        return GenInfo { error: Some(format!("request.json: {e}")), ..Default::default() };
    }
    let argv = match expand_command(
        template,
        &[("request", &request_path.display().to_string()), ("out", &dir_s), ("result", &req.result_path), ("model", model)],
    ) {
        Ok(argv) => argv,
        Err(e) => return GenInfo { error: Some(e), ..Default::default() },
    };
    let outcome = match tools::run(&argv, cwd, timeout, Some(&dir.join("generator.log"))) {
        Ok(o) => o,
        Err(e) => return GenInfo { error: Some(e), ..Default::default() },
    };
    let wall_s = Some(outcome.seconds);
    match std::fs::read_to_string(&req.result_path).map_err(|e| e.to_string()).and_then(|t| GenResult::parse(&t)) {
        Ok(result) => {
            let error = (!result.success).then(|| result.error.clone().unwrap_or_else(|| outcome.reason()));
            GenInfo { result, wall_s, error }
        }
        Err(e) => GenInfo { wall_s, error: Some(format!("no result.json ({e}); generator {}", outcome.reason())), ..Default::default() },
    }
}

/// A path from `result.json`: absolute, relative to the case directory,
/// or (as a generator that echoed a relative `out_dir` writes it) relative
/// to the working directory.
fn resolve(dir: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        return path;
    }
    let beside = dir.join(&path);
    if !beside.exists() && path.exists() {
        // Absolute, since the tools run from the case directory.
        return std::fs::canonicalize(&path).unwrap_or(path);
    }
    beside
}

/// The Motion documents the cut actually plays: the `.splash` files
/// `media.txt` stages beside it for asset ids (the played revisions; a
/// generator's own `motionN.splash` copies can be stale).
pub fn played_motion_docs(edit: &Path) -> Vec<PathBuf> {
    let list = edit.with_file_name("media.txt");
    let base = list.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::read_to_string(&list)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.contains(':'))
        .filter_map(|l| l.split_once('=').map(|(_, f)| base.join(f.trim())))
        .filter(|p| p.extension().is_some_and(|x| x == "splash"))
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|s| is_motion(&s)))
        .collect()
}

/// Whether a document's text is a Motion document: its `Motion{` begins a line
/// (after its `use` lines and `let`s).
fn is_motion(text: &str) -> bool {
    text.lines().any(|l| l.starts_with("Motion"))
}

/// Motion documents beside the cut when the generator did not list them:
/// every `.splash` in `dir` whose source is a `Motion{...}`.
fn find_motion_docs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "splash"))
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|s| is_motion(&s)))
        .collect();
    out.sort();
    out
}

/// Measure, picture and judge one generated (or given) edit.
pub fn score(ctx: &Ctx, case: &Case, run_dir: &Path, dir: &Path, repeat: u32, gen: GenInfo) -> CaseResult {
    let mut r = CaseResult {
        case: case.id.clone(),
        category: case.category.clone(),
        format: format!("{} {}x{}", case.aspect_label(), case.width, case.height),
        repeat,
        dir: dir.strip_prefix(run_dir).unwrap_or(dir).display().to_string(),
        success: gen.error.is_none() && gen.result.success,
        error: gen.error.clone(),
        iterations: gen.result.iterations,
        wall_s: gen.wall_s,
        tokens_in: gen.result.tokens_in,
        tokens_out: gen.result.tokens_out,
        model_used: gen.result.model_used.clone(),
        ..Default::default()
    };
    let Some(edit_rel) = gen.result.cut.clone() else {
        if r.success {
            r.success = false;
            r.error = Some("the generator reported success but named no cut".into());
        }
        return r;
    };
    let edit_path = resolve(dir, &edit_rel);
    r.cut = Some(edit_path.display().to_string());
    let source = match std::fs::read_to_string(&edit_path) {
        Ok(s) => s,
        Err(e) => {
            r.success = false;
            r.error = Some(format!("{}: {e}", edit_path.display()));
            return r;
        }
    };
    let doc = match CutDoc::load_named(&source, &edit_path.display().to_string()) {
        Ok(doc) => doc,
        Err(diags) => {
            r.success = false;
            r.load_errors = diags.iter().take(10).map(|d| d.to_string()).collect();
            r.error = Some(format!("the cut does not load ({} diagnostics)", diags.len()));
            return r;
        }
    };
    let facts = CutFacts::of(&doc);
    // The played revisions first (media.txt), then the generator's list.
    let played = played_motion_docs(&edit_path);
    let motion_paths: Vec<PathBuf> = match &gen.result.motions {
        _ if !played.is_empty() => played,
        Some(list) if !list.is_empty() => list.iter().map(|m| resolve(dir, &m.path)).collect(),
        _ => find_motion_docs(dir),
    };
    // The tools resolve library media through `media.txt` beside the cut.
    // A generator that wrote none gets one with the motion documents it
    // listed (other media stay unresolved; the pictures then say which).
    let media_txt = edit_path.with_file_name("media.txt");
    if !media_txt.exists() {
        let lines: Vec<String> = gen
            .result
            .motions
            .iter()
            .flatten()
            .filter_map(|m| m.asset.as_ref().map(|id| format!("{id}={}", resolve(dir, &m.path).display())))
            .collect();
        if !lines.is_empty() {
            let _ = std::fs::write(&media_txt, format!("# written by makepad-video-eval: the generator's motion documents\n{}\n", lines.join("\n")));
        }
    }
    let sources: Vec<(String, String)> =
        motion_paths.iter().filter_map(|p| std::fs::read_to_string(p).ok().map(|s| (p.display().to_string(), s))).collect();
    r.motion = metrics::motion_stats(&sources, facts.duration);
    r.conformance = metrics::conformance(case, &facts, &r.motion);
    r.facts = Some(facts.clone());

    if let Some(tools) = &ctx.tools {
        measure_with_tools(ctx, tools, case, dir, &edit_path, &motion_paths, &facts, &mut r);
    }
    if case.set == "acceptance" {
        r.acceptance = Some(if case.song {
            lyric_measured(case, &facts, r.wall_s)
        } else {
            acceptance_measured(&edit_path, &motion_paths, &facts, r.wall_s)
        });
    }
    if let (Some(judge), Some(rubric)) = (&ctx.judge, &ctx.rubric) {
        judge_case(ctx, judge, rubric, case, dir, &mut r);
        if rubric.richness.is_some() {
            judge_richness(ctx, judge, rubric, case, dir, &mut r);
        }
        if case.set == "acceptance" && case.song {
            judge_lyric_film(ctx, judge, rubric, case, dir, &edit_path, &mut r);
        } else if case.set == "acceptance" {
            judge_acceptance(ctx, judge, rubric, case, dir, &edit_path, &motion_paths, &mut r);
        }
    }
    r.overall = overall(&r);
    r
}

/// The nested cuts the cut's `media.txt` stages (`edit:<id>=<file>`).
pub fn nested_edit_files(edit: &Path) -> Vec<PathBuf> {
    let list = edit.with_file_name("media.txt");
    let base = list.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::read_to_string(&list)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().strip_prefix("edit:"))
        .filter_map(|l| l.split_once('='))
        .map(|(_, f)| base.join(f.trim()))
        .collect()
}

/// A time budget "well under" Claude Code's 45 min for the same job.
pub const ACCEPTANCE_BUDGET_S: f64 = 30.0 * 60.0;

/// The showreel inside an acceptance result, with its time structure: a
/// nested cut (staged in `media.txt`) or a Motion document, 16:9 and
/// about 15 s (else the first 16:9 one).
pub fn find_reel(edit: &Path, motions: &[PathBuf]) -> Option<(PathBuf, Timing)> {
    let mut found: Vec<(PathBuf, Timing)> = Vec::new();
    for f in nested_edit_files(edit) {
        let Ok(source) = std::fs::read_to_string(&f) else { continue };
        let Ok(doc) = CutDoc::load_named(&source, &f.display().to_string()) else { continue };
        let mut starts: Vec<(String, f64, f64)> = doc
            .nodes
            .iter()
            .filter_map(|n| n.span.map(|s| (n.id.clone(), s.start, s.start + s.length)))
            .chain(doc.graphics.iter().filter_map(|g| g.span.map(|s| (g.id.clone(), s.start, s.start + s.length))))
            .collect();
        starts.sort_by(|a, b| a.1.total_cmp(&b.1));
        let chapters = starts
            .into_iter()
            .map(|(name, start, end)| metrics::Chapter { name, start, end, texts: 0, inner: Vec::new(), words: None })
            .collect();
        let timing = Timing {
            bpm: Some(doc.bpm).filter(|b| *b > 0.0),
            duration: Some(doc.duration),
            size: Some((doc.output.width as f64, doc.output.height as f64)),
            chapters,
        };
        found.push((f, timing));
    }
    for m in motions {
        if let Ok(source) = std::fs::read_to_string(m) {
            found.push((m.clone(), metrics::scan_timing(&source)));
        }
    }
    let wide = |t: &Timing| t.size.is_some_and(|(w, h)| (w * 9.0 - h * 16.0).abs() < 1.0);
    let length = |t: &Timing| t.duration.is_some_and(|d| (d - 15.0).abs() <= 1.5);
    // A nested reel edit often holds one Graphic playing the reel's Motion
    // document: the document with the most chapters is the one whose cuts
    // are the reel's (ties keep the earlier, the nested cut first).
    let best = |pred: &dyn Fn(&Timing) -> bool| {
        found.iter().enumerate().filter(|(_, (_, t))| pred(t)).max_by(|a, b| a.1 .1.chapters.len().cmp(&b.1 .1.chapters.len()).then(b.0.cmp(&a.0))).map(|(i, _)| i)
    };
    let pick = best(&|t| wide(t) && length(t)).or_else(|| best(&|t| wide(t)))?;
    let (path, mut timing) = found.swap_remove(pick);
    // The document may leave the tempo to the cut that plays it.
    if timing.bpm.is_none() {
        timing.bpm = found.iter().find_map(|(_, t)| t.bpm);
    }
    Some((path, timing))
}

/// Cuts are on the beat within 0.05 s (3 frames at 60 fps).
pub const BEAT_TOLERANCE_S: f64 = 0.05;

/// The measured half of the acceptance checklist: the showreel exists as
/// its own 16:9 ~15 s document, its chapters and their cut times against
/// the beat grid (from the document, not the pixels), music, and the time
/// budget.
pub fn acceptance_measured(edit: &Path, motions: &[PathBuf], facts: &CutFacts, wall_s: Option<f64>) -> Vec<Check> {
    let reel = find_reel(edit, motions);
    let nested_audio: u32 = nested_edit_files(edit)
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok().and_then(|s| CutDoc::load_named(&s, &f.display().to_string()).ok()))
        .map(|d| CutFacts::of(&d).audio_clips)
        .sum();
    let music = facts.audio_clips + nested_audio;
    let wall = wall_s.unwrap_or(f64::INFINITY);
    let mut out = Vec::new();
    match &reel {
        Some((path, t)) => {
            let (w, h) = t.size.unwrap_or((0.0, 0.0));
            let d = t.duration.unwrap_or(0.0);
            let file = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            out.push(Check {
                name: "showreel".into(),
                pass: (w * 9.0 - h * 16.0).abs() < 1.0 && (d - 15.0).abs() <= 1.5,
                detail: format!("{file}: {w}x{h}, {d:.2} s"),
            });
            let names: Vec<&str> = t.chapters.iter().map(|c| c.name.as_str()).collect();
            out.push(Check {
                name: "chapter_count".into(),
                pass: t.chapters.len() >= 8,
                detail: format!("{} chapters in the document: {}", t.chapters.len(), names.join(" ")),
            });
            match t.bpm {
                Some(bpm) if t.chapters.len() > 1 => {
                    let offs = metrics::beat_offsets(&t.chapters, bpm);
                    let on = offs.iter().filter(|o| o.3.abs() <= BEAT_TOLERANCE_S).count();
                    let worst = offs.iter().max_by(|a, b| a.3.abs().total_cmp(&b.3.abs()));
                    out.push(Check {
                        name: "beat_grid".into(),
                        pass: on == offs.len(),
                        detail: format!(
                            "{on}/{} chapter cuts within {:.0} ms of a beat at {bpm:.1} BPM{}",
                            offs.len(),
                            BEAT_TOLERANCE_S * 1000.0,
                            worst.map_or(String::new(), |w| format!(" (worst {} at {:.3} s: {:+.3} s)", w.0, w.1, w.3))
                        ),
                    });
                }
                _ => out.push(Check { name: "beat_grid".into(), pass: false, detail: "no bpm or fewer than 2 chapters in the document".into() }),
            }
        }
        None => out.push(Check { name: "showreel".into(), pass: false, detail: "no 16:9 showreel document (nested cut or Motion)".into() }),
    }
    out.push(Check { name: "music".into(), pass: music > 0, detail: format!("{music} audio clips (Short + showreel)") });
    out.push(Check {
        name: "time_budget".into(),
        pass: wall <= ACCEPTANCE_BUDGET_S,
        detail: format!("{:.1} min (budget {:.0} min)", wall / 60.0, ACCEPTANCE_BUDGET_S / 60.0),
    });
    out
}

/// The reel's timing as text for the judge: the beat grid and every
/// chapter's span with its cut against the grid, and the montage and sting
/// chapters' inner layer timings in eighth notes.
pub fn timing_data(t: &Timing) -> String {
    let mut s = String::from("Timing data from the showreel document itself (exact, not from pixels):\n");
    let Some(bpm) = t.bpm else {
        s.push_str("- the document names no bpm\n");
        return s;
    };
    let beat = 60.0 / bpm;
    s.push_str(&format!("- {bpm:.2} BPM: one beat = {beat:.4} s, one eighth note = {:.4} s\n- chapters (start–end s, start in beats, offset from the nearest beat):\n", beat / 2.0));
    for c in &t.chapters {
        let b = c.start / beat;
        s.push_str(&format!(
            "  - {}: {:.3}–{:.3} s, beat {:.2}, offset {:+.3} s{}\n",
            c.name,
            c.start,
            c.end,
            b,
            c.start - b.round() * beat,
            if c.texts > 0 { format!(", {} text layers", c.texts) } else { String::new() }
        ));
    }
    if let Some(m) = montage_chapter(t) {
        let eighth = beat / 2.0;
        if let Some((count, every)) = &m.words {
            s.push_str(&format!(
                "- montage: chapter {} holds a word sequencer of {count} words, one word every `{every}` (as written; beats(0.5) is one eighth note), from {:.3} s\n",
                m.name, m.start
            ));
        }
        s.push_str(&format!("- montage chapter {}; its inner layer spans (relative start, length; in eighth notes):\n", m.name));
        for (name, start, len) in m.inner.iter().take(40) {
            s.push_str(&format!("  - {name}: {start:.3} s + {len:.3} s = {:.2} + {:.2} eighths\n", start / eighth, len / eighth));
        }
    }
    s
}

/// The montage chapter: the one with a word sequencer, else the one with
/// the most text layers (never the last, the sting).
pub fn montage_chapter(t: &Timing) -> Option<&metrics::Chapter> {
    let n = t.chapters.len();
    let body = &t.chapters[..n.saturating_sub(1)];
    body.iter()
        .filter(|c| c.words.is_some())
        .max_by_key(|c| c.words.as_ref().map_or(0, |w| w.0))
        .or_else(|| body.iter().filter(|c| c.texts > 1).max_by_key(|c| c.texts))
}

/// The judge's half of the acceptance checklist, in three turns that each
/// get the pictures and data able to show their items:
/// - overview (chapters, hud, shorts_frame, type_band): the Short's and
///   the reel's contact sheets and two key frames;
/// - cuts (beat_cuts, motivated_transitions): strips at ⅛-beat spacing
///   around every chapter cut, an onion trail across each, and the cut
///   times against the beat grid;
/// - montage and sting (word_montage, logo_sting): ⅛-beat strips of both
///   and the montage's word timings.
#[allow(clippy::too_many_arguments)]
pub fn judge_acceptance(ctx: &Ctx, judge: &str, rubric: &Rubric, case: &Case, dir: &Path, edit: &Path, motions: &[PathBuf], r: &mut CaseResult) {
    judge_acceptance_turns(ctx, judge, rubric, case, dir, edit, motions, None, r)
}

/// [`judge_acceptance`] limited to the named turns (`overview`, `cuts`,
/// `montage`); the other turns' earlier answers are kept.
#[allow(clippy::too_many_arguments)]
pub fn judge_acceptance_turns(
    ctx: &Ctx,
    judge: &str,
    rubric: &Rubric,
    case: &Case,
    dir: &Path,
    edit: &Path,
    motions: &[PathBuf],
    only: Option<&[String]>,
    r: &mut CaseResult,
) {
    if rubric.acceptance.is_none() {
        r.acceptance_error = Some("the rubric has no acceptance section".into());
        return;
    }
    let data = find_reel(edit, motions).map(|(_, t)| timing_data(&t)).unwrap_or_default();
    let pics = |files: &[&str]| -> Vec<(String, Vec<u8>)> {
        files
            .iter()
            .filter_map(|f| r.images.iter().find(|p| p.file == *f))
            .filter_map(|p| std::fs::read(dir.join(&p.file)).ok().map(|b| (p.label.clone(), b)))
            .collect()
    };
    let turns: [(&str, &[&str], &[&str], bool); 3] = [
        ("overview", &["chapters", "hud", "shorts_frame", "type_band"], &["strip.png", "showreel-strip.png", "frame-a.png", "frame-b.png"], false),
        ("cuts", &["beat_cuts", "motivated_transitions"], &["cuts-a.png", "cuts-b.png", "cut-onions.png"], true),
        ("montage", &["word_montage", "logo_sting"], &["montage.png", "sting.png", "showreel-strip.png"], true),
    ];
    let mut replies = String::new();
    let mut errors = Vec::new();
    let mut judged: Vec<Check> = Vec::new();
    let mut asked: Vec<&str> = Vec::new();
    for (turn, items, files, with_data) in turns {
        if only.is_some_and(|o| !o.iter().any(|t| t == turn)) {
            continue;
        }
        asked.extend_from_slice(items);
        let images = pics(files);
        if images.is_empty() {
            errors.push(format!("{turn}: no pictures"));
            continue;
        }
        let labels: Vec<String> = images.iter().map(|(l, _)| l.clone()).collect();
        let mut user = judge::brief_prompt(&case.prompt, &r.format, case.length_s, &labels).replace("Score the video per the rubric.", "");
        if with_data && !data.is_empty() {
            user.push('\n');
            user.push_str(&data);
        }
        match judge::review_acceptance(judge, rubric.acceptance.as_deref().unwrap_or_default(), items, &user, images, ctx.judge_timeout) {
            Ok((checks, reply)) => {
                replies.push_str(&format!("===== {turn} =====\n{reply}\n"));
                judged.extend(checks.into_iter().map(|(name, pass, detail)| Check { name, pass, detail }));
            }
            Err(e) => errors.push(format!("{turn}: {e}")),
        }
    }
    let file = if only.is_some() { "acceptance-rejudge.txt" } else { "acceptance.txt" };
    let _ = std::fs::write(dir.join(file), replies);
    let list = r.acceptance.get_or_insert_with(Vec::new);
    list.retain(|c| !asked.contains(&c.name.as_str()));
    list.extend(judged);
    r.acceptance_error = (!errors.is_empty()).then(|| errors.join("; "));
}

/// Redo the acceptance half of a scored acceptance case from its files
/// (measured items, the reel pictures, the three judge turns), keeping the
/// generation, the six-criterion verdict and richness.
pub fn reaccept(ctx: &Ctx, case: &Case, dir: &Path, only: Option<&[String]>, r: &mut CaseResult) {
    let Some(edit) = r.cut.clone().map(PathBuf::from) else { return };
    let Some(facts) = r.facts.clone() else { return };
    let listed = std::fs::read_to_string(dir.join("result.json")).ok().and_then(|t| GenResult::parse(&t).ok()).and_then(|g| g.motions);
    let played = played_motion_docs(&edit);
    let motions: Vec<PathBuf> = match listed {
        _ if !played.is_empty() => played,
        Some(list) if !list.is_empty() => list.iter().map(|m| resolve(dir, &m.path)).collect(),
        _ => find_motion_docs(dir),
    };
    if case.song {
        let judged: Vec<Check> = r.acceptance.take().unwrap_or_default().into_iter().filter(|c| judge::LYRIC_FILM.contains(&c.name.as_str())).collect();
        r.acceptance = Some(lyric_measured(case, &facts, r.wall_s).into_iter().chain(judged).collect());
        const PICS: &[&str] = &["sync.png", "outro.png", "sets-a.png", "sets-b.png", "set-onions.png"];
        r.images.retain(|p| !PICS.contains(&p.file.as_str()));
        if let Some(tools) = &ctx.tools {
            lyric_pictures(tools, dir, &edit, &facts, r);
        }
        if let (Some(judge), Some(rubric)) = (&ctx.judge, &ctx.rubric) {
            judge_lyric_film_turns(ctx, judge, rubric, case, dir, &edit, only, r);
        }
        return;
    }
    // Measured items are recomputed; judged items survive for the turns
    // not re-judged (`only`), and are replaced by the turns that are.
    let judged: Vec<Check> = r.acceptance.take().unwrap_or_default().into_iter().filter(|c| judge::ACCEPTANCE.contains(&c.name.as_str())).collect();
    r.acceptance = Some(acceptance_measured(&edit, &motions, &facts, r.wall_s).into_iter().chain(judged).collect());
    const PICS: &[&str] = &["showreel-strip.png", "cuts-a.png", "cuts-b.png", "cut-onions.png", "montage.png", "sting.png"];
    r.images.retain(|p| !PICS.contains(&p.file.as_str()));
    if let (Some(tools), Some((path, t))) = (&ctx.tools, find_reel(&edit, &motions)) {
        match tools.strip(&path, 16, &dir.join("showreel-strip.png"), dir) {
            Ok(o) if o.ok() => r.images.push(Picture { label: "contact sheet of the 16:9 showreel document, 16 frames".into(), file: "showreel-strip.png".into() }),
            Ok(o) => r.picture_errors.push(format!("showreel-strip.png: {}", o.reason())),
            Err(e) => r.picture_errors.push(format!("showreel-strip.png: {e}")),
        }
        acceptance_pictures(tools, dir, &path, &t, r);
    }
    if let (Some(judge), Some(rubric)) = (&ctx.judge, &ctx.rubric) {
        judge_acceptance_turns(ctx, judge, rubric, case, dir, &edit, &motions, only, r);
    }
}

/// Frame times one per eighth note over `[from, to)`, each in the middle
/// of its eighth: a word or cut that changes on an eighth boundary is
/// shown after the change, never on the frame just before it (sampling on
/// the boundary showed the outgoing word). Returns `(first, last, count)`
/// for a strip, at most `max` frames.
pub fn eighth_samples(from: f64, to: f64, eighth: f64, max: u32) -> (f64, f64, u32) {
    let count = (((to - from) / eighth + 1e-6).floor() as u32).clamp(1, max.max(1));
    let first = from + eighth / 2.0;
    (first, first + (count - 1) as f64 * eighth, count)
}

/// A lyric film may take longer to make than the 15 s showreel.
pub const LYRIC_FILM_BUDGET_S: f64 = 45.0 * 60.0;

/// The measured half of the lyric-film checklist: the length (the
/// excerpt ±10 %, or a 2–3 min film), music, and the time budget.
pub fn lyric_measured(case: &Case, facts: &CutFacts, wall_s: Option<f64>) -> Vec<Check> {
    let d = facts.duration;
    let wall = wall_s.unwrap_or(f64::INFINITY);
    vec![
        Check {
            name: "length".into(),
            pass: (d - case.length_s).abs() <= case.length_s * 0.1 || (120.0..=180.0).contains(&d),
            detail: format!("{d:.1} s (the {:.0} s excerpt ±10 %, or 120–180 s)", case.length_s),
        },
        Check { name: "music".into(), pass: facts.audio_clips > 0, detail: format!("{} audio clips", facts.audio_clips) },
        Check {
            name: "time_budget".into(),
            pass: wall <= LYRIC_FILM_BUDGET_S,
            detail: format!("{:.1} min (budget {:.0} min)", wall / 60.0, LYRIC_FILM_BUDGET_S / 60.0),
        },
    ]
}

/// Sampling windows for word sync: one 4/4 bar at 20, 40, 60 and 80 %
/// of the film.
fn sync_windows(duration: f64, bpm: f64) -> Vec<(f64, f64)> {
    let bar = 4.0 * 60.0 / bpm;
    [0.2, 0.4, 0.6, 0.8].iter().map(|f| (duration * f, (duration * f + bar).min(duration))).collect()
}

fn edit_bpm(edit: &Path) -> f64 {
    std::fs::read_to_string(edit)
        .ok()
        .and_then(|s| CutDoc::load_named(&s, &edit.display().to_string()).ok())
        .map(|d| d.bpm)
        .filter(|b| *b > 0.0)
        .unwrap_or(120.0)
}

/// Lyric-film pictures: eighth-note strips over one bar at four points
/// (`sync.png`, one row each) and the outro (`outro.png`).
fn lyric_pictures(tools: &CutTools, dir: &Path, edit: &Path, facts: &CutFacts, r: &mut CaseResult) {
    let bpm = edit_bpm(edit);
    let tmp = dir.join(".sync");
    let _ = std::fs::create_dir_all(&tmp);
    let mut rows = Vec::new();
    for (i, (from, to)) in sync_windows(facts.duration, bpm).into_iter().enumerate() {
        let png = tmp.join(format!("sync-{i}.png"));
        let (a, b, n) = eighth_samples(from, to, 30.0 / bpm, 16);
        if tools.strip_range(edit, a, b, n, n, &png, dir).is_ok_and(|o| o.ok()) {
            if let Ok(b) = std::fs::read(&png) {
                rows.push(b);
            }
        }
    }
    if !rows.is_empty() {
        match judge::tile_pngs(&rows, 1).and_then(|png| std::fs::write(dir.join("sync.png"), png).map_err(|e| e.to_string())) {
            Ok(()) => r.images.push(Picture {
                label: format!("word sync: one bar at 20/40/60/80 % of the film, one row each, a frame in the middle of every eighth note at {bpm:.1} BPM"),
                file: "sync.png".into(),
            }),
            Err(e) => r.picture_errors.push(format!("sync.png: {e}")),
        }
    }
    let d = facts.duration;
    match tools.strip_range(edit, d * 0.85, d, 12, 6, &dir.join("outro.png"), dir) {
        Ok(o) if o.ok() => r.images.push(Picture { label: format!("the outro, {:.1}–{d:.1} s, 12 frames", d * 0.85), file: "outro.png".into() }),
        Ok(o) => r.picture_errors.push(format!("outro.png: {}", o.reason())),
        Err(e) => r.picture_errors.push(format!("outro.png: {e}")),
    }
    set_pictures(tools, dir, edit, r);
}

/// Set changes of a lyric film: the starts of its Graphic nodes (without
/// the ones across most of the film, a HUD), sorted, skipping 0.
pub fn set_changes(edit: &Path) -> Vec<f64> {
    let Some(doc) = std::fs::read_to_string(edit).ok().and_then(|s| CutDoc::load_named(&s, &edit.display().to_string()).ok()) else {
        return Vec::new();
    };
    let mut starts: Vec<f64> = doc
        .graphics
        .iter()
        .filter_map(|g| g.span)
        .filter(|s| s.length < 0.8 * doc.duration && s.start > 0.0)
        .map(|s| s.start)
        .collect();
    starts.sort_by(f64::total_cmp);
    starts.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
    starts
}

/// Eighth-note strips across up to 8 set changes (`sets-a/b.png`, one row
/// each) and onion trails across them (`set-onions.png`), so the judge can
/// see a camera move from a cut.
fn set_pictures(tools: &CutTools, dir: &Path, edit: &Path, r: &mut CaseResult) {
    let eighth = 30.0 / edit_bpm(edit);
    let tmp = dir.join(".sets");
    let _ = std::fs::create_dir_all(&tmp);
    let changes: Vec<f64> = set_changes(edit).into_iter().take(8).collect();
    let (mut rows, mut onions) = (Vec::new(), Vec::new());
    for (i, at) in changes.iter().enumerate() {
        let png = tmp.join(format!("set-{i}.png"));
        let (a, b, n) = eighth_samples((at - 2.0 * eighth).max(0.0), at + 6.0 * eighth, eighth, 8);
        if tools.strip_range(edit, a, b, n, 8, &png, dir).is_ok_and(|o| o.ok()) {
            if let Ok(bytes) = std::fs::read(&png) {
                rows.push(bytes);
            }
        }
        let o = tmp.join(format!("onion-{i}.png"));
        if tools.onion(edit, (at - eighth).max(0.0), at + 3.0 * eighth, 5, &o, dir).is_ok_and(|x| x.ok()) {
            if let Ok(bytes) = std::fs::read(&o) {
                onions.push(bytes);
            }
        }
    }
    let times: Vec<String> = changes.iter().map(|t| format!("{t:.2}")).collect();
    let half = rows.len().min(4);
    let mut sheet = |file: &str, label: String, pngs: &[Vec<u8>], columns: usize| {
        if pngs.is_empty() {
            return;
        }
        match judge::tile_pngs(pngs, columns).and_then(|png| std::fs::write(dir.join(file), png).map_err(|e| e.to_string())) {
            Ok(()) => r.images.push(Picture { label, file: file.to_string() }),
            Err(e) => r.picture_errors.push(format!("{file}: {e}")),
        }
    };
    sheet("sets-a.png", format!("one row per set change (at {} s), a frame in the middle of each eighth from 2 before to 6 after", times[..half.min(times.len())].join(", ")), &rows[..half], 1);
    sheet("sets-b.png", format!("one row per set change (at {} s), a frame in the middle of each eighth from 2 before to 6 after", times.get(half..rows.len().min(times.len())).map_or(String::new(), |t| t.join(", "))), &rows[half..], 1);
    sheet("set-onions.png", format!("onion trails across each set change in order ({} s), 1 eighth before to 3 after", times.join(", ")), &onions, 4);
}

/// Timed words from the lyric timing files the cut stages
/// (`lyrics:<id>=<file.json>` in `media.txt`), read loosely: any object
/// with a time (`t`/`start`/`time`/`at`) and a word (`word`/`text`/`w`).
pub fn timed_words(edit: &Path) -> Vec<(f64, String)> {
    let list = edit.with_file_name("media.txt");
    let base = list.parent().map(Path::to_path_buf).unwrap_or_default();
    let doc = std::fs::read_to_string(edit).ok().and_then(|s| CutDoc::load_named(&s, &edit.display().to_string()).ok());
    let mut out = Vec::new();
    for line in std::fs::read_to_string(&list).unwrap_or_default().lines() {
        let Some((id, f)) = line.trim().strip_prefix("lyrics:").and_then(|l| l.split_once('=')) else { continue };
        let Ok(text) = std::fs::read_to_string(base.join(f.trim())) else { continue };
        let Ok(v) = JsonValue::deserialize_json_lenient(&text) else { continue };
        let mut words = Vec::new();
        collect_words(&v, &mut words);
        // The words are on the song's clock: move them to the cut's by
        // the clip that plays that song (its span start minus its `in`).
        let shift = doc.as_ref().map_or(0.0, |d| song_offset(d, id.trim()));
        out.extend(words.into_iter().map(|(t, w)| (t + shift, w)));
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// Where the song `asset` sits on the cut's clock: the first clip or
/// audio clip playing it, as span start minus its source `in`.
fn song_offset(doc: &CutDoc, asset: &str) -> f64 {
    use makepad_story_cut::{CutNodeKind, MediaRef, SourceTime};
    for node in &doc.nodes {
        let (src, source_in) = match &node.kind {
            CutNodeKind::AudioClip { src, source_in, .. } | CutNodeKind::Clip { src, source_in, .. } => (src, source_in),
            _ => continue,
        };
        if matches!(src, MediaRef::Asset(a) if a.id == asset) {
            let start = node.span.map_or(0.0, |s| s.start);
            let inn = match source_in {
                SourceTime::Seconds(s) => *s,
                _ => 0.0,
            };
            return start - inn;
        }
    }
    0.0
}

fn collect_words(v: &JsonValue, out: &mut Vec<(f64, String)>) {
    let num = |x: &JsonValue| x.as_f64().or_else(|| x.as_i64().map(|i| i as f64));
    match v {
        JsonValue::Array(items) => items.iter().for_each(|i| collect_words(i, out)),
        // `vj-lyrics` lines: `text` with `w`, one start time per
        // whitespace-separated word.
        JsonValue::Object(_) if v.get("text").and_then(JsonValue::as_str).is_some() && v.get("w").and_then(JsonValue::as_array).is_some() => {
            let text = v.get("text").and_then(JsonValue::as_str).unwrap_or("");
            let times = v.get("w").and_then(JsonValue::as_array).cloned().unwrap_or_default();
            for (word, t) in text.split_whitespace().zip(times.iter()) {
                if let Some(t) = num(t) {
                    out.push((t, word.to_string()));
                }
            }
        }
        JsonValue::Object(map) => {
            let t = ["t", "start", "time", "at"].iter().find_map(|k| v.get(k).and_then(|x| x.as_f64().or_else(|| x.as_i64().map(|i| i as f64))));
            let w = ["word", "text", "w"].iter().find_map(|k| v.get(k).and_then(JsonValue::as_str));
            if let (Some(t), Some(w)) = (t, w) {
                out.push((t, w.to_string()));
                return;
            }
            map.values().for_each(|x| collect_words(x, out));
        }
        _ => {}
    }
}

/// The lyric film's judge half, in two turns: sync (word_sync, from the
/// eighth-note sync strips and the timed words inside those bars) and the
/// film (the other six, from the contact sheet, key frames and outro).
#[allow(clippy::too_many_arguments)]
pub fn judge_lyric_film(ctx: &Ctx, judge: &str, rubric: &Rubric, case: &Case, dir: &Path, edit: &Path, r: &mut CaseResult) {
    judge_lyric_film_turns(ctx, judge, rubric, case, dir, edit, None, r)
}

/// [`judge_lyric_film`] limited to the named turns (`sync`, `world`,
/// `film`); the other turns' earlier answers are kept.
#[allow(clippy::too_many_arguments)]
pub fn judge_lyric_film_turns(ctx: &Ctx, judge: &str, rubric: &Rubric, case: &Case, dir: &Path, edit: &Path, only: Option<&[String]>, r: &mut CaseResult) {
    let Some(system) = rubric.lyric_film.clone() else {
        r.acceptance_error = Some("the rubric has no lyric-film section".into());
        return;
    };
    let bpm = edit_bpm(edit);
    let words = timed_words(edit);
    let mut data = format!("Timing data from the cut (exact): {bpm:.2} BPM, one eighth note = {:.4} s.\n", 30.0 / bpm);
    if let Some(facts) = &r.facts {
        for (from, to) in sync_windows(facts.duration, bpm) {
            let inside: Vec<String> = words.iter().filter(|(t, _)| *t >= from && *t < to).map(|(t, w)| format!("{w}@{t:.3}")).collect();
            data.push_str(&format!(
                "- bar {from:.2}–{to:.2} s: {}\n",
                if inside.is_empty() { "no timed words staged for this bar".to_string() } else { inside.join(" ") }
            ));
        }
    }
    let pics = |files: &[&str]| -> Vec<(String, Vec<u8>)> {
        files
            .iter()
            .filter_map(|f| r.images.iter().find(|p| p.file == *f))
            .filter_map(|p| std::fs::read(dir.join(&p.file)).ok().map(|b| (p.label.clone(), b)))
            .collect()
    };
    let changes: Vec<String> = set_changes(edit).iter().map(|t| format!("{t:.3}")).collect();
    let world_data = format!("Set changes from the cut (Graphic starts, exact): {} s.\n", changes.join(", "));
    let turns: [(&str, &[&str], &[&str], Option<&str>); 3] = [
        ("sync", &["word_sync"], &["sync.png", "strip.png"], Some(data.as_str())),
        ("world", &["one_world"], &["sets-a.png", "sets-b.png", "set-onions.png", "strip.png"], Some(world_data.as_str())),
        ("film", &["set_per_line", "palette", "running_device", "footnotes", "outro"], &["strip.png", "frame-a.png", "frame-b.png", "outro.png"], None),
    ];
    let mut replies = String::new();
    let mut errors = Vec::new();
    let mut judged: Vec<Check> = Vec::new();
    let mut asked: Vec<&str> = Vec::new();
    for (turn, items, files, extra) in turns {
        if only.is_some_and(|o| !o.iter().any(|t| t == turn)) {
            continue;
        }
        asked.extend_from_slice(items);
        let images = pics(files);
        if images.is_empty() {
            errors.push(format!("{turn}: no pictures"));
            continue;
        }
        let labels: Vec<String> = images.iter().map(|(l, _)| l.clone()).collect();
        let mut user = judge::brief_prompt(&case.prompt, &r.format, case.length_s, &labels).replace("Score the video per the rubric.", "");
        if let Some(extra) = extra {
            user.push('\n');
            user.push_str(extra);
        }
        match judge::review_acceptance(judge, &system, items, &user, images, ctx.judge_timeout) {
            Ok((checks, reply)) => {
                replies.push_str(&format!("===== {turn} =====\n{reply}\n"));
                judged.extend(checks.into_iter().map(|(name, pass, detail)| Check { name, pass, detail }));
            }
            Err(e) => errors.push(format!("{turn}: {e}")),
        }
    }
    let file = if only.is_some() { "acceptance-rejudge.txt" } else { "acceptance.txt" };
    let _ = std::fs::write(dir.join(file), replies);
    let list = r.acceptance.get_or_insert_with(Vec::new);
    list.retain(|c| !asked.contains(&c.name.as_str()));
    list.extend(judged);
    r.acceptance_error = (!errors.is_empty()).then(|| errors.join("; "));
}

/// The acceptance pictures that show time closely: ⅛-beat strips around
/// each chapter cut (4 cuts per sheet), onion trails across the cuts, and
/// ⅛-beat strips of the montage and the sting.
fn acceptance_pictures(tools: &CutTools, dir: &Path, reel: &Path, t: &Timing, r: &mut CaseResult) {
    let Some(bpm) = t.bpm else { return };
    let eighth = 60.0 / bpm / 2.0;
    let tmp = dir.join(".cuts");
    let _ = std::fs::create_dir_all(&tmp);
    let cuts: Vec<(String, f64)> = t.chapters.iter().skip(1).take(8).map(|c| (c.name.clone(), c.start)).collect();
    let mut strips = Vec::new();
    let mut onions = Vec::new();
    for (i, (_, at)) in cuts.iter().enumerate() {
        let s = tmp.join(format!("cut-{i}.png"));
        let (a, b, n) = eighth_samples((at - 2.0 * eighth).max(0.0), at + 6.0 * eighth, eighth, 8);
        if tools.strip_range(reel, a, b, n, 8, &s, dir).is_ok_and(|o| o.ok()) {
            if let Ok(b) = std::fs::read(&s) {
                strips.push(b);
            }
        }
        let o = tmp.join(format!("onion-{i}.png"));
        if tools.onion(reel, (at - eighth).max(0.0), at + 3.0 * eighth, 5, &o, dir).is_ok_and(|o| o.ok()) {
            if let Ok(b) = std::fs::read(&o) {
                onions.push(b);
            }
        }
    }
    let names: Vec<&str> = cuts.iter().map(|c| c.0.as_str()).collect();
    let mut sheet = |file: &str, label: String, pngs: &[Vec<u8>], columns: usize| {
        if pngs.is_empty() {
            return;
        }
        match judge::tile_pngs(pngs, columns).and_then(|png| std::fs::write(dir.join(file), png).map_err(|e| e.to_string())) {
            Ok(()) => r.images.push(Picture { label, file: file.to_string() }),
            Err(e) => r.picture_errors.push(format!("{file}: {e}")),
        }
    };
    let half = strips.len().min(4);
    sheet("cuts-a.png", format!("one row per chapter cut ({}), 8 frames each in the middle of the eighth notes from 2 before to 6 after the cut", names[..half.min(names.len())].join(", ")), &strips[..half], 1);
    sheet("cuts-b.png", format!("one row per chapter cut ({}), 8 frames each in the middle of the eighth notes from 2 before to 6 after the cut", names.get(half..).map_or(String::new(), |n| n.join(", "))), &strips[half..], 1);
    sheet("cut-onions.png", format!("onion-skin trails across each chapter cut in order ({}), from 1 eighth before to 3 after", names.join(", ")), &onions, 4);
    let mut range = |file: &str, label: String, from: f64, to: f64| {
        let (a, b, count) = eighth_samples(from, to, eighth, 24);
        match tools.strip_range(reel, a, b, count, 8, &dir.join(file), dir) {
            Ok(o) if o.ok() && dir.join(file).is_file() => r.images.push(Picture { label, file: file.to_string() }),
            Ok(o) => r.picture_errors.push(format!("{file}: {}", o.reason())),
            Err(e) => r.picture_errors.push(format!("{file}: {e}")),
        }
    };
    if let Some(m) = montage_chapter(t) {
        range("montage.png", format!("the {} chapter (the montage), one frame in the middle of each eighth note, {:.2}–{:.2} s", m.name, m.start, m.end), m.start, m.end);
    }
    if let Some(last) = t.chapters.last() {
        range("sting.png", format!("the last chapter {} (the sting), one frame in the middle of each eighth note, {:.2}–{:.2} s", last.name, last.start, last.end), last.start, last.end);
    }
}

fn measure_with_tools(ctx: &Ctx, tools: &CutTools, case: &Case, dir: &Path, edit: &Path, motions: &[PathBuf], facts: &CutFacts, r: &mut CaseResult) {
    // Inspect the cut and each motion document; their issues add up.
    let mut total = InspectSummary::default();
    let mut any = false;
    for file in std::iter::once(edit.to_path_buf()).chain(motions.iter().cloned()) {
        match tools.inspect(&file, dir) {
            Ok((_, Some(json))) => match JsonValue::deserialize_json_lenient(&json) {
                Ok(v) => {
                    let s = metrics::summarize_inspect(&v);
                    total.errors += s.errors;
                    total.warnings += s.warnings;
                    total.safe_zone += s.safe_zone;
                    total.overflow += s.overflow;
                    for line in s.first {
                        if total.first.len() < 8 {
                            total.first.push(line);
                        }
                    }
                    any = true;
                }
                Err(e) => r.inspect_error = Some(format!("{}: unparsable report ({e:?})", file.display())),
            },
            Ok((o, None)) => r.inspect_error = Some(format!("{}: no JSON report; {}", file.display(), o.reason())),
            Err(e) => r.inspect_error = Some(e),
        }
    }
    if any {
        r.inspect = Some(total);
    }
    if ctx.render {
        match tools.render_draft(edit, &dir.join("draft.mp4"), dir) {
            Ok(o) if o.ok() => r.render_s = Some(o.seconds),
            Ok(o) => r.render_error = Some(o.reason()),
            Err(e) => r.render_error = Some(e),
        }
    }
    let mut picture = |label: String, file: &str, outcome: Result<tools::Outcome, String>| match outcome {
        Ok(o) if o.ok() && dir.join(file).is_file() => r.images.push(Picture { label, file: file.to_string() }),
        Ok(o) => r.picture_errors.push(format!("{file}: {}", o.reason())),
        Err(e) => r.picture_errors.push(format!("{file}: {e}")),
    };
    let d = facts.duration.max(0.0);
    // An acceptance showreel has ≥ 8 chapters in 15 s: twice the frames.
    let count = match (case.set == "acceptance", case.song) {
        (true, true) => ctx.strip_count.max(24),
        (true, false) => ctx.strip_count.max(16),
        _ => ctx.strip_count,
    };
    let sheet = tools.strip(edit, count, &dir.join("strip.png"), dir);
    let exposure = sheet.as_ref().ok().filter(|o| o.ok()).and_then(|o| JsonValue::deserialize_json_lenient(o.stdout.trim()).ok()).as_ref().and_then(metrics::exposure_of);
    picture(format!("contact sheet, {count} frames across {d:.1} s"), "strip.png", sheet);
    let reel = (case.set == "acceptance" && !case.song).then(|| find_reel(edit, motions)).flatten();
    if let Some((path, _)) = &reel {
        picture(
            "contact sheet of the 16:9 showreel document, 16 frames".into(),
            "showreel-strip.png",
            tools.strip(path, 16, &dir.join("showreel-strip.png"), dir),
        );
    }
    for (name, t) in [("frame-a.png", d / 3.0), ("frame-b.png", d * 2.0 / 3.0)] {
        picture(format!("key frame at {t:.1} s"), name, tools.frame(edit, t, 1024, &dir.join(name), dir));
    }
    let to = d.min(1.5);
    picture(format!("onion-skin trail 0.0–{to:.1} s (the opening animation)"), "onion.png", tools.onion(edit, 0.0, to, 6, &dir.join("onion.png"), dir));
    r.exposure = exposure;
    // The cut could not be drawn (unresolved media, say): picture its
    // motion documents alone so the graphics can still be judged.
    if r.images.is_empty() {
        for (i, m) in motions.iter().take(judge::MAX_IMAGES).enumerate() {
            let file = format!("motion{}-strip.png", i + 1);
            let name = m.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            let outcome = tools.strip(m, ctx.strip_count, &dir.join(&file), dir);
            match outcome {
                Ok(o) if o.ok() && dir.join(&file).is_file() => {
                    r.images.push(Picture { label: format!("motion document {name} drawn alone (the cut itself could not be drawn)"), file })
                }
                Ok(o) => r.picture_errors.push(format!("{file}: {}", o.reason())),
                Err(e) => r.picture_errors.push(format!("{file}: {e}")),
            }
        }
    }
    if let Some((path, t)) = &reel {
        acceptance_pictures(tools, dir, path, t, r);
    }
    if case.set == "acceptance" && case.song {
        lyric_pictures(tools, dir, edit, facts, r);
    }
}

fn judge_case(ctx: &Ctx, judge: &str, rubric: &Rubric, case: &Case, dir: &Path, r: &mut CaseResult) {
    let images: Vec<(String, Vec<u8>)> =
        r.images.iter().filter_map(|p| std::fs::read(dir.join(&p.file)).ok().map(|b| (p.label.clone(), b))).collect();
    if images.is_empty() {
        r.judge_error = Some("no pictures to judge".into());
        return;
    }
    let labels: Vec<String> = images.iter().map(|(l, _)| l.clone()).collect();
    let user = judge::brief_prompt(&case.prompt, &r.format, case.length_s, &labels);
    match judge::review(judge, rubric, &user, images, ctx.judge_timeout) {
        Ok((verdict, reply)) => {
            let _ = std::fs::write(dir.join("judge.txt"), reply);
            r.verdict = Some(verdict);
        }
        Err(e) => r.judge_error = Some(e),
    }
}

/// Recompute the measured facts of a scored case (edit facts, motion
/// stats, conformance, overall) from its files, keeping the generation,
/// tool and judge results: for a changed check, without new generations
/// or judge turns.
pub fn recheck(case: &Case, dir: &Path, r: &mut CaseResult) {
    let Some(edit) = r.cut.clone() else { return };
    let Ok(source) = std::fs::read_to_string(&edit) else { return };
    let Ok(doc) = CutDoc::load_named(&source, &edit) else { return };
    let facts = CutFacts::of(&doc);
    let listed = std::fs::read_to_string(dir.join("result.json")).ok().and_then(|t| GenResult::parse(&t).ok()).and_then(|g| g.motions);
    let played = played_motion_docs(Path::new(&edit));
    let motion_paths: Vec<PathBuf> = match listed {
        _ if !played.is_empty() => played,
        Some(list) if !list.is_empty() => list.iter().map(|m| resolve(dir, &m.path)).collect(),
        _ => find_motion_docs(dir),
    };
    let sources: Vec<(String, String)> =
        motion_paths.iter().filter_map(|p| std::fs::read_to_string(p).ok().map(|s| (p.display().to_string(), s))).collect();
    r.motion = metrics::motion_stats(&sources, facts.duration);
    r.conformance = metrics::conformance(case, &facts, &r.motion);
    if case.set == "acceptance" {
        let (measured, judged_items) = if case.song {
            (lyric_measured(case, &facts, r.wall_s), judge::LYRIC_FILM)
        } else {
            (acceptance_measured(Path::new(&edit), &motion_paths, &facts, r.wall_s), judge::ACCEPTANCE)
        };
        let list = r.acceptance.get_or_insert_with(Vec::new);
        list.retain(|c| judged_items.contains(&c.name.as_str()));
        let judged = std::mem::take(list);
        *list = measured.into_iter().chain(judged).collect();
    }
    r.facts = Some(facts);
    r.overall = overall(r);
}

/// The separate visual-richness review of a case's pictures.
pub fn judge_richness(ctx: &Ctx, judge: &str, rubric: &Rubric, case: &Case, dir: &Path, r: &mut CaseResult) {
    let images: Vec<(String, Vec<u8>)> =
        r.images.iter().filter_map(|p| std::fs::read(dir.join(&p.file)).ok().map(|b| (p.label.clone(), b))).collect();
    if images.is_empty() {
        r.richness_error = Some("no pictures to judge".into());
        return;
    }
    let labels: Vec<String> = images.iter().map(|(l, _)| l.clone()).collect();
    let user = judge::brief_prompt(&case.prompt, &r.format, case.length_s, &labels).replace("Score the video per the rubric.", "Score the video's visual richness.");
    match judge::review_richness(judge, rubric, &user, images, ctx.judge_timeout) {
        Ok((score, note, reply)) => {
            let _ = std::fs::write(dir.join("richness.txt"), reply);
            r.richness = Some(score);
            r.richness_note = Some(note);
            r.richness_error = None;
        }
        Err(e) => r.richness_error = Some(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::CriterionScore;

    #[test]
    fn eighth_samples_sit_mid_eighth() {
        // 120 BPM: eighth = 0.25 s. A 1.5 s montage from 10.5 s has six
        // words; each sample is in the middle of its word, never on a change.
        let (a, b, n) = eighth_samples(10.5, 12.0, 0.25, 24);
        assert_eq!(n, 6);
        assert!((a - 10.625).abs() < 1e-9 && (b - 11.875).abs() < 1e-9);
        for k in 0..n {
            let t = a + k as f64 * 0.25;
            let into = (t - 10.5) / 0.25;
            assert!((into.fract() - 0.5).abs() < 1e-9, "sample {k} at {t} is not mid-eighth");
        }
        assert_eq!(eighth_samples(0.0, 100.0, 0.25, 24).2, 24);
        assert_eq!(eighth_samples(0.0, 0.1, 0.25, 24).2, 1);
    }

    #[test]
    fn lyric_film_measures_length_music_and_budget() {
        let case = crate::bench::select("lyric-film").unwrap().remove(0);
        let facts = CutFacts { duration: 62.0, audio_clips: 1, ..Default::default() };
        assert!(lyric_measured(&case, &facts, Some(1200.0)).iter().all(|c| c.pass));
        let film = CutFacts { duration: 150.0, ..facts.clone() };
        assert!(lyric_measured(&case, &film, Some(1200.0))[0].pass);
        let short = CutFacts { duration: 40.0, audio_clips: 0, ..facts };
        let failed: Vec<_> = lyric_measured(&case, &short, Some(4000.0)).into_iter().filter(|c| !c.pass).map(|c| c.name).collect();
        assert_eq!(failed, ["length", "music", "time_budget"]);
        let w = sync_windows(100.0, 120.0);
        assert_eq!(w.len(), 4);
        assert!((w[0].0 - 20.0).abs() < 1e-9 && (w[0].1 - 22.0).abs() < 1e-9);
    }

    #[test]
    fn timed_words_read_loosely() {
        let dir = std::env::temp_dir().join(format!("video-eval-words-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("media.txt"), "lyrics:ast_x=take.json\n").unwrap();
        std::fs::write(dir.join("take.json"), r#"{"lines":[{"words":[{"text":"up","start":1.5},{"text":"ping","start":1.75}]}],"meta":{"t":0}}"#).unwrap();
        let words = timed_words(&dir.join("cut.splash"));
        assert_eq!(words, [(1.5, "up".to_string()), (1.75, "ping".to_string())]);
        // vj-lyrics v4: `text` plus one start per whitespace word in `w`.
        std::fs::write(dir.join("take.json"), r#"{"format":"vj-lyrics","version":4,"lines":[{"t0":2.0,"t1":3.6,"text":"A red thread","w":[2.0,2.5,2.84],"c":false}]}"#).unwrap();
        let words = timed_words(&dir.join("cut.splash"));
        assert_eq!(words, [(2.0, "A".to_string()), (2.5, "red".to_string()), (2.84, "thread".to_string())]);
        let _ = std::fs::remove_file(dir.join("media.txt"));
        let _ = std::fs::remove_file(dir.join("take.json"));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn overall_follows_the_rubric() {
        let mut r = CaseResult { success: true, facts: Some(CutFacts::default()), ..Default::default() };
        assert_eq!(overall(&r), 60.0);
        r.verdict = Some(Verdict {
            criteria: crate::judge::CRITERIA.iter().map(|c| CriterionScore { name: c.to_string(), score: 4.0, note: String::new() }).collect(),
            mean: 4.0,
            ..Default::default()
        });
        assert_eq!(overall(&r), 75.0);
        r.inspect = Some(InspectSummary { errors: 1, safe_zone: 2, overflow: 1, ..Default::default() });
        r.conformance = vec![Check { name: "fps".into(), pass: false, detail: String::new() }];
        assert_eq!(overall(&r), 75.0 - 5.0 - 6.0 - 5.0);
        r.success = false;
        assert_eq!(overall(&r), 0.0);
    }
}
