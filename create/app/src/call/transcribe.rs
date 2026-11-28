//! Transcribe this computer's gated microphone during one consenting stretch of a call.
use makepad_ai_hub::speech::{SpeechReach, SttConfig, SttEngine, SttEngineInfo, SttEvent, SttSession, WakeHook};
use makepad_call::driver::{voice::VOICE_FRAME, Spawn, TapFrame, TranscriptTap};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, Receiver, TryRecvError, TrySendError},
        Arc, OnceLock,
    },
    thread::{self, Thread},
    time::Duration,
};

const FRAME_US: u64 = 20_000;
const SILENCE_FRAMES: usize = 35; // 700 ms
const BATCH: usize = 5 * VOICE_FRAME; // 100 ms
const UI_BUFFER: usize = 16;

pub struct TranscriberConfig {
    /// First signalling message on the microphone's capture clock.
    pub origin_us: u64,
    /// BCP-47, or "auto" for Whisper's language detection.
    pub language: String,
}

impl TranscriberConfig {
    pub fn new(origin_us: u64) -> Self {
        Self { origin_us, language: system_language().unwrap_or_else(|| "auto".into()) }
    }
}

#[cfg(target_os = "macos")]
fn system_language() -> Option<String> {
    use std::ffi::{c_char, c_void, CStr};
    type Cf = *const c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFLocaleCopyPreferredLanguages() -> Cf;
        fn CFArrayGetCount(array: Cf) -> isize;
        fn CFArrayGetValueAtIndex(array: Cf, index: isize) -> Cf;
        fn CFStringGetCString(s: Cf, dest: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(value: Cf);
    }
    // The copied array owns its language strings until release. UTF-8 conversion includes NUL.
    unsafe {
        let languages = CFLocaleCopyPreferredLanguages();
        if languages.is_null() { return None; }
        let mut tag = [0; 256];
        let ok = CFArrayGetCount(languages) > 0 && CFStringGetCString(
            CFArrayGetValueAtIndex(languages, 0), tag.as_mut_ptr(), tag.len() as isize, 0x08000100,
        ) != 0;
        CFRelease(languages);
        ok.then(|| CStr::from_ptr(tag.as_ptr()).to_string_lossy().into_owned())
    }
}

#[cfg(windows)]
fn system_language() -> Option<String> {
    #[link(name = "kernel32")]
    extern "system" { fn GetUserDefaultLocaleName(name: *mut u16, len: i32) -> i32; }
    let mut tag = [0; 85]; // LOCALE_NAME_MAX_LENGTH, including the terminator
    let len = unsafe { GetUserDefaultLocaleName(tag.as_mut_ptr(), tag.len() as i32) };
    (len > 1).then(|| String::from_utf16_lossy(&tag[..len as usize - 1]))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn system_language() -> Option<String> {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"].iter()
        .filter_map(|key| std::env::var(key).ok()).find(|s| !s.is_empty())?;
    let tag = locale.split(['.', '@']).next()?;
    (!matches!(tag, "" | "C" | "POSIX")).then(|| tag.replace('_', "-"))
}

#[derive(Debug, PartialEq, Eq)]
pub enum TranscriberEvent {
    Ready(String),
    Failed(String),
    Line { at_ms: u64, dur_ms: u64, text: String },
}

/// Feed and finish only enqueue work. Poll returns immediately, even during engine selection.
pub trait Recognizer: Send {
    fn feed(&mut self, samples: Vec<f32>);
    fn end_utterance(&mut self) -> u64;
    fn poll(&mut self) -> Vec<SttEvent>;
}

impl Recognizer for SttSession {
    fn feed(&mut self, samples: Vec<f32>) { SttSession::feed(self, samples); }
    fn end_utterance(&mut self) -> u64 { SttSession::end_utterance(self) }
    fn poll(&mut self) -> Vec<SttEvent> { SttSession::poll(self) }
}

/// The call's line when no Whisper is in reach.
pub const NEEDS_WHISPER: &str = "Transcription needs AI Hub with Whisper";

/// Call transcription's hub session. When it finds no Whisper it says so in the call's plain
/// words; the hub's detail goes to the log.
struct WhisperOnly<R>(R);

impl<R: Recognizer> Recognizer for WhisperOnly<R> {
    fn feed(&mut self, samples: Vec<f32>) { self.0.feed(samples); }
    fn end_utterance(&mut self) -> u64 { self.0.end_utterance() }
    fn poll(&mut self) -> Vec<SttEvent> {
        let mut events = self.0.poll();
        for event in &mut events {
            if let SttEvent::Failed(why) = event {
                eprintln!("[transcribe] no Whisper in reach: {why}");
                *why = NEEDS_WHISPER.into();
            }
        }
        events
    }
}

/// Whisper only, wherever AI Hub reaches one: in this process, this computer's hub, or a hub
/// on the LAN when the hub's own LAN setting allows it. Never the OS recognizer.
fn recognizer_config(config: &TranscriberConfig, wake: WakeHook) -> SttConfig {
    SttConfig {
        engine: SttEngine::Whisper, reach: SpeechReach::Lan, on_device_only: false,
        language: config.language.clone(), partial_results: false, wake: Some(wake),
        ..Default::default()
    }
}

/// The engine as the call shows it, with where it runs when that is outside this app:
/// the person should know when their words leave this computer.
fn shown_engine(info: &SttEngineInfo) -> String {
    let Some(url) = info.remote.as_deref() else { return info.engine.clone() };
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.rsplit_once(':').map_or(authority, |(host, _)| host),
    };
    let here = host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    format!("{} on {} computer's AI Hub", info.engine, if here { "this" } else { "another" })
}

