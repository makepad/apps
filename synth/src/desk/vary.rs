//! The voice tree's data and the melody variants of the score's panel: each
//! voice's part frozen from the score (one cycle), its variants as cards
//! (procedural and seeded at once; the AI's from short conversations at
//! once), heard in place: while a card plays, the composer plays the score
//! with that part instead (same instrument, in sync, the written part not
//! heard), from where the voice plays when the song was stopped; kept,
//! applied or merged bar by bar (one edit).

use super::run::RequestJob;
use super::Desk;
use makepad_score_live::vary::{self, Part};
use makepad_music_iter::gen::TurnMonitor;
use makepad_synth_panel::host::{Card, Track, Variants, VaryAction};
use makepad_widgets::*;
use std::sync::atomic::{AtomicBool, Ordering};
use makepad_score_live::model::CompiledShaders;
use makepad_score_live::{LoadError, PrepareControl};
use std::collections::VecDeque;
use std::sync::Arc;

/// A voice's variants.
#[derive(Default)]
pub struct VoiceVariants {
    pub original: Option<Part>,
    /// (label, part, kept).
    pub cards: Vec<(String, Part, bool)>,
    pub region: Option<(f64, f64)>,
    /// The next seed (each make moves it on: the same session, the same
    /// variants; a new make, new ones).
    pub seed: u64,
    pub busy: Option<String>,
    pub playing: Option<usize>,
    /// The AI's variants on their way.
    pub ai: Option<AiAsk>,
}

/// An ask of the agent for `n` variants, each its own conversation.
pub struct AiAsk {
    started: f64,
    kind: String,
    jobs: Vec<Option<FrameJob<Result<String, String>>>>,
    revision: u64,
    /// Each variant's prompt, its tries, and what it came to: None while
    /// asked, Some(Ok(part)) or Some(Err(why)).
    texts: Vec<String>,
    tries: Vec<u8>,
    done: Vec<Option<Result<Part, String>>>,
    cancel: Arc<AtomicBool>,
    monitor: TurnMonitor,
}

impl Drop for AiAsk {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        for job in self.jobs.iter().flatten() { job.cancel(); }
    }
}

/// An answer the same as the original or another variant (its notes and
/// their places; velocities and lengths aside) says nothing new.
fn same_notes(a: &Part, b: &Part) -> bool {
    a.notes.len() == b.notes.len() && a.notes.iter().zip(&b.notes).all(|(x, y)| (x.beat - y.beat).abs() < 1e-3 && x.note == y.note && x.hit == y.hit)
}

/// Asks a variant (again).
fn ask_variant(cx: &Cx, text: String, cancel: Arc<AtomicBool>, monitor: TurnMonitor) -> Result<FrameJob<Result<String, String>>, String> {
    cx.task_pool().submit_frame_job(Class::Visible, move || {
        super::panel::write_with(VARY_SYSTEM, text, &cancel, &monitor, &mut |_| {})
    }).map_err(|error| error.to_string())
}

/// Actions of different kinds keep their order (for instance Region then Make).
struct VariationRequest {
    revision: u64,
    node: String,
    action: Option<VaryAction>,
    region: Option<(f64, f64)>,
    reply: Option<u64>,
    word: PrepareControl,
    job: Option<RequestJob<Result<(Part, PreparedAction), String>>>,
}

enum PreparedAction {
    Selected,
    Cards(Vec<(String, Part)>),
    Audition(usize, Option<(String, Arc<CompiledShaders>)>),
    Write(Part, String),
    Controls(VaryAction),
}

fn same_kind(left: &Option<VaryAction>, right: &Option<VaryAction>) -> bool {
    match (left, right) {
        (Some(VaryAction::Apply(_) | VaryAction::Merge(_)), Some(VaryAction::Apply(_) | VaryAction::Merge(_))) => true,
        (Some(a), Some(b)) => std::mem::discriminant(a) == std::mem::discriminant(b),
        (None, None) => true,
        _ => false,
    }
}

