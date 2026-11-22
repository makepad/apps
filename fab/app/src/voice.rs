//! The headset's microphone into the chat. While a Quest is linked through the XR remote, the chat's
//! microphone button (or `fab.voice`) asks the headset for its microphone; what the wearer says comes
//! up the link as PCM, and when it is stopped it goes to a voice hub for whisper, and the text lands in
//! the chat's composer for the person to read and send.
//!
//! The sound is the wearer's. It is held in memory until the text is made and goes to the voice hub
//! named in `~/.makepad/fab/voice_hub` and nowhere else; it is never written to disk or to a log (the
//! log has counts and timings). Nothing runs on this machine: no recognizer here, and without a
//! headset the button is not offered.

use makepad_ai_hub::speech::{SpeechReach, SttConfig, SttEngine, SttEvent, SttSession};
use makepad_app_shell::chat::VoiceSays;
use makepad_widgets::*;
use makepad_xr_stream::{mic_state, MicEvent, XrServer};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    /// The headset's microphone is on and its sound arrives.
    Listening,
    /// Asked to stop; the last of the sound and the headset's off are still on their way.
    Stopping,
    /// The sound is with the voice hub.
    Transcribing,
}

/// The wrist menu's microphone row was pressed: the chat's button, from the headset.
#[derive(Debug)]
pub enum VoiceAction {
    Toggle,
}

pub const NO_HUB: &str = "Speech to text needs a voice hub, and none is set up for Fab. Put the hub's address in ~/.makepad/fab/voice_hub.";

/// The voice hubs the person named, one base URL a line (`#` lines are notes).
pub fn hubs_from(text: &str) -> Vec<String> {
    let with_port = |l: &str| l.rsplit_once(':').is_some_and(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l {
            _ if l.contains("://") => l.trim_end_matches('/').to_string(),
            _ if with_port(l) => format!("https://{l}"),
            _ => format!("https://{l}:8123"),
        })
        .collect()
}

pub struct XrVoice {
    inbox: Arc<Mutex<VecDeque<MicEvent>>>,
    hooked: bool,
    phase: Phase,
    session: Option<u64>,
    /// The sound so far, mono 16 kHz.
    pcm: Vec<f32>,
    stt: Option<SttSession>,
    /// The utterance at the hub, and when the speech ended.
    waiting: Option<(u64, Instant)>,
    says: Vec<VoiceSays>,
    /// Why the microphone stands as it does, for `fab.voice`.
    last: String,
    /// The headset's microphone is still asked and must be let go.
    release: bool,
}

impl Default for XrVoice {
    fn default() -> Self {
        XrVoice { inbox: Arc::default(), hooked: false, phase: Phase::Idle, session: None, pcm: Vec::new(), stt: None, waiting: None, says: Vec::new(), last: String::new(), release: false }
    }
}

impl XrVoice {
    /// The linked headset and its server, when it has a microphone to give.
    fn server(cx: &mut Cx) -> Option<(XrServer, u64)> {
        let server = XrServer::get(cx)?;
        let headset = server.live_headset()?;
        server.mic_available().then_some((server, headset.session))
    }

    pub fn ui(&self, cx: &mut Cx) -> makepad_app_aichat::VoiceUi {
        use makepad_app_aichat::VoiceUi;
        match self.phase {
            Phase::Idle => {
                if Self::server(cx).is_some() {
                    VoiceUi::Idle
                } else {
                    VoiceUi::Hidden
                }
            }
            Phase::Listening => VoiceUi::Listening,
            Phase::Stopping | Phase::Transcribing => VoiceUi::Busy,
        }
    }

    /// What the microphone does now, in words (the chat's `fab.voice`).
    pub fn describe(&self, cx: &mut Cx) -> String {
        let headset = if Self::server(cx).is_some() { "A device with a microphone is linked." } else { "No device with a microphone is linked: the microphone is not offered." };
        let phase = match self.phase {
            Phase::Idle => "Not listening.",
            Phase::Listening => "Listening to the device's microphone.",
            Phase::Stopping => "Stopping; the last of the sound is arriving.",
            Phase::Transcribing => "Turning what was said into text at the voice hub.",
        };
        if self.last.is_empty() {
            format!("{headset} {phase}")
        } else {
            format!("{headset} {phase} Last: {}", self.last)
        }
    }

    pub fn take_says(&mut self) -> Vec<VoiceSays> {
        std::mem::take(&mut self.says)
    }

    fn fail(&mut self, text: String) {
        // (a microphone still asked is released at the next pump)
        self.release = matches!(self.phase, Phase::Listening | Phase::Stopping);
        self.phase = Phase::Idle;
        self.pcm = Vec::new();
        self.waiting = None;
        self.last = text.clone();
        self.says.push(VoiceSays::Notice(text));
    }

