//! Whole messages on the link's Sync lane. Offers are ephemeral, and a JOIN carries
//! only the signed genesis. Room history follows through the ordinary heads exchange.

use crate::event::{kind, Content, Event, Id, RoomId};
use crate::log::{encode_record, record};
use crate::sync::{SyncError, EVENTS, HEADS, VERSION};

pub(crate) const HEADER_BYTES: usize = 2 + size_of::<RoomId>();

const OFFER: u8 = 3;
const ANSWER: u8 = 4;
const CANCEL: u8 = 5;
const JOIN: u8 = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub projects: u32,
    pub tasks: u32,
    pub docs: u32,
    pub messages: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    This { counts: Counts, since: u64 },
    New,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Message<'a> {
    Heads { room: RoomId, heads: &'a [Id] },
    Events { room: RoomId, records: &'a [u8] },
    Offer { room: RoomId, offer: Offer },
    Answer { room: RoomId, yes: bool },
    Cancel { room: RoomId },
    Join { room: RoomId, genesis: Event },
}

impl<'a> Message<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self, SyncError> {
        let (&[version, ty], body) = bytes.split_first_chunk::<2>()
            .ok_or(SyncError::Malformed("incomplete message header"))?;
        if version != VERSION { return Err(SyncError::Version(version)) }
        let (&room, body) = body.split_first_chunk::<32>()
            .ok_or(SyncError::Malformed("incomplete room id"))?;
        match ty {
            HEADS => {
                let (heads, tail) = body.as_chunks::<32>();
                if !tail.is_empty() { return Err(SyncError::Malformed("incomplete head id")) }
                Ok(Self::Heads { room, heads })
            }
            EVENTS => {
                let mut rest = body;
                while !rest.is_empty() {
                    let (_, n) = record(rest).ok_or(SyncError::Malformed("incomplete record"))?;
                    rest = &rest[n..];
                }
                Ok(Self::Events { room, records: body })
            }
            OFFER => {
                let (&what, fields) = body.split_first().ok_or(SyncError::Malformed("missing offer kind"))?;
                let (&counts, tail) = fields.split_first_chunk::<16>().ok_or(SyncError::Malformed("incomplete offer counts"))?;
                let since = u64::from_le_bytes(tail.try_into().map_err(|_| SyncError::Malformed("invalid offer date length"))?);
                let words: [[u8; 4]; 4] = counts.as_chunks::<4>().0.try_into()
                    .map_err(|_| SyncError::Malformed("invalid offer counts"))?;
                let [projects, tasks, docs, messages] = words.map(u32::from_le_bytes);
                let counts = Counts { projects, tasks, docs, messages };
                match what {
                    1 => Ok(Self::Offer { room, offer: Offer::This { counts, since } }),
                    2 if counts == Counts::default() && since == 0 => Ok(Self::Offer { room, offer: Offer::New }),
                    2 => Err(SyncError::Malformed("new space offer has history")),
                    _ => Err(SyncError::Malformed("unknown offer kind")),
                }
            }
            ANSWER => match body {
                [0] => Ok(Self::Answer { room, yes: false }),
                [1] => Ok(Self::Answer { room, yes: true }),
                _ => Err(SyncError::Malformed("invalid share answer")),
            },
            CANCEL if body.is_empty() => Ok(Self::Cancel { room }),
            CANCEL => Err(SyncError::Malformed("cancel has trailing bytes")),
            JOIN => {
                let ((header, body_bytes, sig), n) = record(body).ok_or(SyncError::Malformed("incomplete genesis record"))?;
                if n != body.len() { return Err(SyncError::Malformed("trailing bytes after genesis")) }
                if Event::genesis_room(header) != room {
                    return Err(SyncError::Malformed("join room differs from its genesis"))
                }
                let event = Event::from_parts(room, header.to_vec(), body_bytes.to_vec(), sig.to_vec()).map_err(SyncError::Event)?;
                if event.header.kind != kind::ROOM || !matches!(event.content().and_then(|b| Content::decode(
                    kind::ROOM, event.header.kind_version, b)), Some(Content::Room { .. })) {
                    return Err(SyncError::Malformed("join requires a room genesis"))
                }
                Ok(Self::Join { room, genesis: event })
            }
            _ => Err(SyncError::Malformed("unknown message kind")),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let (ty, room) = match self {
            Self::Heads { room, .. } => (HEADS, room),
            Self::Events { room, .. } => (EVENTS, room),
            Self::Offer { room, .. } => (OFFER, room),
            Self::Answer { room, .. } => (ANSWER, room),
            Self::Cancel { room } => (CANCEL, room),
            Self::Join { room, .. } => (JOIN, room),
        };
        let mut bytes = Vec::with_capacity(HEADER_BYTES);
        bytes.extend([VERSION, ty]);
        bytes.extend_from_slice(room);
        match self {
            Self::Heads { heads, .. } => bytes.extend_from_slice(heads.as_flattened()),
            Self::Events { records, .. } => bytes.extend_from_slice(records),
            Self::Offer { offer, .. } => {
                let (what, counts, since) = match *offer {
                    Offer::This { counts, since } => (1, counts, since),
                    Offer::New => (2, Counts::default(), 0),
                };
                bytes.push(what);
                for n in [counts.projects, counts.tasks, counts.docs, counts.messages] { bytes.extend(n.to_le_bytes()); }
                bytes.extend(since.to_le_bytes());
            }
            Self::Answer { yes, .. } => bytes.push(u8::from(*yes)),
            Self::Cancel { .. } => {},
            Self::Join { genesis, .. } => encode_record(genesis, &mut bytes),
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::DeviceKey;

    #[test]
    fn whole_messages_round_trip_and_reject_ambiguous_bodies() {
        let key = DeviceKey::generate().unwrap();
        let (room, genesis) = Event::found(&key, 2, 0, 47, [5; 16]).unwrap();
        let mut record = Vec::new();
        encode_record(&genesis, &mut record);
        let counts = Counts { projects: 1, tasks: 0x102, docs: 3, messages: 4 };
        let heads = [genesis.id];
        for message in [Message::Heads { room, heads: &heads }, Message::Events { room, records: &record },
            Message::Offer { room, offer: Offer::This { counts, since: 0x0102030405060708 } }, Message::Offer { room, offer: Offer::New },
            Message::Answer { room, yes: false }, Message::Answer { room, yes: true }, Message::Cancel { room }, Message::Join { room, genesis }] {
            let bytes = message.encode();
            assert_eq!(&bytes[2..34], &room, "the room immediately follows version and kind");
            if matches!(message, Message::Join { .. }) {
                let mut misnamed = bytes.clone();
                misnamed[2] ^= 1;
                assert!(matches!(Message::decode(&misnamed), Err(SyncError::Malformed(_))), "JOIN must name its signed genesis");
                let mut damaged = bytes.clone();
                *damaged.last_mut().unwrap() ^= 1;
                assert!(matches!(Message::decode(&damaged), Err(SyncError::Event(_))), "JOIN verifies the signature of its genesis");
            }
            assert_eq!(Message::decode(&bytes).unwrap(), message, "the whole message round-trips");
            assert!(Message::decode(&bytes[..bytes.len()-1]).is_err(), "a truncated message is refused");
            let mut extra = bytes.clone();
            extra.push(0);
            assert!(Message::decode(&extra).is_err(), "a trailing byte is refused");
            if let Message::Offer { offer: Offer::This { .. }, .. } = message {
                assert_eq!(&bytes[39..43], &[2, 1, 0, 0], "offer counts are little-endian");
                assert_eq!(&bytes[51..], &[8, 7, 6, 5, 4, 3, 2, 1], "genesis time is little-endian");
            }
        }
        let mut offer = Message::Offer { room, offer: Offer::New }.encode();
        offer[34] = 0;
        assert!(Message::decode(&offer).is_err(), "an unknown offer kind is refused");
        let mut invalid = Message::Answer { room, yes: true }.encode();
        invalid[34] = 2;
        assert!(Message::decode(&invalid).is_err(), "yes is exactly zero or one");
        invalid[1] = 255;
        assert!(Message::decode(&invalid).is_err(), "an unknown type is refused");
        for old in [1, 2] {
            assert!(matches!(Message::decode(&[old, HEADS]), Err(SyncError::Version(v)) if v == old), "sync version {old} is refused");
        }
    }
}
