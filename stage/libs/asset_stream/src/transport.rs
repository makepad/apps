//! The one authority link between a DJ load and a VJ load of the same media.
//!
//! While linked the DJ deck is authoritative: it publishes what its audio
//! actually acknowledged (serial, source position, output clock anchor,
//! signed rate, play state, Tail/History, loop, epochs) and the VJ layer
//! follows with its own transport controls disabled. The link is between
//! exact consumer loads after the media identities were checked equal;
//! incidental counters of the two consumers are never compared. Eject,
//! media change or unlink of either load ends the link and the follower's
//! own controls return.
//!
//! This module validates and carries transport state. It does not integrate
//! position, fold loops or quantize to beats: that mathematics belongs to
//! the existing transports and the existing beat clock.

use crate::identity::{ConsumerId, ConsumerLoad, MediaIdentity};
use crate::limits::*;
use crate::timeline::{SampleRange, TimelineSnapshot};

/// Tail follows the committed frontier behind a bounded prebuffer; History
/// plays committed material anywhere (backscrub, loops, stems).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FollowMode {
    Tail { prebuffer_frames: u64 },
    History,
}

/// How a loop folds the unwrapped path coordinate (the existing VJ platter:
/// sawtooth, triangle, clamp).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopShape {
    Once,
    Loop,
    Bounce,
}

/// A loop length in beats of the EXISTING beat clock, as a ratio; the
/// clock itself is not modelled here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BeatQuantize {
    pub beats_num: u32,
    pub beats_den: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopDefinition {
    pub range: SampleRange,
    pub shape: LoopShape,
    pub quantize: Option<BeatQuantize>,
}

/// How a range edit maps the playing position into the new range. There is
/// deliberately no policy-free "set range".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeEditPolicy {
    /// DJ halve/double: the position folds modulo the new length.
    DjFoldModulo,
    /// VJ trim: the phase on the full sweep period is preserved.
    VjTrimPreservePhase,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangeEdit {
    pub range: SampleRange,
    pub policy: RangeEditPolicy,
}

/// The platter's path state, published together so a follower never infers
/// direction from two positions: the unwrapped coordinate `q` (source
/// seconds from the loop origin), whether the current leg runs forward, and
/// the on-screen velocity (signed, natural-speed units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathState {
    pub q: f64,
    pub leg_forward: bool,
    pub screen_velocity: f64,
}

/// Discontinuities the follower must honour instead of smoothing over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TransportEpoch {
    pub cue: u32,
    pub seek: u32,
    /// Loop/range boundary definition changes.
    pub boundary: u32,
}

