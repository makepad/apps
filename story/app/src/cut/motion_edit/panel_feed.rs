//! What a panel's instruments read of the running film (the panel host's
//! `live`, `playhead`, `song`, `beats` and `words`): a snapshot the desk
//! refreshes on its pump, read by the hooks when a widget draws.
//!
//! - `playhead`: the graphic's playhead, its length, whether it plays.
//! - `live(path)`: a top-level number `let`'s value at the playhead, from
//!   the values the desk samples over the film after each load; any other
//!   number path, its value as written (held values of a gesture included,
//!   since they are written as they go).
//! - `song(t0, t1, n)`: the film's sound under the graphic, on its clock
//!   (RMS, 0..1, the sounds under it mixed by power).
//! - `beats`, `words`: what the code computed at load: the rising list of
//!   times with the most entries (its beats), and the spans labelled by
//!   their words with the most entries (the sung words).
//!
//! Main-loop cost: the playhead each pump (three numbers); the sounds and
//! the series when they change (Arcs and a clone of the sampled curves);
//! beats and words once per load.

use makepad_edit_values::ValueKind;
use makepad_library_browser::draw::Peaks;
use super::super::VideoCut;
use makepad_motion::paths;
use std::cell::RefCell;
use std::sync::Arc;

#[derive(Default)]
struct Feed {
    playhead: (f64, f64, bool),
    /// The graphic's start in the edit, and each sound under it: (start,
    /// length, source in, speed, its peaks).
    g0: f64,
    sounds: Vec<(f64, f64, f64, f64, Arc<Peaks>)>,
    /// The model the beats, words and series are of.
    of: Option<(u64, usize)>,
    beats: Vec<f64>,
    words: Vec<(f64, f64, String)>,
    /// Per file (the panel's order: the document, then its non-built-in
    /// modules): (byte, value per sample over the graphic).
    series: Vec<Vec<(usize, Vec<f32>)>>,
    /// The texts and outlines paths resolve against.
    files: Vec<(String, String, makepad_motion::schema::code::CodeOutline)>,
    files_of: Option<u64>,
}

thread_local! {
    static FEED: RefCell<Feed> = RefCell::new(Feed::default());
}

/// The snapshot brought up to date (on the desk's pump).
pub fn refresh(page: &VideoCut) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    FEED.with(|feed| {
        let mut feed = feed.borrow_mut();
        feed.playhead = (session.playhead, session.duration(), session.playing.is_some());
        if feed.files_of != Some(session.revision) {
            feed.files_of = Some(session.revision);
            feed.files = std::iter::once((String::new(), session.source.clone(), session.code.clone()))
                .chain(session.modules.iter().filter(|m| !m.builtin()).map(|m| (m.name.clone(), m.source.clone(), m.code.clone())))
                .collect();
        }
        // The sounds under the graphic (they change with the edit).
        let block = session.node.as_deref().and_then(|n| page.projection.block(n));
        let g0 = block.map_or(0.0, |b| b.start);
        let len = session.duration().max(1e-3);
        let sounds: Vec<(f64, f64, f64, f64, Arc<Peaks>)> = if block.is_none() { Vec::new() } else {
            page.projection.blocks.iter().filter_map(|b| {
                let sound = b.sound.as_ref()?;
                let peaks = page.peaks.get(sound.media.as_ref()?)?;
                (b.start < g0 + len && b.start + b.length > g0).then(|| (b.start, b.length, sound.source_in, sound.speed, peaks.clone()))
            }).collect()
        };
        if g0 != feed.g0 || sounds.len() != feed.sounds.len() || sounds.iter().zip(&feed.sounds).any(|(a, b)| a.0 != b.0 || a.2 != b.2 || !Arc::ptr_eq(&a.4, &b.4)) {
            feed.g0 = g0;
            feed.sounds = sounds;
        }
        // The curves the desk sampled (refreshed as they fill in).
        let (_, sampled) = super::desk::series_rev(page);
        let of = (session.doc_of, sampled);
        if feed.of != Some(of) {
            let load_changed = feed.of.is_none_or(|(d, _)| d != session.doc_of);
            feed.of = Some(of);
            let files = std::iter::once(makepad_motion::schema::CodeFile::Doc)
                .chain(session.modules.iter().filter(|m| !m.builtin()).map(|m| makepad_motion::schema::CodeFile::Module(m.key.clone())));
            feed.series = files.map(|f| super::desk::series(page, &f).cloned().unwrap_or_default()).collect();
            if load_changed {
                let lets = session.doc.as_ref().map(|d| d.bound_at_load()).unwrap_or_default();
                let within = |t: &f64| (0.0..=len + 1e-9).contains(t);
                feed.beats = lets.iter().map(|(_, _, w)| w.ticks.iter().copied().filter(within).collect::<Vec<f64>>()).filter(|t| t.len() >= 4).max_by_key(|t| t.len()).unwrap_or_default();
                feed.words = lets.iter().map(|(_, _, w)| w.spans.iter().filter(|(a, b, word)| !word.is_empty() && (within(a) || within(b))).cloned().collect::<Vec<_>>()).max_by_key(|s| s.len()).unwrap_or_default();
            }
        }
    });
}

pub fn playhead() -> (f64, f64, bool) {
    FEED.with(|f| f.borrow().playhead)
}

