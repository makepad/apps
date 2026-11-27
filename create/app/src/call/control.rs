//! The input topic on the authenticated peer link. Pointers use the shared window's layout points.
use crate::app_link::{AppInput, Mods};
use makepad_town_core::{event::SessionId, tlv::skip_tlv_tail};
use makepad_widgets::KeyCode;

pub type GrantId = [u8; 16];

#[derive(Clone, Default)]
pub struct ControlShown {
    pub peer_watching: bool,
    /// Requests in each direction are independent when both people are sharing.
    pub asking: Option<SessionId>,
    pub requested: Option<SessionId>,
    /// The watched share on which this guest may take control.
    pub allowed: Option<SessionId>,
    /// Identifies this uninterrupted period of control to the panel that drew it.
    pub active: Option<GrantId>,
    pub notice: Option<std::sync::Arc<str>>,
}

#[derive(Clone, Debug)]
pub struct PanelInput {
    pub session: SessionId,
    pub grant: GrantId,
    pub event: AppInput,
}

/// A press or any other discrete event flushes the last move before it. Only a
/// trailing move waits for the next frame. Nothing queues up during pointer motion.
#[derive(Default)]
pub(super) struct FrameMotion(Option<PanelInput>);

impl FrameMotion {
    pub(super) fn push(&mut self, input: PanelInput, mut send: impl FnMut(PanelInput)) -> bool {
        if matches!(input.event, AppInput::PointerMove { .. }) {
            self.0.replace(input).is_none()
        } else {
            self.flush(&mut send);
            send(input);
            false
        }
    }

    pub(super) fn flush(&mut self, send: impl FnOnce(PanelInput)) {
        if let Some(input) = self.0.take() { send(input); }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    TakenBack,
    LocalInput,
    ShareEnded,
    Released,
    Disconnected,
}

#[derive(Debug)]
pub enum ControlMessage {
    Input { session: SessionId, grant: GrantId, event: AppInput },
    /// No reason means control is on. Every end has exactly one reason.
    Control { session: SessionId, grant: GrantId, ended: Option<EndReason> },
    Release { session: SessionId, grant: GrantId },
    Ask { session: SessionId, asking: bool },
    Rights { session: SessionId, allowed: bool },
}

const VERSION: u8 = 1;
const INPUT: u8 = 1;
const CONTROL: u8 = 2;
const RELEASE: u8 = 3;
const ASK: u8 = 4;
const CANCEL_ASK: u8 = 5;
const ALLOWED: u8 = 6;
const NOT_ALLOWED: u8 = 7;

fn pack(m: Mods) -> u8 {
    u8::from(m.shift) | (u8::from(m.control) << 1) | (u8::from(m.alt) << 2) | (u8::from(m.logo) << 3)
}

fn unpack(bits: u8) -> Option<Mods> {
    (bits < 16).then_some(Mods {
        shift: bits & 1 != 0, control: bits & 2 != 0, alt: bits & 4 != 0, logo: bits & 8 != 0,
    })
}

impl ControlMessage {
    pub fn encode(&self) -> Vec<u8> {
        let (tag, session) = match self {
            Self::Input { session, .. } => (INPUT, session),
            Self::Control { session, .. } => (CONTROL, session),
            Self::Release { session, .. } => (RELEASE, session),
            Self::Ask { session, asking } => (if *asking { ASK } else { CANCEL_ASK }, session),
            Self::Rights { session, allowed } => (if *allowed { ALLOWED } else { NOT_ALLOWED }, session),
        };
        let mut bytes = Vec::with_capacity(68);
        bytes.extend([VERSION, tag]);
        bytes.extend(session);
        match self {
            Self::Ask { .. } | Self::Rights { .. } => {}
            Self::Release { grant, .. } => bytes.extend(grant),
            Self::Control { ended, grant, .. } => {
                bytes.extend(grant);
                let reason = match ended {
                    None => 0,
                    Some(EndReason::TakenBack) => 1,
                    Some(EndReason::LocalInput) => 2,
                    Some(EndReason::ShareEnded) => 3,
                    Some(EndReason::Released) => 4,
                    Some(EndReason::Disconnected) => 5,
                };
                bytes.extend([u8::from(ended.is_none()), reason]);
            }
            Self::Input { event, grant, .. } => {
                bytes.extend(grant);
                use AppInput::*;
                match event {
                    PointerMove { x, y } | PointerDown { x, y, .. } | PointerUp { x, y, .. } | Scroll { x, y, .. } => {
                        bytes.push(match event { PointerMove { .. } => 1, PointerDown { .. } => 2, PointerUp { .. } => 3, _ => 4 });
                        bytes.extend(x.to_le_bytes());
                        bytes.extend(y.to_le_bytes());
                        match event {
                            PointerDown { button, mods, .. } | PointerUp { button, mods, .. } => bytes.extend([*button, pack(*mods)]),
                            Scroll { dx, dy, mods, .. } => {
                                bytes.extend(dx.to_le_bytes());
                                bytes.extend(dy.to_le_bytes());
                                bytes.push(pack(*mods));
                            }
                            _ => {}
                        }
                    }
                    KeyDown { key, mods } | KeyUp { key, mods } => {
                        bytes.push(if matches!(event, KeyDown { .. }) { 5 } else { 6 });
                        bytes.extend(key.to_index().to_le_bytes());
                        bytes.push(pack(*mods));
                    }
                    Text(s) => { bytes.push(7); bytes.extend_from_slice(s.as_bytes()); }
                }
            }
        }
        bytes
    }

