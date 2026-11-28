//! The person's AI and the call: it reads what Town really knows, and it can only lower things.
//! No tool starts or answers a call, unmutes, shares, records or opens Watch, and none returns
//! the call's media, levels or transcript. Each closing tool presses the button's own `Press`.
use super::{CallAction, CallShown, Phase, Press, ShareRole};
use makepad_town_core::{
    event::SessionId, key::PublicKey, peers::Peers,
    trust::{AiJson, AiOut, AiTextBuilder, Gate, Withheld},
};
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
            Self::ControlTakeBack => (shown.share.control.active.is_some() && shown.share.control_session.and_then(|s| shown.share.offer(s)).is_some_and(|o| matches!(o.role, ShareRole::Sharer { .. }))).then_some(Press::TakeBack),
            Self::RecordingStop => shown.share.capture.recording.is_some().then_some(Press::StopRecording),
        }
    }

    pub fn plan(self, gate: &Gate<'_>, shown: &CallShown, page: Option<SessionId>, me: &PublicKey) -> Result<CallToolPlan, Withheld> {
        gate.town_readable()?;
        if self == Self::State { return Ok(CallToolPlan::Action(None)) }
        if self != Self::ShareStop {
            return Ok(CallToolPlan::Action(self.press(shown).map(CallAction::Press)))
        }
        let own = || shown.share.own_offers();
        let count = own().count();
        let target = if count <= 1 {
            own().next()
        } else if let Some(selected) = own().find(|offer| Some(offer.session) == page) {
            Some(selected)
        } else {
            let mut msg = AiTextBuilder::new();
            if count == 2 { msg.push_static("Two"); } else { msg.push_usize(count); }
            msg.push_static(" shares are running (");
            for (i, offer) in own().enumerate() {
                if i != 0 { msg.push_static(", "); }
                msg.push_for_ai(gate.share_peer_text(me, offer.title.clone())?);
            }
            msg.push_static("). Open the one to stop, or stop it on its page.");
            return Ok(CallToolPlan::AmbiguousShareStop(msg))
        };
        Ok(CallToolPlan::Action(target.map(|offer| CallAction::OnShare(offer.session, Press::StopShare))))
    }
}

pub enum CallToolPlan {
    Action(Option<CallAction>),
    AmbiguousShareStop(AiTextBuilder),
}

pub struct CallGateContext<'a> {
    pub call_peers: &'a [PublicKey],
    pub peers: Option<&'a Peers>,
    pub me: PublicKey,
    pub person_name: Option<&'a str>,
}

