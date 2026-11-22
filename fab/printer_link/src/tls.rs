//! Pinned CA signature, configured serial CN, and leaf TOFU, before MQTT.

use crate::secrets::{directory, PrinterConfig};
use makepad_network::tls::{fingerprint, KnownHosts};
use makepad_network::SocketStream;
use std::time::Duration;

// Certificate SHA-256 from the transport research, not a learned root.
const CA_FINGERPRINT: [u8; 32] = [
    0x03, 0x0b, 0xca, 0x81, 0xce, 0xce, 0x18, 0xb7, 0xef, 0xf3, 0xcf, 0xd2, 0xb7, 0x5d, 0x09, 0xd3,
    0xef, 0xca, 0x89, 0x3b, 0xc0, 0x69, 0x60, 0x9e, 0x37, 0xfa, 0x04, 0x25, 0x7f, 0xe4, 0xd8, 0x40,
];
const CA_FAILURE: &str = "Printer certificate not signed by its CA";

#[derive(Debug)]
pub(crate) enum ConnectError {
    PinMismatch,
    Offline(&'static str),
}

pub(crate) fn connect(name: &str, config: &PrinterConfig) -> Result<(SocketStream, String), ConnectError> {
    let (mut stream, chain) = SocketStream::connect_capture_chain(&config.host, "8883")
        .map_err(|_| ConnectError::Offline("printer TLS connection failed"))?;
    let checked = (|| {
        let serial = authenticate(&chain, &CA_FINGERPRINT, config.serial.as_deref())?;
        let der = &chain[0];
        let path = directory().ok_or(ConnectError::Offline("home directory unavailable"))?.join(format!("{name}.known-hosts"));
        let hosts = KnownHosts::new(path);
        pin(&hosts, &fingerprint(der))?;
        stream.set_read_timeout(Some(Duration::from_millis(250))).map_err(|_| ConnectError::Offline("cannot set printer read timeout"))?;
        stream.set_write_timeout(Some(Duration::from_secs(2))).map_err(|_| ConnectError::Offline("cannot set printer write timeout"))?;
        Ok(serial)
    })();
    match checked {
        Ok(serial) => Ok((stream, serial)),
        Err(error) => { stream.shutdown(); Err(error) }
    }
}

/// The printer's pinned leaf (set and CA-checked by the MQTT connection), for FTPS and RTSPS.
pub(crate) fn pinned_leaf(name: &str) -> Option<[u8; 32]> {
    let path = directory()?.join(format!("{name}.known-hosts"));
    KnownHosts::new(path).get("printer")
}

fn authenticate(chain: &[Vec<u8>], ca_pin: &[u8; 32], serial: Option<&str>) -> Result<String, ConnectError> {
    if !signed_by_ca(chain, ca_pin).unwrap_or(false) { return Err(ConnectError::Offline(CA_FAILURE)); }
    let cn = leaf_cn(&chain[0]).ok_or(ConnectError::Offline("printer certificate has no valid serial CN"))?;
    if serial.is_some_and(|s| s != cn) { return Err(ConnectError::Offline("Printer certificate serial does not match configuration")); }
    Ok(cn)
}

fn pin(hosts: &KnownHosts, seen: &[u8; 32]) -> Result<(), ConnectError> {
    // Never call observe on a change: it would replace the trusted pin.
    match hosts.get("printer") {
        Some(old) if old != *seen => return Err(ConnectError::PinMismatch),
        Some(_) => return Ok(()),
        None if hosts.path().try_exists().map_err(|_| ConnectError::Offline("cannot read printer pin"))? => {
            return Err(ConnectError::Offline("printer pin file is invalid or unreadable"));
        }
        None => {}
    }
    hosts.observe("printer", seen);
    if hosts.get("printer").as_ref() != Some(seen) {
        return Err(ConnectError::Offline("cannot persist printer certificate pin"));
    }
    Ok(())
}

fn tlv<'a>(input: &mut &'a [u8]) -> Option<(u8, &'a [u8])> {
    let tag = *input.first()?;
    let first = *input.get(1)?;
    let (length, header) = if first < 128 {
        (first as usize, 2)
    } else {
        let count = (first & 127) as usize;
        if count == 0 || count > std::mem::size_of::<usize>() { return None; }
        if *input.get(2)? == 0 { return None; }
        let mut n = 0usize;
        for b in input.get(2..2 + count)? { n = n.checked_mul(256)?.checked_add(*b as usize)?; }
        if n < 128 { return None; }
        (n, 2 + count)
    };
    let end = header.checked_add(length)?;
    let value = input.get(header..end)?;
    *input = input.get(end..)?;
    Some((tag, value))
}

