//! One audio flow played without a live host: an app that is not Stage
//! (the film player, on the desktop and in the browser alike) loads a flow
//! document, and its audio slice runs in the same [`AudioEngine`] Stage
//! runs, with the same nodes and the same endpoints (a deck's `play`,
//! `speed`, `scratch`, `scratch_at`, `brake`, `fling`, `source`, its cues).
//! What a live host adds (revisions while running, many instances, owner
//! commands over the wire, a preparing thread) is not here: the flow is
//! fixed, prepared on the calling thread once the device's rate is known,
//! and again when that rate changes (decks keep their place).
//!
//! The host half ([`EmbeddedAudio`]) stays on the app's thread; the engine
//! goes to the device callback. They share the control bank (atomics), the
//! command rings and the published telemetry, as under the live host.

use crate::deck::{DeckCommand, DeckSource, DeckTrack, TrackLease};
use crate::engine::{AudioEngine, AudioProbePacket, Command, DeckObservation, Packet, Retired, Telemetry, AUDIO_PROBE_QUEUE};
use crate::paged::{PagePacket, PageReturn, PAGE_ROUTES};
use crate::schedule::{compile, AudioBudget};
use makepad_audio_engine::schedule::{ControlBank, Layout};
use makepad_flow::document::closure::Closure;
use makepad_flow::document::Registry;
use makepad_flow::engine::providers::ExecutorRegistry;
use makepad_flow::graph::FlowVm;
use makepad_flow::live::plan::{partition, OwnerSlice};
use makepad_lock_free::{ring, triple_buffer, Consumer, Producer, Reader};
use makepad_flow::ModuleSource;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// What a deck was loaded with, to load again after a new rate.
#[derive(Clone)]
enum Loaded {
    Clip(Arc<DeckTrack>),
    Feed(Arc<crate::feed::FeedRing>, bool),
}

/// A deck as the engine last rendered it: its observation at the end of
/// the callback that finished at `at` (the engine's clock, see
/// [`crate::clock`]), at the device's `rate`.
#[derive(Clone, Copy, Debug)]
pub struct DeckNow {
    pub deck: DeckObservation,
    pub at: Option<f64>,
    pub rate: u32,
    pub publication: u64,
}

pub struct EmbeddedAudio {
    slice: OwnerSlice,
    budget: Arc<AudioBudget>,
    layout: Option<Arc<Layout>>,
    /// The layout of the schedule the engine runs (the base of the next).
    active_layout: Option<Arc<Layout>>,
    bank: Option<Arc<ControlBank>>,
    /// What the schedule prepared last publishes.
    board: Option<crate::board::BoardReader>,
    /// The values written so far, by `node.param`: a schedule prepared for a
    /// new rate starts from them.
    written: Vec<(String, f32)>,
    /// Each deck's loaded clip (to load again after a new rate) by node.
    tracks: Vec<(String, u64, Loaded)>,
    format: Arc<AtomicU64>,
    prepared_format: u64,
    /// Clips wait for the schedule to run.
    unsent: bool,
    generation: u64,
    commands: Producer<Command>,
    retired: Consumer<Retired>,
    packets: Producer<Packet>,
    telemetry: Reader<Telemetry>,
    _page_commands: Producer<PagePacket>,
    _page_returns: Consumer<PageReturn>,
    _probes: Consumer<AudioProbePacket>,
}

