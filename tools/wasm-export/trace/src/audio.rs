//! The page's audio output in the unshared-memory profile, as the loader runs it (web.js "audio
//! output without threads": `MakepadAudioFifoOutput` and `makepad_audio_fifo_worklet`) on the
//! virtual clock. The app asks for an output (`FromWasmStartAudioOutput`); the first user
//! gesture opens it (before one, the ask waits, as `had_user_gesture` makes it wait). From then
//! on the page tops a FIFO up to [`LEAD_S`] ahead of the worklet, in [`BLOCK_FRAMES`] blocks
//! (`wasm_audio_output_entrypoint`), from a [`PUMP_INTERVAL_S`] timer and at every animation
//! frame. The worklet takes one render quantum at the context's rate per quantum of time and
//! plays silence for what is missing (an underrun, once fed). Its output latency is not
//! modelled: a block is heard when the worklet plays it.
//!
//! The constants are web.js's (`MAKEPAD_AUDIO_*`; a test reads them there): the trace runs
//! the pump the page ships.

/// Frames per render call: the worklet's render quantum (`MAKEPAD_AUDIO_BLOCK_FRAMES`).
pub const BLOCK_FRAMES: u32 = 128;
/// How far ahead of the worklet the FIFO is kept (`MAKEPAD_AUDIO_FIFO_LEAD_S`).
pub const LEAD_S: f64 = 0.040;
/// The pump's timer (`MAKEPAD_AUDIO_PUMP_INTERVAL_S`).
pub const PUMP_INTERVAL_S: f64 = 0.010;
/// The virtual context's rate: a common output device's.
pub const SAMPLE_RATE: f64 = 48_000.0;

/// What the outputs did over the run.
#[derive(Clone, Debug, Default)]
pub struct AudioStats {
    /// Outputs opened (a gesture with an ask waiting, or an ask after one).
    pub starts: u64,
    /// Blocks the app rendered (entrypoint calls), and those with a sample above -60 dB.
    pub blocks: u64,
    pub audible_blocks: u64,
    /// Render quanta the worklet played short after it was first fed, and the frames missing.
    pub underruns: u64,
    pub underrun_frames: u64,
    /// The FIFO's depth (frames) when a pump found it, once fed: least and most.
    pub min_lead: Option<u64>,
    pub max_lead: u64,
}

impl AudioStats {
    pub fn line(&self) -> String {
        let ms = |frames: u64| frames as f64 / SAMPLE_RATE * 1000.0;
        format!(
            "audio: {} output(s), {} blocks rendered ({} audible), lead {:.1}..{:.1} ms (target {:.1} ms), {} underruns ({:.1} ms)",
            self.starts,
            self.blocks,
            self.audible_blocks,
            self.min_lead.map_or(0.0, ms),
            ms(self.max_lead),
            ms(lead_frames()),
            self.underruns,
            ms(self.underrun_frames),
        )
    }
}

/// The FIFO's target depth: the lead in whole blocks (web.js `lead_frames`).
pub fn lead_frames() -> u64 {
    ((LEAD_S * SAMPLE_RATE / BLOCK_FRAMES as f64).ceil() as u64) * BLOCK_FRAMES as u64
}

/// One open output: the context, the worklet and the page's pump.
struct Stream {
    context_ptr: u32,
    /// When the context's clock started (virtual seconds).
    start: f64,
    /// Render quanta the worklet has played.
    quanta: u64,
    /// Stream frames sent, and played.
    written: u64,
    consumed: u64,
    fed: bool,
    next_pump: f64,
}

#[derive(Default)]
pub struct AudioOut {
    gesture: bool,
    /// An ask that waits for a gesture: the app's audio access.
    waiting: Option<u32>,
    stream: Option<Stream>,
    pub stats: AudioStats,
}

/// One block for the app to render: `(context_ptr, frames, sample_rate, stream_frame,
/// heard_at)`, answered with whether the block is audible.
pub type Render<'a> = dyn FnMut(u32, u32, f64, f64, f64) -> bool + 'a;

impl AudioOut {
    /// `FromWasmStartAudioOutput`: opens now after a gesture, else waits for one.
    pub fn ask(&mut self, context_ptr: u32, now: f64) {
        if self.stream.is_some() {
            return;
        }
        if self.gesture {
            self.open(context_ptr, now);
        } else {
            self.waiting = Some(context_ptr);
        }
    }