/// The caller's town-filtered call snapshot, with names and captured titles checked at read time.
pub fn state(gate: &Gate<'_>, shown: &CallShown, now: Instant, ctx: &CallGateContext<'_>,
    message: Option<AiTextBuilder>) -> Result<AiOut, Withheld> {
    gate.town_readable()?;
    let name = |peer: &PublicKey| {
        let pin = ctx.peers.and_then(|p| p.device(peer)).map(|device| device.name.as_str());
        gate.profile_or_fallback_name(peer, pin, &ctx.me, ctx.person_name).map(|name| name.cleaned_person_label(false))
    };
    let view = &shown.view;
    let phase = match view.phase {
        Phase::Idle => "idle",
        Phase::Ringing => "ringing_in",
        Phase::Calling => "ringing_out",
        Phase::Live => "live",
    };
    let mut fields = vec![("phase", AiJson::static_str(phase))];
    let mut participants = Vec::new();
    if view.phase != Phase::Idle {
        let mut names = AiTextBuilder::new();
        if view.phase == Phase::Live && !view.participants.is_empty() {
            for (i, participant) in view.participants.iter().enumerate() {
                let label = name(&participant.peer)?;
                if i != 0 { names.push_static(", "); }
                names.push_for_ai_str(label.as_for_ai_str());
                participants.push(AiJson::object(vec![
                    ("name", AiJson::text(label)),
                    ("muted", AiJson::bool(participant.muted)),
                    ("transcribing", AiJson::bool(participant.transcribing)),
                ]));
            }
        } else if ctx.call_peers.is_empty() {
            names.push_static("Peer");
        } else {
            for (i, peer) in ctx.call_peers.iter().enumerate() {
                if i != 0 { names.push_static(", "); }
                names.push_for_ai(name(peer)?);
            }
        }
        fields.push(("peer", AiJson::built_str(names)));
    }
    if view.phase == Phase::Live {
        fields.extend([
            ("participants", AiJson::array(participants)),
            ("muted", AiJson::bool(view.muted)),
            ("peer_muted", AiJson::bool(view.peer_muted)),
            ("duration_secs", AiJson::int(view.duration_secs as i64)),
            ("transcribing", AiJson::bool(view.transcribing_me)),
            ("peer_transcribing", AiJson::bool(view.peer_transcribing)),
        ]);
    }
    let mut offers = Vec::new();
    for offer in shown.share.offers.iter() {
        let (role, sharer) = match offer.role {
            ShareRole::Sharer { .. } => ("sharing", ctx.me),
            ShareRole::Viewer { sharer } => ("viewing", sharer),
        };
        let Ok(title) = gate.share_peer_text(&sharer, offer.title.clone()) else { continue };
        let controlled = shown.share.control.active.is_some() && shown.share.control_session == Some(offer.session);
        let controlled_by = match (controlled, offer.role) {
            (false, _) => AiJson::null(),
            (true, ShareRole::Viewer { .. }) => AiJson::static_str("you"),
            (true, ShareRole::Sharer { .. }) => {
                let holder = shown.share.control.holder.as_ref().map(|(peer, _)| name(peer)).transpose()?;
                AiJson::opt_text(holder)
            }
        };
        let mut offer_fields = vec![
            ("state", AiJson::static_str(role)), ("title", AiJson::text(title)),
            ("watching", AiJson::bool(shown.share.watching == Some(offer.session))),
            ("controlled_by", controlled_by),
        ];
        match offer.role {
            ShareRole::Sharer { .. } => {
                let mut watchers = Vec::new();
                for (peer, _) in &offer.watchers {
                    watchers.push(AiJson::text(name(peer)?));
                }
                let mut requests = Vec::new();
                for (session, peer, _) in &shown.share.control.requests {
                    if *session == offer.session { requests.push(AiJson::text(name(peer)?)); }
                }
                offer_fields.push(("watchers", AiJson::array(watchers)));
                offer_fields.push(("control_requests", AiJson::array(requests)));
            }
            ShareRole::Viewer { sharer } => offer_fields.push(("sharer", AiJson::text(name(&sharer)?))),
        }
        offers.push(AiJson::object(offer_fields));
    }
    let none = || AiJson::object(vec![("state", AiJson::static_str("none"))]);
    let share = match offers.len() {
        0 => none(),
        1 => offers.remove(0),
        _ => AiJson::object(vec![("state", AiJson::static_str("multiple")), ("offers", AiJson::array(offers))]),
    };
    fields.push(("share", share));
    let recording = shown.share.capture.recording.as_ref().and_then(|running| {
        let title = gate.share_peer_text(&ctx.me, running.title.to_string()).ok()?;
        Some(AiJson::object(vec![
            ("state", AiJson::static_str("recording")), ("title", AiJson::text(title)),
            ("elapsed_secs", AiJson::int(now.saturating_duration_since(running.started).as_secs() as i64)),
        ]))
    });
    fields.push(("recording", recording.unwrap_or_else(none)));
    if let Some(msg) = message { fields.push(("message", AiJson::built_str(msg))); }
    gate.finish_json(AiJson::object(fields))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::call::{CallController, CallEnv, Message, ParticipantShown, ShareOfferExt};
    use crate::model::{tests::{group_town, ShareHomes}, Model};
    use crate::record::capture::RecordingShown;
    use makepad_strict_json::{parse, s, Value};
    use makepad_town_core::{share::hello::MediaCaps, trust::LocalPolicy};
    use std::sync::Arc;

    fn shown(phase: Phase, edit: impl FnOnce(&mut super::super::CallView)) -> CallShown {
        let mut view = super::super::CallView { phase, peer_name: "UI label".into(), ..Default::default() };
        edit(&mut view);
        CallShown { view: Arc::new(view), ..Default::default() }
    }

    /// A live call in the first model's town, with one window shared and another watched.
    pub(crate) fn multiparty_shown(label: &str) -> (ShareHomes, Vec<Model>, CallShown) {
        let (homes, models) = group_town(label, 4);
        let town = models[0].open_id();
        let (ben, kit, ana) = (models[1].me(), models[2].me(), models[3].me());
        let (own_session, watched_session) = ([1; 16], [2; 16]);
        let mut ctl = CallController::default();
        ctl.configure(CallEnv { files: Default::default(), devices_path: homes.0.join("devices"),
            voice: makepad_call::driver::VoiceSource::Off, signal: Arc::new(|| {}),
            spawn: Arc::new(|_, _| panic!("receiving the offer needs no media worker")),
            jobs: Arc::new(|_, _| panic!("receiving the offer needs no audio job")) });
        let mut cx = makepad_widgets::Cx::new(Box::new(|_, _| {}));
        let port = ctl.link_up(&mut cx, models[0].me(), ana, "UI label sharer Ana").unwrap();
        ctl.set_open_town(Some((town, vec![(ana, "UI label sharer Ana".into())])));
        ctl.set_peer_caps(ana, MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
        port.messages_in.try_send(Message::Share { session: watched_session, title: "Canvas".into(), w: 20., h: 20.,
            ext: ShareOfferExt { town: Some(town), ..Default::default() } }.encode()).unwrap();
        ctl.poll(&mut cx);
        let mut live = shown(Phase::Live, |view| {
            view.town = Some(town);
            view.peer_name = "UI label call peer".into();
            view.participants = vec![
                ParticipantShown { peer: ben, name: "UI label participant Ben".into(), muted: true, transcribing: false },
                ParticipantShown { peer: kit, name: "UI label participant Kit".into(), muted: false, transcribing: true },
            ];
            view.transcribing_me = true;
        });
        let share = &mut live.share;
        let viewer = ctl.shown().share.offer(watched_session).unwrap().clone();
        let mut own = viewer.clone();
        let offer = Arc::make_mut(&mut own);
        offer.role = ShareRole::Sharer { link: crate::app_link::LinkId::TEST, window: crate::app_link::WindowId(1) };
        offer.session = own_session;
        offer.title = "Fab".into();
        offer.sharer_name = "UI label local sharer".into();
        offer.watchers = vec![(ben, "UI label watcher Ben".into()), (kit, "UI label watcher Kit".into())];
        share.watchers = offer.watchers.clone();
        share.offers = Arc::new(vec![own, viewer]);
        share.watching = Some(watched_session);
        share.watching_sharer = Some(ana);
        share.control_session = Some(own_session);
        share.control.active = Some([8; 16]);
        share.control.holder = Some((kit, "UI label holder Kit".into()));
        share.control.requests = vec![(own_session, ben, "UI label requester Ben".into())];
        share.capture.recording = Some(RecordingShown { title: "Demo".into(), started: Instant::now() });
        (homes, models, live)
    }

    #[test]
    fn call_state_reports_multiparty_participants_watchers_and_holder_without_banned_fields() {
        use crate::model::tests::record_call_event;
        use makepad_town_core::event::Content;

        let (_homes, mut models, mut live) = multiparty_shown("call-state-multiparty");
        for (i, profile) in [(1, "Ben"), (2, "Kit"), (3, "Ana")] {
            record_call_event(&mut models, i, Content::Profile { name: profile.into(), colour: 0 });
        }
        let (ben, kit) = (models[1].me(), models[2].me());
        let ctx = CallGateContext { call_peers: &[], peers: None, me: models[0].me(), person_name: None };
        let render = |model: &Model, snapshot: &CallShown| {
            let gate = Gate::new(&model.state, LocalPolicy::Members);
            let out = state(&gate, snapshot, Instant::now(), &ctx, None).unwrap();
            assert!(!out.as_str().contains("UI label"), "a display label reached call_state");
            for banned in ["camera", "quality", "reconnecting", "transcript", "audio", "level", "talking", "media"] {
                assert!(!out.as_str().contains(banned), "call_state invents or leaks `{banned}`");
            }
            parse(out.as_str().as_bytes()).unwrap()
        };

        let viewer = Arc::make_mut(&mut live.share.offers).pop().unwrap();
        let own = live.share.own_offers().next().unwrap().clone();
        let value = render(&models[0], &live);
        assert_eq!(value.get("peer"), Some(&s("Ben, Kit")));
        assert_eq!(value.get("participants"), Some(&parse(br#"[
            {"name":"Ben","muted":true,"transcribing":false},
            {"name":"Kit","muted":false,"transcribing":true}
        ]"#).unwrap()));
        let share = value.get("share").unwrap();
        assert_eq!(share.get("title"), Some(&s("Fab")));
        assert_eq!(share.get("watchers"), Some(&Value::Arr(vec![s("Ben"), s("Kit")])));
        assert_eq!(share.get("controlled_by"), Some(&s("Kit")));
        assert_eq!(share.get("control_requests"), Some(&Value::Arr(vec![s("Ben")])));

        live.share.offers = Arc::new(vec![viewer.clone()]);
        let value = render(&models[0], &live);
        let share = value.get("share").unwrap();
        assert_eq!(share.get("sharer"), Some(&s("Ana")));
        assert_eq!(share.get("watching"), Some(&Value::Bool(true)));
        assert!(share.get("watchers").is_none() && share.get("control_requests").is_none());

        // Interleave requests for a second local share to check per-offer FIFO order.
        let mut second = own.clone();
        Arc::make_mut(&mut second).session = [3; 16];
        live.share.control.requests.extend([
            (second.session, kit, "UI label second requester".into()),
            (own.session, kit, "UI label requester Kit".into()),
        ]);
        live.share.offers = Arc::new(vec![own, second, viewer]);
        let value = render(&models[0], &live);
        let offers = value.get("share").unwrap().get("offers").unwrap().as_arr().unwrap();
        assert_eq!(offers[0].get("control_requests"), Some(&Value::Arr(vec![s("Ben"), s("Kit")])));
        assert_eq!(offers[1].get("control_requests"), Some(&Value::Arr(vec![s("Kit")])));
        assert_eq!(offers[1].get("controlled_by"), Some(&Value::Null));
        assert_eq!(offers[2].get("sharer"), Some(&s("Ana")));

        models[0].remove_device(ben, false).unwrap();
        let fallback = format!("Member {}", makepad_town_core::hex(&ben[..4]));
        let removed = render(&models[0], &live);
        let offers = removed.get("share").unwrap().get("offers").unwrap().as_arr().unwrap();
        assert_eq!(removed.get("peer"), Some(&s(format!("{fallback}, Kit"))));
        assert_eq!(removed.get("participants").unwrap().as_arr().unwrap()[0].get("name"), Some(&s(&fallback)));
        for key in ["watchers", "control_requests"] {
            assert_eq!(offers[0].get(key), Some(&Value::Arr(vec![s(&fallback), s("Kit")])), "{key} hides the removed profile");
        }
    }

    #[test]
    fn filter_call_shown_hides_foreign_town_call_and_offers() {
        use crate::tools::filter_call_shown;

        let (homes, models, live) = multiparty_shown("call-state-filter");
        let other = Model::open(&homes.0.join("other")).unwrap().0;
        let town = models[0].open_id();
        let filter = |caller| filter_call_shown(&live, Some(town), |_| Some(town), Some(town), caller);
        let foreign = filter(other.open_id());
        assert_eq!(foreign.view.phase, Phase::Idle);
        assert!(foreign.view.participants.is_empty());
        assert!(foreign.share.offers.is_empty() && foreign.share.watchers.is_empty());
        assert!(foreign.share.watching.is_none() && foreign.share.watching_sharer.is_none());
        assert!(foreign.share.control_session.is_none() && foreign.share.control.active.is_none());
        assert!(foreign.share.control.holder.is_none() && foreign.share.control.requests.is_empty());
        assert!(foreign.share.capture.recording.is_none());
        let gate = Gate::new(&other.state, LocalPolicy::Members);
        let ctx = CallGateContext { call_peers: &[], peers: None, me: other.me(), person_name: None };
        let now = Instant::now();
        let render = |snapshot| state(&gate, snapshot, now, &ctx, None).unwrap();
        assert_eq!(render(&foreign), render(&CallShown::default()), "another town sees the idle reply");

        let local = filter(town);
        assert_eq!(local.view.phase, Phase::Live);
        assert_eq!(local.view.participants, live.view.participants);
        assert_eq!(local.share.offers.len(), live.share.offers.len());
        for offer in live.share.offers.iter() {
            let kept = local.share.offer(offer.session).unwrap();
            assert_eq!(kept.role, offer.role);
            assert_eq!(kept.watchers, offer.watchers, "watcher order on the offer survives filtering");
        }
        let mut watchers = live.share.watchers.clone();
        watchers.sort_by_key(|(key, _)| *key);
        assert_eq!(local.share.watchers, watchers);
        assert_eq!(local.share.watching, live.share.watching);
        assert_eq!(local.share.watching_sharer, live.share.watching_sharer);
        assert_eq!(local.share.control_session, live.share.control_session);
        assert_eq!(local.share.control.active, live.share.control.active);
        assert_eq!(local.share.control.holder, live.share.control.holder);
        assert_eq!(local.share.control.requests, live.share.control.requests);
        assert_eq!(local.share.capture.recording, live.share.capture.recording);
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
        use crate::call::{CallController, CallEnv, Message, ShareOfferExt};
        use crate::model::tests::{group_town, record_call_event};
        use makepad_town_core::{event::Content, share::hello::MediaCaps, trust::LocalPolicy};

        let (homes, mut models) = group_town("call-state", 2);
        record_call_event(&mut models, 1, Content::Profile { name: "Ada".into(), colour: 0 });
        let peer = models[1].me();
        let model = &mut models[0];
        let ctx = CallGateContext { call_peers: &[peer], peers: None, me: model.me(), person_name: None };
        let gate = Gate::new(&model.state, LocalPolicy::Members);
        let now = Instant::now();
        let idle = state(&gate, &CallShown::default(), now, &ctx, None).unwrap();
        let idle = parse(idle.as_str().as_bytes()).unwrap();
        assert_eq!(idle.get("phase").and_then(Value::as_str), Some("idle"));
        assert_eq!(idle.get("share").and_then(|v| v.get("state")).and_then(Value::as_str), Some("none"));
        assert!(idle.get("peer").is_none() && idle.get("duration_secs").is_none(), "an idle call has no peer or clock");
        let mut live = shown(Phase::Live, |v| { v.duration_secs = 7; v.peer_muted = true; v.peer_transcribing = true; });
        live.share.capture.recording = Some(RecordingShown { title: "Demo · 2".into(), started: now });
        let town = model.open_id();
        let mut ctl = CallController::default();
        ctl.configure(CallEnv { files: Default::default(), devices_path: homes.0.join("devices"),
            voice: makepad_call::driver::VoiceSource::Off, signal: Arc::new(|| {}),
            spawn: Arc::new(|_, _| panic!("an unwatched offer needs no media worker")),
            jobs: Arc::new(|_, _| panic!("an unwatched offer needs no audio job")) });
        let mut cx = makepad_widgets::Cx::new(Box::new(|_, _| {}));
        let port = ctl.link_up(&mut cx, ctx.me, peer, "UI label").unwrap();
        ctl.set_open_town(Some((town, vec![(peer, "UI label".into())])));
        ctl.set_peer_caps(peer, MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
        port.messages_in.try_send(Message::Share { session: [1; 16], title: "Canvas".into(), w: 20., h: 20.,
            ext: ShareOfferExt { town: Some(town), ..Default::default() } }.encode()).unwrap();
        ctl.poll(&mut cx);
        live.share.offers = ctl.shown().share.offers;

        let out = state(&gate, &live, now + std::time::Duration::from_millis(65_400), &ctx, None).unwrap();
        let json = out.as_str();
        let value = parse(json.as_bytes()).unwrap();
        assert_eq!(value.get("phase").and_then(Value::as_str), Some("live"));
        assert_eq!(value.get("peer").and_then(Value::as_str), Some("Ada"), "a signed profile supplies the active caller name");
        assert_eq!(value.get("share").and_then(|s| s.get("title")).and_then(Value::as_str), Some("Canvas"),
            "a current member's viewer offer is visible");
        assert_eq!(value.get("duration_secs").and_then(Value::as_i64), Some(7));
        assert_eq!(value.get("peer_muted").and_then(Value::as_bool), Some(true));
        assert_eq!(value.get("peer_transcribing").and_then(Value::as_bool), Some(true));
        let rec = value.get("recording").unwrap();
        assert_eq!((rec.get("title").and_then(Value::as_str), rec.get("elapsed_secs").and_then(Value::as_i64)), (Some("Demo · 2"), Some(65)),
            "the recording's own title, separators and all, and its whole seconds since the start");
        for banned in ["camera", "quality", "reconnecting", "transcript", "audio", "level", "talking", "media"] {
            assert!(!json.contains(banned), "call_state invents or leaks `{banned}`");
        }
        let ring = state(&gate, &shown(Phase::Calling, |_| {}), now, &ctx, None).unwrap();
        let ring = parse(ring.as_str().as_bytes()).unwrap();
        assert_eq!(ring.get("phase").and_then(Value::as_str), Some("ringing_out"));
        assert!(ring.get("muted").is_none(), "mute marks belong to a live call");

        model.remove_device(peer, false).unwrap();
        let removed = state(&Gate::new(&model.state, LocalPolicy::Members), &live, now, &ctx, None).unwrap();
        let removed = parse(removed.as_str().as_bytes()).unwrap();
        let fallback = format!("Member {}", makepad_town_core::hex(&peer[..4]));
        assert_eq!(removed.get("peer").and_then(Value::as_str), Some(fallback.as_str()), "removal hides the profile");
        assert_eq!(removed.get("share"), Some(&makepad_strict_json::obj(vec![("state", makepad_strict_json::s("none"))])),
            "removal omits the viewer offer even from the same controller snapshot");
        model.state.unknown_critical.push([2; 32]);
        assert_eq!(state(&Gate::new(&model.state, LocalPolicy::Members), &live, now, &ctx, None),
            Err(Withheld::TownUnknownCritical), "an unknown critical record withholds the whole call state");
    }
}