async fn prepare_action(control: &makepad_score_live::PrepareControl, source: &str, shaders: &Arc<CompiledShaders>, node: &str, action: Option<VaryAction>,
    previous: Option<Part>, cards: Vec<Part>, seed: u64, region: Option<(f64, f64)>, requested_region: Option<(f64, f64)>,
) -> Result<(Part, PreparedAction), String> {
    let loaded = shaders.evaluate(source, control).await.map_err(|error| error.describe(|errors| errors.into_iter().map(|d| d.message).collect::<Vec<_>>().join("; ")))?;
    let original = vary::part_from(loaded, node)?;
    let same = previous.as_ref().is_some_and(|part| same_notes(part, &original));
    let cards = if same { &cards[..] } else { &[] };
    let card = |i: usize| if i == 0 { Ok(&original) } else { cards.get(i - 1).ok_or("no such card") };
    let prepared = match action {
        None => PreparedAction::Selected,
        Some(VaryAction::Make { kind, ai: false, n }) => {
            let mut made = vary::vary(&original, &kind, n, seed.max(1), requested_region.or(if same { region } else { None }))?;
            made.retain(|(_, part)| !same_notes(part, &original));
            if made.is_empty() { return Err(format!("{kind} changes no note of this part")); }
            PreparedAction::Cards(made)
        }
        Some(VaryAction::Audition(Some(i))) => {
            let next = if i == 0 { None } else { Some(vary::apply_prepared_async(source, node, card(i)?, Some(shaders), control).await?) };
            PreparedAction::Audition(i, next)
        }
        Some(VaryAction::Apply(i)) => {
            let part = card(i)?.clone();
            let next = vary::apply_async(source, node, &part, control).await?;
            PreparedAction::Write(part, next)
        }
        Some(VaryAction::Merge(picks)) => {
            let parts: Vec<_> = std::iter::once(&original).chain(cards.iter()).collect();
            let merged = vary::merge(&parts, &picks);
            let next = vary::apply_async(source, node, &merged, control).await?;
            PreparedAction::Write(merged, next)
        }
        Some(action) => PreparedAction::Controls(action),
    };
    Ok((original, prepared))
}

/// The desk's side of the variants and the tree.
#[derive(Default)]
pub struct VaryDesk {
    pub voices: std::collections::HashMap<String, VoiceVariants>,
    /// The tree's voices, made for a score revision (off the UI thread).
    pub tracks: Vec<Track>,
    pub(super) tracks_rev: u64,
    requests: VecDeque<VariationRequest>,
    pub(super) tracks_job: Option<(u64, RequestJob<Result<Vec<Track>, LoadError>>)>,
    /// The card the composer plays in place: (voice, card), and whether
    /// the audition started the song (it stops it again).
    pub live: Option<(String, usize)>,
    started_song: bool,
}

impl VaryDesk {
    /// Inspect the completed edit before applying it would persist a draft.
    #[cfg(test)]
    pub(super) fn take_prepared_write(&mut self, id: u64) -> Option<String> {
        let request = self.requests.front_mut().expect("variation: the edit is queued");
        assert_eq!(request.reply, Some(id), "variation: the edit belongs to this request");
        let (_, action) = request.job.as_mut().expect("variation: the edit was submitted")
            .try_take()?.expect("variation: the job completes").expect("variation: the rewrite validates");
        self.requests.pop_front();
        let PreparedAction::Write(_, next) = action else { panic!("variation: Apply prepares an edit") };
        Some(next)
    }

    pub(super) fn reply_word(&self, id: u64) -> Option<&PrepareControl> {
        self.requests.iter().find(|request| request.reply == Some(id)).map(|request| &request.word)
    }

    pub(super) fn stop_reply(&mut self, id: u64) -> bool {
        let Some(i) = self.requests.iter().position(|request| request.reply == Some(id)) else { return false };
        let request = self.requests.remove(i).unwrap();
        match request.job {
            Some(mut job) => job.cancel(),
            None => request.word.cancel(),
        }
        true
    }

    pub(super) fn cancel_requests(&mut self) {
        self.requests.clear();
        self.tracks_job = None;
    }

    pub(super) fn not_responding(&self) -> bool {
        self.voices.values().filter_map(|voice| voice.ai.as_ref()).any(|ask| ask.monitor.not_responding())
    }

    pub(super) fn stop_writers(&mut self) {
        for voice in self.voices.values_mut() {
            if voice.ai.take().is_some() {
                voice.busy = None;
            }
        }
    }

    pub fn view(&self, node: &str) -> Variants {
        let Some(v) = self.voices.get(node) else { return Variants::default() };
        let card = |label: &str, part: &Part, kept: bool| Card {
            label: label.to_string(),
            notes: part.notes.iter().map(|n| (n.beat, n.note, n.dur, n.vel, n.hit)).collect(),
            kept,
        };
        let mut cards = Vec::new();
        if let Some(o) = &v.original {
            cards.push(card("Original", o, true));
        }
        cards.extend(v.cards.iter().map(|(l, p, k)| card(l, p, *k)));
        Variants { beats: v.original.as_ref().map_or(0.0, |o| o.beats), cards, busy: v.busy.clone().or_else(|| self.requests.iter().any(|r| r.node == node).then(|| "Working…".into())), playing: v.playing, region: v.region }
    }
}