    pub fn decode(bytes: &[u8], size: (f64, f64)) -> Result<Option<Self>, &'static str> {
        Self::decode_captured(bytes, size, 0)
    }

    pub(super) fn decode_captured(bytes: &[u8], size: (f64, f64), buttons: u8) -> Result<Option<Self>, &'static str> {
        let (&[version, tag], rest) = bytes.split_first_chunk().ok_or("truncated control header")?;
        if version != VERSION { return Err("unsupported version") }
        if !matches!(tag, INPUT..=NOT_ALLOWED) { return Ok(None) }
        let (&session, mut rest) = rest.split_first_chunk().ok_or("truncated control session")?;
        if matches!(tag, ASK | CANCEL_ASK | ALLOWED | NOT_ALLOWED) {
            skip_tlv_tail(&mut rest)?;
            return Ok(Some(match tag {
                ASK | CANCEL_ASK => Self::Ask { session, asking: tag == ASK },
                _ => Self::Rights { session, allowed: tag == ALLOWED },
            }));
        }
        let (&grant, mut body) = rest.split_first_chunk().ok_or("truncated control grant")?;
        Ok(Some(match tag {
            INPUT => {
                let Some(event) = decode_input(body, size, buttons)? else { return Ok(None) };
                Self::Input { session, grant, event }
            }
            CONTROL => {
                let (state, mut tail) = body.split_first_chunk().ok_or("truncated control state")?;
                skip_tlv_tail(&mut tail)?;
                let ended = match state {
                    [1, 0] => None,
                    [0, 1] => Some(EndReason::TakenBack),
                    [0, 2] => Some(EndReason::LocalInput),
                    [0, 3] => Some(EndReason::ShareEnded),
                    [0, 4] => Some(EndReason::Released),
                    [0, 5] => Some(EndReason::Disconnected),
                    _ => return Err("invalid control state"),
                };
                Self::Control { session, grant, ended }
            }
            RELEASE => {
                skip_tlv_tail(&mut body)?;
                Self::Release { session, grant }
            }
            _ => unreachable!(),
        }))
    }
}

pub(super) fn valid_point((w, h): (f64, f64), x: f64, y: f64, captured: bool) -> bool {
    if !w.is_finite() || !h.is_finite() || w <= 0. || h <= 0. || !x.is_finite() || !y.is_finite() { return false }
    if captured {
        (-w..=2. * w).contains(&x) && (-h..=2. * h).contains(&y)
    } else {
        (0.0..=w).contains(&x) && (0.0..=h).contains(&y)
    }
}

