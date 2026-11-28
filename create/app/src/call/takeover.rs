//! Each spell of control is bound to a share, an authenticated connection and a fresh grant.
use super::{CallController, ShareRole};
use crate::app_link::{AppInput, AppLinks, LinkPhase};
use crate::call::{control::{ControlMessage, EndReason, GrantId, PanelInput}, topic};
use makepad_town_core::{event::SessionId, key::PublicKey};
use makepad_widgets::log;

struct Grant {
    session: SessionId,
    id: GrantId,
    peer: (PublicKey, u64),
    buttons: u8,
}

impl Grant {
    fn observe(&mut self, event: &AppInput) -> bool {
        match *event {
            AppInput::PointerDown { button, .. } if button < 3 => {
                if self.buttons & (1 << button) != 0 { return false }
                self.buttons |= 1 << button;
            }
            AppInput::PointerUp { button, .. } if button < 3 => {
                if self.buttons & (1 << button) == 0 { return false }
                self.buttons &= !(1 << button);
            }
            AppInput::PointerDown { .. } | AppInput::PointerUp { .. } => return false,
            _ => {}
        }
        true
    }
}

/// Why this seat turned a control input away. Every refusal has exactly one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The viewer's panel sent input while not watching, or the sharer's own panel did.
    NotWatching,
    /// The input named no current Allow: before one, after it ended, or for another share.
    NoAllow,
    /// A press without its release, or a release without its press.
    Unpaired,
    /// No live share to take it.
    NoShare,
    /// The bytes didn't read as a control message for this window.
    Unreadable,
    /// A control or release message that doesn't fit this seat's state.
    Stale,
}

impl Refusal {
    const ALL: [Refusal; 6] = [Self::NotWatching, Self::NoAllow, Self::Unpaired, Self::NoShare, Self::Unreadable, Self::Stale];

    fn words(self) -> &'static str {
        match self {
            Self::NotWatching => "not watching",
            Self::NoAllow => "no current Allow",
            Self::Unpaired => "unpaired button",
            Self::NoShare => "no live share",
            Self::Unreadable => "unreadable",
            Self::Stale => "stale control message",
        }
    }
}

/// Control input on this seat, for the share summary line: what the viewer sent, what the
/// sharer received, handed to its app and the app applied, and every refusal by its reason.
#[derive(Default)]
pub(super) struct InputCounts {
    sent: u64,
    received: u64,
    handed: u64,
    refused: [u64; Refusal::ALL.len()],
    /// The reasons already logged since the current Allow began.
    logged: u8,
    undelivered: u64,
    /// The last press this seat sent or handed over, in the shared window's points.
    last_press: Option<(f64, f64)>,
    /// The app link's own counts for the shared window, read as input arrives.
    applied: u64,
    app_refused: u64,
}

impl InputCounts {
    fn refuse(&mut self, why: Refusal) {
        let i = Refusal::ALL.iter().position(|r| *r == why).unwrap_or(0);
        self.refused[i] += 1;
        if self.logged & (1 << i) == 0 {
            self.logged |= 1 << i;
            log!("Share control input refused: {} (total {})", why.words(), self.refused[i]);
        }
    }

    pub(super) fn summary(&self) -> String {
        let total: u64 = self.refused.iter().sum();
        let reasons: Vec<_> = Refusal::ALL.iter().zip(self.refused).filter(|(_, n)| *n > 0)
            .map(|(r, n)| format!("{} {n}", r.words())).collect();
        let press = self.last_press.map_or("none".into(), |(x, y)| format!("{x:.0},{y:.0}"));
        format!("control input sent {}, received {}, handed {}, applied {}, app refused {}, refused {total}{}, undelivered {}, last press {press}",
            self.sent, self.received, self.handed, self.applied, self.app_refused,
            if reasons.is_empty() { String::new() } else { format!(" ({})", reasons.join(", ")) }, self.undelivered)
    }
}