    /// Start or stop listening (the button, `fab.voice`). An answer in words for the AI.
    pub fn toggle(&mut self, cx: &mut Cx, listen: Option<bool>) -> Result<String, String> {
        let want = listen.unwrap_or(self.phase == Phase::Idle);
        match (self.phase, want) {
            (Phase::Idle, true) => {
                let Some((server, session)) = Self::server(cx) else {
                    return Err("No device with a microphone is linked.".into());
                };
                let hubs = self.hubs();
                if hubs.is_empty() {
                    self.fail(NO_HUB.into());
                    return Err(NO_HUB.into());
                }
                if !self.hooked {
                    self.hooked = true;
                    let inbox = self.inbox.clone();
                    server.set_mic_sink(Some(Arc::new(move |_, event| {
                        inbox.lock().unwrap().push_back(event);
                        SignalToUI::set_ui_signal();
                    })));
                }
                // (the hub is reached while the wearer speaks: the answer waits on nothing else)
                if self.stt.is_none() {
                    self.stt = Some(SttSession::start(SttConfig {
                        engine: SttEngine::Whisper,
                        reach: SpeechReach::Local,
                        language: "auto".into(),
                        partial_results: false,
                        timestamps: false,
                        hubs,
                        wake: Some(Arc::new(SignalToUI::set_ui_signal)),
                        ..SttConfig::default()
                    }));
                }
                self.pcm = Vec::new();
                self.session = Some(session);
                self.phase = Phase::Listening;
                self.last.clear();
                server.set_mic(session, true);
                log!("xr voice: listening");
                Ok("Listening to the device's microphone. Stop it with fab.voice listen=false or the chat's microphone button; the text lands in the chat's composer.".into())
            }
            (Phase::Listening, false) => {
                let Some((server, _)) = Self::server(cx) else {
                    // (the link is gone already: nothing to wait for)
                    self.fail("The device is no longer linked; the microphone is off.".into());
                    return Ok("The device is no longer linked.".into());
                };
                if let Some(session) = self.session {
                    server.set_mic(session, false);
                }
                self.phase = Phase::Stopping;
                Ok("Stopping; the text follows in the chat's composer.".into())
            }
            (Phase::Idle, false) => Ok("Not listening.".into()),
            (Phase::Listening, true) => Ok("Already listening.".into()),
            (Phase::Stopping | Phase::Transcribing, _) => Ok(self.describe(cx)),
        }
    }

    fn hubs(&self) -> Vec<String> {
        let path = makepad_fab_settings::home::user_home().join(".makepad").join("fab").join("voice_hub");
        std::fs::read_to_string(path).map(|t| hubs_from(&t)).unwrap_or_default()
    }

    /// The headset's sound and states, and the hub's answers (from the window's pump).
    pub fn pump(&mut self, cx: &mut Cx) {
        let events: Vec<MicEvent> = self.inbox.lock().unwrap().drain(..).collect();
        for event in events {
            match event {
                MicEvent::Pcm(samples) => {
                    if matches!(self.phase, Phase::Listening | Phase::Stopping) {
                        self.pcm.extend(samples.iter().map(|&v| v as f32 / 32768.0));
                    }
                }
                MicEvent::State { state, text } => match state {
                    mic_state::OFF if matches!(self.phase, Phase::Listening | Phase::Stopping) => self.finish(),
                    mic_state::FAILED if self.phase != Phase::Idle => self.fail(format!("The device's microphone: {text}.")),
                    _ => {}
                },
            }
        }
        let mut stt_events = Vec::new();
        if let Some(stt) = &self.stt {
            stt_events = stt.poll();
        }
        for event in stt_events {
            match event {
                SttEvent::Failed(why) => {
                    // (a fresh session on the next press)
                    self.stt = None;
                    if self.phase != Phase::Idle {
                        self.fail(format!("Speech to text is not available: {why}."));
                    } else {
                        self.last = format!("Speech to text is not available: {why}.");
                    }
                }
                SttEvent::Final { utterance, transcript, .. } => {
                    if let Some((_, ended)) = self.waiting.filter(|(id, _)| *id == utterance) {
                        let text = transcript.text();
                        log!("xr voice: text {} ms after the end of speech ({} words)", ended.elapsed().as_millis(), text.split_whitespace().count());
                        self.waiting = None;
                        self.phase = Phase::Idle;
                        if text.trim().is_empty() {
                            self.last = "Nothing was understood.".into();
                            self.says.push(VoiceSays::Notice("Nothing was understood in what was said.".into()));
                        } else {
                            self.last = "Text written into the composer.".into();
                            self.says.push(VoiceSays::Text(text));
                        }
                    }
                }
                SttEvent::Error { message, .. } => {
                    if self.phase == Phase::Transcribing {
                        self.fail(format!("The voice hub could not read it: {message}."));
                    }
                }
                SttEvent::Loading { .. } | SttEvent::Ready(_) | SttEvent::Level(_) | SttEvent::Partial(_) | SttEvent::ListenEnded => {}
            }
        }
        self.release_now(cx);
    }

    fn release_now(&mut self, cx: &mut Cx) {
        if std::mem::take(&mut self.release) {
            if let (Some(server), Some(session)) = (XrServer::get(cx), self.session) {
                server.set_mic(session, false);
            }
        }
    }

    fn finish(&mut self) {
        let pcm = std::mem::take(&mut self.pcm);
        log!("xr voice: {:.1} s of sound", pcm.len() as f32 / 16_000.0);
        if pcm.is_empty() {
            self.fail("No sound arrived from the device.".into());
            return;
        }
        let Some(stt) = &self.stt else {
            self.fail("Speech to text is not available.".into());
            return;
        };
        self.waiting = Some((stt.transcribe(pcm), Instant::now()));
        self.phase = Phase::Transcribing;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_lines_become_base_urls() {
        assert_eq!(hubs_from("# the voice hub\n\nvoice.example:9000\nhttp://192.0.2.7:8765/\nhub.example\n"), vec!["https://voice.example:9000".to_string(), "http://192.0.2.7:8765".to_string(), "https://hub.example:8123".to_string()]);
    }
}