/// Where the output clock stood when `position` was true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputAnchor {
    pub output_frame: u64,
    pub output_rate: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuthoritySnapshot {
    /// Strictly increasing per link: every published state, including plain
    /// position updates while playing.
    pub publication: u64,
    /// The authority's last operator command serial its audio acknowledged;
    /// never regresses, repeats while no new command was accepted.
    pub accepted_serial: u64,
    /// Source sample position (source frames, fractional).
    pub position: f64,
    pub anchor: OutputAnchor,
    /// Signed: negative plays backwards.
    pub rate: f64,
    pub playing: bool,
    pub mode: FollowMode,
    pub looping: Option<LoopDefinition>,
    pub path: Option<PathState>,
    pub epoch: TransportEpoch,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransportError {
    NotFinite(&'static str),
    OutsideCommitted(&'static str),
    EmptyLoop,
    TailWithLoop,
    /// Kept for compatibility; [`AuthoritySnapshot::validate`] no longer
    /// reports it (an accepted tail may drain a final timeline).
    TailOnFinal,
    PrebufferTooLong,
    BadQuantize,
    PathWithoutLoop,
    NoOutputRate,
}

impl AuthoritySnapshot {
    /// Check against the timeline the authority reads right now. Loops must
    /// lie in committed material; nothing refers to an expected future
    /// length.
    ///
    /// This validates an accepted OBSERVATION of the authority, not a
    /// command: a follower that accepted `Tail` keeps it while the timeline
    /// turns terminal and drains what was committed, so `Tail` on a final
    /// timeline is not refused here. A NEW tail command is refused where
    /// commands are admitted (the audio deck's tail check), not here.
    pub fn validate(&self, timeline: &TimelineSnapshot) -> Result<(), TransportError> {
        let committed = timeline.committed();
        if !self.position.is_finite() {
            return Err(TransportError::NotFinite("position"));
        }
        if !self.rate.is_finite() {
            return Err(TransportError::NotFinite("rate"));
        }
        if self.anchor.output_rate == 0 {
            return Err(TransportError::NoOutputRate);
        }
        if self.position < committed.start as f64 || self.position > committed.end as f64 {
            return Err(TransportError::OutsideCommitted("position"));
        }
        match self.mode {
            FollowMode::Tail { prebuffer_frames } => {
                if self.looping.is_some() {
                    return Err(TransportError::TailWithLoop);
                }
                let rate = timeline.audio.map_or(0, |audio| audio.sample_rate) as u64;
                if prebuffer_frames > rate * MAX_TAIL_PREBUFFER_SECS {
                    return Err(TransportError::PrebufferTooLong);
                }
            }
            FollowMode::History => {}
        }
        if let Some(looping) = &self.looping {
            if looping.range.is_empty() {
                return Err(TransportError::EmptyLoop);
            }
            if !looping.range.within(&committed) {
                return Err(TransportError::OutsideCommitted("loop"));
            }
            if looping.quantize.is_some_and(|q| q.beats_num == 0 || q.beats_den == 0) {
                return Err(TransportError::BadQuantize);
            }
        }
        match (&self.path, &self.looping) {
            (Some(_), None) => return Err(TransportError::PathWithoutLoop),
            (Some(path), Some(_)) if !path.q.is_finite() || !path.screen_velocity.is_finite() => {
                return Err(TransportError::NotFinite("path"));
            }
            _ => {}
        }
        Ok(())
    }

    /// Setting a history loop leaves Tail: the one legal way to loop while
    /// following a growing recording.
    pub fn with_history_loop(mut self, looping: LoopDefinition, path: PathState) -> Self {
        self.mode = FollowMode::History;
        self.looping = Some(looping);
        self.path = Some(path);
        self.epoch.boundary = self.epoch.boundary.wrapping_add(1);
        self
    }
}

// ---------------------------------------------------------------------------
// links
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LinkId(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub struct AuthorityLink {
    pub id: LinkId,
    pub media: MediaIdentity,
    pub authority: ConsumerLoad,
    pub follower: ConsumerLoad,
    pub latest: Option<AuthoritySnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LinkError {
    DifferentMedia,
    SameConsumer,
    FollowerAlreadyLinked,
    FollowerIsAuthority,
    Full,
    UnknownLink,
    NotAuthority,
    NotAParty,
    TimelineMismatch,
    StalePublication { latest: u64 },
    SerialRegressed { latest: u64 },
    Invalid(TransportError),
}

/// Every live authority link, bounded. Owned by the application on its UI
/// thread; plain data, no locks.
#[derive(Default)]
pub struct AuthorityLinks {
    links: Vec<AuthorityLink>,
    next: u64,
}

impl AuthorityLinks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Link `follower` to `authority`. Each side names the media it has
    /// loaded; they must be the same identity. One authority may lead
    /// several followers; a follower follows one authority and is not
    /// itself an authority.
    pub fn link(
        &mut self,
        authority: ConsumerLoad,
        authority_media: MediaIdentity,
        follower: ConsumerLoad,
        follower_media: MediaIdentity,
    ) -> Result<LinkId, LinkError> {
        if authority_media != follower_media {
            return Err(LinkError::DifferentMedia);
        }
        if authority.consumer == follower.consumer {
            return Err(LinkError::SameConsumer);
        }
        if self.links.iter().any(|link| link.follower.consumer == follower.consumer) {
            return Err(LinkError::FollowerAlreadyLinked);
        }
        if self.links.iter().any(|link| link.follower.consumer == authority.consumer || link.authority.consumer == follower.consumer) {
            return Err(LinkError::FollowerIsAuthority);
        }
        if self.links.len() >= MAX_AUTHORITY_LINKS {
            return Err(LinkError::Full);
        }
        self.next += 1;
        let id = LinkId(self.next);
        self.links.push(AuthorityLink { id, media: authority_media, authority, follower, latest: None });
        Ok(id)
    }

    /// The authority publishes what its audio acknowledged, checked against
    /// the timeline it reads.
    pub fn publish(&mut self, id: LinkId, from: ConsumerLoad, timeline: &TimelineSnapshot, snapshot: AuthoritySnapshot) -> Result<(), LinkError> {
        let link = self.links.iter_mut().find(|link| link.id == id).ok_or(LinkError::UnknownLink)?;
        if link.authority != from {
            return Err(LinkError::NotAuthority);
        }
        if timeline.media != link.media {
            return Err(LinkError::TimelineMismatch);
        }
        if let Some(latest) = &link.latest {
            if snapshot.publication <= latest.publication {
                return Err(LinkError::StalePublication { latest: latest.publication });
            }
            if snapshot.accepted_serial < latest.accepted_serial {
                return Err(LinkError::SerialRegressed { latest: latest.accepted_serial });
            }
        }
        snapshot.validate(timeline).map_err(LinkError::Invalid)?;
        link.latest = Some(snapshot);
        Ok(())
    }

    /// What `follower` (that exact load) follows, if it is linked.
    pub fn following(&self, follower: ConsumerLoad) -> Option<&AuthorityLink> {
        self.links.iter().find(|link| link.follower == follower)
    }

    /// Whether this consumer's own transport controls are disabled: only
    /// while it follows an authority.
    pub fn controls_locked(&self, consumer: ConsumerId) -> bool {
        self.links.iter().any(|link| link.follower.consumer == consumer)
    }

    pub fn links(&self) -> &[AuthorityLink] {
        &self.links
    }

    /// Either party ends the link.
    pub fn unlink(&mut self, id: LinkId, by: ConsumerLoad) -> Result<AuthorityLink, LinkError> {
        let index = self.links.iter().position(|link| link.id == id).ok_or(LinkError::UnknownLink)?;
        let link = &self.links[index];
        if link.authority != by && link.follower != by {
            return Err(LinkError::NotAParty);
        }
        Ok(self.links.swap_remove(index))
    }

    /// A consumer ejected or loaded something else: every link of any of
    /// its loads other than `current` (or all of them, for `None`) ends.
    pub fn consumer_changed(&mut self, consumer: ConsumerId, current: Option<ConsumerLoad>) -> Vec<AuthorityLink> {
        let mut ended = Vec::new();
        let mut index = 0;
        while index < self.links.len() {
            let link = &self.links[index];
            let involved = |load: &ConsumerLoad| load.consumer == consumer && Some(*load) != current;
            if involved(&link.authority) || involved(&link.follower) {
                ended.push(self.links.swap_remove(index));
            } else {
                index += 1;
            }
        }
        ended
    }
}
