//! The person's AI and the call: it reads what Town really knows, and it can only lower things.
//! No tool starts or answers a call, unmutes, shares, records or opens Watch, and none returns
//! the call's media, levels or transcript. Each closing tool presses the button's own `Press`.
use super::{AudioHost, CallAction, CallController, CallShown, Phase, Press, ShareRole};
use crate::record::capture::RecordingShown;
use makepad_strict_json::{obj, s, Value};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallTool {
    State,
    HangUp,
    Decline,
    Mute,
    TranscribeOff,
    ShareStop,
    ShareClose,
    ControlTakeBack,
    RecordingStop,
}

impl CallTool {
    pub const ALL: [(&'static str, CallTool); 9] = [
        ("call_state", Self::State), ("call_hang_up", Self::HangUp), ("call_decline", Self::Decline),
        ("call_mute", Self::Mute), ("transcribe_off", Self::TranscribeOff), ("share_stop", Self::ShareStop),
        ("share_close", Self::ShareClose), ("control_take_back", Self::ControlTakeBack),
        ("recording_stop", Self::RecordingStop),
    ];

    pub fn named(tool: &str) -> Option<Self> {
        Self::ALL.iter().find(|(name, _)| *name == tool).map(|(_, tool)| *tool)
    }

    /// The press the matching button would send now, if there is anything to close.
    pub fn press(self, shown: &CallShown) -> Option<Press> {
        let view = &shown.view;
        let sharer = shown.share.own_offers().next().is_some();
        match self {
            Self::State => None,
            Self::HangUp => match view.phase {
                Phase::Calling => Some(Press::Cancel),
                Phase::Live => Some(Press::HangUp),
                _ => None,
            },
            Self::Decline => (view.phase == Phase::Ringing).then_some(Press::Decline),
            Self::Mute => (view.phase == Phase::Live && !view.muted).then_some(Press::Mute),
            Self::TranscribeOff => (view.phase == Phase::Live && view.transcribing_me).then_some(Press::StopTranscribing),
            Self::ShareStop => sharer.then_some(Press::StopShare),
            Self::ShareClose => shown.share.watching.is_some().then_some(Press::CloseWatch),
            Self::ControlTakeBack => (shown.share.control.active.is_some() && shown.share.control_session.and_then(|s| shown.share.offer(s)).is_some_and(|o| o.role != ShareRole::Viewer)).then_some(Press::TakeBack),
            Self::RecordingStop => shown.share.capture.recording.is_some().then_some(Press::StopRecording),
        }
    }

    /// Runs the tool through the controller. Returns `call_state` as it is after, what was done,
    /// and whether the controller changed.
    pub fn run(self, ctl: &mut CallController, host: &mut dyn AudioHost, page: Option<makepad_town_core::event::SessionId>) -> (String, String, bool) {
        let shown = ctl.shown();
        if self == Self::State { return (state(&shown, Instant::now()), "call state".into(), false) }
        let action = if self == Self::ShareStop {
            let mut own = shown.share.own_offers();
            let first = own.next();
            let target = if own.next().is_some() {
                let selected = page.and_then(|s| shown.share.offer(s)).filter(|o| o.role != ShareRole::Viewer);
                if selected.is_none() {
                    let titles: Vec<_> = shown.share.own_offers().map(|o| o.title.as_str()).collect();
                    let count = if titles.len() == 2 { "Two".into() } else { titles.len().to_string() };
                    let message = format!("{count} shares are running ({}). Open the one to stop, or stop it on its page.", titles.join(", "));
                    return (state_with_message(&shown, Instant::now(), Some(&message)), message, false)
                }
                selected
            } else { first };
            target.map(|o| CallAction::OnShare(o.session, Press::StopShare))
        } else { self.press(&shown).map(CallAction::Press) };
        let changed = action.is_some_and(|action| ctl.call_actions(host, action));
        (state(&ctl.shown(), Instant::now()), if changed { "closed" } else { "nothing to close" }.into(), changed)
    }
}

fn recording(running: &RecordingShown, now: Instant) -> Value {
    obj(vec![
        ("state", s("recording")),
        ("title", s(&*running.title)),
        ("elapsed_secs", Value::Int(now.saturating_duration_since(running.started).as_secs() as i64)),
    ])
}

/// What Town knows at `now`. There is no camera, reconnecting phase or quality here because Town
/// has none.
pub fn state(shown: &CallShown, now: Instant) -> String {
    state_with_message(shown, now, None)
}

fn state_with_message(shown: &CallShown, now: Instant, message: Option<&str>) -> String {
    let view = &shown.view;
    let phase = match view.phase {
        Phase::Idle => "idle",
        Phase::Ringing => "ringing_in",
        Phase::Calling => "ringing_out",
        Phase::Live => "live",
    };
    let live = view.phase == Phase::Live;
    let mut fields = vec![("phase", s(phase))];
    if view.phase != Phase::Idle { fields.push(("peer", s(view.peer_name.as_str()))) }
    if live {
        fields.push(("muted", Value::Bool(view.muted)));
        fields.push(("peer_muted", Value::Bool(view.peer_muted)));
        fields.push(("duration_secs", Value::Int(view.duration_secs as i64)));
        fields.push(("transcribing", Value::Bool(view.transcribing_me)));
        fields.push(("peer_transcribing", Value::Bool(view.peer_transcribing)));
    }
    let share_state = |offer: &super::ShareOffer| {
        let sharer = matches!(offer.role, ShareRole::Sharer { .. });
        // The Allow's holder: the peer when I share, me when I watch.
        let controlled_by = match (shown.share.control.active.is_some() && shown.share.control_session == Some(offer.session), sharer) {
            (false, _) => Value::Null,
            (true, true) => s(view.peer_name.as_str()),
            (true, false) => s("you"),
        };
        obj(vec![
            ("state", s(if sharer { "sharing" } else { "viewing" })),
            ("title", s(offer.title.as_str())),
            ("watching", Value::Bool(shown.share.watching == Some(offer.session))),
            ("controlled_by", controlled_by),
        ])
    };
    let share = match shown.share.offers.as_slice() {
        [] => obj(vec![("state", s("none"))]),
        [offer] => share_state(offer),
        offers => obj(vec![("state", s("multiple")), ("offers", Value::Arr(offers.iter().map(|o| share_state(o)).collect()))]),
    };
    if let Some(message) = message { fields.push(("message", s(message))); }
    fields.push(("share", share));
    fields.push(("recording", match &shown.share.capture.recording {
        Some(running) => recording(running, now),
        None => obj(vec![("state", s("none"))]),
    }));
    obj(fields).to_json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_strict_json::parse;
    use std::sync::Arc;

    fn shown(phase: Phase, edit: impl FnOnce(&mut super::super::CallView)) -> CallShown {
        let mut view = super::super::CallView { phase, peer_name: "Ada".into(), ..Default::default() };
        edit(&mut view);
        CallShown { view: Arc::new(view), ..Default::default() }
    }

    #[test]
    fn closing_tools_press_their_buttons_only_when_there_is_something_to_close() {
        let tool = |name| CallTool::named(name).unwrap();
        let press = |name, shown: &CallShown| tool(name).press(shown);
        let idle = shown(Phase::Idle, |_| {});
        for (name, _) in CallTool::ALL { assert_eq!(press(name, &idle), None, "{name} has nothing to close when idle"); }
        let out = shown(Phase::Calling, |_| {});
        assert_eq!(press("call_hang_up", &out), Some(Press::Cancel), "hanging up a ring I placed is the Cancel button");
        let ring = shown(Phase::Ringing, |_| {});
        assert_eq!((press("call_decline", &ring), press("call_hang_up", &ring)), (Some(Press::Decline), None));
        let mut live = shown(Phase::Live, |v| v.transcribing_me = true);
        assert_eq!(press("call_hang_up", &live), Some(Press::HangUp));
        assert_eq!(press("call_mute", &live), Some(Press::Mute));
        assert_eq!(press("transcribe_off", &live), Some(Press::StopTranscribing));
        assert_eq!(press("call_decline", &live), None);
        let muted = shown(Phase::Live, |v| v.muted = true);
        assert_eq!((press("call_mute", &muted), press("transcribe_off", &muted)), (None, None), "mute is never undone and an off switch stays off");
        live.share.capture.recording = Some(RecordingShown { title: "Demo".into(), started: Instant::now() });
        assert_eq!(press("recording_stop", &live), Some(Press::StopRecording));
        assert_eq!(press("share_stop", &live), None);
        assert_eq!(press("share_close", &live), None);
        assert_eq!(press("control_take_back", &live), None);
    }

    #[test]
    fn call_state_reports_only_what_town_knows() {
        let now = Instant::now();
        let idle = parse(state(&CallShown::default(), now).as_bytes()).unwrap();
        assert_eq!(idle.get("phase").and_then(Value::as_str), Some("idle"));
        assert_eq!(idle.get("share").and_then(|v| v.get("state")).and_then(Value::as_str), Some("none"));
        assert!(idle.get("peer").is_none() && idle.get("duration_secs").is_none(), "an idle call has no peer or clock");
        let mut live = shown(Phase::Live, |v| { v.duration_secs = 7; v.peer_muted = true; v.peer_transcribing = true; });
        live.share.capture.recording = Some(RecordingShown { title: "Demo · 2".into(), started: now });
        let json = state(&live, now + std::time::Duration::from_millis(65_400));
        let value = parse(json.as_bytes()).unwrap();
        assert_eq!(value.get("phase").and_then(Value::as_str), Some("live"));
        assert_eq!(value.get("peer").and_then(Value::as_str), Some("Ada"));
        assert_eq!(value.get("duration_secs").and_then(Value::as_i64), Some(7));
        assert_eq!(value.get("peer_muted").and_then(Value::as_bool), Some(true));
        assert_eq!(value.get("peer_transcribing").and_then(Value::as_bool), Some(true));
        let rec = value.get("recording").unwrap();
        assert_eq!((rec.get("title").and_then(Value::as_str), rec.get("elapsed_secs").and_then(Value::as_i64)), (Some("Demo · 2"), Some(65)),
            "the recording's own title, separators and all, and its whole seconds since the start");
        for banned in ["camera", "quality", "reconnecting", "transcript", "audio", "level", "talking", "media"] {
            assert!(!json.contains(banned), "call_state invents or leaks `{banned}`");
        }
        let ring = parse(state(&shown(Phase::Calling, |_| {}), now).as_bytes()).unwrap();
        assert_eq!(ring.get("phase").and_then(Value::as_str), Some("ringing_out"));
        assert!(ring.get("muted").is_none(), "mute marks belong to a live call");
    }
}
