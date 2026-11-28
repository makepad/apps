//! The fixed greeting and its optional, length-delimited source snapshot.
use crate::sync::SyncError;

/// Multi-party voice calls (`RingExt.town/roster`, `CallJoin`/`CallJoinAck`).
pub const MULTIPARTY_CALL_LEVEL: u16 = 4;
/// Structural design requests and notices on the Tweak topic.
pub const DESIGN_LEVEL: u16 = MULTIPARTY_CALL_LEVEL + 1;
/// Multi-viewer town-tagged shares and single-holder takeover.
pub const MULTIPARTY_SHARE_LEVEL: u16 = DESIGN_LEVEL + 1;
/// Raise when a landing introduces behaviour that requires a level gate.
pub const LEVEL: u16 = MULTIPARTY_SHARE_LEVEL;
/// Raise behind the fleet as support for older levels is retired.
pub const MIN_PEER: u16 = 0;
const SOURCES: u8 = 1;
pub const TAG_MEDIA_CAPS: u8 = 4;

/// Media capabilities advertised in a greeting or share watch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediaCaps(pub u32);

impl MediaCaps {
    pub const NONE: Self = Self(0);
    pub const H264_DECODE: u32 = 1 << 0;
    pub const HEVC_DECODE: u32 = 1 << 1;
    pub const H264_444_DECODE: u32 = 1 << 2;
    pub const HEVC_444_DECODE: u32 = 1 << 3;
    pub const GPU_DECODE: u32 = 1 << 4;
    pub const MULTIPARTY_V1: u32 = 1 << 5;
    pub const MULTIPARTY_SHARE_V1: u32 = 1 << 6;

    pub fn supports(self, bit: u32) -> bool { self.0 & bit != 0 }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub key: String,
    pub branch: String,
    pub upstream: [u8; 20],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hello {
    pub level: u16,
    pub min_peer: u16,
    pub sources: Vec<Source>,
    pub media_caps: MediaCaps,
}

fn public(key: &str) -> bool {
    matches!(key, "github.com/makepad/makepad" | "github.com/makepad/apps")
}

impl Hello {
    pub fn local<'a>(repos: impl Iterator<Item = (&'a str, &'a str, &'a str)>) -> Self {
        let sources = repos.filter_map(|(key, branch, oid)| {
            if key.len() > 255 || branch.len() > 255 {
                eprintln!("town: skipping a source whose key or branch exceeds 255 bytes");
                return None
            }
            if !public(key) { return None }
            let upstream = if oid.len() == 40 { makepad_network::tls::from_hex(oid).unwrap_or_default() } else { [0; 20] };
            Some(Source { key: key.into(), branch: branch.into(), upstream })
        }).collect();
        Self { level: LEVEL, min_peer: MIN_PEER, sources, media_caps: MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1) }
    }

    pub fn compatible(&self, peer: &Self) -> bool {
        peer.level >= self.min_peer && self.level >= peer.min_peer
    }

    pub fn needs_update(&self, peer: &Self) -> bool {
        peer.level > self.level || self.sources.iter().any(|ours| peer.sources.iter()
            .any(|theirs| theirs.key == ours.key && theirs.upstream != ours.upstream))
    }

    pub(super) fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.extend(self.level.to_le_bytes());
        bytes.extend(self.min_peer.to_le_bytes());
        let mut tag = bytes.len();
        bytes.extend([SOURCES, 1, 0, 0]);
        for source in &self.sources {
            let (Ok(key), Ok(branch)) = (u8::try_from(source.key.len()), u8::try_from(source.branch.len())) else { continue };
            if !public(&source.key) { continue }
            let len = bytes.len() - tag - 3 + 22 + usize::from(key) + usize::from(branch);
            if bytes[tag+3] == u8::MAX || len > usize::from(u16::MAX) {
                tag = bytes.len();
                bytes.extend([SOURCES, 1, 0, 0]);
            }
            bytes[tag+3] += 1;
            bytes.push(key);
            bytes.extend(source.key.as_bytes());
            bytes.push(branch);
            bytes.extend(source.branch.as_bytes());
            bytes.extend(source.upstream);
            let len = (bytes.len() - tag - 3) as u16;
            bytes[tag+1..tag+3].copy_from_slice(&len.to_le_bytes());
        }
        if self.media_caps.0 != 0 {
            crate::tlv::write_tlv(bytes, TAG_MEDIA_CAPS, &self.media_caps.0.to_le_bytes());
        }
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, SyncError> {
        let (&[a,b,c,d], mut tail) = bytes.split_first_chunk().ok_or(SyncError::Malformed("short Hello prefix"))?;
        let mut hello = Self { level: u16::from_le_bytes([a,b]), min_peer: u16::from_le_bytes([c,d]), ..Self::default() };
        loop {
            match crate::tlv::read_tlv(&mut tail) {
                Ok(Some((SOURCES, body))) => {
                    if read_sources(body, &mut hello.sources).is_none() {
                        eprintln!("town: ignoring a malformed SOURCES tag");
                    }
                }
                Ok(Some((TAG_MEDIA_CAPS, body))) => match body.try_into() {
                    Ok(bytes) => hello.media_caps = MediaCaps(u32::from_le_bytes(bytes)),
                    Err(_) => eprintln!("town: ignoring a malformed MEDIA_CAPS tag"),
                },
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(why) => {
                    eprintln!("town: ignoring {why} in Hello");
                    break;
                }
            }
        }
        Ok(hello)
    }
}