    /// A user gesture (`resume_audio_from_gesture`): opens a waiting ask.
    pub fn gesture(&mut self, now: f64) {
        self.gesture = true;
        if self.stream.is_none() {
            if let Some(context_ptr) = self.waiting.take() {
                self.open(context_ptr, now);
            }
        }
    }

    /// `FromWasmStopAudioOutput`.
    pub fn stop(&mut self) {
        self.waiting = None;
        self.stream = None;
    }

    pub fn running(&self) -> bool {
        self.stream.is_some()
    }

    fn open(&mut self, context_ptr: u32, now: f64) {
        self.stats.starts += 1;
        self.stream = Some(Stream { context_ptr, start: now, quanta: 0, written: 0, consumed: 0, fed: false, next_pump: now });
    }

    /// When the pump's timer fires next.
    pub fn next_due(&self) -> Option<f64> {
        self.stream.as_ref().map(|s| s.next_pump)
    }

    /// The timer, when due at `now`: a pump, and the next one scheduled.
    pub fn timer(&mut self, now: f64, render: &mut Render) {
        let Some(s) = &mut self.stream else { return };
        if s.next_pump > now {
            return;
        }
        while s.next_pump <= now {
            s.next_pump += PUMP_INTERVAL_S;
        }
        self.pump(now, render);
    }