fn live(path: &str) -> Option<f64> {
    FEED.with(|feed| {
        let feed = feed.borrow();
        let files: Vec<paths::File> = feed.files.iter().map(|(name, text, code)| paths::File { name, text, code }).collect();
        let site = paths::resolve_exact(&files, path).ok()?;
        if !matches!(site.kind, ValueKind::Number | ValueKind::Time | ValueKind::Angle | ValueKind::Length) {
            return None;
        }
        // A top-level `let` sampled over the film: its value at the playhead.
        let (t, duration, _) = feed.playhead;
        // (A bare `let` of the document, or `module.NAME`.)
        let parts = path.split(['.', '[']).count();
        let bare = if files[site.file].name.is_empty() { parts == 1 } else { parts == 2 };
        let region = files[site.file].code.regions.iter()
            .find(|r| bare && r.depth == 0 && r.kind == makepad_motion::schema::code::RegionKind::Let && r.range.start <= site.range.start && site.range.end <= r.range.end);
        if let Some(region) = region {
            let sampled = feed.series.get(site.file).and_then(|s| s.iter().find(|(byte, _)| region.range.contains(byte)));
            if let Some((_, values)) = sampled.filter(|(_, v)| !v.is_empty()) {
                let k = ((t / duration.max(1e-6)) * values.len() as f64).floor().clamp(0.0, values.len() as f64 - 1.0) as usize;
                if values[k].is_finite() {
                    return Some(values[k] as f64);
                }
            }
        }
        let text = site.text.trim();
        let digits = text.trim_start_matches('-');
        let end = digits.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(digits.len());
        digits[..end].parse::<f64>().ok().map(|v| if text.starts_with('-') { -v } else { v })
    })
}

/// The host's `animatable(path)`: keyed already, keyable (a value written
/// as one literal), or why not (a value computed where it is drawn). No
/// key at all for what is not a value on a curve: a name of another value
/// (`BONE`: that one is where it is keyed), words, a choice, a font.
fn animatable(path: &str) -> Option<Result<bool, String>> {
    FEED.with(|feed| {
        let feed = feed.borrow();
        let files: Vec<paths::File> = feed.files.iter().map(|(name, text, code)| paths::File { name, text, code }).collect();
        let site = match paths::resolve_exact(&files, path) {
            Ok(site) => site,
            Err(why) => return Some(Err(why)),
        };
        let text = site.text.trim();
        if text.starts_with("keys(") {
            return Some(Ok(true));
        }
        let name = !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') && !text.starts_with(|c: char| c.is_ascii_digit());
        match site.kind {
            ValueKind::Number | ValueKind::Time | ValueKind::Angle | ValueKind::Length | ValueKind::Colour => Some(Ok(false)),
            ValueKind::Code if !name => Some(Err("it is computed where it is drawn: ask the chat to expose it as one value first".into())),
            _ => None,
        }
    })
}

thread_local! {
    /// Animate requests from the panel (path, on), for the desk's pump.
    static ANIMATE: RefCell<Vec<(String, bool)>> = const { RefCell::new(Vec::new()) };
}

pub fn animate(path: &str, on: bool) {
    ANIMATE.with(|a| a.borrow_mut().push((path.to_string(), on)));
}

/// The panel's animate requests, taken.
pub fn take_animate() -> Vec<(String, bool)> {
    ANIMATE.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

fn song(t0: f64, t1: f64, n: usize) -> Vec<f32> {
    FEED.with(|feed| {
        let feed = feed.borrow();
        if feed.sounds.is_empty() || n == 0 || t1 <= t0 {
            return Vec::new();
        }
        let dt = (t1 - t0) / n as f64;
        (0..n).map(|k| {
            let t = feed.g0 + t0 + k as f64 * dt;
            let power: f64 = feed.sounds.iter().filter(|(start, length, ..)| t >= *start && t < start + length).map(|(start, _, source_in, speed, peaks)| {
                let s = source_in + (t - start) * speed;
                peaks.rms_over(s, s + dt * speed).powi(2)
            }).sum();
            power.sqrt().min(1.0) as f32
        }).collect()
    })
}

pub fn beats(t0: f64, t1: f64) -> Vec<f64> {
    FEED.with(|f| f.borrow().beats.iter().copied().filter(|t| *t >= t0 && *t <= t1).collect())
}

pub fn words(t0: f64, t1: f64) -> Vec<(f64, f64, String)> {
    FEED.with(|f| f.borrow().words.iter().filter(|(a, b, _)| *b >= t0 && *a <= t1).cloned().collect())
}

/// The hooks, set on the host the desk installs.
pub fn hooks(host: makepad_value_pickers::panel::host::PanelHost) -> makepad_value_pickers::panel::host::PanelHost {
    makepad_value_pickers::panel::host::PanelHost {
        live: Some(Box::new(live)),
        playhead: Some(Box::new(playhead)),
        song: Some(Box::new(song)),
        beats: Some(Box::new(beats)),
        words: Some(Box::new(words)),
        focus: Some(Box::new(super::panel_time::focus)),
        animatable: Some(Box::new(animatable)),
        animate: Some(Box::new(animate)),
        spans: Some(Box::new(super::panel_time::spans_of_paths)),
        selection_label: Some(Box::new(super::panel_time::selection_label)),
        clear_selection_filter: Some(Box::new(super::panel_time::clear_selection_filter)),
        active: Some(Box::new(super::panel_time::active)),
        picked: Some(Box::new(super::panel_time::picked)),
        lit_rev: Some(Box::new(super::panel_time::lit_rev)),
        reveal: Some(Box::new(super::panel_time::reveal_rev)),
        ask: Some(Box::new(super::variants::queue_panel_ask)),
        ask_status: Some(Box::new(super::variants::panel_ask_status)),
        ..host
    }
}