trait TapSource {
    fn dropped(&self) -> u64;
    fn pop(&self, samples: &mut Vec<f32>) -> Option<TapFrame>;
}

impl TapSource for TranscriptTap {
    fn dropped(&self) -> u64 { self.dropped() }
    fn pop(&self, samples: &mut Vec<f32>) -> Option<TapFrame> { self.pop(samples) }
}

struct Span {
    first: u64,
    last: u64,
    prefix: bool,
    samples: usize,
}

struct Pending {
    id: Option<u64>,
    span: Span,
    suffix: bool,
    result: Option<Result<String, String>>,
}

struct Drain {
    origin: u64,
    // Empty the realtime ring first. All processing below works on owned, growable storage.
    frames: Vec<TapFrame>,
    pcm: Vec<f32>,
    scratch: Vec<f32>,
    observed_drops: u64,
    unplaced: u64,
    last_capture: Option<u64>,
    mark_at: Option<u64>,
    open: Option<Span>,
    silence: usize,
    batch: Vec<f32>,
    pending: VecDeque<Pending>,
    events: VecDeque<TranscriberEvent>,
    outstanding_samples: usize,
    warned: bool,
    failed: bool,
}

impl Drain {
    fn new(origin: u64, dropped: u64) -> Self {
        Self {
            origin, frames: Vec::new(), pcm: Vec::new(), scratch: Vec::new(),
            observed_drops: dropped, unplaced: 0, last_capture: None, mark_at: None,
            open: None, silence: 0, batch: Vec::with_capacity(BATCH),
            pending: VecDeque::new(), events: VecDeque::new(), outstanding_samples: 0,
            warned: false, failed: false,
        }
    }

    fn observe(&mut self, dropped: u64) {
        self.unplaced += dropped - self.observed_drops;
        self.observed_drops = dropped;
    }

