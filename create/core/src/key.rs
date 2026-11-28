//! This device's key: a P-256 key made by the OS and kept in the home directory (0600). It is
//! Town's own key, never the fleet's, so Town's identity and the fleet's admin rights stay
//! apart. The private half is used only by two functions, each taking its purpose:
//! [`DeviceKey::sign`] with a [`Domain`], and [`DeviceKey::agree`] with an [`Agreement`]. Each
//! purpose has its own label (see [`label`]), so a signature or a secret made for one can never
//! pass as another.

use makepad_network::at_rest::{at_rest_protection, protect_at_rest, unprotect_at_rest, Protection};
use makepad_network::digest::sha256_hash;
use makepad_network::p2p::words;
use makepad_network::tls;
use makepad_network::tunnel::hmac_sha256;
use std::io;
use std::path::Path;

/// An uncompressed P-256 public point (`04 | X | Y`).
pub type PublicKey = [u8; 65];

/// Every label Town hashes, signs or derives under: `makepad create v1 <purpose>`, NUL-ended,
/// so none is a prefix of another. They are wire constants and never change; a new purpose gets a
/// new label.
pub mod label {
    pub const EVENT: &[u8] = b"makepad create v1 event\0";
    pub const TOWN_ID: &[u8] = b"makepad create v1 group\0";
    pub const LINK: &[u8] = b"makepad create v1 link\0";
    pub const DEVICE_INTRO: &[u8] = b"makepad create v1 device intro\0";
    pub const MEMBER: &[u8] = b"makepad create v1 member\0";
    pub const LINK_SECRET: &[u8] = b"makepad create v1 link secret\0";
    pub const DEVICE_WORDS: &[u8] = b"makepad create v1 device words\0";
    /// Reserved, and unused: call media draws its own secrets and takes no key from the link.
    pub const MEDIA: &[u8] = b"makepad create v1 media\0";
    pub const BLOB_LEAF: &[u8] = b"makepad create v1 blob leaf\0";
    pub const BLOB_NODE: &[u8] = b"makepad create v1 blob node\0";
    pub const BLOB_ID: &[u8] = b"makepad create v1 blob id\0";

    /// All of them, for the test that keeps them apart.
    #[cfg(test)]
    pub const ALL: &[&[u8]] = &[
        EVENT, TOWN_ID, LINK, DEVICE_INTRO, MEMBER, LINK_SECRET, DEVICE_WORDS, MEDIA,
        BLOB_LEAF, BLOB_NODE, BLOB_ID,
    ];
}

/// What a signature is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    /// The header of a log event.
    Event,
    /// One side's proof in a link handshake, by its auth key.
    Link,
    /// A first contact's introduction of a device key, signed by that key and by the invite key.
    DeviceIntro,
    /// A membership entry: a device added to a group or removed from it.
    Member,
}

impl Domain {
    fn prefix(self) -> &'static [u8] {
        match self {
            Domain::Event => label::EVENT,
            Domain::Link => label::LINK,
            Domain::DeviceIntro => label::DEVICE_INTRO,
            Domain::Member => label::MEMBER,
        }
    }
}

/// What a key agreement is for. The raw ECDH output never leaves [`DeviceKey::agree`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agreement {
    /// The secret two devices share, with nothing sent: it keys the link gate and the probe tags.
    /// See [`DeviceKey::link_secret`].
    LinkSecret,
}

impl Agreement {
    fn label(self) -> &'static [u8] {
        match self {
            Agreement::LinkSecret => label::LINK_SECRET,
        }
    }
}

pub struct DeviceKey {
    /// X9.63: `04 | X | Y | D`.
    private: [u8; tls::X963_KEY_LEN],
}

impl DeviceKey {
    /// A new key from the OS (tests, and the in-memory invite key; the app keeps its device key
    /// with [`DeviceKey::load_or_create`]).
    pub fn generate() -> io::Result<DeviceKey> {
        Ok(DeviceKey { private: tls::generate_p256()? })
    }