fn press_point(event: &AppInput) -> Option<(f64, f64)> {
    match *event {
        AppInput::PointerDown { x, y, .. } => Some((x, y)),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct Takeover {
    grant: Option<Grant>,
    /// A new subscription to check once the app links are available in receive_control.
    pub(super) new_watchers: Vec<(SessionId, PublicKey)>,
    released: Option<(SessionId, GrantId, PublicKey)>,
    pub(super) input: InputCounts,
}

impl CallController {
    pub fn inputs_refused(&self) -> u64 { self.takeover.input.refused.iter().sum() }

    // The linked takeover test runs on unix only.
    #[cfg(all(test, unix))]
    pub(super) fn inputs_refused_for(&self, why: Refusal) -> u64 {
        self.takeover.input.refused[Refusal::ALL.iter().position(|r| *r == why).unwrap()]
    }

    pub(super) fn local_control(&self) -> bool {
        self.sharing.shown.control_session.and_then(|s| self.sharing.shown.offer(s)).is_some_and(|o| matches!(o.role, ShareRole::Sharer { .. }))
    }

    fn matches_grant(&self, sender: PublicKey, session: SessionId, id: GrantId) -> bool {
        self.takeover.grant.as_ref().is_some_and(|g| g.id == id && g.session == session
            && sender == g.peer.0 && self.peer_epoch_of(&sender) == Some(g.peer))
    }

    fn begin_control(&mut self, session: SessionId, id: GrantId, peer: (PublicKey, u64)) {
        self.takeover.grant = Some(Grant { session, id, peer, buttons: 0 });
        self.takeover.input.logged = 0;
        self.takeover.released = None;
        if self.sharing.shown.watching == Some(session) {
            self.sharing.shown.control.asking = None;
        } else {
            self.sharing.shown.control.requests.retain(|(s, p, _)| (*s, *p) != (session, peer.0));
            self.sharing.shown.control.holder = Some((peer.0, self.peer_display_name(&peer.0)));
        }
        self.sharing.shown.control_session = Some(session);
        self.sharing.shown.control.active = Some(id);
        self.sharing.shown.control.notice = None;
    }

    pub(super) fn control_send(&mut self, peer: PublicKey, message: ControlMessage) -> bool {
        if self.send_peer_to(peer, topic::INPUT, message.encode()).is_err() {
            self.takeover.input.undelivered += 1;
            self.input_undelivered(1);
            return false
        }
        true
    }

    /// An ask uses the sharer's existing right or requests one.
    pub fn ask_control(&mut self, session: SessionId, asking: bool) -> bool {
        if self.sharing.shown.watching != Some(session) || self.sharing.shown.control.asking.is_some() == asking
            || (asking && self.matches_session_grant(session)) { return false }
        let Some(peer) = self.sharing.shown.watching_sharer else { return false };
        if !self.control_send(peer, ControlMessage::Ask { session, asking }) { return false }
        self.sharing.shown.control.asking = asking.then_some(session);
        true
    }

    fn matches_session_grant(&self, session: SessionId) -> bool {
        self.takeover.grant.as_ref().is_some_and(|grant| grant.session == session)
    }

    pub(super) fn watcher_started(&mut self, session: SessionId, peer: PublicKey) {
        let allowed = self.peer_can_control(session, &peer);
        self.control_send(peer, ControlMessage::Rights { session, allowed });
        if allowed && self.takeover.grant.is_none() { self.takeover.new_watchers.push((session, peer)); }
    }

    pub(super) fn grant_control(&mut self, apps: &mut AppLinks, session: SessionId, next_peer: PublicKey) -> bool {
        let Some(offer) = self.sharing.shown.offer(session) else { return false };
        let ShareRole::Sharer { link, window } = offer.role else { return false };
        let Some(peer) = self.peer_epoch_of(&next_peer) else { return false };
        if !self.is_viewer(session, &next_peer) || !self.peer_can_control(session, &next_peer)
            || !apps.state(link).is_some_and(|s| s.phase == LinkPhase::Live
                && s.windows.iter().any(|w| w.id == window)) { return false }
        if let Some(old) = &self.takeover.grant {
            if old.session == session && old.peer == peer { return false }
            self.end_control(EndReason::TakenBack, true);
        }
        let mut grant = [0; 16];
        if let Err(e) = makepad_network::tls::os_random(&mut grant) {
            log!("Could not grant control: {e}");
            return false
        }
        self.revoke_ended_shares(apps);
        apps.allow_input(link);
        if !apps.state(link).is_some_and(|s| s.input_allowed) { return false }
        self.begin_control(session, grant, peer);
        self.control_send(next_peer, ControlMessage::Control { session, grant, ended: None });
        true
    }

    pub fn panel_input(&mut self, input: &PanelInput) {
        let refusal = if self.local_control() || self.sharing.shown.watching.is_none() { Some(Refusal::NotWatching) }
            else if !self.sharing.shown.watching_sharer.is_some_and(|p| self.matches_grant(p, input.session, input.grant)) { Some(Refusal::NoAllow) }
            else if !self.takeover.grant.as_mut().is_some_and(|g| g.observe(&input.event)) { Some(Refusal::Unpaired) }
            else { None };
        if let Some(why) = refusal { return self.takeover.input.refuse(why) }
        let peer = self.sharing.shown.watching_sharer.unwrap();
        if self.control_send(peer, ControlMessage::Input { session: input.session, grant: input.grant, event: input.event.clone() }) {
            let counts = &mut self.takeover.input;
            counts.sent += 1;
            if let Some((x, y)) = press_point(&input.event) {
                counts.last_press = Some((x, y));
                log!("Share control press sent at {x:.0},{y:.0} (input {} sent)", counts.sent);
            }
        }
    }

    pub(super) fn end_control(&mut self, reason: EndReason, notify: bool) -> bool {
        let Some(active) = self.takeover.grant.take() else { return false };
        self.sharing.shown.control.active = None;
        self.sharing.shown.control.holder = None;
        let Some(offer) = self.sharing.shown.offer(active.session).cloned() else { return true };
        let (session, grant) = (active.session, active.id);
        let connected = self.peer_epoch_of(&active.peer.0) == Some(active.peer);
        let local = self.local_control();
        if let ShareRole::Sharer { link, .. } = offer.role {
            self.sharing.revokes.push(link);
            if connected && notify {
                self.control_send(active.peer.0, ControlMessage::Control { session, grant, ended: Some(reason) });
                if let Some(link) = self.links.get_mut(&active.peer.0) { link.flush_peer(&mut self.sent); }
            }
        } else if connected && notify {
            if self.control_send(active.peer.0, ControlMessage::Release { session, grant }) {
                self.takeover.released = Some((session, grant, active.peer.0));
            }
        }
        let peer = self.peer_display_name(&active.peer.0);
        let title = &offer.title;
        let line = match reason {
            EndReason::TakenBack if local => "You took back control.".into(),
            EndReason::TakenBack => format!("{peer} took back control."),
            EndReason::LocalInput if local => format!("Paused: you used {title}"),
            EndReason::LocalInput => format!("Paused: {peer} used {title}"),
            EndReason::Released if local => format!("{peer} released control."),
            EndReason::Released => "You released control.".into(),
            EndReason::ShareEnded => "Control ended with the share.".into(),
            EndReason::Disconnected => "Control ended when the link disconnected.".into(),
        };
        self.sharing.shown.control.notice = Some(line.into());
        true
    }

    /// Drain only the input topic. The peer-event dispatcher separately accounts for undelivered messages.
    pub fn receive_control(&mut self, apps: &mut AppLinks) -> bool {
        let mut changed = false;
        for (s, p) in std::mem::take(&mut self.takeover.new_watchers) {
            if self.takeover.grant.is_none() { changed |= self.grant_control(apps, s, p); }
        }
        for (sender, body) in self.take_peer_with_sender(topic::INPUT) {
            if self.sharing.shown.offers.is_empty() {
                self.takeover.input.refuse(Refusal::NoShare);
                continue
            }
            // Input coordinates need the named window's size. Use the grant's window for
            // classifying a foreign message; the grant/session checks still reject it.
            let session = body.get(2..18).and_then(|id| <SessionId>::try_from(id).ok());
            let offer = session.and_then(|s| self.sharing.shown.offer(s));
            let size = offer.or_else(|| self.sharing.shown.control_session.and_then(|s| self.sharing.shown.offer(s)))
                .map_or((0., 0.), |o| o.window_size);
            let buttons = self.takeover.grant.as_ref().filter(|g| g.peer.0 == sender && Some(g.session) == session).map_or(0, |g| g.buttons);
            let msg = match ControlMessage::decode_captured(&body, size, buttons) {
                Ok(Some(msg)) => msg,
                Ok(None) => continue,
                Err(_) => {
                    self.takeover.input.refuse(Refusal::Unreadable);
                    continue;
                }
            };
            if matches!(msg, ControlMessage::Input { .. }) { self.takeover.input.received += 1; }
            let Some(offer) = offer.cloned() else {
                self.takeover.input.refuse(match msg {
                    ControlMessage::Input { .. } => Refusal::NoAllow, _ => Refusal::Stale,
                });
                continue
            };
            match (offer.role, msg) {
                (ShareRole::Sharer { .. }, ControlMessage::Ask { session, asking }) => {
                    if !self.is_recipient(session, &sender) { self.takeover.input.refuse(Refusal::Stale); continue }
                    if self.takeover.grant.as_ref().is_some_and(|g| g.session == session && g.peer.0 == sender) { continue }
                    if asking && self.takeover.grant.is_none() && self.peer_can_control(session, &sender)
                        && self.grant_control(apps, session, sender) { changed = true; continue }
                    let pos = self.sharing.shown.control.requests.iter().position(|(s, p, _)| (*s, *p) == (session, sender));
                    match (asking, pos) {
                        (true, None) => {
                            let name = self.peer_display_name(&sender);
                            self.sharing.shown.control.requests.push((session, sender, name));
                        }
                        (false, Some(i)) => { self.sharing.shown.control.requests.remove(i); }
                        _ => continue,
                    }
                    changed = true;
                }
                (ShareRole::Sharer { link, window }, ControlMessage::Input { session, grant, event })
                    if offer.session == session && self.matches_grant(sender, session, grant)
                        && apps.state(link).is_some_and(|s| s.input_allowed) => {
                    if self.takeover.grant.as_mut().is_some_and(|g| g.observe(&event)) {
                        let press = press_point(&event);
                        apps.inject(link, window, event);
                        let counts = &mut self.takeover.input;
                        counts.handed += 1;
                        if let Some((x, y)) = press {
                            counts.last_press = Some((x, y));
                            log!("Share control press at {x:.0},{y:.0} handed to the app (input {} received)", counts.received);
                        }
                    } else { self.takeover.input.refuse(Refusal::Unpaired); }
                }
                (ShareRole::Sharer { .. }, ControlMessage::Release { session, grant })
                    if offer.session == session && self.matches_grant(sender, session, grant) => {
                    changed |= self.end_control(EndReason::Released, true);
                    self.revoke_ended_shares(apps);
                }
                (ShareRole::Viewer { sharer }, ControlMessage::Control { session, grant, ended: None })
                    if sharer == sender && offer.session == session => {
                    if self.matches_grant(sender, session, grant) { continue }
                    if self.sharing.shown.watching != Some(session) || self.takeover.grant.is_some()
                        || (!self.flow.is_voice_peer(&sender) && self.sharing.shown.control.asking != Some(session)
                            && self.sharing.shown.control.allowed != Some(session)) {
                        self.control_send(sender, ControlMessage::Release { session, grant });
                    } else if let Some(peer) = self.peer_epoch_of(&sender) {
                        self.begin_control(session, grant, peer);
                        changed = true;
                    }
                }
                (ShareRole::Viewer { sharer }, ControlMessage::Rights { session, allowed }) if sharer == sender => {
                    if self.sharing.shown.watching == Some(session) {
                        let right = allowed.then_some(session);
                        changed |= self.sharing.shown.control.allowed != right;
                        self.sharing.shown.control.allowed = right;
                    }
                }
                (ShareRole::Viewer { sharer }, ControlMessage::Control { session, grant, ended: Some(EndReason::Released) })
                    if sharer == sender && self.takeover.released == Some((session, grant, sender)) => self.takeover.released = None,
                (ShareRole::Viewer { sharer }, ControlMessage::Control { session, grant, ended: Some(reason) })
                    if sharer == sender && self.matches_grant(sender, session, grant) => changed |= self.end_control(reason, false),
                (_, ControlMessage::Input { .. }) => self.takeover.input.refuse(Refusal::NoAllow),
                _ => self.takeover.input.refuse(Refusal::Stale),
            }
        }
        if let Some(ShareRole::Sharer { link, .. }) = self.sharing.shown.control_session.and_then(|s| self.sharing.shown.offer(s)).map(|o| o.role) {
            let counts = &mut self.takeover.input;
            (counts.applied, counts.app_refused) = (apps.inputs_delivered(link), apps.inputs_refused(link));
        }
        changed
    }
}
