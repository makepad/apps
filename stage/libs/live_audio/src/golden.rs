//! A transport clocks a single `Score` node feeding a mixer and audio output.
//! The tests lower, partition, compile and render this flow through the
//! live engine. An empty score slot yields silence. A populated slot
//! plays the score player's samples at the mixer's 0.8 master level.

use crate::engine::{AudioEngine, AudioProbePacket, Command, Packet, Retired, Telemetry, AUDIO_PROBE_QUEUE};
use crate::paged::{PagePacket, PageReturn, PAGE_ROUTES};
use crate::schedule::{compile, AudioBudget};
use makepad_flow::document::closure::Closure;
use makepad_flow::document::Registry;
use makepad_flow::engine::providers::ExecutorRegistry;
use makepad_flow::graph::FlowVm;
use makepad_flow::live::plan::partition;
use makepad_lock_free::{ring, triple_buffer};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

const ROOT: &str = include_str!("../fixtures/root.flow.splash");
const FORMAT: u64 = (1u64 << 32) | 48_000;

/// Render the root for `frames` frames with the transport playing and the
/// output gate open; the output samples, left and right interleaved.
pub(crate) fn render_root(frames: usize) -> Vec<f32> {
    render_root_with(frames, 1.0).0
}

/// `render_root` with the transport at `playing`; also the time the render
/// calls took.
pub(crate) fn render_root_with(frames: usize, playing: f32) -> (Vec<f32>, std::time::Duration) {
    render_root_fed(ROOT, frames, playing, None, &mut |_| {})
}

/// `render_root_with`, the transport at `bpm` when given, `feed(frame)`
/// called before each buffer.
fn render_root_fed(root: &str, frames: usize, playing: f32, bpm: Option<f32>, feed: &mut dyn FnMut(usize)) -> (Vec<f32>, std::time::Duration) {
    let (out, spent, _) = render_root_counted(root, frames, playing, bpm, true, feed);
    (out, spent)
}

/// `render_root_fed` with resting on or off for this engine; also the
/// passes the engine skipped while resting, and the buffers rendered.
fn render_root_counted(root: &str, frames: usize, playing: f32, bpm: Option<f32>, resting: bool, feed: &mut dyn FnMut(usize)) -> (Vec<f32>, std::time::Duration, (u64, u64)) {
    let mut registry = Registry::with_builtins();
    let mut executors = ExecutorRegistry::default();
    crate::nodes::register(&mut registry, &mut executors).unwrap();
    let closure = Closure { modules: Vec::new() };
    let (_vm, graph) = FlowVm::load_with_registry(root, "root.flow.splash", &closure, &registry).unwrap();
    let lowered = makepad_flow::engine::lower::lower(&graph).unwrap();
    let plan = partition(&lowered, &registry, &|owner| owner == crate::nodes::OWNER, "stage.audio", 1).unwrap();
    let slice = plan.slices.iter().find(|slice| slice.owner == crate::nodes::OWNER).unwrap();
    let budget = Arc::new(AudioBudget::default());
    let (compiled, _board) = compile(slice, 1, 1, FORMAT, None, &budget).unwrap();
    let (bank, layout) = (compiled.schedule.controls.clone(), compiled.layout.clone());
    for (name, value) in [("transport.playing", playing), ("out.enabled", 1.0)] {
        bank.set_manual(layout.controls[name].0, value);
    }
    if let Some(bpm) = bpm {
        bank.set_manual(layout.controls["transport.bpm"].0, bpm);
    }
    let (mut commands, command_queue) = ring::<Command>(8);
    let (retire, mut retired) = ring::<Retired>(8);
    let (_packets, packet_queue) = ring::<Packet>(8);
    let (writer, _telemetry) = triple_buffer(Telemetry::default());
    let (audio_writer, _audio) = ring::<AudioProbePacket>(AUDIO_PROBE_QUEUE);
    let (_pages, page_queue) = ring::<PagePacket>(PAGE_ROUTES);
    let (page_writer, _page_returns) = ring::<PageReturn>(PAGE_ROUTES);
    let mut engine = AudioEngine::new(command_queue, retire, packet_queue, writer, audio_writer, Arc::new(AtomicU64::new(FORMAT)), page_queue, page_writer);
    engine.set_resting(resting);
    assert!(commands.push(Command::Install { slot: 0, schedule: compiled.schedule }).is_ok());
    let mut out = Vec::with_capacity(frames * 2);
    let mut buffers = 0u64;
    let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    let mut done = 0;
    let mut spent = std::time::Duration::ZERO;
    while done < frames {
        let n = 256.min(frames - done);
        feed(done);
        let start = std::time::Instant::now();
        engine.render(48_000.0, &mut left[..n], &mut right[..n]);
        spent += start.elapsed();
        buffers += 1;
        for i in 0..n {
            out.push(left[i]);
            out.push(right[i]);
        }
        done += n;
        while retired.pop().is_some() {}
    }
    (out, spent, (engine.rested_passes(), buffers))
}