/// The flow `source` (a Splash flow document at `path`, with the component
/// `modules` it uses), its audio slice to be played: the host half and the
/// engine for the device callback.
pub fn embedded(source: &str, path: &str, modules: Vec<ModuleSource>) -> Result<(EmbeddedAudio, AudioEngine), String> {
    let mut registry = Registry::with_builtins();
    let mut executors = ExecutorRegistry::default();
    crate::nodes::register(&mut registry, &mut executors)?;
    let closure = Closure { modules };
    let (_vm, graph) = FlowVm::load_with_registry(source, path, &closure, &registry).map_err(|e| format!("{path}: {e:?}"))?;
    let lowered = makepad_flow::engine::lower::lower(&graph).map_err(|e| format!("{path}: {e:?}"))?;
    let plan = partition(&lowered, &registry, &|owner| owner == crate::nodes::OWNER, crate::nodes::OWNER, 1).map_err(|e| format!("{path}: {e:?}"))?;
    let slice = plan.slices.into_iter().find(|slice| slice.owner == crate::nodes::OWNER).ok_or_else(|| format!("{path}: no audio nodes"))?;
    let (commands, command_queue) = ring::<Command>(8);
    let (retire, retired) = ring::<Retired>(8);
    let (packets, packet_queue) = ring::<Packet>(64);
    let (writer, telemetry) = triple_buffer(Telemetry::default());
    let (probe_writer, probes) = ring::<AudioProbePacket>(AUDIO_PROBE_QUEUE);
    let (page_commands, page_queue) = ring::<PagePacket>(PAGE_ROUTES);
    let (page_writer, page_returns) = ring::<PageReturn>(PAGE_ROUTES);
    let format = Arc::new(AtomicU64::new(0));
    let engine = AudioEngine::new(command_queue, retire, packet_queue, writer, probe_writer, format.clone(), page_queue, page_writer);
    let host = EmbeddedAudio {
        slice,
        budget: Arc::new(AudioBudget::default()),
        layout: None,
        active_layout: None,
        bank: None,
        board: None,
        written: Vec::new(),
        tracks: Vec::new(),
        format,
        prepared_format: 0,
        unsent: false,
        generation: 0,
        commands,
        retired,
        packets,
        telemetry,
        _page_commands: page_commands,
        _page_returns: page_returns,
        _probes: probes,
    };
    Ok((host, engine))
}

impl EmbeddedAudio {
    /// Prepare for the device's rate once the device has said it (and again
    /// when it changes), and drop what the engine hands back. Call it often
    /// (every display frame).
    pub fn pump(&mut self) -> Result<(), String> {
        // The schedule the engine runs: the base a next one is prepared on.
        let active = self.telemetry.latest().slots.first().map_or(0, |slot| slot.generation);
        if active != 0 && self.layout.as_ref().is_some_and(|layout| layout.generation == active) {
            self.active_layout = self.layout.clone();
        }
        let mut refused = None;
        while let Some(back) = self.retired.pop() {
            // The engine refused the schedule prepared last: made for a
            // format, or on a base, it no longer runs (a device that came up
            // at another rate than the first render's). Prepared again below.
            if let (Some(reason), Some(schedule)) = (back.rejected, &back.schedule) {
                if schedule.generation == self.generation {
                    refused = Some(reason);
                }
            }
        }
        if let Some(reason) = refused {
            // On the schedule the engine runs, or afresh when it runs none.
            self.layout = self.active_layout.clone().filter(|layout| active != 0 && layout.generation == active);
            self.prepared_format = 0;
            eprintln!(
                "embedded audio: the engine refused generation {} ({reason:?}); preparing it again {}",
                self.generation,
                if self.layout.is_some() { format!("on generation {active}") } else { "afresh".to_string() }
            );
        }
        // Clips go to a schedule once the engine runs it.
        if self.unsent && self.installed() {
            self.unsent = false;
            for (node, sequence, track) in self.tracks.clone() {
                self.send_track(&node, sequence, track)?;
            }
        }
        let format = self.format.load(Ordering::Acquire);
        if format == 0 || format == self.prepared_format {
            return Ok(());
        }
        self.generation += 1;
        let (compiled, board) = compile(&self.slice, self.generation, 1, format, self.layout.as_deref(), &self.budget).map_err(|e| format!("the audio flow: {e:?}"))?;
        self.board = Some(board);
        for (name, value) in &self.written {
            if let Some((index, _)) = compiled.layout.controls.get(name) {
                compiled.schedule.controls.set_manual(*index, *value);
            }
        }
        let carried = self.layout.is_some();
        self.layout = Some(compiled.layout.clone());
        self.bank = Some(compiled.schedule.controls.clone());
        self.prepared_format = format;
        self.commands.push(Command::Install { slot: 0, schedule: compiled.schedule }).map_err(|_| "the audio engine's command ring is full".to_string())?;
        // A deck prepared afresh (not carried from an earlier rate) loads
        // its clip again, once the schedule runs.
        self.unsent |= !carried;
        Ok(())
    }

    fn installed(&mut self) -> bool {
        let generation = self.generation;
        self.telemetry.latest().slots.first().is_some_and(|slot| generation != 0 && slot.generation == generation)
    }

