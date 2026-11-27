//! Whole messages on the link's Sync lane. Offers are ephemeral, and a JOIN carries
//! only the signed genesis. Town history follows through the ordinary heads exchange.

use crate::event::{kind, Content, Event, Id, TownId};
use crate::log::{encode_record, record};
use crate::sync::{SyncError, EVENTS, HEADS, VERSION};

pub(crate) const HEADER_BYTES: usize = 2 + size_of::<TownId>();
pub mod hello;
use hello::Hello;

const OFFER: u8 = 3;
const ANSWER: u8 = 4;
const CANCEL: u8 = 5;
const JOIN: u8 = 6;
const HEADS_DONE: u8 = 7;
const HELLO: u8 = 8;

pub fn is_hello(bytes: &[u8]) -> bool { bytes.starts_with(&[VERSION, HELLO]) }

/// These locally initiated floor messages retain their order even before the peer speaks.
pub fn unprompted(bytes: &[u8]) -> bool {
    matches!(bytes, [VERSION, HELLO | OFFER | CANCEL, ..])
}

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
    Hello(Hello),
    Heads { town: TownId, heads: &'a [Id] },
    HeadsDone { town: TownId, heads: &'a [Id] },
    Events { town: TownId, records: &'a [u8] },
    Offer { town: TownId, offer: Offer },
    Answer { town: TownId, yes: bool },
    Cancel { town: TownId },
    Join { town: TownId, genesis: Event },
}

impl<'a> Message<'a> {
    /// An unknown version or kind has no message for this receiver.
    pub fn decode(bytes: &'a [u8]) -> Result<Option<Self>, SyncError> {
        let (&[version, ty], body) = bytes.split_first_chunk::<2>()
            .ok_or(SyncError::Malformed("incomplete message header"))?;
        if version != VERSION { return Ok(None) }
        let town_body = || body.split_first_chunk::<32>().ok_or(SyncError::Malformed("incomplete town id"));
        Ok(Some(match ty {
            HELLO => {
                let (town, body) = town_body()?;
                if town != &[0; 32] { return Err(SyncError::Malformed("Hello names a town")) }
                Self::Hello(Hello::decode(body)?)
            }
            HEADS | HEADS_DONE => {
                let (&town, body) = town_body()?;
                let (heads, tail) = body.as_chunks::<32>();
                if !tail.is_empty() { return Err(SyncError::Malformed("incomplete head id")) }
                if ty == HEADS { Self::Heads { town, heads } } else { Self::HeadsDone { town, heads } }
            }
            EVENTS => {
                let (&town, body) = town_body()?;
                let mut rest = body;
                while !rest.is_empty() {
                    let (_, n) = record(rest).ok_or(SyncError::Malformed("incomplete record"))?;
                    rest = &rest[n..];
                }
                Self::Events { town, records: body }
            }
            OFFER => {
                let (&town, body) = town_body()?;
                let (&what, fields) = body.split_first().ok_or(SyncError::Malformed("missing offer kind"))?;
                let (&counts, tail) = fields.split_first_chunk::<16>().ok_or(SyncError::Malformed("incomplete offer counts"))?;
                let since = u64::from_le_bytes(tail.try_into().map_err(|_| SyncError::Malformed("invalid offer date length"))?);
                let words: [[u8; 4]; 4] = counts.as_chunks::<4>().0.try_into()
                    .map_err(|_| SyncError::Malformed("invalid offer counts"))?;
                let [projects, tasks, docs, messages] = words.map(u32::from_le_bytes);
                let counts = Counts { projects, tasks, docs, messages };
                match what {
                    1 => Self::Offer { town, offer: Offer::This { counts, since } },
                    2 if counts == Counts::default() && since == 0 => Self::Offer { town, offer: Offer::New },
                    2 => return Err(SyncError::Malformed("new town offer has history")),
                    _ => return Err(SyncError::Malformed("unknown offer kind")),
                }
            }
            ANSWER => {
                let (&town, body) = town_body()?;
                match body {
                    [0] => Self::Answer { town, yes: false },
                    [1] => Self::Answer { town, yes: true },
                    _ => return Err(SyncError::Malformed("invalid share answer")),
                }
            }
            CANCEL => {
                let (&town, body) = town_body()?;
                if !body.is_empty() { return Err(SyncError::Malformed("cancel has trailing bytes")) }
                Self::Cancel { town }
            }
            JOIN => {
                let (&town, body) = town_body()?;
                let ((header, body_bytes, sig), n) = record(body).ok_or(SyncError::Malformed("incomplete genesis record"))?;
                if n != body.len() { return Err(SyncError::Malformed("trailing bytes after genesis")) }
                if Event::genesis_town(header) != town {
                    return Err(SyncError::Malformed("join town differs from its genesis"))
                }
                let event = Event::from_parts(town, header.to_vec(), body_bytes.to_vec(), sig.to_vec()).map_err(SyncError::Event)?;
                if event.header.kind != kind::TOWN || !matches!(event.content().and_then(|b| Content::decode(
                    kind::TOWN, event.header.kind_version, b)), Some(Content::Town { .. })) {
                    return Err(SyncError::Malformed("join requires a town genesis"))
                }
                Self::Join { town, genesis: event }
            }
            _ => return Ok(None),
        }))
    }

    pub fn encode(&self) -> Vec<u8> {
        let (ty, town) = match self {
            Self::Hello(_) => (HELLO, &[0; 32]),
            Self::Heads { town, .. } => (HEADS, town),
            Self::HeadsDone { town, .. } => (HEADS_DONE, town),
            Self::Events { town, .. } => (EVENTS, town),
            Self::Offer { town, .. } => (OFFER, town),
            Self::Answer { town, .. } => (ANSWER, town),
            Self::Cancel { town } => (CANCEL, town),
            Self::Join { town, .. } => (JOIN, town),
        };
        let mut bytes = Vec::with_capacity(HEADER_BYTES);
        bytes.extend([VERSION, ty]);
        bytes.extend_from_slice(town);
        match self {
            Self::Hello(hello) => hello.encode(&mut bytes),
            Self::Heads { heads, .. } | Self::HeadsDone { heads, .. } => bytes.extend_from_slice(heads.as_flattened()),
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
        let (town, genesis) = Event::found(&key, 2, 0, 47, [5; 16]).unwrap();
        let mut record = Vec::new();
        encode_record(&genesis, &mut record);
        let counts = Counts { projects: 1, tasks: 0x102, docs: 3, messages: 4 };
        let heads = [genesis.id];
        for message in [Message::Heads { town, heads: &heads }, Message::HeadsDone { town, heads: &heads }, Message::Events { town, records: &record },
            Message::Offer { town, offer: Offer::This { counts, since: 0x0102030405060708 } }, Message::Offer { town, offer: Offer::New },
            Message::Answer { town, yes: false }, Message::Answer { town, yes: true }, Message::Cancel { town }, Message::Join { town, genesis }] {
            let bytes = message.encode();
            assert_eq!(&bytes[2..34], &town, "the town immediately follows version and kind");
            if matches!(message, Message::Join { .. }) {
                let mut misnamed = bytes.clone();
                misnamed[2] ^= 1;
                assert!(matches!(Message::decode(&misnamed), Err(SyncError::Malformed(_))), "JOIN must name its signed genesis");
                let mut damaged = bytes.clone();
                *damaged.last_mut().unwrap() ^= 1;
                assert!(matches!(Message::decode(&damaged), Err(SyncError::Event(_))), "JOIN verifies the signature of its genesis");
            }
            assert_eq!(Message::decode(&bytes).unwrap().as_ref(), Some(&message), "the whole message round-trips");
            assert!(Message::decode(&bytes[..bytes.len()-1]).is_err(), "a truncated message is refused");
            let mut extra = bytes.clone();
            extra.push(0);
            assert!(Message::decode(&extra).is_err(), "a trailing byte is refused");
            if let Message::Offer { offer: Offer::This { .. }, .. } = message {
                assert_eq!(&bytes[39..43], &[2, 1, 0, 0], "offer counts are little-endian");
                assert_eq!(&bytes[51..], &[8, 7, 6, 5, 4, 3, 2, 1], "genesis time is little-endian");
            }
        }
        let mut offer = Message::Offer { town, offer: Offer::New }.encode();
        offer[34] = 0;
        assert!(Message::decode(&offer).is_err(), "an unknown offer kind is refused");
        let mut invalid = Message::Answer { town, yes: true }.encode();
        invalid[34] = 2;
        assert!(Message::decode(&invalid).is_err(), "yes is exactly zero or one");
        invalid[1] = 255;
        assert_eq!(Message::decode(&invalid).unwrap(), None, "an unknown kind is ignored");
        for version in [1, 2, 4] {
            assert_eq!(Message::decode(&[version, HEADS]).unwrap(), None, "sync version {version} is ignored");
        }
    }
}