fn fingerprint(samples: &[f32]) -> u64 {
    samples.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, s| (h ^ s.to_bits() as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

#[test]
fn the_audio_root_prepares_and_is_silent_without_a_score() {
    let out = render_root(96_000);
    let peak = out.iter().fold(0.0f32, |a, x| a.max(x.abs()));
    eprintln!("audio root: peak {peak} fingerprint {:016x}", fingerprint(&out));
    assert_eq!(peak, 0.0);

    #[cfg(not(target_arch = "wasm32"))]
    supersede_playing_root();
}

#[cfg(not(target_arch = "wasm32"))]
fn supersede_playing_root() {
    use makepad_flow::document::ident::InstanceId;
    use makepad_flow::live::{ApplyRevision, EndpointValue, LiveEvent, LiveHost};
    use std::time::{Duration, Instant};

    let mut registry = Registry::with_builtins();
    crate::nodes::register(&mut registry, &mut ExecutorRegistry::default()).unwrap();
    let registry = Arc::new(registry);
    let (owner, runtime, device) = crate::audio_owner(Some(48_000));
    let mut host = LiveHost::new(registry.clone(), vec![(owner, runtime)], Default::default()).unwrap();
    let instance = InstanceId("root".into());
    let apply = |host: &mut LiveHost, revision, value: &str| {
        let source = format!(r#"use mod.flow.*
let tone = ShaderFx{{shader: "fn effect(l,r) {{ {value} }}"}}
let level = Gain{{audio: tone.out(@audio) gain: 1}}
let out = AudioOutput{{enabled: 1 audio: level.out(@audio)}}
Flow{{tone, level, out}}"#);
        let graph = makepad_flow::graph::evaluate_with_registry(&source, "root.splash", &Closure { modules: vec![] }, &registry).unwrap();
        host.apply(ApplyRevision { instance: instance.clone(), definition: "root".into(), revision, authored: Arc::new(graph) }).unwrap();
    };
    let installed = |host: &mut LiveHost, wanted| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let events = host.pump();
            if let [LiveEvent::Installed { generation, revision, .. }] = &events[..] {
                assert_eq!(*revision, wanted, "only the newest revision installs");
                return *generation;
            }
            assert!(events.is_empty(), "unexpected root events: {events:?}");
            assert!(Instant::now() < deadline, "root revision {wanted} did not install");
            std::thread::yield_now();
        }
    };
    let sample = || {
        let (mut l, mut r) = ([0.0; 256], [0.0; 256]);
        device.render_offline(48_000, &mut l, &mut r).unwrap();
        l[255]
    };
    apply(&mut host, 1, "0.25");
    let original = installed(&mut host, 1);
    host.write(&instance, "level", "gain", EndpointValue::Number(0.5)).unwrap();
    sample();
    let audible = sample();
    assert!(audible > 0.0);
    apply(&mut host, 2, "unknown_value");
    assert!(host.pump().is_empty());
    assert_eq!(host.status(&instance).unwrap().pending.map(|(_, rev, phase)| (rev, phase)), Some((2, "preparing")));
    apply(&mut host, 3, "-0.25");
    assert_eq!(host.status(&instance).unwrap().running, Some((original, 1)));
    assert_eq!(host.control(&instance, "level", "gain"), Some(0.5));
    assert_eq!(sample(), audible, "the original source and its control stay audible during replacement");
    assert_ne!(installed(&mut host, 3), original);
    assert_eq!(host.control(&instance, "level", "gain"), Some(0.5));
    sample();
    assert_eq!(sample(), -audible, "the newest valid shader reaches the device");
}

/// With the transport stopped and the gate open, the engine
/// rests once it has been silent for a hold, skipping its graph, and changes
/// no sample. Counts the skipped passes (the timings are only printed).
#[test]
fn the_idle_root_rests_after_its_hold_and_changes_no_sample() {
    let seconds = 30;
    let frames = 48_000 * seconds;
    let (always, always_time, (always_skipped, buffers)) = render_root_counted(ROOT, frames, 0.0, None, false, &mut |_| {});
    let (rests, rests_time, (skipped, rests_buffers)) = render_root_counted(ROOT, frames, 0.0, None, true, &mut |_| {});
    eprintln!("idle root, {seconds} s of audio: graph always run {always_time:?}, resting {rests_time:?}; {skipped} of {rests_buffers} passes skipped");
    assert_eq!(fingerprint(&always), fingerprint(&rests), "resting changes no sample");
    assert_eq!(buffers, rests_buffers);
    assert_eq!(always_skipped, 0, "with resting off no pass is skipped");
    let hold_passes = (makepad_audio_engine::rest::HOLD_SECONDS * 48_000.0 / 256.0).ceil() as u64;
    assert!(skipped + hold_passes + 1 >= buffers, "after the hold nearly every pass is skipped: {skipped} of {buffers}, hold {hold_passes}");
    assert!(skipped <= buffers - hold_passes + 1, "no pass inside the hold is skipped: {skipped} of {buffers}, hold {hold_passes}");
}

/// The SCORE page's path: a score in the `score` slot plays through the
/// root's `Score` node, on the root's transport, as the score's own player
/// renders it through the flow mixer's 0.8 master once the gain has ramped in.
#[test]
fn the_audio_root_plays_the_score_slot_through_its_score_node() {
    use makepad_audio_engine::feed::{automation_feed, loop_feed, tempo_feed};
    use makepad_score_live::compose::Composer;
    let source = makepad_score_live::templates::template("techno").unwrap();
    let loaded = makepad_score_live::model::load_controlled(source, &makepad_score_live::PrepareControl::default()).unwrap();
    let doc = loaded.doc.clone();
    let bpm = makepad_score_live::render::transport_bpm(&doc);
    // Its own slot (the other tests' roots play `score`, empty).
    let root = ROOT.replace(r#"score: "score""#, r#"score: "golden_score""#);
    assert_ne!(root, ROOT);
    makepad_score_live::node::slot("golden_score").set_doc(doc.clone()).unwrap();
    // The same composer feeds the slot's loops and the reference player's.
    let reference = makepad_score_live::render::render(source, &makepad_score_live::RenderOpts { seconds: Some(3.0), ..Default::default() }, &makepad_score_live::NoSamples, &makepad_score_live::PrepareControl::default()).unwrap();
    let mut composer = Composer::new(loaded.into_evaluated());
    composer.wait_for_phrases = true;
    let mut events = Vec::new();
    let frames = 3 * 48_000;
    let latency = reference.latency as usize;
    let (out, _) = render_root_fed(&root, frames + latency, 1.0, Some(bpm as f32), &mut |done| {
        let beat = done as f64 / 48_000.0 * bpm / 60.0;
        composer.pump(beat + 256.0 * 400.0 / 60.0 / 48_000.0 + 1e-9, &mut events);
        composer.osc_out.clear();
        composer.history.clear();
        for change in composer.tempo_out.drain(..) {
            tempo_feed("transport").send(change);
        }
        for (fx, event) in composer.automation_out.drain(..) {
            automation_feed(&format!("golden_score/{fx}")).send(event);
        }
        for event in events.drain(..) {
            if let Some(name) = doc.feed_names().into_iter().find(|n| makepad_score_live::key(n) == event.loop_key) {
                loop_feed(&format!("golden_score/{name}")).send(event);
            }
        }
    });
    let left: Vec<f32> = out.iter().step_by(2).skip(latency).copied().collect();
    let peak = left.iter().fold(0.0f32, |a, x| a.max(x.abs()));
    let worst = (512..frames).map(|i| (left[i] - (reference.left[i] * 0.8).clamp(-1.0, 1.0)).abs()).fold(0.0f32, f32::max);
    eprintln!("audio root with a score: peak {peak}, largest difference from the player's render x0.8 {worst:e}");
    assert!(peak > 0.05, "the score is silent in the root");
    assert!(worst < 1e-3, "the root's score differs from the player's: {worst}");
}