/// The voices of a score as the tree shows them (cycle notes included).
#[cfg(test)]
pub(super) fn tracks_of(source: &str, shaders: &CompiledShaders, control: &PrepareControl) -> Result<Vec<Track>, LoadError> {
    makepad_score_live::drive(tracks_of_async(source, shaders, control))
}

async fn tracks_of_async(source: &str, shaders: &CompiledShaders, control: &PrepareControl) -> Result<Vec<Track>, LoadError> {
    let loaded = shaders.evaluate(source, control).await?;
    let doc = loaded.doc.clone();
    let cycles = makepad_score_live::render::cycle_notes_of(loaded);
    let bar = doc.beats_per_bar();
    let sections: Vec<(String, f64, Vec<String>)> = doc.arrange.iter().filter_map(|name| doc.sections.iter().find(|s| &s.name == name)).map(|s| (s.name.clone(), s.bars * bar, s.loops.clone())).collect();
    let outline = makepad_score_live::source::outline(source);
    Ok(doc.loops.iter().filter(|l| l.stinger.is_none()).map(|l| {
        let cycle = cycles.iter().find(|(n, ..)| *n == l.name);
        let notes = cycle.map(|(_, _, _, notes, durs)| notes.iter().enumerate().map(|(i, (b, n, v, h))| (*b, *n, durs.get(i).copied().filter(|d| *d > 0.0).unwrap_or(0.25), *v, *h)).collect()).unwrap_or_default();
        let inst_label = l.play.as_deref().and_then(|p| outline.node(p)).map(|c| {
            let fields: Vec<String> = c.fields.iter().filter(|f| matches!(f.key.as_str(), "preset" | "lib" | "kind" | "kit" | "program" | "osc" | "profile")).map(|f| f.value.text(source).trim_matches('"').to_string()).collect();
            if fields.is_empty() { c.type_name.clone() } else { format!("{} {}", c.type_name, fields.join(" ")) }
        }).unwrap_or_default();
        Track {
            name: l.name.clone(),
            instrument: l.play.clone(),
            inst_label,
            amp: l.amp as f64,
            pan: l.pan as f64,
            mute: l.mute,
            solo: l.solo,
            notes,
            beats: cycle.map_or(l.beats.unwrap_or(4.0), |c| c.2).max(1.0),
            beats_per_bar: bar,
            sections: if sections.is_empty() { vec![(0.0, 4.0 * bar, true)] } else {
                let mut at = 0.0;
                sections.iter().map(|(_, beats, loops)| {
                    let s = (at, *beats, loops.contains(&l.name));
                    at += beats;
                    s
                }).collect()
            },
        }
    }).collect())
}

/// What an AI variant writer is told.
const VARY_SYSTEM: &str = "You rewrite one part of a piece of live-coded music: you are given the part's notes for one \
cycle as `[[beat note dur vel] ...]` (beats from the cycle's start, note names like @a2, drum names like @kick, durations \
in beats, velocities 0..1), the cycle's length, its key, and how to change it. Answer with only the new notes in the same \
form, one list, nothing else: no prose, no code fences. Keep the cycle's length, the key, the register and the part's \
character unless the ask says otherwise; change only the bars you are told to. Make it musical: a phrase that breathes, \
strong beats where the groove wants them.";

