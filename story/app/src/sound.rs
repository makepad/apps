//! Makepad Story's sound: the one audio root the pages play through.
//!
//! The CUT viewer, the graphic desk and DECK play a cut's mix through the
//! edit renderer's preview feed (`makepad_story_edit_render::sound`: a
//! feeder mixes the cut a little ahead into a ring). Here a one-deck flow
//! (`flows/sound.splash`) on Stage's audio engine plays that ring into the
//! device: the deck is loaded with the feed once, and the engine renders
//! from the output callback. Nothing else of Stage's audio root (decks,
//! mixer, score) is here.
//!
//! The caption's master volume and mute act on the device sink: every
//! buffer the device takes is scaled by the master as it leaves the flow
//! ([`apply_master`]), so they reach the device on the next buffer.
//!
//! A hidden or muted instance (`MAKEPAD_HIDE_WINDOWS`, `MAKEPAD_MUTE=1`:
//! tests and agents) opens no device: the feed's clock still runs (the
//! edit renderer feeds silence then).

use makepad_story_cut::mix::SAMPLE_RATE;
use makepad_stage_live_audio::embedded::{embedded, EmbeddedAudio};
use makepad_stage_live_audio::{AudioEngine, DeckCommand};
use makepad_widgets::drop_slider::DropSliderAction;
use makepad_widgets::*;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// The flow: one deck into the output.
const FLOW: &str = include_str!("../flows/sound.splash");

/// The device side: the engine whose deck plays the preview feed.
pub(crate) struct StorySound {
    host: EmbeddedAudio,
    linked: bool,
    _engine: Arc<Mutex<AudioEngine>>,
}

impl StorySound {
    /// Open the device, unless muted (`MAKEPAD_MUTE`, hidden windows).
    pub(crate) fn open(cx: &mut Cx) -> Result<Option<StorySound>, String> {
        if makepad_story_edit_render::sound::muted() {
            return Ok(None);
        }
        let (mut host, mut engine) = embedded(FLOW, "story/sound.splash", Vec::new())?;
        // Prepared at the mix's rate; the device's own follows its first render.
        let (mut l, mut r) = ([0.0f32; 1], [0.0f32; 1]);
        engine.render(SAMPLE_RATE as f64, &mut l, &mut r);
        host.pump()?;
        let engine = Arc::new(Mutex::new(engine));
        let device = engine.clone();
        cx.audio_output(0, move |info, buffer| {
            let Ok(mut engine) = device.try_lock() else {
                buffer.zero();
                return;
            };
            if buffer.channel_count() >= 2 {
                let (left, right) = buffer.stereo_mut();
                engine.render(info.sample_rate, left, right);
                apply_master(left, right);
            } else {
                engine.render_silent(info.sample_rate, buffer.frame_count());
                buffer.zero();
            }
        });
        Ok(Some(StorySound { host, linked: false, _engine: engine }))
    }

    /// The preview feed loaded into the deck (once), and the host's commands.
    pub(crate) fn pump(&mut self) {
        if !self.linked {
            let ring = makepad_story_edit_render::sound::ring().clone();
            match self.host.load("deck", DeckCommand::LoadFeed { sequence: 1, ring, ramp: true }) {
                Ok(()) => self.linked = true,
                Err(error) => log!("story: sound: {error}"),
            }
        }
        if let Err(error) = self.host.pump() {
            log!("story: sound: {error}");
        }
    }
}

/// The sound's master: the caption's volume and mute, which the UI writes,
/// and what the device callback reads for every buffer and publishes back
/// (atomics only: the callback never waits on the UI).
struct Master {
    /// Linear gain as `f32` bits: 1 is unity, 2 the slider's top.
    gain: AtomicU32,
    muted: AtomicBool,
    /// The gain the last buffer ended at (the callback's own): each buffer
    /// ramps from it to the next, so a move or a mute never clicks.
    applied: AtomicU32,
    /// The highest level since the meter last took it, as `f32` bits (never
    /// negative, so the bits order as the values do).
    peak: AtomicU32,
}

static MASTER: Master = Master { gain: AtomicU32::new(1.0f32.to_bits()), muted: AtomicBool::new(false), applied: AtomicU32::new(1.0f32.to_bits()), peak: AtomicU32::new(0) };

/// Set the master volume (linear, 0 to 2; 1 is unity).
pub(crate) fn set_volume(gain: f32) {
    MASTER.gain.store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
}

/// The master volume (linear; 1 is unity).
pub(crate) fn volume() -> f32 {
    f32::from_bits(MASTER.gain.load(Ordering::Relaxed))
}

/// Mute or unmute the sound (the volume is kept).
pub(crate) fn set_muted(muted: bool) {
    MASTER.muted.store(muted, Ordering::Relaxed);
}

pub(crate) fn muted() -> bool {
    MASTER.muted.load(Ordering::Relaxed)
}