fn decode_input(body: &[u8], size: (f64, f64), buttons: u8) -> Result<Option<AppInput>, &'static str> {
    let (&kind, payload) = body.split_first().ok_or("missing input kind")?;
    let len = match kind {
        1 => 16,             // x, y
        2 | 3 => 18,         // x, y, button, modifiers
        4 => 33,             // x, y, dx, dy, modifiers
        5 | 6 => 5,          // key index, modifiers
        7 => {
            let text = std::str::from_utf8(payload).map_err(|_| "invalid input text")?;
            return Ok(Some(AppInput::Text(text.to_owned())));
        }
        _ => return Ok(None),
    };
    let (fields, mut tail) = payload.split_at_checked(len).ok_or("truncated input event")?;
    skip_tlv_tail(&mut tail)?;
    Ok(Some(match kind {
        1..=4 => {
            let x = f64::from_le_bytes(fields[..8].try_into().unwrap());
            let y = f64::from_le_bytes(fields[8..16].try_into().unwrap());
            let captured = match kind {
                1 => buttons != 0,
                3 => fields[16] <= 2 && buttons & (1 << fields[16]) != 0,
                _ => false,
            };
            if !valid_point(size, x, y, captured) { return Err("invalid pointer coordinates") }
            match kind {
                1 => AppInput::PointerMove { x, y },
                2 | 3 => {
                    let button = fields[16];
                    if button > 2 { return Err("invalid pointer button") }
                    let mods = unpack(fields[17]).ok_or("invalid modifiers")?;
                    if kind == 2 { AppInput::PointerDown { x, y, button, mods } }
                    else { AppInput::PointerUp { x, y, button, mods } }
                }
                _ => {
                    let dx = f64::from_le_bytes(fields[16..24].try_into().unwrap());
                    let dy = f64::from_le_bytes(fields[24..32].try_into().unwrap());
                    let mods = unpack(fields[32]).ok_or("invalid modifiers")?;
                    if !dx.is_finite() || !dy.is_finite() { return Err("invalid scroll delta") }
                    AppInput::Scroll { x, y, dx, dy, mods }
                }
            }
        }
        5 | 6 => {
            let key = KeyCode::from_index(u32::from_le_bytes(fields[..4].try_into().unwrap()));
            let mods = unpack(fields[4]).ok_or("invalid modifiers")?;
            if key.is_unknown() { return Err("invalid key") }
            if kind == 5 { AppInput::KeyDown { key, mods } } else { AppInput::KeyUp { key, mods } }
        }
        _ => unreachable!(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn take_back_refuses_queued_input_while_the_shared_app_is_busy() {
        let mut rig = crate::app_link::RecordingRig::new();
        rig.app.scan();
        rig.wait(|r| !r.app.apps().is_empty());
        let key = rig.app.apps()[0].key.clone();
        let link = rig.app.link(&key);
        rig.live(link);
        let window = rig.app.state(link).unwrap().windows[0].id;
        rig.app.allow_input(link);
        rig.hold_input(true);
        let (x, y, button, mods) = (2., 3., 0, Mods::default());
        rig.app.inject(link, window, AppInput::PointerDown { x, y, button, mods });
        rig.wait_for_held_input();
        rig.app.inject(link, window, AppInput::PointerUp { x, y, button, mods });
        rig.app.revoke_input(link);
        assert!(!rig.app.state(link).unwrap().input_allowed);
        assert_eq!(rig.app.inputs_delivered(link), 0, "the shared app is still holding the first command");
        rig.app.inject(link, window, AppInput::Text("three".into()));
        rig.hold_input(false);
        rig.barrier(link);
        let commands = |rig: &crate::app_link::RecordingRig| rig.input_lines().iter()
            .map(|line| makepad_remote::Req::parse(line)).collect::<Vec<_>>();
        let log = commands(&rig);
        assert_eq!(log.len(), 2, "only the admitted Down and the worker's release reach the app");
        assert_eq!((log[0].get("k"), log[1].get("k")), (Some("down"), Some("up")));
        for field in ["w", "x", "y", "b", "if_user_seq"] {
            assert_eq!(log[0].get(field), log[1].get(field), "the release preserves {field}");
        }
        assert_eq!(rig.app.inputs_refused(link), 2, "queued and later viewer input were refused");
        assert_eq!(rig.app.inputs_delivered(link), 1, "only the viewer command already handed over completed");

        let key = KeyCode::KeyA;
        rig.app.allow_input(link);
        rig.hold_input(true);
        rig.app.inject(link, window, AppInput::KeyDown { key, mods });
        rig.wait_for_held_input();
        rig.app.revoke_input(link);
        rig.app.inject(link, window, AppInput::Text("after revoke".into()));
        rig.app.allow_input(link);
        rig.app.inject(link, window, AppInput::Text("new grant".into()));
        rig.hold_input(false);
        rig.barrier(link);
        let log = commands(&rig);
        assert_eq!(log.len(), 5, "regrant leaves no path for the revoked text");
        assert_eq!((log[2].cmd.as_str(), log[2].get("k")), ("key", Some("down")));
        assert_eq!((log[3].cmd.as_str(), log[3].get("k")), ("key", Some("up")));
        for field in ["w", "c", "if_user_seq"] { assert_eq!(log[2].get(field), log[3].get(field)); }
        assert_eq!(log[4].get("t"), Some("new grant"), "cleanup precedes the new grant's input");

        rig.app.inject(link, window, AppInput::PointerDown { x, y, button, mods });
        rig.app.inject(link, window, AppInput::PointerMove { x: -4., y: 7. });
        rig.app.inject(link, window, AppInput::KeyDown { key, mods });
        rig.app.inject(link, window, AppInput::KeyDown { key, mods });
        rig.barrier(link);
        rig.app.unlink(link);
        rig.wait(|r| matches!(&r.app.state(link).unwrap().phase, crate::app_link::LinkPhase::Ended(_)));
        let log = commands(&rig);
        let cleanup = &log[9..];
        assert_eq!(cleanup.len(), 2, "unlink releases the held button and the repeated key once each");
        let button_up = cleanup.iter().find(|r| r.cmd == "mouse").unwrap();
        assert_eq!((button_up.get("k"), button_up.get("x"), button_up.get("y")), (Some("up"), Some("-4"), Some("7")),
            "unlink finishes the drag at its last applied outside position");
        let key_up = cleanup.iter().find(|r| r.cmd == "key").unwrap();
        assert_eq!((key_up.get("k"), key_up.get("c")), (Some("up"), Some("KeyA")));

        let link = rig.app.link(&rig.app.apps()[0].key.clone());
        rig.live(link);
        rig.app.allow_input(link);
        rig.hold_input(true);
        rig.app.inject(link, window, AppInput::KeyDown { key, mods });
        rig.wait_for_held_input();
        assert_eq!(rig.app.quit_input(false), crate::app_link::InputQuit::Wait,
            "a Down already handed over can still apply after the first quit request");
        rig.app.shutdown();
        assert!(!rig.app.state(link).unwrap().input_allowed, "quit closes admission before the app replies");
        rig.app.inject(link, window, AppInput::Text("after quit".into()));
        rig.hold_input(false);
        rig.wait(|r| matches!(&r.app.state(link).unwrap().phase, crate::app_link::LinkPhase::Ended(_)));
        let log = commands(&rig);
        assert_eq!(log.len(), 13);
        assert_eq!((log[11].get("k"), log[12].get("k"), log[12].get("c")),
            (Some("down"), Some("up"), Some("KeyA")), "worker shutdown finishes the release before Ended");
    }

    #[test]
    fn a_press_separates_two_bursts_of_pointer_motion_in_one_frame() {
        let mut motion = FrameMotion::default();
        let mut received = Vec::new();
        let mut emit = |i: PanelInput| received.push(i.event);
        let panel = |event| PanelInput { session: [1; 16], grant: [2; 16], event };
        let moved = |x| panel(AppInput::PointerMove { x, y: 0. });
        assert!(motion.push(moved(1.), &mut emit));
        assert!(!motion.push(moved(2.), &mut emit));
        let down = panel(AppInput::PointerDown { x: 3., y: 0., button: 0, mods: Mods::default() });
        assert!(!motion.push(down, &mut emit));
        assert!(motion.push(moved(4.), &mut emit));
        assert!(!motion.push(moved(5.), &mut emit));
        motion.flush(&mut emit);
        motion.flush(emit);
        assert!(matches!(received.as_slice(), [
            AppInput::PointerMove { x: 2., .. }, AppInput::PointerDown { x: 3., .. }, AppInput::PointerMove { x: 5., .. }
        ]), "each uninterrupted run contributes its last move, in order");
    }

    #[test]
    fn control_wire_ignores_unknown_tag_and_tlv_tail() {
        let session = [10; 16];
        let grant = [11; 16];
        let size = (100., 80.);
        assert!(matches!(ControlMessage::decode(&[VERSION, 99], size), Ok(None)));
        assert!(matches!(ControlMessage::decode(&[2, 99], size), Err("unsupported version")));
        assert!(matches!(ControlMessage::decode(&[2], size), Err("truncated control header")));
        for message in [
            ControlMessage::Ask { session, asking: true },
            ControlMessage::Rights { session, allowed: false },
            ControlMessage::Release { session, grant },
            ControlMessage::Control { session, grant, ended: Some(EndReason::LocalInput) },
            ControlMessage::Input { session, grant, event: AppInput::PointerMove { x: 7., y: 4. } },
        ] {
            let prefix = message.encode();
            let mut wire = prefix.clone();
            makepad_town_core::tlv::write_tlv(&mut wire, 0x7f, b"ext");
            let decoded = ControlMessage::decode(&wire, size).unwrap().expect("known tag");
            assert_eq!(decoded.encode(), prefix);
            wire.pop();
            assert!(ControlMessage::decode(&wire, size).is_err());
        }
        let mut wire = vec![VERSION, INPUT];
        wire.extend(session);
        wire.extend(grant);
        wire.push(0);
        assert!(matches!(ControlMessage::decode(&wire, size), Ok(None)));
    }

    #[test]
    fn input_topic_accepts_only_typed_events_in_the_shared_window() {
        use AppInput::*;
        let size = (300., 120.);
        let session = [25; 16];
        let grant = [19; 16];
        let mods = Mods { shift: true, control: false, alt: true, logo: false };
        let message = |event| ControlMessage::Input { session, grant, event };
        let round_trip = |msg: ControlMessage| {
            let bytes = msg.encode();
            let decoded = ControlMessage::decode(&bytes, size).expect("valid message").expect("known control tag");
            assert_eq!(format!("{decoded:?}"), format!("{msg:?}"), "all fields survive");
            assert_eq!(decoded.encode(), bytes);
        };
        for event in [
            PointerMove { x: 0., y: 120. }, PointerDown { x: 300., y: 0., button: 0, mods },
            PointerUp { x: 1.25, y: 17., button: 2, mods },
            Scroll { x: 2., y: 4., dx: -12.5, dy: 6., mods },
            KeyDown { key: KeyCode::KeyA, mods }, KeyUp { key: KeyCode::KeyA, mods },
            Text("first\n次".into()), Text(String::new()),
        ] { round_trip(message(event)); }
        for ended in [None, Some(EndReason::TakenBack), Some(EndReason::LocalInput),
            Some(EndReason::Released), Some(EndReason::ShareEnded), Some(EndReason::Disconnected)] {
            round_trip(ControlMessage::Control { session, grant, ended });
        }
        round_trip(ControlMessage::Release { session, grant });
        for enabled in [true, false] {
            for msg in [ControlMessage::Ask { session, asking: enabled }, ControlMessage::Rights { session, allowed: enabled }] {
                let mut bytes = msg.encode();
                assert_eq!(bytes[0], 1);
                assert!(bytes.len() < 34, "a version-one reader expecting a grant skips this message");
                round_trip(msg);
                bytes.push(0);
                assert!(ControlMessage::decode(&bytes, size).is_err(), "a session-only message rejects an incomplete tail");
            }
        }
        for event in [PointerMove { x: -0.01, y: 0. }, PointerMove { x: 1., y: 120.01 },
            PointerMove { x: f64::NAN, y: 1. }, Scroll { x: 1., y: 2., dx: f64::INFINITY, dy: 0., mods },
            PointerDown { x: 0., y: 3., button: 3, mods }, KeyUp { key: KeyCode::Unknown, mods }] {
            assert!(ControlMessage::decode(&message(event).encode(), size).is_err());
        }
        let captured = |event| matches!(ControlMessage::decode_captured(&message(event).encode(), size, 1), Ok(Some(_)));
        assert!(captured(PointerMove { x: -300., y: 240. }));
        assert!(captured(PointerUp { x: 600., y: -120., button: 0, mods }));
        assert!(!captured(PointerUp { x: 600., y: -120., button: 1, mods }), "only the captured button");
        assert!(!captured(PointerMove { x: -300.01, y: 0. }), "one window beyond each edge");
        assert!(!captured(PointerMove { x: f64::NAN, y: 0. }));
        assert!(!captured(PointerDown { x: -1., y: 0., button: 0, mods }), "a new press starts inside");
        assert!(!captured(Scroll { x: -1., y: 0., dx: 1., dy: 0., mods }), "scroll never inherits capture");
        let press = message(PointerDown { x: 1., y: 2., button: 0, mods }).encode();
        for end in 0..press.len() {
            assert!(ControlMessage::decode(&press[..end], size).is_err(), "truncated at {end}");
        }
        for (offset, byte) in [(0, 2), (press.len() - 1, 16)] {
            let mut bad = press.clone();
            bad[offset] = byte;
            assert!(ControlMessage::decode(&bad, size).is_err());
        }
        for offset in [1, 34] {
            let mut future = press.clone();
            future[offset] = 55;
            assert!(matches!(ControlMessage::decode(&future, size), Ok(None)));
        }
        let mut bad = message(Text(String::new())).encode();
        bad.push(0xff);
        assert!(ControlMessage::decode(&bad, size).is_err(), "text must be UTF-8");
        for extra in [ControlMessage::Release { session, grant }, ControlMessage::Control { session, grant, ended: None },
            message(KeyDown { key: KeyCode::ReturnKey, mods })] {
            let mut bytes = extra.encode();
            bytes.push(0);
            assert!(ControlMessage::decode(&bytes, size).is_err(), "fixed fields reject an incomplete tail");
        }
    }
}