impl Desk {
    /// The tree's voices, made again when the score changes.
    pub(super) fn pump_tracks(&mut self, cx: &mut Cx) {
        let Some(applied) = &self.loads.applied else { return };
        let revision = applied.revision;
        let desk = &mut self.vary;
        if desk.tracks_job.as_ref().is_some_and(|(r, _)| *r != revision) {
            if let Some((_, mut job)) = desk.tracks_job.take() { job.cancel(); }
        }
        if !self.loads.diags.is_empty() { return; }
        if desk.tracks_rev != revision && desk.tracks_job.is_none() {
            let Some(shaders) = self.loads.shaders.clone() else { return };
            let source = applied.source.clone();
            match RequestJob::submit(cx, PrepareControl::default(), move |word| async move { tracks_of_async(&source, &shaders, &word).await }) {
                Ok(job) => desk.tracks_job = Some((revision, job)),
                Err(error) => log!("score tracks: {error}"),
            }
        }
        if let Some(result) = desk.tracks_job.as_mut().and_then(|(_, job)| job.try_take()) {
            desk.tracks_job = None;
            desk.tracks_rev = revision;
            match result {
                Ok(Ok(tracks)) => { desk.tracks = tracks; makepad_editor_panel::changed(); }
                Ok(Err(LoadError::Problems(_))) => { desk.tracks.clear(); makepad_editor_panel::changed(); }
                Ok(Err(LoadError::Cancelled)) => {}
                Ok(Err(error @ (LoadError::Unfinished { .. } | LoadError::Memory { .. }))) => makepad_widgets::log!("score tracks: {error}"),
                Err(error) => log!("score tracks: {error}"),
            }
        }
        if desk.tracks_rev != revision { self.frame = cx.new_next_frame(); }
    }

    pub(super) fn vary_action(&mut self, cx: &mut Cx, node: &str, action: Option<VaryAction>, region: Option<(f64, f64)>, reply: Option<u64>) -> Result<(), String> {
        let applied = self.loads.applied.as_ref().filter(|r| r.revision == self.revision && self.loads.diags.is_empty())
            .ok_or("the current score has not loaded")?;
        let revision = applied.revision;
        if matches!(action, Some(VaryAction::Make { .. })) { self.vary.stop_writers(); }
        let pending = &mut self.vary.requests;
        pending.retain_mut(|old| {
            if !same_kind(&old.action, &action) { return true; }
            if let Some(mut job) = old.job.take() { job.cancel(); }
            if let Some(id) = old.reply { self.jobs.panel_answer(id, Err("superseded".into())); }
            false
        });
        let voice = self.vary.voices.entry(node.into()).or_default();
        if voice.ai.is_none() {
            voice.busy = None;
        }
        pending.push_back(VariationRequest { revision, node: node.into(), action, region, reply, word: PrepareControl::default(), job: None });
        self.frame = cx.new_next_frame();
        makepad_editor_panel::changed();
        Ok(())
    }

    pub(super) fn pump_panel_actions(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(mut request) = self.vary.requests.pop_front() else { return };
        if self.revision != request.revision {
            if let Some(mut job) = request.job { job.cancel(); }
            if let Some(id) = request.reply { self.jobs.panel_answer(id, Err("superseded".into())); }
        } else {
            // A request that cannot start fails at once: it never waits on a closed pool.
            let mut failed = None;
            if request.job.is_none() {
                let started = match (self.loads.applied.as_ref(), self.loads.shaders.clone()) {
                    (Some(applied), Some(shaders)) => {
                        let source = applied.source.clone();
                        let node = request.node.clone();
                        let action = request.action.clone();
                        let voice = self.vary.voices.entry(node.clone()).or_default();
                        let previous = voice.original.clone();
                        let cards = voice.cards.iter().map(|c| c.1.clone()).collect();
                        let (seed, region, requested_region) = (voice.seed, voice.region, request.region);
                        RequestJob::submit(cx, request.word.clone(), move |control| async move {
                            prepare_action(&control, &source, &shaders, &node, action, previous, cards, seed, region, requested_region).await
                        }).map_err(|error| error.to_string())
                    }
                    _ => Err("the current score has not loaded".to_string()),
                };
                match started {
                    Ok(job) => request.job = Some(job),
                    Err(why) => failed = Some(why),
                }
            }
            let outcome = match failed {
                Some(why) => Some(Err(why)),
                None => request.job.as_mut().and_then(RequestJob::try_take).map(|result| result.map_err(|e| e.to_string()).and_then(|r| r)),
            };
            if let Some(result) = outcome {
                let answer = result
                    .and_then(|(part, action)| self.finish_variation(cx, ui, &request.node, part, action, request.region));
                if let Some(id) = request.reply {
                    use makepad_strict_json as json;
                    let revision = self.revision;
                    self.jobs.panel_answer(id, answer.map(|done| json::obj(vec![
                        ("done", json::s(done)), ("revision", json::Value::Int(revision as i64))])));
                } else if let Err(why) = answer {
                    self.vary.voices.entry(request.node).or_default().busy = Some(why);
                }
                makepad_editor_panel::changed();
            } else {
                self.vary.requests.push_front(request);
            }
        }
        if !self.vary.requests.is_empty() { self.frame = cx.new_next_frame(); }
    }