/// The caption's master chips (`utility_master_level`, `utility_mute`) on
/// `actions`: the volume slider's newest value (it reports live while
/// dragged) and a mute click. True when the mute changed, so the caption
/// paints the chip's lit state again.
pub(crate) fn handle_master_actions(cx: &mut Cx, ui: &WidgetRef, actions: &Actions) -> bool {
    let level = ui.widget(cx, ids!(utility_master_level)).widget_uid();
    if let Some(value) = actions.filter_widget_actions_cast::<DropSliderAction>(level).filter_map(|action| match action {
        DropSliderAction::Changed(value) => Some(value),
        DropSliderAction::None => None,
    }).last() {
        set_volume(value as f32);
    }
    let mute = ui.button(cx, ids!(utility_mute)).clicked(actions);
    if mute {
        set_muted(!muted());
    }
    mute
}

/// The highest master level since the last call (a share of full scale,
/// at the volume, muted or not: what the master meter shows).
pub(crate) fn take_peak() -> f32 {
    f32::from_bits(MASTER.peak.swap(0, Ordering::Relaxed))
}

/// The master on one rendered stereo buffer, in place: the volume (none
/// while muted), ramped across the buffer from where the last one ended.
/// Called by the device callback; no allocation, no lock.
pub(crate) fn apply_master(left: &mut [f32], right: &mut [f32]) {
    let frames = left.len().min(right.len());
    if frames == 0 {
        return;
    }
    let gain = volume();
    let target = if muted() { 0.0 } else { gain };
    let from = f32::from_bits(MASTER.applied.load(Ordering::Relaxed));
    let step = (target - from) / frames as f32;
    let mut peak = 0.0f32;
    for frame in 0..frames {
        let level = from + step * (frame + 1) as f32;
        peak = peak.max(left[frame].abs()).max(right[frame].abs());
        left[frame] *= level;
        right[frame] *= level;
    }
    MASTER.applied.store(target.to_bits(), Ordering::Relaxed);
    MASTER.peak.fetch_max((peak * gain).to_bits(), Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::test_cx::checkout_test_cx;

    fn register(vm: &mut ScriptVm) {
        let _ = <crate::StoryApp as AppMain>::script_mod(vm);
    }

    /// What `emit` sends, handled as the window handles it.
    fn deliver(cx: &mut Cx, ui: &WidgetRef, emit: impl FnOnce(&mut Cx)) -> bool {
        emit(cx);
        let actions = std::mem::take(&mut cx.new_actions);
        handle_master_actions(cx, ui, &actions)
    }

    /// One buffer of a full-scale signal through the device sink: its last
    /// sample (where the buffer's ramp has arrived) and its peak.
    fn sink() -> (f32, f32) {
        let (mut left, mut right) = (vec![1.0f32; 256], vec![-1.0f32; 256]);
        apply_master(&mut left, &mut right);
        assert_eq!(left[255], -right[255], "both channels take the same level");
        (left[255], left.iter().chain(&right).fold(0.0f32, |peak, v| peak.max(v.abs())))
    }

    /// The caption's chips drive the level the device sink applies: the
    /// volume slider's value scales what plays, and mute silences it.
    #[test]
    fn the_caption_volume_and_mute_set_what_the_device_plays() {
        let mut cx = checkout_test_cx(register);
        let ui = cx.with_vm(|vm| {
            let value = script_eval!(vm, {
                use mod.prelude.widgets.*
                use mod.widgets.*
                mod.story.Ui{}
            });
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "the Story UI does not evaluate: {errors:?}");
            WidgetRef::script_from_value(vm, value)
        });
        let level = ui.widget(&mut cx, ids!(utility_master_level)).widget_uid();
        let mute = ui.button(&mut cx, ids!(utility_mute)).widget_uid();
        let click = move |cx: &mut Cx| cx.widget_action(mute, ButtonAction::Clicked(KeyModifiers::default()));
        assert_eq!(sink().0, 1.0, "the sound plays at unity until the volume moves");
        take_peak();
        // The slider dragged through 40% and let go at 50%: the newest wins.
        assert!(!deliver(&mut cx, &ui, |cx| {
            cx.widget_action(level, DropSliderAction::Changed(0.4));
            cx.widget_action(level, DropSliderAction::Changed(0.5));
        }));
        assert_eq!(volume(), 0.5, "the slider's newest value is the master volume");
        sink();
        let (last, peak) = sink();
        assert!((last - 0.5).abs() < 1e-6 && (peak - 0.5).abs() < 1e-6, "the device plays at half: {last} {peak}");
        assert!((take_peak() - 0.5).abs() < 1e-6, "the meter reads the level at the volume");
        // Mute: the next buffer ramps down, the one after is silent.
        assert!(deliver(&mut cx, &ui, click), "a mute click changes the mute");
        assert!(muted());
        sink();
        assert_eq!(sink().1, 0.0, "muted, the device plays silence");
        // Unmute: back at the volume, which mute kept.
        assert!(deliver(&mut cx, &ui, click));
        assert!(!muted());
        sink();
        assert!((sink().0 - 0.5).abs() < 1e-6, "unmuted, the device plays at the kept volume");
    }
}