fn take<'a>(input: &mut &'a [u8], tag: u8) -> Option<&'a [u8]> {
    let (seen, value) = tlv(input)?;
    (seen == tag).then_some(value)
}

/// Read the subject's CN, never an issuer CN or a SAN.
fn leaf_cn(mut der: &[u8]) -> Option<String> {
    let mut cert = take(&mut der, 0x30)?;
    if !der.is_empty() { return None; }
    let mut tbs = take(&mut cert, 0x30)?;
    if tbs.first() == Some(&0xa0) { take(&mut tbs, 0xa0)?; }
    take(&mut tbs, 0x02)?; // serial number
    for _ in 0..3 { take(&mut tbs, 0x30)?; } // algorithm, issuer, validity
    let mut subject = take(&mut tbs, 0x30)?;
    let mut cn = None;
    while !subject.is_empty() {
        let mut set = take(&mut subject, 0x31)?;
        while !set.is_empty() {
            let mut attribute = take(&mut set, 0x30)?;
            let oid = take(&mut attribute, 0x06)?;
            let (tag, text) = tlv(&mut attribute)?;
            if !attribute.is_empty() { return None; }
            if oid == [0x55, 0x04, 0x03] {
                if cn.is_some() || !matches!(tag, 0x0c | 0x13) || text.is_empty() || !text.iter().all(u8::is_ascii_alphanumeric) { return None; }
                cn = Some(std::str::from_utf8(text).ok()?.to_string());
            }
        }
    }
    cn
}

const SHA256_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 11];
const RSA_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1];
const SHA256_INFO: &[u8] = &[0x30, 0x31, 0x30, 0x0d, 6, 9, 0x60, 0x86, 0x48, 1, 0x65, 3, 4, 2, 1, 5, 0, 4, 0x20];

fn algorithm(mut value: &[u8], oid: &[u8]) -> Option<()> {
    if take(&mut value, 6)? != oid { return None; }
    if !value.is_empty() && !take(&mut value, 5)?.is_empty() { return None; }
    value.is_empty().then_some(())
}

fn integer<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
    let mut n = take(input, 2)?;
    if *n.first()? & 0x80 != 0 { return None; }
    if n[0] == 0 {
        n = n.get(1..)?;
        if *n.first()? & 0x80 == 0 { return None; }
    }
    Some(n)
}

struct Certificate<'a> {
    tbs: &'a [u8],
    algorithm: &'a [u8],
    tbs_algorithm: &'a [u8],
    signature: &'a [u8],
    spki: &'a [u8],
}

fn certificate(mut der: &[u8]) -> Option<Certificate<'_>> {
    let mut cert = take(&mut der, 0x30)?;
    if !der.is_empty() { return None; }
    let start = cert;
    let mut fields = take(&mut cert, 0x30)?;
    let tbs = &start[..start.len()-cert.len()]; // includes the SEQUENCE tag and length
    let algorithm = take(&mut cert, 0x30)?;
    let signature = take(&mut cert, 3)?;
    if signature.first() != Some(&0) || !cert.is_empty() { return None; }
    if fields.first() == Some(&0xa0) {
        let mut version = take(&mut fields, 0xa0)?;
        if take(&mut version, 2)? != [2] || !version.is_empty() { return None; }
    }
    integer(&mut fields)?;
    let tbs_algorithm = take(&mut fields, 0x30)?;
    for _ in 0..3 { take(&mut fields, 0x30)?; } // issuer, validity, subject
    let spki = take(&mut fields, 0x30)?;
    // Optional signed unique IDs and extensions; reject malformed lengths.
    while !fields.is_empty() {
        let (tag, _) = tlv(&mut fields)?;
        if !matches!(tag, 0x81 | 0x82 | 0xa3) { return None; }
    }
    Some(Certificate { tbs, algorithm, tbs_algorithm, signature: &signature[1..], spki })
}

fn public_key(mut spki: &[u8]) -> Option<(&[u8], &[u8])> {
    algorithm(take(&mut spki, 0x30)?, RSA_KEY)?;
    let bits = take(&mut spki, 3)?;
    if bits.first() != Some(&0) || !spki.is_empty() { return None; }
    let mut bits = &bits[1..];
    let mut key = take(&mut bits, 0x30)?;
    if !bits.is_empty() { return None; }
    let n = integer(&mut key)?;
    let e = integer(&mut key)?;
    if !key.is_empty() || e == [1] || e.last()? & 1 == 0 { return None; }
    Some((n, e))
}