    /// The device's rate, once it has rendered.
    pub fn rate(&self) -> Option<u32> {
        let format = self.format.load(Ordering::Acquire);
        (format != 0).then_some(format as u32)
    }

    /// Whether the flow is prepared and running on a device.
    pub fn running(&self) -> bool {
        self.layout.is_some()
    }

    /// Write `node.param` (a control endpoint: a number, a toggle as 0/1).
    pub fn set(&mut self, node: &str, param: &str, value: f32) {
        let name = format!("{node}.{param}");
        if let (Some(layout), Some(bank)) = (&self.layout, &self.bank) {
            if let Some((index, _)) = layout.controls.get(&name) {
                bank.set_manual(*index, value);
            }
        }
        match self.written.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 = value,
            None => self.written.push((name, value)),
        }
    }

    /// Reserve the PCM bytes of a clip before decoding it.
    pub fn reserve_track(&self, bytes: usize) -> Result<TrackLease, String> {
        TrackLease::reserve(&self.budget, bytes)
    }

    /// Load `track` on `node` (an `AudioDeck`); `sequence` increases per deck.
    pub fn load(&mut self, node: &str, command: DeckCommand) -> Result<(), String> {
        let (sequence, source) = match command {
            DeckCommand::Load { sequence, track } => (sequence, Loaded::Clip(track)),
            DeckCommand::LoadFeed { sequence, ring, ramp } => (sequence, Loaded::Feed(ring, ramp)),
            _ => return Err("an embedded deck loads clips and feeds".into()),
        };
        self.tracks.retain(|(n, ..)| n != node);
        self.tracks.push((node.to_string(), sequence, source.clone()));
        if self.installed() {
            self.send_track(node, sequence, source)?;
        } else {
            self.unsent = true;
        }
        Ok(())
    }

    fn send_track(&mut self, node: &str, sequence: u64, source: Loaded) -> Result<(), String> {
        let layout = self.layout.as_ref().ok_or("not prepared")?;
        let (target, continuity) = *layout.triggers.get(node).ok_or_else(|| format!("no AudioDeck {node}"))?;
        let track = match source {
            Loaded::Clip(track) => DeckSource::Clip(track),
            Loaded::Feed(ring, ramp) => DeckSource::Feed(Box::new(crate::feed::FeedSource::new(ring, ramp))),
        };
        self.commands
            .push(Command::Track { slot: 0, incarnation: 1, target, continuity, sequence, track: Some(track) })
            .map_err(|_| "the audio engine's command ring is full".to_string())
    }

    /// Jump `node`'s clip to `seconds` (under the deck's dip).
    pub fn cue(&mut self, node: &str, seconds: f64) -> Result<(), String> {
        let Some(layout) = &self.layout else { return Ok(()) };
        let (target, continuity) = *layout.triggers.get(node).ok_or_else(|| format!("no AudioDeck {node}"))?;
        let Some((_, sequence, _)) = self.tracks.iter().find(|(n, ..)| n == node) else { return Ok(()) };
        self.packets
            .push(Packet::Seek { slot: 0, incarnation: 1, target, continuity, sequence: *sequence, seconds })
            .map_err(|_| "the audio engine's packet ring is full".to_string())
    }

    /// `node`'s deck as last rendered.
    pub fn deck(&mut self, node: &str) -> Option<DeckNow> {
        let index = *self.layout.as_ref()?.decks.get(node)? as usize;
        if self.telemetry.latest().slots.first()?.rate_mismatch {
            return None;
        }
        let board = self.board.as_mut()?.latest();
        if board.publication == 0 {
            return None;
        }
        Some(DeckNow { deck: *board.decks.get(index)?, at: board.at, rate: board.rate, publication: board.publication })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_stage_audio_media::pcm::TrackPcm;

    const FLOW: &str = r#"
use mod.flow.*
let deck = AudioDeck{ play: 0 speed: 1 }
let out = AudioOutput{ enabled: 1 audio: deck.out(@audio) }
Flow{ label: "Film" deck, out }
"#;

    fn render(engine: &mut AudioEngine, seconds: f64) -> Vec<f32> {
        let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let mut out = Vec::new();
        for _ in 0..(seconds * 48_000.0 / 256.0).round() as usize {
            engine.render(48_000.0, &mut left, &mut right);
            out.extend_from_slice(&left);
        }
        out
    }

    #[test]
    fn a_flow_deck_plays_and_brakes_under_a_touch() {
        let (mut host, mut engine) = embedded(FLOW, "film/audio.splash", Vec::new()).unwrap();
        // Prepared once the device has run.
        render(&mut engine, 0.01);
        host.pump().unwrap();
        assert!(host.running());
        let frames: Vec<[i16; 2]> = (0..48_000 * 10).map(|i| { let v = ((i as f64 / 48_000.0 * 220.0 * std::f64::consts::TAU).sin() * 12_000.0) as i16; [v, v] }).collect();
        let lease = host.reserve_track(frames.capacity() * 4).unwrap();
        let track = lease.finish(TrackPcm { frames, sample_rate: 48_000 }).unwrap();
        host.load("deck", DeckCommand::Load { sequence: 1, track }).unwrap();
        host.set("deck", "play", 1.0);
        render(&mut engine, 0.01);
        host.pump().unwrap();
        let out = render(&mut engine, 1.0);
        host.pump().unwrap();
        assert!(out.iter().any(|v| v.abs() > 0.1), "the deck sounds");
        let now = host.deck("deck").unwrap();
        let t = now.deck.position / 48_000.0;
        assert!(t > 0.9 && t < 1.01, "played to {t}");
        // A braking touch: the record slows to a stop.
        host.set("deck", "brake", 1.0);
        host.set("deck", "scratch_at", 5.0);
        host.set("deck", "scratch", 1.0);
        render(&mut engine, 0.5);
        let held = host.deck("deck").unwrap().deck.position / 48_000.0;
        render(&mut engine, 0.5);
        let still = host.deck("deck").unwrap().deck.position / 48_000.0;
        assert!((held - still).abs() < 1e-3, "stopped under the hand: {held} {still}");
        assert!(held - t > 0.05 && held - t < 0.2, "braked over {}", held - t);
        // A cue jumps.
        host.set("deck", "scratch", 0.0);
        host.cue("deck", 4.0).unwrap();
        render(&mut engine, 0.2);
        let cued = host.deck("deck").unwrap().deck.position / 48_000.0;
        assert!(cued > 4.0 && cued < 4.25, "cued to {cued}");
    }

    /// No guessed bound: more decks than the old eight, a mixer with more
    /// than the old sixteen inputs, and their observations all published.
    #[test]
    fn a_graph_takes_as_many_decks_and_mixer_inputs_as_it_names() {
        let n = 24;
        let decks: String = (0..n).map(|i| format!("let deck{i} = AudioDeck{{ play: 0 speed: 1 }}\n")).collect();
        let inputs: Vec<String> = (0..n).map(|i| format!("conn({}, deck{i}.out(@audio))", i + 1)).collect();
        let names: Vec<String> = (0..n).map(|i| format!("deck{i}")).collect();
        let flow = format!("use mod.flow.*\n{decks}let mix = AudioMixer{{ master: 1 inputs: [{}] }}\nlet out = AudioOutput{{ enabled: 1 audio: mix.out(@audio) }}\nFlow{{ label: \"Many\" {}, mix, out }}\n", inputs.join(", "), names.join(", "));
        let (mut host, mut engine) = embedded(&flow, "many.splash", Vec::new()).unwrap();
        render(&mut engine, 0.01);
        host.pump().unwrap();
        assert!(host.running());
        let frames = vec![[8000i16, 8000]; 48_000];
        let lease = host.reserve_track(frames.capacity() * 4).unwrap();
        let track = lease.finish(TrackPcm { frames, sample_rate: 48_000 }).unwrap();
        render(&mut engine, 0.01);
        host.pump().unwrap();
        host.load(&format!("deck{}", n - 1), DeckCommand::Load { sequence: 1, track }).unwrap();
        host.set(&format!("deck{}", n - 1), "play", 1.0);
        render(&mut engine, 0.01);
        host.pump().unwrap();
        let out = render(&mut engine, 0.2);
        assert!(out.iter().any(|v| v.abs() > 0.1), "the last deck sounds through the wide mixer");
        let last = host.deck(&format!("deck{}", n - 1)).unwrap();
        assert!(last.deck.position > 0.0);
        assert!(host.deck("deck0").is_some());
    }

    const VERB_FLOW: &str = "use mod.flow.*\nlet deck = AudioDeck{ play: 0 speed: 1 }\nlet verb = Reverb{ room: 0.6 mix: 0.5 audio: deck.out(@audio) }\nlet out = AudioOutput{ enabled: 1 audio: verb.out(@audio) }\nFlow{ label: \"Verb\" deck, verb, out }\n";

    fn verb_clip(host: &mut EmbeddedAudio, sequence: u64) {
        let frames: Vec<[i16; 2]> = (0..9_600).map(|i| { let v = ((i as f64 / 48_000.0 * 440.0 * std::f64::consts::TAU).sin() * 12_000.0) as i16; [v, v] }).collect();
        let lease = host.reserve_track(frames.capacity() * 4).unwrap();
        let track = lease.finish(TrackPcm { frames, sample_rate: 48_000 }).unwrap();
        host.load("deck", DeckCommand::Load { sequence, track }).unwrap();
    }

    fn resting(host: &mut EmbeddedAudio) -> bool {
        host.telemetry.latest().slots[0].resting
    }

    /// Play a 0.2 s clip through a reverb, let `wait` seconds pass after it
    /// (play stays set), load a second clip and return the peaks of the
    /// buffers that follow, with the blocks that were audible, whether it
    /// rested before the second clip, and the audio before the rest.
    fn verb_run(wait: f64, rest: bool) -> (Vec<f32>, bool, Vec<f32>) {
        let (mut host, mut engine) = embedded(VERB_FLOW, "verb.splash", Vec::new()).unwrap();
        engine.set_resting(rest);
        render(&mut engine, 0.01);
        host.pump().unwrap();
        assert!(host.running());
        verb_clip(&mut host, 1);
        render(&mut engine, 0.01);
        host.pump().unwrap();
        host.set("deck", "play", 1.0);
        let played = render(&mut engine, 0.3);
        assert!(played.iter().any(|v| v.abs() > 0.1), "the deck sounds");
        assert!(!resting(&mut host));
        let mut all = Vec::new();
        let mut rested = false;
        for block in 0..(wait * 48_000.0 / 256.0) as usize {
            all.extend(render(&mut engine, 256.0 / 48_000.0));
            if block % 8 == 0 { host.pump().unwrap(); }
            rested |= resting(&mut host);
        }
        verb_clip(&mut host, 2);
        let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let mut peaks = Vec::new();
        for _ in 0..6 {
            engine.render(48_000.0, &mut left, &mut right);
            peaks.push(left.iter().fold(0.0f32, |m, v| m.max(v.abs())));
        }
        (peaks, rested, all)
    }

    /// A deck through a reverb: the engine rests once the tail has decayed
    /// below audibility (not before, nothing cut short), and a clip that
    /// starts after the rest sounds exactly as it does from an engine that
    /// never rested.
    #[test]
    fn a_silent_flow_rests_after_its_tail_and_wakes_on_play() {
        let (awake, rested, _) = verb_run(12.0, false);
        assert!(!rested, "an engine told not to rest never does");
        let (_, rested, _) = verb_run(0.5, true);
        assert!(!rested, "a tail still ringing does not rest");
        let (woken, rested, tail) = verb_run(12.0, true);
        assert!(rested, "the engine rests when the tail is gone");
        let audible: Vec<usize> = tail.chunks(256).enumerate().filter(|(_, b)| b.iter().any(|v| v.abs() >= crate::engine::REST_SILENCE)).map(|(i, _)| i).collect();
        assert!(audible.len() > 48_000 / 256, "the tail rings for over a second ({} blocks)", audible.len());
        let last = *audible.last().unwrap();
        // It rang out to the end: the last audible block is a faded one.
        let end = tail[last * 256..(last + 1) * 256].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(end < 1e-4, "the tail faded out before the rest: {end}");
        // The clip loaded from rest is heard from its first buffer, with
        // the level of an engine that was awake all along.
        for (a, w) in awake.iter().zip(&woken) {
            assert!((a - w).abs() < 1e-3, "awake {awake:?} woken {woken:?}");
        }
        assert!(woken.iter().any(|p| *p > 0.1), "the new clip sounds: {woken:?}");
    }
}
