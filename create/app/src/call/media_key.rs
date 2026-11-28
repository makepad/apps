//! One direction's media key for the lifetime of an authenticated link.
use super::signal::{Secret, MEDIA_KEY, VERSION};
use super::CallId;
use makepad_town_core::key::{hkdf_sha256, PublicKey};
use std::io;

// Keep the bytes in one allocation as the Link moves. A MediaKey cannot be copied or logged.
#[cfg_attr(test, derive(PartialEq))]
pub(super) struct MediaKey(Box<Secret>);

fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY: each byte is exclusively borrowed and writable. Volatile keeps the
        // overwrite observable even when the allocation is about to be freed.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

impl Drop for MediaKey {
    fn drop(&mut self) { wipe(self.0.as_mut()); }
}

impl MediaKey {
    pub(super) fn generate() -> io::Result<Self> {
        let mut key = Self(Box::new([0; 32]));
        makepad_network::tls::os_random(key.0.as_mut())?;
        Ok(key)
    }

    pub(super) fn encode(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(34);
        body.extend([VERSION, MEDIA_KEY]);
        body.extend_from_slice(self.0.as_ref());
        body
    }

    /// Consume the key bytes from a complete Call-channel body, before call-ID dispatch.
    pub(super) fn take_from(body: &mut [u8]) -> Option<Self> {
        let [VERSION, MEDIA_KEY, bytes @ ..] = body else { return None };
        let secret: &mut Secret = bytes.try_into().ok()?;
        let mut key = Self(Box::new([0; 32]));
        key.0.copy_from_slice(secret);
        wipe(secret);
        Some(key)
    }

    pub(super) fn media_id(n: u32, (me, local): (&PublicKey, &Self), (peer, remote): (&PublicKey, &Self)) -> CallId {
        let (lo, hi) = if me < peer { (local, remote) } else { (remote, local) };
        let mut ikm = [0; 64];
        ikm[..32].copy_from_slice(lo.0.as_ref());
        ikm[32..].copy_from_slice(hi.0.as_ref());
        let mut id = [0; 16];
        let info = [b"makepad town v1 media id\0".as_slice(), &n.to_be_bytes()].concat();
        hkdf_sha256(&[], &ikm, &info, &mut id);
        wipe(&mut ikm);
        id
    }

    pub(super) fn session_secret(&self, id: &CallId, sender: &PublicKey) -> Secret {
        let info = [b"makepad town v1 media secret\0".as_slice(), sender].concat();
        let mut secret = [0; 32];
        hkdf_sha256(id, self.0.as_ref(), &info, &mut secret);
        secret
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_complete_media_key_is_taken_and_its_wire_payload_is_wiped() {
        let key = MediaKey(Box::new([0xa5; 32]));
        let mut body = key.encode();
        assert!(MediaKey::take_from(&mut body[..33]).is_none());
        body.push(0);
        assert!(MediaKey::take_from(&mut body).is_none());
        body.pop();
        body[0] = 2;
        assert!(MediaKey::take_from(&mut body).is_none());
        body[0] = 1;
        body[1] = 9;
        assert!(MediaKey::take_from(&mut body).is_none());
        body[1] = 12;
        assert!(MediaKey::take_from(&mut body) == Some(key));
        assert_eq!(body[2..], [0; 32]);
    }

    #[test]
    fn media_id_and_directional_secrets_match_the_hkdf_vectors() {
        let (mut lo, mut hi) = ([0x11; 65], [0xbc; 65]);
        lo[0] = 4;
        hi[0] = 4;
        let (a, b) = (MediaKey(Box::new([0xe1; 32])), MediaKey(Box::new([0x2c; 32])));
        let n = 0x01020304;
        let id = MediaKey::media_id(n, (&lo, &a), (&hi, &b));
        let send = a.session_secret(&id, &lo);
        let recv = b.session_secret(&id, &hi);
        // Independently calculated with RFC 5869 extract and one SHA-256 expand block.
        assert_eq!(id, [0xd8,0x00,0x43,0x10,0x86,0x1c,0x7b,0x0e,0xda,0x43,0xe2,0x8d,0xbe,0xd2,0xb9,0x33]);
        assert_eq!(send, [
            0x95,0xac,0xc0,0x38,0x66,0x53,0xd2,0x35,0x14,0x84,0x25,0xe9,0x5e,0xd6,0x55,0xb4,
            0x8b,0x44,0x6e,0x8d,0xa2,0x68,0x1e,0x50,0x29,0xde,0x4e,0xd2,0xea,0xfd,0x9d,0x80,
        ]);
        assert_eq!(recv, [
            0xf2,0x4a,0x5e,0x2c,0xd2,0x79,0xb8,0x68,0x19,0xb1,0xbb,0xe3,0x66,0x40,0x8a,0x5d,
            0x49,0x0e,0x93,0x17,0x9e,0xb8,0xef,0x65,0x22,0x26,0xfe,0x3a,0x44,0x84,0x59,0xf4,
        ]);
        assert_eq!(id, MediaKey::media_id(n, (&hi, &b), (&lo, &a)), "both seats order by device key");
        let next = MediaKey::media_id(n + 1, (&lo, &a), (&hi, &b));
        assert_ne!(next, id);
        assert_ne!(a.session_secret(&next, &lo), send);
        assert_ne!(a.session_secret(&id, &hi), send, "the sender's public key binds the direction");
        assert_ne!(b.session_secret(&id, &lo), send, "changing only the sender's secret changes the result");
        assert_ne!(id, MediaKey::media_id(n, (&lo, &b), (&hi, &b)));
        assert_ne!(id, MediaKey::media_id(n, (&lo, &a), (&hi, &a)));
    }
}