    fn read(&mut self, tap: &impl TapSource, rec: &mut dyn Recognizer, log: &mut dyn FnMut(String)) {
        self.observe(tap.dropped());
        self.frames.clear();
        self.pcm.clear();
        while let Some(frame) = tap.pop(&mut self.scratch) {
            self.frames.push(frame);
            self.pcm.extend_from_slice(&self.scratch);
        }
        self.observe(tap.dropped());
        let mut offset = 0;
        for i in 0..self.frames.len() {
            let f = self.frames[i];
            if let Some(prev) = self.last_capture {
                let missing = (f.capture_us.saturating_sub(prev) + FRAME_US / 2) / FRAME_US;
                let lost = self.unplaced.min(missing.saturating_sub(1));
                if lost > 0 { self.gap(prev + FRAME_US, lost, rec, log); }
            }
            self.last_capture = Some(f.capture_us);
            if f.silent {
                if self.open.is_some() {
                    self.silence += 1;
                    if self.silence == SILENCE_FRAMES { self.end(rec, false); }
                }
                continue;
            }
            let span = self.open.get_or_insert_with(|| Span {
                first: f.capture_us, last: f.capture_us, prefix: self.mark_at.take().is_some(), samples: 0,
            });
            span.last = f.capture_us;
            // A short pause inside a phrase keeps its duration. Leading and trailing mute is never fed.
            let pause = self.silence * VOICE_FRAME;
            span.samples += pause + VOICE_FRAME;
            self.outstanding_samples += pause + VOICE_FRAME;
            for _ in 0..self.silence {
                Self::append_frame(&mut self.batch, &[0.0; VOICE_FRAME], rec);
            }
            self.silence = 0;
            Self::append_frame(&mut self.batch, &self.pcm[offset..offset + VOICE_FRAME], rec);
            offset += VOICE_FRAME;
        }
        if !self.warned && self.outstanding_samples > 60 * 16_000 {
            log("over a minute of call audio awaits a final transcript".into());
            self.warned = true;
        }
    }

    fn append_frame(batch: &mut Vec<f32>, frame: &[f32], rec: &mut dyn Recognizer) {
        batch.extend_from_slice(frame);
        if batch.len() == BATCH {
            rec.feed(std::mem::replace(batch, Vec::with_capacity(BATCH)));
        }
    }

    fn end(&mut self, rec: &mut dyn Recognizer, suffix: bool) {
        if let Some(span) = self.open.take() {
            if !self.batch.is_empty() {
                rec.feed(std::mem::replace(&mut self.batch, Vec::with_capacity(BATCH)));
            }
            self.pending.push_back(Pending { id: Some(rec.end_utterance()), span, suffix, result: None });
        }
        self.silence = 0;
    }

    fn gap(&mut self, at: u64, lost: u64, rec: &mut dyn Recognizer, log: &mut dyn FnMut(String)) {
        self.unplaced -= lost;
        log(format!("call transcript gap at {at} capture µs: {lost} dropped frames"));
        if self.open.is_some() { self.end(rec, true); } else { self.mark_at.get_or_insert(at); }
    }

    fn finish(&mut self, rec: &mut dyn Recognizer, log: &mut dyn FnMut(String)) {
        if self.unplaced > 0 {
            self.gap(self.last_capture.map_or(self.origin, |t| t + FRAME_US), self.unplaced, rec, log);
        }
        self.end(rec, false);
        if let Some(at) = self.mark_at.take() {
            self.pending.push_back(Pending {
                id: None, span: Span { first: at, last: at, prefix: false, samples: 0 },
                suffix: false, result: Some(Ok("…".into())),
            });
        }
    }

    fn collect(&mut self, rec: &mut dyn Recognizer) {
        for event in rec.poll() {
            let (id, result) = match event {
                SttEvent::Ready(info) => { self.events.push_back(TranscriberEvent::Ready(shown_engine(&info))); continue; }
                SttEvent::Failed(why) | SttEvent::Error { utterance: None, message: why } => {
                    self.events.push_back(TranscriberEvent::Failed(why));
                    self.failed = true;
                    return;
                }
                SttEvent::Final { utterance, transcript, .. } => (utterance, Ok(transcript.text())),
                SttEvent::Error { utterance: Some(id), message } => (id, Err(message)),
                _ => continue,
            };
            if let Some(p) = self.pending.iter_mut().find(|p| p.id == Some(id)) {
                p.result = Some(result);
            }
            self.publish_lines();
        }
        self.publish_lines();
    }

