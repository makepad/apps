//! The audio node types: the engine's own (`makepad-audio-engine`)
//! and the deck, Stage's asset-backed player. Every parameter says what it
//! is: an authored setting (a source edit, a new revision) or an endpoint a
//! running plan admits (a control, a toggle, a trigger).

pub use makepad_audio_engine::nodes::*;
use makepad_flow::document::{EndpointClass, ParamDescriptor, ParamType, Registry};
use makepad_flow::engine::providers::ExecutorRegistry;
use makepad_flow::Literal;

/// Register the audio types, the engine's nodes, the score (`Score`, a
/// nested player) and the deck, and name `stage.audio` a live owner so a
/// run knows its nodes are not jobs.
pub fn register(registry: &mut Registry, executors: &mut ExecutorRegistry) -> Result<(), String> {
    register_nodes(registry)?;
    makepad_score_live::node::register(registry)?;
    let descriptors = [
        node(
            "AudioDeck",
            "Plays one prepared source package: original stereo mix and optional vocals/drums/bass/other stems, synchronized to one playhead, loop, cue and speed. Every transport change is dipped. stems_available reports an acknowledged complete stem set.",
            vec![
                ParamDescriptor::toggle("play", false, "Whether the clip advances. Pausing dips to silence and holds the position."),
                ParamDescriptor::control("speed", 1.0, -4.0, 4.0, "Playback speed; the pitch follows (varispeed) unless `keylock` holds it. Its size is 0.25 to 4; negative plays backwards (stopping softly at the start)."),
                ParamDescriptor::control("loop_in", 0.0, 0.0, 36000.0, "Loop start in seconds of the clip."),
                ParamDescriptor::control("loop_out", 0.0, 0.0, 36000.0, "Loop end in seconds; at or before the start means the end of the clip."),
                ParamDescriptor::toggle("loop_enabled", false, "Whether the clip loops between `loop_in` and `loop_out`."),
                ParamDescriptor::control("cue_at", 0.0, 0.0, 36000.0, "Where `cue` jumps to, in seconds."),
                ParamDescriptor::toggle("scratch", false, "A hand on the record: while on, the playhead follows `scratch_at` either way (reverse and standstill included) and sounds even when paused; letting go spins back to the transport."),
                ParamDescriptor::control("scratch_at", 0.0, 0.0, 36000.0, "Where the hand holds the record, in seconds; only its movement since `scratch` went on matters."),
                ParamDescriptor::toggle("brake", false, "A touch (`scratch` going on) brakes a spinning record to a stop in 0.3 s, as a hand pressed on a platter, instead of stopping it under the hand; moving the hand takes over at once."),
                ParamDescriptor::control("fling", 0.0, -16.0, 16.0, "Read when the hand lets go: a non-zero speed (multiples of the source's own, signed) flings the record, which then eases to the transport (or to rest) in about a second instead of the plain release."),
                ParamDescriptor::control("key", 0.0, -12.0, 12.0, "Key shift in semitones: the pitch moves, the tempo does not (time-stretched)."),
                ParamDescriptor::toggle("keylock", false, "Tempo changes keep the key: `speed` moves the tempo only (time-stretched). A hand on the record is varispeed either way."),
                ParamDescriptor {
                    name: "cue".into(), expected: ParamType::Literal, default: Literal::Null,
                    doc: Some("Jump to `cue_at`: an ordered pulse, never coalesced, dipped.".into()),
                    range: None, endpoint: Some(EndpointClass::Trigger), signal: None,
                },
                ParamDescriptor {
                    name: "source".into(), expected: ParamType::Literal, default: Literal::Null,
                    doc: Some("The clip: an owner command (`DeckCommand`) from the application, never a source edit.".into()),
                    range: None, endpoint: Some(EndpointClass::Trigger), signal: None,
                },
            ],
            vec![],
            vec![output("audio", AUDIO), output("vocals", AUDIO), output("drums", AUDIO), output("bass", AUDIO), output("other", AUDIO), output("position", LEVEL), output("end", LEVEL), output("stems_available", LEVEL)],
        ),
    ];
    for descriptor in descriptors {
        registry.register(descriptor).map_err(|error| error.to_string())?;
    }
    executors.register_live(OWNER)
}