fn signed_by_ca(chain: &[Vec<u8>], ca_pin: &[u8; 32]) -> Option<bool> {
    let leaf = certificate(chain.first()?)?;
    algorithm(leaf.algorithm, SHA256_RSA)?;
    algorithm(leaf.tbs_algorithm, SHA256_RSA)?;
    let ca = chain.iter().skip(1).find(|der| &fingerprint(der) == ca_pin)?;
    let ca = certificate(ca)?;
    let (modulus, exponent) = public_key(ca.spki)?;
    if leaf.signature.len() != modulus.len() { return Some(false); }
    let expected = encoded_hash(leaf.tbs, modulus.len())?;
    Some(modexp(leaf.signature, exponent, modulus)? == expected)
}

fn encoded_hash(message: &[u8], size: usize) -> Option<Vec<u8>> {
    let padding = size.checked_sub(3 + SHA256_INFO.len() + 32)?;
    if padding < 8 { return None; } // PKCS#1 v1.5 requires at least eight FF bytes.
    let mut block = vec![0, 1];
    block.resize(2 + padding, 0xff);
    block.push(0);
    block.extend_from_slice(SHA256_INFO);
    block.extend_from_slice(&makepad_network::digest::sha256_hash(message));
    Some(block)
}

// Unsigned little-endian base-2^32 limbs. Only public-key verification runs
// here in production, once per connection on the link worker. No secret-key
// timing guarantees are needed; tests use the same arithmetic to sign fixtures.
fn limbs(bytes: &[u8], count: usize) -> Vec<u32> {
    let mut out = vec![0; count];
    for (i, byte) in bytes.iter().rev().enumerate() { out[i/4] |= (*byte as u32) << ((i%4)*8); }
    out
}

fn less(a: &[u32], b: &[u32]) -> bool {
    a.iter().rev().cmp(b.iter().rev()).is_lt()
}

fn reduce_sum(a: &mut [u32], n: &[u32], carry: u64) {
    if carry == 0 && less(a, n) { return; }
    let mut borrow = 0u64;
    for (a, &n) in a.iter_mut().zip(n) {
        let sub = n as u64 + borrow;
        let before = *a as u64;
        *a = before.wrapping_sub(sub) as u32;
        borrow = u64::from(before < sub);
    }
}

fn multiply(a: &[u32], b: &[u32], n: &[u32]) -> Vec<u32> {
    let mut out = vec![0u32; n.len()];
    for word in b.iter().rev() {
        for bit in (0..32).rev() {
            let mut carry = 0u64;
            for v in &mut out {
                let sum = (*v as u64) * 2 + carry;
                *v = sum as u32; carry = sum >> 32;
            }
            reduce_sum(&mut out, n, carry);
            if word & (1 << bit) != 0 {
                let mut carry = 0u64;
                for (v, a) in out.iter_mut().zip(a) {
                    let sum = *v as u64 + *a as u64 + carry;
                    *v = sum as u32; carry = sum >> 32;
                }
                reduce_sum(&mut out, n, carry);
            }
        }
    }
    out
}