    fn publish_lines(&mut self) {
        while self.pending.front().is_some_and(|p| p.result.is_some()) {
            let p = self.pending.pop_front().unwrap();
            self.outstanding_samples -= p.span.samples;
            let mut text = match p.result.unwrap() {
                Ok(text) => text.trim().to_owned(),
                Err(why) => {
                    self.events.push_back(TranscriberEvent::Failed(why));
                    String::new()
                }
            };
            if p.span.prefix { text = format!("… {text}").trim_end().to_owned(); }
            if p.suffix { text = format!("{text} …").trim_start().to_owned(); }
            if !text.is_empty() {
                self.events.push_back(TranscriberEvent::Line {
                    at_ms: p.span.first.saturating_sub(self.origin) / 1_000,
                    dur_ms: if p.id.is_some() { (p.span.last.saturating_sub(p.span.first) + FRAME_US) / 1_000 } else { 0 },
                    text,
                });
            }
        }
    }
}

pub struct Transcriber {
    tap: Option<Arc<TranscriptTap>>,
    stopping: Arc<AtomicBool>,
    abandoned: Arc<AtomicBool>,
    worker: Arc<OnceLock<Thread>>,
    events: Receiver<TranscriberEvent>,
    finished: bool,
}

impl Transcriber {
    /// Look for Whisper through AI Hub, never the OS recognizer. With none in reach the
    /// session fails with [`NEEDS_WHISPER`] and the call goes on. Starting selection
    /// dispatches to the hub worker. Its wake is the same UI signal passed to `start`.
    pub fn hub_recognizer(config: &TranscriberConfig, wake: WakeHook) -> Box<dyn Recognizer> {
        Box::new(WhisperOnly(SttSession::start(recognizer_config(config, wake))))
    }

    pub fn start(tap: TranscriptTap, config: TranscriberConfig, mut rec: Box<dyn Recognizer>, spawn: &Spawn, wake: WakeHook) -> Self {
        let tap = Arc::new(tap);
        let mut drain = Drain::new(config.origin_us, tap.dropped());
        let (tx, events) = sync_channel(UI_BUFFER);
        let stopping = Arc::new(AtomicBool::new(false));
        let abandoned = Arc::new(AtomicBool::new(false));
        let worker = Arc::new(OnceLock::new());
        let handle = Self {
            tap: Some(tap.clone()), stopping: stopping.clone(), abandoned: abandoned.clone(),
            worker: worker.clone(), events, finished: false,
        };
        tap.set_on(true);
        spawn("call transcribe", Box::new(move || {
            let _ = worker.set(thread::current());
            let mut closed = false;
            let mut log = |message| eprintln!("{message}");
            while !abandoned.load(Ordering::Acquire) {
                if !closed {
                    // Observe stop before draining: after set_on(false) these are the last queued frames.
                    let last = stopping.load(Ordering::Acquire);
                    drain.read(&*tap, &mut *rec, &mut log);
                    if last { drain.finish(&mut *rec, &mut log); closed = true; }
                }
                drain.collect(&mut *rec);
                if drain.failed && !closed {
                    tap.set_on(false);
                    // Engine selection failed. Audio recorded since the last drain belongs to
                    // this failed stretch, never to the next stretch using the returned tap.
                    while tap.pop(&mut drain.scratch).is_some() {}
                    closed = true;
                }
                let mut sent = false;
                while let Some(event) = drain.events.pop_front() {
                    match tx.try_send(event) {
                        Ok(()) => sent = true,
                        Err(TrySendError::Full(event)) => { drain.events.push_front(event); break; }
                        Err(TrySendError::Disconnected(_)) => return,
                    }
                }
                if sent { wake(); }
                if closed && (drain.failed || drain.pending.is_empty()) && drain.events.is_empty() { break; }
                thread::park_timeout(Duration::from_millis(40));
            }
            drop(tap);
            drop(tx);
            wake();
        }));
        handle
    }

    fn unpark(&self) { if let Some(worker) = self.worker.get() { worker.unpark(); } }

    /// Turn off microphone capture immediately. Keep polling for the unfinished utterance.
    pub fn stop(&mut self) {
        if let Some(tap) = &self.tap { tap.set_on(false); }
        self.stopping.store(true, Ordering::Release);
        self.unpark();
    }

