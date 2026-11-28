//! Bodies on the link's reliable Call channel. Versions, flags and tags are single bytes.

use makepad_town_core::{key::PublicKey, share::hello::MediaCaps, tlv::{read_tlv, skip_tlv_tail, write_tlv}};

pub type CallId = [u8; 16];
pub type Secret = [u8; 32];

pub(super) const VERSION: u8 = 1;
// Tags 7 and 8 are reserved for Camera and Resume.
const RING: u8 = 1;
const ANSWER: u8 = 2;
const DECLINE: u8 = 3;
const CANCEL: u8 = 4;
const END: u8 = 5;
const MUTE: u8 = 6;
const PEER: u8 = 9;
// The high bit marks a non-final piece of a peer message on the bounded carrier.
const PEER_MORE: u8 = PEER | 0x80;
pub(super) const PEER_PAYLOAD: usize = makepad_town_core::connect::MAX_CALL - 3;

pub mod topic {
    pub const TWEAK: u8 = 1;
    pub const INPUT: u8 = 2;
    pub const BOARD_LIVE: u8 = 3;
    /// Tests send it behind earlier messages to learn those have arrived. Other builds drop it.
    #[cfg(test)]
    pub const PROBE: u8 = 255;

    /// Whether this build takes a peer topic. A newer peer may send topics this build does not
    /// know; they are dropped on arrival, so they never pile up for the life of the link.
    pub fn taken(topic: u8) -> bool {
        #[cfg(test)]
        if topic == PROBE { return true }
        matches!(topic, TWEAK | INPUT | BOARD_LIVE)
    }
}

pub(super) fn encode_peer(topic: u8, body: &[u8], complete: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(3 + body.len());
    bytes.extend_from_slice(&[VERSION, if complete { PEER } else { PEER_MORE }, topic]);
    bytes.extend_from_slice(body);
    bytes
}

/// Split out the opaque topic envelope before the call flow sees an incoming message.
pub(super) fn peer_payload(bytes: &[u8]) -> Option<(u8, bool, &[u8])> {
    match bytes {
        [VERSION, tag @ (PEER | PEER_MORE), topic, body @ ..] => Some((*topic, *tag == PEER, body)),
        _ => None,
    }
}