fn modexp(base: &[u8], exponent: &[u8], modulus: &[u8]) -> Option<Vec<u8>> {
    if modulus.first().copied()? == 0 || modulus.last()? & 1 == 0 || modulus == [1] || base.len() > modulus.len() { return None; }
    let count = modulus.len().div_ceil(4);
    let n = limbs(modulus, count);
    let base = limbs(base, count);
    if !less(&base, &n) { return None; }
    let mut result = vec![0; count];
    result[0] = 1;
    for byte in exponent {
        for bit in (0..8).rev() {
            result = multiply(&result, &result, &n);
            if byte & (1 << bit) != 0 { result = multiply(&result, &base, &n); }
        }
    }
    let bytes: Vec<_> = result.iter().rev().flat_map(|w| w.to_be_bytes()).collect();
    Some(bytes[bytes.len()-modulus.len()..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn der(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if body.len() < 128 { out.push(body.len() as u8); }
        else {
            let bytes = body.len().to_be_bytes();
            let first = bytes.iter().position(|b| *b != 0).unwrap();
            out.push(0x80 | (bytes.len()-first) as u8);
            out.extend_from_slice(&bytes[first..]);
        }
        out.extend_from_slice(body);
        out
    }
    fn name(text: &[u8]) -> Vec<u8> {
        let mut attr = der(0x06, &[0x55, 0x04, 0x03]);
        attr.extend(der(0x0c, text));
        der(0x30, &der(0x31, &der(0x30, &attr)))
    }
    #[test]
    fn cn_is_from_subject_and_malformed_der_is_rejected() {
        let mut tbs = der(0xa0, &der(0x02, &[2]));
        tbs.extend(der(0x02, &[1]));
        tbs.extend(der(0x30, &[]));
        tbs.extend(name(b"TESTISSUER"));
        tbs.extend(der(0x30, &[]));
        tbs.extend(name(b"TESTSERIAL0001"));
        let cert = der(0x30, &der(0x30, &tbs));
        assert_eq!(leaf_cn(&cert).as_deref(), Some("TESTSERIAL0001"));
        for i in 0..cert.len() { assert!(leaf_cn(&cert[..i]).is_none()); }
        assert!(leaf_cn(&[0x30, 0xff, 0]).is_none());
    }

    // Synthetic RSA keys from fixed Mersenne primes (521/607 and 521/127 bits).
    const TEST_N0: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7ffffffffffffffffffffe0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001";
    const TEST_D0: &str = "2a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a5555aaaa5555aaaa5555aa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa01";
    const TEST_N1: &str = "fffffffffffffffffffffffffffffffdffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff80000000000000000000000000000001";
    const TEST_D1: &str = "2a7fd5802a7fd5802a7fd5802a7fd57f80807f7f80807f7f80807f7f80807f7f80807f7f80807f7f80807f7f80807f7f80807f7f80807f7f80807f7f80807f7f8055ffaa0055ffaa0055ffaa0055ffaa01";

    fn unhex(s: &str) -> Vec<u8> {
        s.as_bytes().chunks_exact(2).map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap()).collect()
    }

    fn alg(oid: &[u8]) -> Vec<u8> {
        let mut body = der(6, oid);
        body.extend(der(5, &[]));
        der(0x30, &body)
    }

    fn int(bytes: &[u8]) -> Vec<u8> {
        let mut bytes = bytes.to_vec();
        if bytes[0] & 0x80 != 0 { bytes.insert(0, 0); }
        der(2, &bytes)
    }

    fn test_certificate(subject: &[u8], n: &[u8], issuer_n: &[u8], issuer_d: &[u8]) -> Vec<u8> {
        let mut key = int(n);
        key.extend(int(&[1, 0, 1]));
        let mut bits = vec![0];
        bits.extend(der(0x30, &key));
        let mut spki = alg(RSA_KEY);
        spki.extend(der(3, &bits));
        let mut tbs = der(0xa0, &der(2, &[2]));
        tbs.extend(int(&[1]));
        tbs.extend(alg(SHA256_RSA));
        tbs.extend(name(b"TESTCA"));
        let mut validity = der(0x17, b"250101000000Z");
        validity.extend(der(0x17, b"350101000000Z"));
        tbs.extend(der(0x30, &validity));
        tbs.extend(name(subject));
        tbs.extend(der(0x30, &spki));
        let tbs = der(0x30, &tbs);
        let block = encoded_hash(&tbs, issuer_n.len()).unwrap();
        let mut sig = vec![0];
        sig.extend(modexp(&block, issuer_d, issuer_n).unwrap());
        let mut cert = tbs;
        cert.extend(alg(SHA256_RSA));
        cert.extend(der(3, &sig));
        der(0x30, &cert)
    }

    #[test]
    fn ca_signature_serial_and_leaf_pin_are_independent_checks() {
        let (n, d) = (unhex(TEST_N0), unhex(TEST_D0));
        let ca = test_certificate(b"TESTCA", &n, &n, &d);
        // The leaf has a different public key: only the CA key verifies it.
        let leaf_key = unhex(TEST_N1);
        let leaf = test_certificate(b"TESTSERIAL0001", &leaf_key, &n, &d);
        let pin = fingerprint(&ca);
        let chain = vec![leaf.clone(), ca.clone()];
        assert_eq!(authenticate(&chain, &pin, Some("TESTSERIAL0001")).unwrap(), "TESTSERIAL0001");
        assert_eq!(authenticate(&chain, &pin, None).unwrap(), "TESTSERIAL0001");
        assert!(matches!(authenticate(&chain, &pin, Some("TESTSERIAL0002")), Err(ConnectError::Offline("Printer certificate serial does not match configuration"))));
        assert!(matches!(authenticate(&chain, &CA_FINGERPRINT, Some("TESTSERIAL0001")), Err(ConnectError::Offline(CA_FAILURE))));
        assert!(matches!(authenticate(&[leaf.clone()], &pin, Some("TESTSERIAL0001")), Err(ConnectError::Offline(CA_FAILURE))));

        let mut tampered = leaf.clone();
        let at = tampered.windows(14).position(|s| s == b"TESTSERIAL0001").unwrap();
        tampered[at+13] = b'2';
        // Match the altered CN: failure must come from the signed TBS bytes.
        assert!(matches!(authenticate(&[tampered, ca.clone()], &pin, Some("TESTSERIAL0002")), Err(ConnectError::Offline(CA_FAILURE))));
        let mut bad_signature = leaf.clone();
        *bad_signature.last_mut().unwrap() ^= 1;
        assert!(matches!(authenticate(&[bad_signature, ca.clone()], &pin, Some("TESTSERIAL0001")), Err(ConnectError::Offline(CA_FAILURE))));

        let (wrong_n, wrong_d) = (unhex(TEST_N1), unhex(TEST_D1));
        let wrong_ca = test_certificate(b"TESTCA", &wrong_n, &wrong_n, &wrong_d);
        let wrong_leaf = test_certificate(b"TESTSERIAL0001", &n, &wrong_n, &wrong_d);
        assert!(matches!(authenticate(&[wrong_leaf, ca.clone()], &pin, Some("TESTSERIAL0001")), Err(ConnectError::Offline(CA_FAILURE))));
        assert!(matches!(authenticate(&[leaf.clone(), wrong_ca.clone()], &pin, Some("TESTSERIAL0001")), Err(ConnectError::Offline(CA_FAILURE))));
        // Even pinning a different CA cannot make its key verify this leaf.
        assert!(matches!(authenticate(&[leaf.clone(), wrong_ca.clone()], &fingerprint(&wrong_ca), Some("TESTSERIAL0001")), Err(ConnectError::Offline(CA_FAILURE))));
        for end in 0..leaf.len() {
            assert!(certificate(&leaf[..end]).is_none(), "truncated certificate at {end}");
        }
    }

    #[test]
    fn rsa_modexp_and_pkcs1_padding_are_exact() {
        // Independent textbook RSA vector, plus limb carry and reduction cases.
        assert_eq!(modexp(&[0, 65], &[17], &[0x0c, 0xa1]), Some(vec![0x0a, 0xe6]));
        assert_eq!(modexp(&[0x0a, 0xe6], &[0x0a, 0xc1], &[0x0c, 0xa1]), Some(vec![0, 65]));
        for n in [0xffff_fffbu64, 0x1_0000_0001, u64::MAX - 58] {
            let base = n-2;
            let expected = ((base as u128 * base as u128) % n as u128) as u64;
            // Equal-width unsigned encodings, without a leading zero.
            let bytes = n.to_be_bytes();
            let start = bytes.iter().position(|b| *b != 0).unwrap();
            assert_eq!(modexp(&base.to_be_bytes()[start..], &[2], &bytes[start..]), Some(expected.to_be_bytes()[start..].to_vec()));
        }
        let (n, d) = (unhex(TEST_N0), unhex(TEST_D0));
        let block = encoded_hash(b"fixture", n.len()).unwrap();
        let sig = modexp(&block, &d, &n).unwrap();
        assert_eq!(modexp(&sig, &[1, 0, 1], &n), Some(block.clone()));
        for index in [0, 1, 2, n.len()-33, n.len()-1] {
            let mut malformed = block.clone();
            malformed[index] ^= 1;
            let sig = modexp(&malformed, &d, &n).unwrap();
            assert_ne!(modexp(&sig, &[1, 0, 1], &n), Some(block.clone()));
        }
        assert!(modexp(&n, &[1, 0, 1], &n).is_none());
        assert!(encoded_hash(b"fixture", 61).is_none());
        let mut overlong: &[u8] = &[0x30, 0x81, 0x01, 0];
        assert!(take(&mut overlong, 0x30).is_none());
    }

    #[test]
    fn tofu_pin_change_is_refused_without_replacing_the_original() {
        let dir = std::env::temp_dir().join(format!("fab-printer-pin-test-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let _cleanup = Cleanup(dir.clone());
        let hosts = KnownHosts::new(dir.join("test.known-hosts"));
        pin(&hosts, &[1; 32]).unwrap();
        pin(&hosts, &[1; 32]).unwrap();
        assert!(matches!(pin(&hosts, &[2; 32]), Err(ConnectError::PinMismatch)));
        assert_eq!(hosts.get("printer"), Some([1; 32]));
        std::fs::write(hosts.path(), b"broken pin").unwrap();
        assert!(matches!(pin(&hosts, &[2; 32]), Err(ConnectError::Offline(_))));
    }
}