    /// The worklet's quanta up to `now`, then the FIFO topped up to the lead (web.js `pump`).
    pub fn pump(&mut self, now: f64, render: &mut Render) {
        let Some(s) = &mut self.stream else { return };
        let block = BLOCK_FRAMES as u64;
        // The context derives time from its frame counter. Multiplying by a
        // pre-divided quantum rounds differently at a pump/frame boundary and
        // can consume one more block than the packaged context at that instant.
        while s.start + (s.quanta * block) as f64 / SAMPLE_RATE <= now {
            let queued = s.written - s.consumed;
            let take = queued.min(block);
            s.consumed += take;
            if take < block && s.fed {
                self.stats.underruns += 1;
                self.stats.underrun_frames += block - take;
            }
            s.quanta += 1;
        }
        let queued = s.written - s.consumed;
        if s.fed {
            self.stats.min_lead = Some(self.stats.min_lead.map_or(queued, |m| m.min(queued)));
            self.stats.max_lead = self.stats.max_lead.max(queued);
        }
        let lead = lead_frames();
        if queued >= lead {
            return;
        }
        let blocks = (lead - queued).div_ceil(block);
        // Stream frame `written` plays once what is queued before it has played, from the
        // worklet's next quantum on.
        // Preserve web.js heard_ms/pump's operation order, including its
        // millisecond timestamp. The app converts this to integer nanoseconds;
        // an algebraically equivalent expression can round to a different tick.
        let context_time = (s.quanta * block) as f64 / SAMPLE_RATE;
        let queued_time = context_time + queued as f64 / SAMPLE_RATE;
        let heard_ms = (s.start + context_time) * 1000.0 + (queued_time - context_time) * 1000.0;
        for b in 0..blocks {
            let at = (heard_ms + (b * block) as f64 / SAMPLE_RATE * 1000.0) / 1000.0;
            let audible = render(s.context_ptr, BLOCK_FRAMES, SAMPLE_RATE, s.written as f64, at);
            s.written += block;
            self.stats.blocks += 1;
            self.stats.audible_blocks += audible as u64;
        }
        s.fed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page's schedule from `from` to `to`: the pump timer and 60 Hz animation frames
    /// (each pumps first), every render recorded as (now, stream frame, heard at).
    fn play(out: &mut AudioOut, from: f64, to: f64, renders: &mut Vec<(f64, f64, f64)>) {
        let mut next_frame = from;
        let mut now = from;
        while now < to {
            let mut render = |_ctx: u32, frames: u32, rate: f64, stream_frame: f64, heard_at: f64| {
                assert_eq!((frames, rate), (BLOCK_FRAMES, SAMPLE_RATE));
                renders.push((now, stream_frame, heard_at));
                true
            };
            if out.next_due().is_some_and(|t| t <= now) {
                out.timer(now, &mut render);
            }
            if next_frame <= now {
                out.pump(now, &mut render);
                next_frame += 1.0 / 60.0;
            }
            now = out.next_due().unwrap_or(f64::INFINITY).min(next_frame).min(to);
        }
    }

    #[test]
    fn output_starts_after_a_gesture_and_renders_at_the_device_rate() {
        let mut out = AudioOut::default();
        let mut renders = Vec::new();
        out.ask(0x1234, 0.1);
        play(&mut out, 0.1, 0.5, &mut renders);
        assert!(!out.running() && renders.is_empty(), "no output before a gesture");
        out.gesture(0.5);
        assert!(out.running());
        play(&mut out, 0.5, 2.5, &mut renders);
        eprintln!("steady: {}", out.stats.line());
        // Two seconds at the device's rate, plus the lead rendered ahead.
        let expected = (2.0 * SAMPLE_RATE / BLOCK_FRAMES as f64) as i64 + (lead_frames() / BLOCK_FRAMES as u64) as i64;
        assert!((renders.len() as i64 - expected).abs() <= 2, "{} blocks, expected about {}", renders.len(), expected);
        assert_eq!(out.stats.blocks as usize, renders.len());
        assert_eq!(out.stats.underruns, 0);
        assert!(out.stats.max_lead <= lead_frames(), "{:?}", out.stats);
        assert!(out.stats.min_lead.unwrap() >= lead_frames() - (PUMP_INTERVAL_S * SAMPLE_RATE) as u64 - BLOCK_FRAMES as u64, "{:?}", out.stats);
        for (i, &(now, frame, heard)) in renders.iter().enumerate() {
            assert_eq!(frame, (i as u64 * BLOCK_FRAMES as u64) as f64);
            assert!(heard >= now && heard <= now + LEAD_S + 0.003, "block {i}: heard {heard} at {now}");
        }
    }

    #[test]
    fn an_ask_after_a_gesture_opens_at_once_and_stop_restarts_the_stream() {
        let mut out = AudioOut::default();
        out.gesture(0.2);
        assert!(!out.running());
        out.ask(7, 0.3);
        assert!(out.running());
        let mut renders = Vec::new();
        play(&mut out, 0.3, 0.6, &mut renders);
        out.stop();
        assert!(!out.running() && out.next_due().is_none());
        out.ask(7, 0.7);
        let mut again = Vec::new();
        play(&mut out, 0.7, 0.8, &mut again);
        assert_eq!(again[0].1, 0.0, "a new output's stream starts at frame zero");
        assert_eq!(out.stats.starts, 2);
    }

    #[test]
    fn a_page_that_stops_pumping_underruns() {
        let mut out = AudioOut::default();
        out.gesture(0.0);
        out.ask(1, 0.0);
        let mut renders = Vec::new();
        play(&mut out, 0.0, 1.0, &mut renders);
        let mut render = |_: u32, _: u32, _: f64, _: f64, _: f64| false;
        out.pump(1.1, &mut render);
        // 100 ms without a pump against a 40 ms FIFO.
        eprintln!("100 ms stall: {}", out.stats.line());
        let missing_ms = out.stats.underrun_frames as f64 / SAMPLE_RATE * 1000.0;
        assert!(out.stats.underruns > 0 && missing_ms > 55.0 && missing_ms < 80.0, "{:?}", out.stats);
    }

    /// The constants are the loader's.
    #[test]
    fn constants_match_the_loader() {
        let web_js = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../platform/src/os/web/web.js");
        let src = std::fs::read_to_string(&web_js).unwrap();
        let value = |name: &str| -> f64 {
            let at = src.find(&format!("export const {name} = ")).unwrap_or_else(|| panic!("web.js has no {name}"));
            let rest = &src[at + name.len() + "export const  = ".len()..];
            rest[..rest.find(';').unwrap()].trim().parse().unwrap()
        };
        assert_eq!(value("MAKEPAD_AUDIO_BLOCK_FRAMES"), BLOCK_FRAMES as f64);
        assert_eq!(value("MAKEPAD_AUDIO_FIFO_LEAD_S"), LEAD_S);
        assert_eq!(value("MAKEPAD_AUDIO_PUMP_INTERVAL_S"), PUMP_INTERVAL_S);
    }
}