fn string<'a>(input: &mut &'a [u8]) -> Option<&'a str> {
    let (&len, tail) = input.split_first()?;
    let (bytes, tail) = tail.split_at_checked(usize::from(len))?;
    *input = tail;
    std::str::from_utf8(bytes).ok()
}

fn read_sources(body: &[u8], sources: &mut Vec<Source>) -> Option<()> {
    let (&count, mut body) = body.split_first()?;
    let mut parsed = Vec::new();
    for _ in 0..count {
        let key = string(&mut body)?;
        let branch = string(&mut body)?;
        let (&upstream, tail) = body.split_first_chunk()?;
        if public(key) { parsed.push(Source { key: key.into(), branch: branch.into(), upstream }); }
        body = tail;
    }
    if !body.is_empty() { return None }
    sources.append(&mut parsed);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_encodes_multiparty_share_level_and_caps() {
        let hello = Hello::local(std::iter::empty());
        assert_eq!(hello.level, LEVEL, "the local greeting advertises the current level");
        assert!(hello.level >= MULTIPARTY_SHARE_LEVEL, "the advertised level includes multi-viewer shares");
        assert!(MULTIPARTY_SHARE_LEVEL > MULTIPARTY_CALL_LEVEL, "voice-only copies recognize a newer greeting");
        assert!(MULTIPARTY_SHARE_LEVEL > DESIGN_LEVEL, "copies with design requests but no multi-viewer shares recognize a newer greeting");
        assert_eq!(hello.min_peer, MIN_PEER, "the greeting keeps the minimum peer level");
        assert_eq!(hello.media_caps, MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1),
            "the greeting advertises both multi-party voice and multi-viewer shares");
        let mut wire = Vec::new();
        hello.encode(&mut wire);
        assert!(wire.ends_with(&[TAG_MEDIA_CAPS, 4, 0, 96, 0, 0, 0]), "both capabilities fit in the trailing seven-byte entry");
        assert_eq!(Hello::decode(&wire).unwrap(), hello, "the greeting round-trips with its capability");
    }

    #[test]
    fn hello_roundtrips_media_caps_and_old_decoder_skips_tag_4() {
        let mut hello = Hello::local([("github.com/makepad/apps", "main", "")].into_iter());
        hello.media_caps = MediaCaps::NONE;
        let mut legacy = Vec::new();
        hello.encode(&mut legacy);
        let mut tail = &legacy[4..];
        assert!(matches!(crate::tlv::read_tlv(&mut tail), Ok(Some((SOURCES, _)))));
        assert!(tail.is_empty());

        hello.media_caps = MediaCaps(MediaCaps::H264_DECODE | MediaCaps::GPU_DECODE);
        let mut wire = Vec::new();
        hello.encode(&mut wire);
        assert_eq!(wire.strip_prefix(legacy.as_slice()), Some([4, 4, 0, 17, 0, 0, 0].as_slice()));
        assert_eq!(Hello::decode(&wire).unwrap(), hello);
        assert_eq!(Hello::decode(&wire[..legacy.len()]).unwrap().media_caps, MediaCaps(0));

        let mut old_sources = Vec::new();
        let mut tail = &wire[4..];
        while let Some((tag, body)) = crate::tlv::read_tlv(&mut tail).unwrap() {
            if tag == SOURCES { read_sources(body, &mut old_sources).unwrap(); }
        }
        assert_eq!(old_sources, hello.sources);
        crate::tlv::write_tlv(&mut wire, TAG_MEDIA_CAPS, &[1]);
        assert_eq!(Hello::decode(&wire).unwrap(), hello);
        wire.extend([TAG_MEDIA_CAPS, 4, 0, 2]);
        assert_eq!(Hello::decode(&wire).unwrap(), hello);
    }
}