    fn finish_variation(&mut self, cx: &mut Cx, ui: &WidgetRef, node: &str, now: Part, action: PreparedAction, region: Option<(f64, f64)>) -> Result<String, String> {
        let voice = self.vary.voices.entry(node.to_owned()).or_default();
        if !voice.original.as_ref().is_some_and(|o| same_notes(o, &now)) {
            voice.original = Some(now.clone());
            voice.cards.clear();
            voice.playing = None;
            voice.ai = None;
            voice.busy = None;
            voice.region = None;
            voice.seed = voice.seed.max(1);
        }
        if voice.ai.is_none() { voice.busy = None; }
        if let Some(region) = region { voice.region = Some(region); }
        let original = now;
        match action {
            PreparedAction::Selected => Ok("selected".into()),
            PreparedAction::Cards(made) => {
                voice.seed += 1;
                let labels: Vec<String> = made.iter().map(|(l, _)| l.clone()).collect();
                voice.cards.extend(made.into_iter().map(|(l, p)| (l, p, false)));
                Ok(format!("made {}", labels.join(", ")))
            }
            PreparedAction::Controls(VaryAction::Make { kind, ai: true, n }) => {
                let (root, scale) = vary::key_of(&original);
                let key = format!("{} {}", makepad_score_live::theory::note_name(root), if scale == makepad_score_live::theory::scale_kind("major").unwrap() { "major" } else { "minor" });
                let bars = match voice.region {
                    Some((a, b)) => format!("bars {}-{} only (beats {}-{}); the rest exactly as it is", (a / original.beats_per_bar) as usize + 1, (b / original.beats_per_bar) as usize, a, b),
                    None => "the whole part".into(),
                };
                let n = n.clamp(1, 6);
                let cancel = Arc::new(AtomicBool::new(false));
                let monitor = TurnMonitor::default();
                let texts: Vec<String> = (0..n).map(|k| format!("The part ({} beats a cycle, {} beats a bar, in {key}):\n{}\nChange {bars}: {kind}. This is variant {} of {n}: make it different from the others, and change the notes (not only their loudness or length).", original.beats, original.beats_per_bar, original.text(), k + 1)).collect();
                let jobs = texts.iter().map(|text| ask_variant(cx, text.clone(), cancel.clone(), monitor.clone()).map(Some)).collect::<Result<_, _>>()?;
                voice.busy = Some(format!("the agent is writing {n} variants: {kind}"));
                voice.ai = Some(AiAsk { started: Cx::time_now(), kind, jobs, revision: self.revision, tries: vec![1; n], done: (0..n).map(|_| None).collect(), texts, cancel, monitor });
                self.frame = cx.new_next_frame();
                Ok(format!("asked for {n}"))
            }
            PreparedAction::Controls(VaryAction::Region(region)) => {
                voice.region = region;
                Ok("region set".into())
            }
            PreparedAction::Controls(VaryAction::Keep(i)) => {
                if let Some(card) = i.checked_sub(1).and_then(|k| voice.cards.get_mut(k)) {
                    card.2 = !card.2;
                }
                Ok("kept".into())
            }
            PreparedAction::Controls(VaryAction::Clear) => {
                voice.cards.retain(|c| c.2);
                Ok("cleared".into())
            }
            PreparedAction::Controls(VaryAction::Audition(None)) => {
                voice.playing = None;
                self.vary_stop();
                Ok("stopped".into())
            }
            PreparedAction::Audition(i, prepared) => {
                let (next, shaders) = prepared.unzip();
                for v in self.vary.voices.values_mut() {
                    v.playing = None;
                }
                if let Some(v) = self.vary.voices.get_mut(node) {
                    v.playing = Some(i);
                }
                self.ensure_live();
                self.send();
                self.variant_live = next;
                self.variant_shaders = shaders;
                self.send();
                self.vary.live = Some((node.to_string(), i));
                // Stopped: the song starts where the voice first plays.
                if !self.engine.playing() {
                    let start = self.doc.as_ref().and_then(|d| makepad_score_live::audition::first_beat(d, &[node.to_string()]));
                    if let (Some(live), Some(beat)) = (&self.live, start) {
                        live.seek(beat, 0.0);
                    }
                    self.transport(true);
                    self.vary.started_song = true;
                }
                Ok(format!("{node} plays card {i} in place"))
            }
            PreparedAction::Write(part, next) => self.vary_write(cx, ui, node, &part, next),
            PreparedAction::Controls(_) => unreachable!("worker prepared the rewrite"),
        }
    }