    /// The key at `path`, made and written (protected, 0600, atomically) the first time, and the
    /// level it is protected at. A file that is torn, the wrong length or won't unprotect is an
    /// error: it never leads to a new key.
    pub fn load_or_create(path: &Path) -> io::Result<(DeviceKey, Protection)> {
        const PURPOSE: &str = "makepad create device key";
        match std::fs::read(path) {
            Ok(stored) => {
                let not_a_key = |why: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{} is not a device key ({why})", path.display()));
                let bytes = unprotect_at_rest(PURPOSE, &stored).map_err(|e| not_a_key(&e.to_string()))?;
                let private = bytes.try_into().map_err(|_| not_a_key("wrong length"))?;
                Ok((DeviceKey { private }, at_rest_protection()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let key = DeviceKey::generate()?;
                let (stored, level) = protect_at_rest(PURPOSE, &key.private)?;
                crate::home::write_private(path, &stored)?;
                Ok((key, level))
            }
            Err(e) => Err(e),
        }
    }

    pub fn public(&self) -> PublicKey {
        let mut public = [0u8; 65];
        public.copy_from_slice(&self.private[..65]);
        public
    }

    /// A DER ECDSA-SHA256 signature over `msg` for `domain`.
    pub fn sign(&self, domain: Domain, msg: &[u8]) -> io::Result<Vec<u8>> {
        tls::sign_p256_sha256(&self.private, &[domain.prefix(), msg].concat())
    }

    /// A 32-byte secret shared with `peer` for `purpose`: HKDF-SHA256 over the ECDH output, with
    /// the purpose's label and then `context` as its info. Refuses an agreement with itself.
    pub fn agree(&self, purpose: Agreement, peer: &PublicKey, context: &[u8]) -> io::Result<[u8; 32]> {
        if *peer == self.public() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a key agreement with itself"));
        }
        let shared = tls::ecdh_p256(&self.private, peer)?;
        let mut out = [0; 32];
        hkdf_sha256(&[], &shared, &[purpose.label(), context].concat(), &mut out);
        Ok(out)
    }

    /// The link secret this device shares with `peer`, one per device pair and whatever towns the
    /// two share: the context is `lower key | higher key`, so both sides derive the same bytes.
    pub fn link_secret(&self, peer: &PublicKey) -> io::Result<[u8; 32]> {
        let me = self.public();
        let (lower, higher) = if me < *peer { (&me, peer) } else { (peer, &me) };
        self.agree(Agreement::LinkSecret, peer, &[&lower[..], &higher[..]].concat())
    }
}

/// Whether `sig` is `public`'s signature over `msg` for `domain`.
pub fn verify(domain: Domain, public: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    tls::verify_p256_sha256(public, &[domain.prefix(), msg].concat(), sig)
}

/// HKDF-SHA256 (RFC 5869) into `out`: extract with `salt` (an empty salt is the RFC's zeros),
/// then expand with `info`. `out` is at most 255 blocks (8160 bytes), as the RFC defines it.
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) {
    assert!(out.len() <= 255 * 32, "HKDF-SHA256 output is at most 8160 bytes");
    let prk = hmac_sha256(salt, ikm);
    let mut block: Vec<u8> = Vec::new();
    for (i, chunk) in out.chunks_mut(32).enumerate() {
        block = hmac_sha256(&prk, &[&block[..], info, &[i as u8 + 1]].concat()).to_vec();
        chunk.copy_from_slice(&block[..chunk.len()]);
    }
}

/// The 8 words that name a device key: the first 88 bits of its hash stretched 2^20 times, 11 bits
/// a word from the invite word list. Each person reads their own words to the others, who check
/// them on that member's card. Takes the key exactly as pinned. Run it off the UI thread.
pub fn device_words(public: &PublicKey) -> [&'static str; 8] {
    let mut hash = sha256_hash(&[label::DEVICE_WORDS, public].concat());
    for _ in 0..(1 << 20) {
        hash = sha256_hash(&hash);
    }
    let mut first = [0; 16];
    first[..11].copy_from_slice(&hash[..11]);
    let bits = u128::from_be_bytes(first);
    let list = words::list();
    std::array::from_fn(|i| list[(bits >> (128 - words::BITS_PER_WORD * (i + 1))) as usize & 0x7ff])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn only_the_domain_functions_use_the_device_key() {
        // The private half is a private field: only `sign` and `agree` read it. What remains to
        // hold is that their purposes stay apart.
        for (i, a) in label::ALL.iter().enumerate() {
            assert!(a.ends_with(b"\0") && a.iter().filter(|&&b| b == 0).count() == 1, "label {i} ends in its only NUL");
            for b in &label::ALL[i + 1..] {
                assert!(!a.starts_with(b) && !b.starts_with(a), "labels are prefix-free");
            }
        }
        let (a, b) = (DeviceKey::generate().unwrap(), DeviceKey::generate().unwrap());
        let sig = a.sign(Domain::Link, b"msg").unwrap();
        assert!(verify(Domain::Link, &a.public(), b"msg", &sig), "a signature verifies in its own domain");
        for other in [Domain::Event, Domain::DeviceIntro, Domain::Member] {
            assert!(!verify(other, &a.public(), b"msg", &sig), "a Link signature is no {other:?} signature");
        }
        let secret = a.link_secret(&b.public()).unwrap();
        assert_eq!(secret, b.link_secret(&a.public()).unwrap(), "both sides derive one link secret");
        let c = DeviceKey::generate().unwrap();
        assert_ne!(secret, a.link_secret(&c.public()).unwrap(), "each peer has its own link secret");
        assert_ne!(secret, a.agree(Agreement::LinkSecret, &b.public(), &[7; 32]).unwrap(), "the context is in the secret");
        assert!(a.link_secret(&a.public()).is_err(), "no agreement with itself");
    }

    #[test]
    fn hkdf_matches_rfc_5869() {
        let mut okm = [0; 42];
        hkdf_sha256(&(0..13).collect::<Vec<u8>>(), &[0x0b; 22], &(0xf0..=0xf9).collect::<Vec<u8>>(), &mut okm);
        assert_eq!(hex(&okm), "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865", "test case 1");
        hkdf_sha256(&[], &[0x0b; 22], &[], &mut okm);
        assert_eq!(hex(&okm), "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8", "test case 3: empty salt and info");
    }

    #[test]
    fn device_words_have_known_answers() {
        // Independently computed (Python hashlib) for two fixed points.
        let a: PublicKey = std::array::from_fn(|i| if i == 0 { 4 } else { 0x11 });
        let b: PublicKey = std::array::from_fn(|i| if i == 0 { 4 } else { 0x22 });
        assert_eq!(device_words(&a).join(" "), "cowhide tapping radish feathers rutabaga employer excite wasps", "key a");
        assert_eq!(device_words(&b).join(" "), "wittiest recycle prompt ravioli pawprint tramline mimics acoustic", "key b");
    }
}