    /// Take available events in order. Reading makes room for any lines the worker retained.
    pub fn poll(&mut self) -> Vec<TranscriberEvent> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => { self.finished = true; break; }
            }
        }
        if !events.is_empty() { self.unpark(); }
        events
    }

    /// After poll has drained the last line (or startup failure), reclaim the disabled tap.
    /// The UI need never join the worker or wait for recognition.
    pub fn take_stopped_tap(&mut self) -> Option<TranscriptTap> {
        if !self.finished { return None; }
        self.tap.take().map(|tap| Arc::try_unwrap(tap).unwrap_or_else(|_| unreachable!("worker released its tap before closing events")))
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        self.stop();
        self.abandoned.store(true, Ordering::Release);
        self.unpark();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_ai_hub::{pipe::PipeId, speech::{SttCapabilities, SttEngineInfo, Transcript}};
    use makepad_call::driver::voice::{voice_in, VoiceMarks, VOICE_RATE};
    use std::{cell::{Cell, RefCell}, sync::mpsc::{channel, Sender}, time::Instant};

    fn ready() -> SttEvent {
        SttEvent::Ready(SttEngineInfo {
            engine: "recording fake".into(), pipe: PipeId::new("stt.fake"), remote: None,
            capabilities: SttCapabilities { pcm_input: true, engine_mic: false, offline: true, partial_results: false },
        })
    }

    #[test]
    fn shown_engine_says_where_whisper_runs() {
        let at = |remote: Option<&str>| {
            let SttEvent::Ready(mut info) = ready() else { unreachable!() };
            info.remote = remote.map(Into::into);
            shown_engine(&info)
        };
        assert_eq!(at(None), "recording fake", "in this app: the engine alone");
        for here in ["http://127.0.0.1:8123", "http://localhost:8123/", "http://[::1]:8123"] {
            assert_eq!(at(Some(here)), "recording fake on this computer's AI Hub", "{here}");
        }
        for lan in ["http://192.0.2.10:8123", "https://hub.example:8123"] {
            assert_eq!(at(Some(lan)), "recording fake on another computer's AI Hub", "{lan}");
        }
    }

    fn result(id: u64, samples: usize) -> SttEvent {
        SttEvent::Final { utterance: id, secs: 0.0, transcript: Transcript::from_text(format!("turn {id}: {samples} samples")) }
    }

    struct Slow {
        input: Sender<Option<Vec<f32>>>,
        events: Receiver<SttEvent>,
        ended: Sender<u64>,
        next: u64,
    }

    impl Recognizer for Slow {
        fn feed(&mut self, pcm: Vec<f32>) { self.input.send(Some(pcm)).unwrap(); }
        fn end_utterance(&mut self) -> u64 {
            self.next += 1;
            self.input.send(None).unwrap();
            self.ended.send(self.next).unwrap();
            self.next
        }
        fn poll(&mut self) -> Vec<SttEvent> { self.events.try_iter().collect() }
    }

    fn test_spawn() -> Spawn {
        Arc::new(|name, job| { thread::Builder::new().name(name.into()).spawn(job).unwrap(); })
    }

    fn wake_caller() -> WakeHook {
        let caller = thread::current();
        Arc::new(move || caller.unpark())
    }

    fn finish_polling(t: &mut Transcriber) -> (Vec<TranscriberEvent>, TranscriptTap) {
        let mut events = Vec::new();
        loop {
            events.extend(t.poll());
            if let Some(tap) = t.take_stopped_tap() { return (events, tap); }
            thread::park();
        }
    }

    #[test]
    fn a_slow_recognizer_keeps_every_microphone_frame_and_finishes_the_last_turn_on_stop() {
        // More than a ring of speech and more finished lines than the UI channel can hold.
        const TURNS: usize = UI_BUFFER + 2;
        const FRAMES: usize = 10;
        let (requests, input) = channel::<Option<Vec<f32>>>();
        let (results, events) = channel();
        let (ended, boundaries) = channel();
        let (release, gate) = channel();
        let engine = thread::spawn(move || {
            results.send(ready()).unwrap();
            let mut gate = Some(gate);
            let (mut id, mut samples) = (0, 0);
            while let Ok(batch) = input.recv() {
                if let Some(batch) = batch { samples += batch.len(); continue; }
                if let Some(gate) = gate.take() { gate.recv().unwrap(); }
                id += 1;
                results.send(result(id, samples)).unwrap();
                samples = 0;
            }
            id
        });
        let muted = Arc::new(AtomicBool::new(false));
        let clock = Instant::now() - Duration::from_secs(10);
        let (mut mic, mut network) = voice_in(clock, muted.clone(), Arc::new(VoiceMarks::default()));
        let tap = network.transcript_tap().unwrap();
        let origin_us = 1_000_000;
        let config = TranscriberConfig { origin_us, language: "en-US".into() };
        let mut worker = Transcriber::start(tap, config, Box::new(Slow { input: requests, events, ended, next: 0 }), &test_spawn(), wake_caller());
        let speech: Vec<_> = (0..FRAMES * VOICE_FRAME).map(|n| 0.3 * (n as f32 * 0.17).sin()).collect();
        let mut expected = Vec::new();
        let mut payload = Vec::new();
        for n in 1..=TURNS {
            muted.store(false, Ordering::Relaxed);
            mic.push_mono(VOICE_RATE, &speech);
            let sent: Vec<_> = std::iter::from_fn(|| network.pop(&mut payload)).collect();
            assert_eq!(sent.len(), FRAMES, "turn {n} must put its ten voice frames on the call");
            let first = sent.first().unwrap().capture_us;
            let last = sent.last().unwrap().capture_us;
            expected.push(TranscriberEvent::Line {
                at_ms: (first - origin_us) / 1000, dur_ms: (last - first + FRAME_US) / 1000,
                text: format!("turn {n}: {} samples", speech.len()),
            });
            if n < TURNS {
                // The input still has a loud signal. Only mute closes this utterance.
                muted.store(true, Ordering::Relaxed);
                mic.push_mono(VOICE_RATE, &[0.3; SILENCE_FRAMES * VOICE_FRAME]);
                while network.pop(&mut payload).is_some() {
                    assert!(payload.is_empty(), "a muted block must put only silent frames on the call");
                }
                worker.unpark();
            } else {
                worker.stop();
            }
            assert_eq!(boundaries.recv().unwrap(), n as u64, "drain must finish turn {n} while recognition is gated");
        }
        assert!(worker.take_stopped_tap().is_none(), "the tap handoff must follow the outstanding final lines");
        release.send(()).unwrap();
        let (events, tap) = finish_polling(&mut worker);
        assert_eq!(events.first(), Some(&TranscriberEvent::Ready("recording fake".into())), "UI sees the selected engine");
        assert_eq!(&events[1..], &expected, "every speech sample, capture span and ordered line survives both queues and stop");
        assert_eq!(tap.dropped(), 0, "a slow recognizer must never overflow the microphone tap");
        mic.push_mono(VOICE_RATE, &speech);
        assert!(tap.pop(&mut Vec::new()).is_none(), "the returned tap must be off");
        assert_eq!(engine.join().unwrap(), TURNS as u64, "recognition must finish the open turn before shutdown");
        eprintln!("{TURNS} lines, {} speech samples, {} dropped tap frames", TURNS * speech.len(), tap.dropped());

        // Reuse the same tap for a later stretch. An unavailable recognizer returns it too.
        let why = "No on-device speech recognizer here.";
        let fake = Immediate { events: vec![SttEvent::Failed(why.into())], ..Default::default() };
        let mut later = Transcriber::start(tap, TranscriberConfig::new(0), Box::new(fake), &test_spawn(), wake_caller());
        let (events, _) = finish_polling(&mut later);
        assert_eq!(events, [TranscriberEvent::Failed(why.into())], "startup failure must preserve the engine's reason");
    }

    #[derive(Default)]
    struct Script {
        frames: RefCell<VecDeque<TapFrame>>,
        dropped: Cell<u64>,
    }

    impl Script {
        fn put(&self, n: u64, silent: bool) {
            self.frames.borrow_mut().push_back(TapFrame { silent, talk_start: false, capture_us: n * FRAME_US });
        }
    }

    impl TapSource for Script {
        fn dropped(&self) -> u64 { self.dropped.get() }
        fn pop(&self, samples: &mut Vec<f32>) -> Option<TapFrame> {
            let f = self.frames.borrow_mut().pop_front()?;
            samples.clear();
            if !f.silent { samples.resize(VOICE_FRAME, 0.25); }
            Some(f)
        }
    }

    #[test]
    fn call_transcription_asks_for_whisper_only_and_says_so_when_none_is_in_reach() {
        let config = recognizer_config(&TranscriberConfig::new(0), wake_caller());
        assert_eq!((config.engine, config.reach, config.on_device_only), (SttEngine::Whisper, SpeechReach::Lan, false),
            "any Whisper AI Hub reaches, never the OS recognizer");
        // The hub session's answer when no Whisper is in reach; no real hub.
        let hub = "whisper is not compiled into this build; no node in reach (Lan) serves stt.whisper";
        let mut session = WhisperOnly(Immediate { events: vec![SttEvent::Failed(hub.into())], ..Default::default() });
        assert_eq!(session.poll(), [SttEvent::Failed(NEEDS_WHISPER.into())],
            "no Whisper: the call is told in one plain line and no engine becomes ready");
    }

    #[derive(Default)]
    struct Immediate { id: u64, samples: usize, events: Vec<SttEvent> }
    impl Recognizer for Immediate {
        fn feed(&mut self, pcm: Vec<f32>) { self.samples += pcm.len(); }
        fn end_utterance(&mut self) -> u64 {
            self.id += 1;
            self.events.push(result(self.id, std::mem::take(&mut self.samples)));
            self.id
        }
        fn poll(&mut self) -> Vec<SttEvent> { std::mem::take(&mut self.events) }
    }

    #[test]
    fn dropped_frames_mark_the_capture_gap_in_speech_in_silence_and_at_stop() {
        let tap = Script::default();
        tap.dropped.set(7); // drops during a previous stretch do not belong to this one
        let mut d = Drain::new(FRAME_US, tap.dropped());
        let mut rec = Immediate::default();
        let mut logs = Vec::new();
        let mut log = |s| logs.push(s);
        tap.put(1, false);
        tap.put(2, false);
        tap.dropped.set(9);
        d.read(&tap, &mut rec, &mut log);
        assert_eq!(rec.id, 0, "the count alone cannot place a gap ahead of speech still in the ring");
        tap.put(5, true); // 3 and 4 lost during speech
        tap.put(6, true);
        tap.dropped.set(10); // 7 lost in silence
        tap.put(8, false);
        for n in 9..43 { tap.put(n, true); }
        d.read(&tap, &mut rec, &mut log);
        assert_eq!(rec.id, 1, "680 ms of silence must keep the second phrase open");
        tap.put(43, false); // pause is inside this phrase
        for n in 44..79 { tap.put(n, true); }
        d.read(&tap, &mut rec, &mut log);
        assert_eq!(rec.id, 2, "700 ms of silence must finish the second phrase");
        tap.dropped.set(13); // stretch ends before another frame can expose this gap
        d.read(&tap, &mut rec, &mut log);
        d.finish(&mut rec, &mut log);
        d.collect(&mut rec);
        let line = |at_ms, dur_ms, text: &str| TranscriberEvent::Line { at_ms, dur_ms, text: text.into() };
        assert_eq!(Vec::from(d.events), [
            line(0, 40, "turn 1: 640 samples …"), line(140, 720, "… turn 2: 11520 samples"),
            line(1560, 0, "…"),
        ], "gaps must mark the right words or stand alone at the first missing capture");
        assert_eq!(logs, [
            "call transcript gap at 60000 capture µs: 2 dropped frames",
            "call transcript gap at 140000 capture µs: 1 dropped frames",
            "call transcript gap at 1580000 capture µs: 3 dropped frames",
        ], "each log must locate the gap and count its missing microphone frames");
    }
}