    /// The audition ends: the composer plays the document again, and the
    /// song stops when the audition started it.
    fn vary_stop(&mut self) {
        self.vary.live = None;
        self.variant_live = None;
        self.variant_shaders = None;
        self.send();
        if std::mem::take(&mut self.vary.started_song) {
            self.transport(false);
        }
    }

    /// A voice made to play `part`: one undo step; its variants start from it.
    fn vary_write(&mut self, cx: &mut Cx, ui: &WidgetRef, node: &str, part: &Part, next: String) -> Result<String, String> {
        // What plays in place now is written: the song plays on with it.
        self.vary.live = None;
        self.vary.started_song = false;
        for v in self.vary.voices.values_mut() {
            v.playing = None;
        }
        self.set_source(cx, ui, next);
        if let Some(voice) = self.vary.voices.get_mut(node) {
            voice.original = Some(part.clone());
            voice.cards.retain(|c| c.2);
            voice.playing = None;
        }
        Ok(format!("{node} plays it now (revision {})", self.revision))
    }

    /// The AI's variants and the auditions as they come.
    pub(super) fn pump_vary(&mut self, cx: &mut Cx) {
        for voice in self.vary.voices.values_mut() {
            if voice.ai.as_ref().is_some_and(|a| a.revision != self.revision) { voice.ai = None; voice.busy = None; }
            let Some(ask) = voice.ai.as_mut() else { continue };
            let Some(original) = voice.original.clone() else {
                voice.ai = None;
                continue;
            };
            // Each answer as it comes: one that changes no note, or repeats
            // another card, is asked again (twice at most).
            for k in 0..ask.jobs.len() {
                let Some(result) = ask.jobs[k].as_mut().and_then(FrameJob::try_take) else { continue };
                ask.jobs[k] = None;
                let answer = result.map_err(|e| e.to_string()).and_then(|r| r);
                let result = answer.and_then(|text| vary::parse_notes(&text, &original));
                let repeat = match &result {
                    Ok(part) => same_notes(part, &original) || voice.cards.iter().any(|c| same_notes(&c.1, part)) || ask.done.iter().flatten().flatten().any(|d| same_notes(d, part)),
                    Err(_) => false,
                };
                if (repeat || result.is_err()) && ask.tries[k] < 3 {
                    ask.tries[k] += 1;
                    let why = if repeat { "That answer changed no note of the part (or repeated another variant): change the notes themselves.".to_string() } else { format!("That answer could not be read ({}): answer with only the list.", result.as_ref().err().cloned().unwrap_or_default()) };
                    match ask_variant(cx, format!("{}\n{why}", ask.texts[k]), ask.cancel.clone(), ask.monitor.clone()) {
                        Ok(job) => ask.jobs[k] = Some(job),
                        Err(why) => ask.done[k] = Some(Err(why)),
                    }
                    continue;
                }
                ask.done[k] = Some(if repeat { Err("the same notes again".into()) } else { result });
            }
            let n = ask.done.len();
            if ask.done.iter().any(|d| d.is_none()) {
                voice.busy = Some(if ask.monitor.not_responding() {
                    "not responding — Stop?".into()
                } else {
                    format!("the agent is writing {n} variants: {} ({:.0} s)", ask.kind, Cx::time_now() - ask.started)
                });
                self.frame = cx.new_next_frame();
                continue;
            }
            let mut ask = voice.ai.take().expect("checked above");
            let secs = Cx::time_now() - ask.started;
            voice.busy = None;
            let mut made = 0;
            let mut why = None;
            let label: String = ask.kind.chars().take(24).collect();
            for (k, done) in ask.done.drain(..).enumerate() {
                match done.expect("all done") {
                    Ok(part) => {
                        made += 1;
                        voice.cards.push((format!("AI: {label} {}", k + 1), part, false));
                    }
                    Err(e) => why = Some(e),
                }
            }
            let retries: u8 = ask.tries.iter().map(|t| t - 1).sum();
            log!("score vary: {made} AI variants in {secs:.1} s ({retries} asked again){}", why.map(|w| format!(" (one failed: {w})")).unwrap_or_default());
            self.vary_ai_made = Some((made, secs));
        }
        // An edit ended the audition (the composer plays the document).
        if self.vary.live.as_ref().is_some_and(|(_, card)| *card > 0) && self.variant_live.is_none() {
            self.vary.live = None;
            self.vary.started_song = false;
            for v in self.vary.voices.values_mut() {
                v.playing = None;
            }
        }
    }
}