const TRANSCRIBE: u8 = 0x54;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Why {
    Declined,
    Busy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ShareEndReason {
    Stopped = 0,
    AppClosedOrRestarted = 1,
    LinkDropped = 2,
}

const CALL_JOIN: u8 = 16;
const CALL_JOIN_ACK: u8 = 17;
const MEDIA_CAPS_NOTICE: u8 = 18;
const RECORDING_STATE: u8 = 19;
const RECORDING_CONSENT: u8 = 20;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RingExt {
    pub town: Option<[u8; 32]>,
    pub roster: Vec<PublicKey>,
    pub media_caps: Option<MediaCaps>,
    /// The intended callee seat on a card call or call-back.
    pub target_seat: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AnswerExt {
    pub town: Option<[u8; 32]>,
    pub media_caps: Option<MediaCaps>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ShareOfferExt {
    pub town: Option<[u8; 32]>,
    pub codecs: Option<u32>,
    pub full_chroma: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ShareWatchExt {
    pub media_caps: Option<MediaCaps>,
}

#[derive(PartialEq)]
#[cfg_attr(test, derive(Debug))]
pub enum Message {
    Ring { call_id: CallId, secret: Secret, video: bool, ext: RingExt },
    Answer { call_id: CallId, secret: Secret, ext: AnswerExt },
    Decline { call_id: CallId, why: Why },
    Cancel { call_id: CallId },
    End { call_id: CallId },
    Mute { call_id: CallId, on: bool },
    /// The sender transcribes their own microphone. Never permission to transcribe the receiver.
    Transcribe { call_id: CallId, on: bool },
    Share { session: CallId, title: String, w: f32, h: f32, ext: ShareOfferExt },
    ShareOff { session: CallId, reason: ShareEndReason },
    View { session: CallId, width: u32, height: u32, fps: u32, ext: ShareWatchExt },
    CallJoin { call_id: CallId, town: [u8; 32], muted: bool, transcribing: bool },
    /// A card call carries the caller and callee seats, in that order.
    CallJoinAck { call_id: CallId, muted: bool, transcribing: bool, seats: Option<[[u8; 32]; 2]> },
    MediaCapsNotice { caps: MediaCaps },
    RecordingState { call_id: CallId, recording: bool },
    RecordingConsent { call_id: CallId, allow: bool },
}

impl Message {
    pub fn call_id(&self) -> Option<CallId> {
        match *self {
            Self::Ring { call_id, .. }
            | Self::Answer { call_id, .. }
            | Self::Decline { call_id, .. }
            | Self::Cancel { call_id }
            | Self::End { call_id }
            | Self::Mute { call_id, .. }
            | Self::Transcribe { call_id, .. }
            | Self::Share { session: call_id, .. } | Self::ShareOff { session: call_id, .. }
            | Self::View { session: call_id, .. }
            | Self::CallJoin { call_id, .. } | Self::CallJoinAck { call_id, .. }
            | Self::RecordingState { call_id, .. } | Self::RecordingConsent { call_id, .. } => Some(call_id),
            Self::MediaCapsNotice { .. } => None,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let tag = match self {
            Self::Ring { .. } => RING,
            Self::Answer { .. } => ANSWER,
            Self::Decline { .. } => DECLINE,
            Self::Cancel { .. } => CANCEL,
            Self::End { .. } => END,
            Self::Mute { .. } => MUTE,
            Self::Transcribe { .. } => TRANSCRIBE,
            Self::Share { .. } | Self::ShareOff { .. } | Self::View { .. } => return self.encode_share(),
            Self::CallJoin { .. } => CALL_JOIN,
            Self::CallJoinAck { .. } => CALL_JOIN_ACK,
            Self::MediaCapsNotice { .. } => MEDIA_CAPS_NOTICE,
            Self::RecordingState { .. } => RECORDING_STATE,
            Self::RecordingConsent { .. } => RECORDING_CONSENT,
        };
        let mut body = Vec::with_capacity(51);
        body.extend([VERSION, tag]);
        if let Some(id) = self.call_id() {
            if matches!(tag, CALL_JOIN | CALL_JOIN_ACK | RECORDING_STATE | RECORDING_CONSENT) {
                write_tlv(&mut body, 1, &id);
            } else {
                body.extend(id);
            }
        }
        match self {
            Self::Ring { secret, video, ext, .. } => {
                body.extend(secret);
                body.push(*video as u8);
                if let Some(town) = &ext.town { write_tlv(&mut body, 1, town); }
                if !ext.roster.is_empty() { write_tlv(&mut body, 2, ext.roster.as_flattened()); }
                if let Some(caps) = ext.media_caps { write_tlv(&mut body, 3, &caps.0.to_le_bytes()); }
                if let Some(seat) = &ext.target_seat { write_tlv(&mut body, 4, seat); }
            }
            Self::Answer { secret, ext, .. } => {
                body.extend(secret);
                if let Some(town) = &ext.town { write_tlv(&mut body, 1, town); }
                if let Some(caps) = ext.media_caps { write_tlv(&mut body, 2, &caps.0.to_le_bytes()); }
            }
            Self::Decline { why, .. } => body.push((*why == Why::Busy) as u8),
            Self::Mute { on, .. } | Self::Transcribe { on, .. } => body.push(*on as u8),
            Self::CallJoin { town, muted, transcribing, .. } => {
                write_tlv(&mut body, 2, town);
                write_tlv(&mut body, 3, &[*muted as u8]);
                write_tlv(&mut body, 4, &[*transcribing as u8]);
            }
            Self::CallJoinAck { muted, transcribing, seats, .. } => {
                write_tlv(&mut body, 2, &[*muted as u8]);
                write_tlv(&mut body, 3, &[*transcribing as u8]);
                if let Some(pair) = seats { write_tlv(&mut body, 4, pair.as_flattened()); }
            }
            Self::MediaCapsNotice { caps } => write_tlv(&mut body, 1, &caps.0.to_le_bytes()),
            Self::RecordingState { recording, .. } => write_tlv(&mut body, 2, &[*recording as u8]),
            Self::RecordingConsent { allow, .. } => write_tlv(&mut body, 2, &[*allow as u8]),
            Self::Cancel { .. } | Self::End { .. } => {}
            Self::Share { .. } | Self::ShareOff { .. } | Self::View { .. } => unreachable!(),
        }
        body
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refused {
    Version,
    /// A different known envelope, or an invalid value in a known message.
    Tag,
    Length,
}

enum Field { Bytes(usize), Roster, Bool }

/// Known fields have consecutive tags starting at 1. Borrow them until the whole
/// tail is framed, including unknown fields, before interpreting their values.
fn fields<const N: usize>(mut tail: &[u8], layout: [Field; N]) -> Result<[Option<&[u8]>; N], Refused> {
    let mut values = [None; N];
    let mut invalid_bool = false;
    while let Some((tag, value)) = read_tlv(&mut tail).map_err(|_| Refused::Length)? {
        let i = usize::from(tag.wrapping_sub(1));
        let Some(field) = layout.get(i) else { continue };
        match field {
            Field::Bytes(size) if value.len() != *size => return Err(Refused::Length),
            Field::Roster if value.len() % 65 != 0 => return Err(Refused::Length),
            Field::Bool => {
                let [byte] = value else { return Err(Refused::Length) };
                invalid_bool |= *byte > 1;
            }
            _ => {}
        }
        values[i] = Some(value);
    }
    if invalid_bool { return Err(Refused::Tag) }
    Ok(values)
}

fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    bytes.try_into().expect("checked field width")
}

fn caps(bytes: &[u8]) -> MediaCaps { MediaCaps(u32::from_le_bytes(array(bytes))) }

fn skip_tail(mut tail: &[u8]) -> Result<(), Refused> {
    skip_tlv_tail(&mut tail).map_err(|_| Refused::Length)
}

fn flag(byte: u8) -> Result<bool, Refused> {
    match byte { 0 => Ok(false), 1 => Ok(true), _ => Err(Refused::Tag) }
}

fn decode_fields(tag: u8, tail: &[u8]) -> Result<Message, Refused> {
    use Field::{Bool, Bytes};
    Ok(match tag {
        CALL_JOIN => {
            let [Some(id), Some(town), muted, transcribing] = fields(tail, [Bytes(16), Bytes(32), Bool, Bool])?
                else { return Err(Refused::Length) };
            Message::CallJoin { call_id: array(id), town: array(town), muted: matches!(muted, Some([1])),
                transcribing: matches!(transcribing, Some([1])) }
        }
        CALL_JOIN_ACK => {
            let [Some(id), muted, transcribing, seats] = fields(tail, [Bytes(16), Bool, Bool, Bytes(64)])?
                else { return Err(Refused::Length) };
            Message::CallJoinAck { call_id: array(id), muted: matches!(muted, Some([1])),
                transcribing: matches!(transcribing, Some([1])), seats: seats.map(|pair| [array(&pair[..32]), array(&pair[32..])]) }
        }
        MEDIA_CAPS_NOTICE => {
            let [Some(value)] = fields(tail, [Bytes(4)])? else { return Err(Refused::Length) };
            Message::MediaCapsNotice { caps: caps(value) }
        }
        RECORDING_STATE | RECORDING_CONSENT => {
            let [Some(id), Some(value)] = fields(tail, [Bytes(16), Bool])? else { return Err(Refused::Length) };
            let call_id = array(id);
            if tag == RECORDING_STATE { Message::RecordingState { call_id, recording: value == [1] } }
            else { Message::RecordingConsent { call_id, allow: value == [1] } }
        }
        _ => unreachable!(),
    })
}

pub fn decode(body: &[u8]) -> Result<Option<Message>, Refused> {
    let (&[version, tag], payload) = body.split_first_chunk().ok_or(Refused::Length)?;
    if version != VERSION { return Err(Refused::Version) }
    let size = match tag {
        SHARE | VIEW => return Message::decode_share(tag, body),
        CALL_JOIN..=RECORDING_CONSENT => return decode_fields(tag, payload).map(Some),
        PEER | PEER_MORE | MEDIA_KEY | WANT | MEDIA | SHARE_PROBLEM | SHARE_PART => return Err(Refused::Tag),
        RING => 51,
        ANSWER => 50,
        DECLINE | MUTE | TRANSCRIBE => 19,
        CANCEL | END => 18,
        _ => return Ok(None),
    };
    let (prefix, tail) = body.split_at_checked(size).ok_or(Refused::Length)?;
    let call_id = array(&prefix[2..18]);
    Ok(Some(match tag {
        RING => {
            let [town, roster, media_caps, target_seat] = fields(tail, [Field::Bytes(32), Field::Roster, Field::Bytes(4), Field::Bytes(32)])?;
            let ext = RingExt { town: town.map(array), roster: roster.unwrap_or_default().as_chunks::<65>().0.to_vec(),
                media_caps: media_caps.map(caps), target_seat: target_seat.map(array) };
            Message::Ring { call_id, secret: array(&prefix[18..50]), video: flag(prefix[50])?, ext }
        }
        ANSWER => {
            let [town, media_caps] = fields(tail, [Field::Bytes(32), Field::Bytes(4)])?;
            let ext = AnswerExt { town: town.map(array), media_caps: media_caps.map(caps) };
            Message::Answer { call_id, secret: array(&prefix[18..50]), ext }
        }
        _ => {
            skip_tail(tail)?;
            match tag {
                DECLINE => Message::Decline { call_id, why: if flag(prefix[18])? { Why::Busy } else { Why::Declined } },
                CANCEL => Message::Cancel { call_id },
                END => Message::End { call_id },
                MUTE => Message::Mute { call_id, on: flag(prefix[18])? },
                TRANSCRIBE => Message::Transcribe { call_id, on: flag(prefix[18])? },
                _ => unreachable!(),
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_layout_and_round_trips() {
        let call_id = [0x45; 16];
        let secret = [0x87; 32];
        for (message, tag, tail, flag) in [
            (Message::Ring { call_id, secret, video: false, ext: Default::default() }, 1, true, Some(0)),
            (Message::Ring { call_id, secret, video: true, ext: Default::default() }, 1, true, Some(1)),
            (Message::Answer { call_id, secret, ext: Default::default() }, 2, true, None),
            (Message::Decline { call_id, why: Why::Declined }, 3, false, Some(0)),
            (Message::Decline { call_id, why: Why::Busy }, 3, false, Some(1)),
            (Message::Cancel { call_id }, 4, false, None),
            (Message::End { call_id }, 5, false, None),
            (Message::Mute { call_id, on: true }, 6, false, Some(1)),
            (Message::Mute { call_id, on: false }, 6, false, Some(0)),
            (Message::Transcribe { call_id, on: true }, 0x54, false, Some(1)),
            (Message::Transcribe { call_id, on: false }, 0x54, false, Some(0)),
        ] {
            let mut bytes = vec![1, tag];
            bytes.extend(call_id);
            if tail {
                bytes.extend(secret);
            }
            bytes.extend(flag);
            assert_eq!(message.encode(), bytes);
            assert_eq!(decode(&bytes), Ok(Some(message)));
            assert_eq!(decode(&bytes[..bytes.len() - 1]), Err(Refused::Length));
            bytes.push(0);
            assert_eq!(decode(&bytes), Err(Refused::Length));
        }
        for (signal, wire) in [
            (MediaSignal::Want { on: false, last: 0, running: false }, vec![1, 13, 0, 0, 0, 0, 0, 0]),
            (MediaSignal::Want { on: false, last: 1, running: true }, vec![1, 13, 0, 0, 0, 0, 1, 1]),
            (MediaSignal::Want { on: true, last: 0x12345678, running: false }, vec![1, 13, 1, 0x12, 0x34, 0x56, 0x78, 0]),
            (MediaSignal::Media { n: 1, on: false }, vec![1, 14, 0, 0, 0, 1, 0]),
            (MediaSignal::Media { n: 0x89abcdef, on: true }, vec![1, 14, 0x89, 0xab, 0xcd, 0xef, 1]),
        ] {
            assert_eq!(Vec::try_from(signal), Ok(wire.clone()));
            assert!(MediaSignal::has_tag(&wire));
            assert_eq!(MediaSignal::decode(&wire), Some(signal));
            assert_eq!(decode(&wire), Err(Refused::Tag), "a link media signal has no call ID");
            assert!(MediaSignal::decode(&wire[..wire.len() - 1]).is_none());
            let mut long = wire;
            long.push(0);
            assert!(MediaSignal::decode(&long).is_none());
            long.pop();
            write_tlv(&mut long, 0xf0, b"later");
            assert_eq!(MediaSignal::decode(&long), Some(signal));
        }
    }

    #[test]
    fn signal_ring_and_answer_accept_empty_and_unknown_tlv_tails() {
        let call_id = [4; 16];
        let secret = [5; 32];
        let town = Some([6; 32]);
        let media_caps = Some(MediaCaps(MediaCaps::HEVC_DECODE));
        for (message, size) in [
            (Message::Ring { call_id, secret, video: true, ext: RingExt::default() }, 51),
            (Message::Answer { call_id, secret, ext: AnswerExt::default() }, 50),
            (Message::Ring { call_id, secret, video: false,
                ext: RingExt { town, roster: vec![[7; 65], [8; 65]], media_caps, target_seat: None } }, 51 + 35 + 133 + 7),
            (Message::Answer { call_id, secret, ext: AnswerExt { town, media_caps } }, 50 + 35 + 7),
        ] {
            let mut wire = message.encode();
            assert_eq!(wire.len(), size);
            assert_eq!(decode(&wire).unwrap().as_ref(), Some(&message));
            write_tlv(&mut wire, 0xfe, b"future");
            assert_eq!(decode(&wire), Ok(Some(message)));
            wire.extend([0xfe, 5, 0, 1]);
            assert_eq!(decode(&wire), Err(Refused::Length));
        }
        let mut ring = Message::Ring { call_id, secret, video: false, ext: RingExt::default() }.encode();
        write_tlv(&mut ring, 2, &[0; 64]);
        assert_eq!(decode(&ring), Err(Refused::Length));
    }

    #[test]
    fn ring_target_seat_and_join_ack_seats_roundtrip_and_older_decoders_skip_tag_4() {
        use Field::{Bool, Bytes, Roster};
        let call_id = [10; 16];
        let secret = [11; 32];
        let town = [12; 32];
        let peer = [7; 65];
        let caps = MediaCaps(MediaCaps::MULTIPARTY_V1);
        let cap_bytes = caps.0.to_le_bytes();
        let ring = |target_seat| Message::Ring { call_id, secret, video: true, ext: RingExt {
            town: Some(town), roster: vec![peer], media_caps: Some(caps), target_seat,
        } };
        let ack = |seats| Message::CallJoinAck { call_id, muted: true, transcribing: false, seats };

        let mut town_ring = vec![VERSION, RING];
        town_ring.extend(call_id);
        town_ring.extend(secret);
        town_ring.push(1);
        write_tlv(&mut town_ring, 1, &town);
        write_tlv(&mut town_ring, 2, &peer);
        write_tlv(&mut town_ring, 3, &cap_bytes);
        let mut town_ack = vec![VERSION, CALL_JOIN_ACK];
        write_tlv(&mut town_ack, 1, &call_id);
        write_tlv(&mut town_ack, 2, &[1]);
        write_tlv(&mut town_ack, 3, &[0]);
        assert_eq!(ring(None).encode(), town_ring, "town rings retain the original wire layout");
        assert_eq!(ack(None).encode(), town_ack, "town acknowledgements retain the original wire layout");

        let target = [9; 32];
        let seats = [[13; 32], [14; 32]];
        let card_ring = ring(Some(target)).encode();
        let card_ack = ack(Some(seats)).encode();
        let mut expected_ring = town_ring.clone();
        write_tlv(&mut expected_ring, 4, &target);
        let mut expected_ack = town_ack.clone();
        write_tlv(&mut expected_ack, 4, seats.as_flattened());
        assert_eq!(card_ring, expected_ring, "the sole addition to a card ring is its 35-byte callee field");
        assert_eq!(card_ack, expected_ack, "the sole addition to a card ack is its 67-byte caller/callee field");
        assert_eq!(decode(&card_ring), Ok(Some(ring(Some(target)))), "the recipient recovers the target");
        assert_eq!(decode(&card_ack), Ok(Some(ack(Some(seats)))), "the joiner recovers both seats in order");
        assert_eq!(fields(&card_ring[51..], [Bytes(32), Roster, Bytes(4)]),
            Ok([Some(town.as_slice()), Some(peer.as_slice()), Some(cap_bytes.as_slice())]),
            "an older ring reader skips the seat and retains the town, roster and capabilities");
        assert_eq!(fields(&card_ack[2..], [Bytes(16), Bool, Bool]),
            Ok([Some(call_id.as_slice()), Some(&[1][..]), Some(&[0][..])]),
            "an older ack reader skips the seats and retains the call ID and both flags");
        for (prefix, sizes) in [(&town_ring, &[31, 33][..]), (&town_ack, &[32, 63, 65][..])] {
            for &size in sizes {
                let mut malformed = prefix.clone();
                write_tlv(&mut malformed, 4, &vec![0; size]);
                assert_eq!(decode(&malformed), Err(Refused::Length), "scope field of width {size} must be refused");
            }
        }
    }

    #[test]
    fn signal_unknown_v1_tag_returns_ok_none() {
        for wire in [&[1, 7][..], &[1, 8], &[1, 99, 1, 2, 0, 0xaa, 0xbb], &[1, 99, 0xff]] {
            assert_eq!(decode(wire), Ok(None));
        }
        assert_eq!(decode(&[2, 1]), Err(Refused::Version));
        for tag in [PEER, PEER_MORE, MEDIA_KEY, WANT, MEDIA, SHARE_PROBLEM, SHARE_PART] {
            assert_eq!(decode(&[1, tag]), Err(Refused::Tag));
        }
    }

    #[test]
    fn signal_pure_tlv_tags_roundtrip_and_skip_unknown_fields() {
        let call_id = [10; 16];
        let town = [12; 32];
        let caps = MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::H264_444_DECODE);
        for (tag, message) in [
            (16, Message::CallJoin { call_id, town, muted: true, transcribing: false }),
            (17, Message::CallJoinAck { call_id, muted: false, transcribing: true, seats: None }),
            (18, Message::MediaCapsNotice { caps }),
            (19, Message::RecordingState { call_id, recording: false }),
            (20, Message::RecordingConsent { call_id, allow: true }),
            (10, Message::Share { session: call_id, title: "長".repeat(22_000), w: 160., h: 90.,
                ext: ShareOfferExt { town: Some(town), codecs: Some(3), full_chroma: true } }),
            (11, Message::View { session: call_id, width: 160, height: 90, fps: 30,
                ext: ShareWatchExt { media_caps: Some(caps) } }),
        ] {
            let mut wire = message.encode();
            assert_eq!(&wire[..2], &[1, tag]);
            if tag == SHARE {
                assert_eq!(wire[18], 2);
                assert_eq!(&wire[27..31], &66_000u32.to_le_bytes());
            }
            assert_eq!(message.call_id(), (tag != MEDIA_CAPS_NOTICE).then_some(call_id));
            write_tlv(&mut wire, 0xfe, b"future");
            assert_eq!(decode(&wire), Ok(Some(message)));
            wire.push(0xfe);
            assert_eq!(decode(&wire), Err(Refused::Length));
        }

        for tag in 16..=20 {
            assert_eq!(decode(&[1, tag]), Err(Refused::Length));
        }
        let mut join = vec![1, 16];
        write_tlv(&mut join, 2, &town);
        write_tlv(&mut join, 1, &call_id);
        assert_eq!(decode(&join), Ok(Some(Message::CallJoin { call_id, town, muted: false, transcribing: false })));
        write_tlv(&mut join, 4, &[2]);
        assert_eq!(decode(&join), Err(Refused::Tag));
        join.push(1);
        assert_eq!(decode(&join), Err(Refused::Length));
        let mut future_offer = vec![1, SHARE];
        future_offer.extend(call_id);
        future_offer.extend([3, 0xfe]);
        assert_eq!(decode(&future_offer), Ok(None));
    }

    #[test]
    fn refuses_invalid_headers_and_discriminants() {
        for (body, why) in [
            (&[][..], Refused::Length),
            (&[0], Refused::Length),
            (&[1], Refused::Length),
        ] {
            assert_eq!(decode(body), Err(why));
        }
        for (tag, size) in [(1, 51), (3, 19), (6, 19), (0x54, 19)] {
            let mut body = vec![0; size];
            body[..2].copy_from_slice(&[1, tag]);
            body[size - 1] = 2;
            assert_eq!(decode(&body), Err(Refused::Tag));
        }
        for wire in [
            &[][..], &[1], &[2, 13, 0, 0, 0, 0, 0, 0], &[2, 14, 0, 0, 0, 1, 0],
            &[1, 12, 0, 0, 0, 0, 1, 0], &[1, 15, 0, 0, 0, 1, 0],
            &[1, 13, 2, 0, 0, 0, 0, 0], &[1, 13, 0, 0, 0, 0, 0, 2],
            &[1, 14, 0, 0, 0, 1, 2], &[1, 14, 0, 0, 0, 0, 0], &[1, 14, 0, 0, 0, 0, 1],
            &[1, 13, 1, 0, 0, 0, 0, 1],
        ] {
            assert!(MediaSignal::decode(wire).is_none(), "accepted invalid media signal {wire:?}");
        }
        assert_eq!(Vec::try_from(MediaSignal::Media { n: 0, on: true }), Err(Refused::Tag));
        assert_eq!(Vec::try_from(MediaSignal::Want { on: true, last: 0, running: true }), Err(Refused::Tag),
            "a Want can't hold a session before the first");
    }
}

const SHARE: u8 = 10;
const VIEW: u8 = 11;
pub(super) const MEDIA_KEY: u8 = 12;
const WANT: u8 = 13;
const MEDIA: u8 = 14;
const SHARE_PROBLEM: u8 = 15;
const SHARE_PART: u8 = SHARE | 0x80;

/// One display line of diagnostic text. Leave room for the ellipsis inside the byte limit.
const MAX_PROBLEM_BYTES: usize = 240;

#[derive(Debug, Eq, PartialEq)]
pub(super) struct ShareProblem {
    pub session: CallId,
    why: String,
}

impl ShareProblem {
    pub fn new(session: CallId, why: &str) -> Self {
        let mut line = String::new();
        for c in why.trim().chars() {
            if c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                continue;
            }
            if line.len() + c.len_utf8() > MAX_PROBLEM_BYTES {
                line.truncate(line.floor_char_boundary(MAX_PROBLEM_BYTES - '…'.len_utf8()));
                line.push('…');
                break;
            }
            line.push(c);
        }
        if line.trim().is_empty() { line = "No details available.".into(); }
        Self { session, why: line }
    }

    pub fn why(&self) -> &str { &self.why }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(20 + self.why.len());
        bytes.extend([VERSION, SHARE_PROBLEM]);
        bytes.extend(self.session);
        bytes.extend((self.why.len() as u16).to_le_bytes());
        bytes.extend(self.why.as_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Refused> {
        let [version, tag, body @ ..] = bytes else { return Err(Refused::Length) };
        if *version != VERSION { return Err(Refused::Version) }
        if *tag != SHARE_PROBLEM { return Err(Refused::Tag) }
        let session = body.get(..16).ok_or(Refused::Length)?.try_into().unwrap();
        let [lo, hi, text @ ..] = &body[16..] else { return Err(Refused::Length) };
        let len = usize::from(u16::from_le_bytes([*lo, *hi]));
        if len == 0 { return Err(Refused::Length) }
        let (text, tail) = text.split_at_checked(len).ok_or(Refused::Length)?;
        skip_tail(tail)?;
        let why = std::str::from_utf8(text).map_err(|_| Refused::Tag)?;
        Ok(Self::new(session, why))
    }
}

/// Link-level media signalling, outside the call-ID protocol.
#[cfg_attr(test, derive(Hash))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MediaSignal {
    Want { on: bool, last: u32, running: bool },
    Media { n: u32, on: bool },
}

impl MediaSignal {
    pub(super) fn has_tag(body: &[u8]) -> bool {
        matches!(body, [_, WANT | MEDIA, ..])
    }

    pub(super) fn decode(body: &[u8]) -> Option<Self> {
        match *body {
            [VERSION, WANT, on @ (0 | 1), a, b, c, d, running @ (0 | 1), ref tail @ ..] => {
                skip_tail(tail).ok()?;
                let last = u32::from_be_bytes([a, b, c, d]);
                // Nothing runs before the first session.
                (running == 0 || last != 0).then_some(Self::Want { on: on == 1, last, running: running == 1 })
            }
            [VERSION, MEDIA, a, b, c, d, on @ (0 | 1), ref tail @ ..] => {
                skip_tail(tail).ok()?;
                let n = u32::from_be_bytes([a, b, c, d]);
                (n != 0).then_some(Self::Media { n, on: on == 1 })
            }
            _ => None,
        }
    }
}

impl TryFrom<MediaSignal> for Vec<u8> {
    type Error = Refused;

    fn try_from(signal: MediaSignal) -> Result<Self, Refused> {
        match signal {
            MediaSignal::Want { running: true, last: 0, .. } => Err(Refused::Tag),
            MediaSignal::Want { on, last, running } => {
                let [a, b, c, d] = last.to_be_bytes();
                Ok(vec![VERSION, WANT, on as u8, a, b, c, d, running as u8])
            }
            MediaSignal::Media { n: 0, .. } => Err(Refused::Tag),
            MediaSignal::Media { n, on } => {
                let [a, b, c, d] = n.to_be_bytes();
                Ok(vec![VERSION, MEDIA, a, b, c, d, on as u8])
            }
        }
    }
}

impl Message {
    /// Yield bounded carrier messages. A long UTF-8 title is decoded after the final piece.
    pub(super) fn send(self, mut send: impl FnMut(Vec<u8>)) {
        let bytes = self.encode();
        let max = makepad_town_core::connect::MAX_CALL;
        if bytes.len() <= max { send(bytes); return }
        let mut chunks = bytes.chunks(max - 3).peekable();
        while let Some(chunk) = chunks.next() {
            let mut frame = Vec::with_capacity(3 + chunk.len());
            frame.extend([VERSION, SHARE_PART, chunks.peek().is_none() as u8]);
            frame.extend(chunk);
            send(frame);
        }
    }

    pub(super) fn part(bytes: &[u8]) -> Option<(bool, &[u8])> {
        match bytes {
            [VERSION, SHARE_PART, last @ (0 | 1), payload @ ..] => Some((*last == 1, payload)),
            _ => None,
        }
    }

    fn encode_share(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        match self {
            Self::Share { session, title, w, h, ext } => {
                bytes.reserve_exact(27 + title.len());
                bytes.extend([VERSION, SHARE]);
                bytes.extend(session);
                let extended = *ext != ShareOfferExt::default();
                bytes.push(if extended { 2 } else { 1 });
                bytes.extend(w.to_le_bytes());
                bytes.extend(h.to_le_bytes());
                if extended {
                    bytes.extend(u32::try_from(title.len()).expect("title must fit its wire length").to_le_bytes());
                }
                bytes.extend(title.as_bytes());
                if let Some(town) = &ext.town { write_tlv(&mut bytes, 1, town); }
                if let Some(codecs) = ext.codecs { write_tlv(&mut bytes, 2, &codecs.to_le_bytes()); }
                if ext.full_chroma { write_tlv(&mut bytes, 3, &[1]); }
            }
            Self::ShareOff { session, reason } => {
                bytes.reserve_exact(20);
                bytes.extend([VERSION, SHARE]);
                bytes.extend(session);
                bytes.extend([0, *reason as u8]);
            }
            Self::View { session, width, height, fps, ext } => {
                bytes.reserve_exact(30);
                bytes.extend([VERSION, VIEW]);
                bytes.extend(session);
                for field in [width, height, fps] { bytes.extend(field.to_le_bytes()); }
                if let Some(caps) = ext.media_caps { write_tlv(&mut bytes, 1, &caps.0.to_le_bytes()); }
            }
            _ => unreachable!("only share signals use this layout"),
        }
        bytes
    }

    fn decode_share(tag: u8, bytes: &[u8]) -> Result<Option<Self>, Refused> {
        let session = bytes.get(2..18).ok_or(Refused::Length)?.try_into().unwrap();
        match (tag, &bytes[18..]) {
            (SHARE, [0, reason, tail @ ..]) => {
                skip_tail(tail)?;
                let reason = match reason {
                    0 => ShareEndReason::Stopped,
                    1 => ShareEndReason::AppClosedOrRestarted,
                    2 => ShareEndReason::LinkDropped,
                    _ => return Err(Refused::Tag),
                };
                Ok(Some(Self::ShareOff { session, reason }))
            }
            (SHARE, [kind @ (1 | 2), rest @ ..]) if rest.len() >= 8 => {
                let w = f32::from_le_bytes(rest[..4].try_into().unwrap());
                let h = f32::from_le_bytes(rest[4..8].try_into().unwrap());
                let mut title = &rest[8..];
                let mut ext = ShareOfferExt::default();
                if *kind == 2 {
                    let (&len, rest) = title.split_first_chunk().ok_or(Refused::Length)?;
                    let (text, tail) = rest.split_at_checked(u32::from_le_bytes(len) as usize).ok_or(Refused::Length)?;
                    let [town, codecs, full_chroma] = fields(tail, [Field::Bytes(32), Field::Bytes(4), Field::Bool])?;
                    ext = ShareOfferExt { town: town.map(array), codecs: codecs.map(|b| u32::from_le_bytes(array(b))),
                        full_chroma: matches!(full_chroma, Some([1])) };
                    title = text;
                }
                if !w.is_finite() || !h.is_finite() || w <= 0. || h <= 0. {
                    return Err(Refused::Tag)
                }
                let title = std::str::from_utf8(title).map_err(|_| Refused::Tag)?.to_owned();
                Ok(Some(Self::Share { session, title, w, h, ext }))
            }
            (SHARE, [3..=255, ..]) => Ok(None),
            (VIEW, payload) if payload.len() >= 12 => {
                let [media_caps] = fields(&payload[12..], [Field::Bytes(4)])?;
                let width = u32::from_le_bytes(payload[..4].try_into().unwrap());
                let height = u32::from_le_bytes(payload[4..8].try_into().unwrap());
                let fps = u32::from_le_bytes(payload[8..12].try_into().unwrap());
                if (width == 0 || height == 0 || fps == 0) && (width, height, fps) != (0, 0, 0) {
                    return Err(Refused::Tag)
                }
                Ok(Some(Self::View { session, width, height, fps, ext: ShareWatchExt { media_caps: media_caps.map(caps) } }))
            }
            _ => Err(Refused::Length),
        }
    }

    pub(super) fn is_share_message(bytes: &[u8]) -> bool {
        matches!(bytes, [VERSION, SHARE | VIEW | SHARE_PROBLEM, ..])
    }
}

#[cfg(test)]
mod share_wire_tests {
    use super::*;

    #[test]
    fn picture_problem_checks_its_session_length_and_utf8() {
        let session = [32; 16];
        let problem = ShareProblem::new(session, "encoder stopped · 窗");
        let bytes = problem.encode();
        assert_eq!(&bytes[..2], &[VERSION, 15], "distinct share problem tag");
        assert_eq!(&bytes[2..18], &session, "the reason belongs to this offer");
        assert_eq!(&bytes[18..20], &(problem.why.len() as u16).to_le_bytes(), "length counts UTF-8 bytes");
        assert!(Message::is_share_message(&bytes), "route the reason to the share before call dispatch");
        assert_eq!(ShareProblem::decode(&bytes), Ok(problem), "the reason round trips");
        for end in 0..bytes.len() {
            assert_eq!(ShareProblem::decode(&bytes[..end]), Err(Refused::Length), "truncation at byte {end}");
        }
        let mut invalid = bytes.clone();
        invalid.push(0);
        assert_eq!(ShareProblem::decode(&invalid), Err(Refused::Length), "trailing data");
        invalid.pop();
        write_tlv(&mut invalid, 0xff, b"extension");
        assert_eq!(ShareProblem::decode(&invalid).unwrap().why(), "encoder stopped · 窗");
        invalid.truncate(20);
        invalid[18..20].fill(0);
        assert_eq!(ShareProblem::decode(&invalid), Err(Refused::Length), "missing reason");
        invalid = bytes.clone();
        invalid[0] += 1;
        assert_eq!(ShareProblem::decode(&invalid), Err(Refused::Version), "unknown version");
        invalid = bytes.clone();
        invalid[1] = VIEW;
        assert_eq!(ShareProblem::decode(&invalid), Err(Refused::Tag), "wrong signal tag");

        // A peer can send well-formed text that did not pass through our encoder.
        let hostile = format!("failed:\n\r\t\0\u{1b}\u{85}\u{2028}\u{2029}\u{202e}\u{2067} {}", "窗".repeat(1000));
        let mut raw = bytes[..18].to_vec();
        raw.extend((hostile.len() as u16).to_le_bytes());
        raw.extend(hostile.bytes());
        let why = ShareProblem::decode(&raw).unwrap().why;
        assert_eq!(why, format!("failed: {}…", "窗".repeat((MAX_PROBLEM_BYTES - 8 - 3) / 3)),
            "even a peer-supplied reason fits one line and ends on a character boundary");
        *raw.last_mut().unwrap() = 0xff;
        assert_eq!(ShareProblem::decode(&raw), Err(Refused::Tag), "invalid UTF-8 beyond the displayed prefix");
    }
    #[test]
    fn share_and_view_preserve_window_points_and_subscription() {
        let session = [19; 16];
        for signal in [
            Message::Share { session, title: "a · 窗".into(), w: 819.5, h: 480.25, ext: Default::default() },
            Message::View { session, width: 713, height: 400, fps: 30, ext: Default::default() },
            Message::View { session, width: 0, height: 0, fps: 0, ext: Default::default() },
        ] {
            let bytes = signal.encode();
            assert!(Message::is_share_message(&bytes));
            assert_eq!(decode(&bytes), Ok(Some(signal)));
            assert_eq!(decode(&bytes[..17]), Err(Refused::Length));
        }
        let mut bytes = Message::View { session, width: 0, height: 42, fps: 30, ext: Default::default() }.encode();
        assert_eq!(decode(&bytes), Err(Refused::Tag));
        bytes.push(0);
        assert_eq!(decode(&bytes), Err(Refused::Length));
        let mut offer = Message::Share { session, title: "x".into(), w: 32., h: 24., ext: Default::default() }.encode();
        offer[27] = 255;
        assert_eq!(decode(&offer), Err(Refused::Tag));
        offer.truncate(27);
        offer[19..23].copy_from_slice(&f32::NAN.to_le_bytes());
        assert_eq!(decode(&offer), Err(Refused::Tag));
        let title = "窗".repeat(makepad_town_core::connect::MAX_CALL / 2);
        let mut joined = Vec::new();
        let mut ends = 0;
        Message::Share { session, title: title.clone(), w: 1., h: 1., ext: Default::default() }.send(|frame| {
            assert!(frame.len() <= makepad_town_core::connect::MAX_CALL);
            let (last, part) = Message::part(&frame).unwrap();
            assert_eq!(ends, 0);
            joined.extend_from_slice(part);
            ends += usize::from(last);
        });
        assert_eq!(ends, 1);
        assert!(matches!(decode(&joined), Ok(Some(Message::Share { title: text, .. })) if text == title));
    }

    #[test]
    fn share_off_has_one_reason_byte_and_no_title_or_size() {
        let session = [42; 16];
        let mut body = vec![1, 10];
        body.extend(session);
        body.push(0);
        assert_eq!(decode(&body), Err(Refused::Length));
        body.push(0);
        for (byte, reason) in [(0, ShareEndReason::Stopped), (1, ShareEndReason::AppClosedOrRestarted),
            (2, ShareEndReason::LinkDropped)] {
            body[19] = byte;
            let message = Message::ShareOff { session, reason };
            assert_eq!(message.encode(), body);
            assert_eq!(decode(&body), Ok(Some(message)));
        }
        body[19] = 3;
        assert_eq!(decode(&body), Err(Refused::Tag));
        body[19] = 4;
        assert_eq!(decode(&body), Err(Refused::Tag));
        body[19] = 0;
        body.push(0);
        assert_eq!(decode(&body), Err(Refused::Length));
        body.resize(27, 0); // Previous wire layout, even with an empty title.
        assert_eq!(decode(&body), Err(Refused::Length));
        body.extend(b"title");
        assert_eq!(decode(&body), Err(Refused::Length));
    }
}
