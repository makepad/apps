//! Internet checksums and private wire encoding primitives.

pub fn checksum(bytes: &[u8]) -> u16 { finish(sum(bytes)) }

pub fn transport_checksum(src: [u8; 4], dst: [u8; 4], protocol: u8, bytes: &[u8]) -> u16 {
    finish(sum(&src) + sum(&dst) + u64::from(protocol) + bytes.len() as u64 + sum(bytes))
}

fn sum(bytes: &[u8]) -> u64 {
    let mut words = bytes.chunks_exact(2);
    let mut sum = words.by_ref().map(|v| u64::from(u16::from_be_bytes([v[0], v[1]]))).sum::<u64>();
    if let Some(&byte) = words.remainder().first() { sum += u64::from(byte) << 8; }
    sum
}

fn finish(mut sum: u64) -> u16 {
    while sum >> 16 != 0 { sum = (sum & 0xffff) + (sum >> 16); }
    !(sum as u16)
}

pub(crate) fn u16_at(b: &[u8], n: usize) -> u16 { u16::from_be_bytes([b[n], b[n + 1]]) }
pub(crate) fn u32_at(b: &[u8], n: usize) -> u32 { u32::from_be_bytes(b[n..n + 4].try_into().unwrap()) }
pub(crate) fn put16(b: &mut [u8], n: usize, v: u16) { b[n..n + 2].copy_from_slice(&v.to_be_bytes()); }
pub(crate) fn put32(b: &mut [u8], n: usize, v: u32) { b[n..n + 4].copy_from_slice(&v.to_be_bytes()); }

pub(crate) fn ethernet(src: [u8; 6], dst: [u8; 6], kind: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.extend_from_slice(&dst);
    frame.extend_from_slice(&src);
    frame.extend_from_slice(&kind.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

pub(crate) fn ipv4(src: [u8; 4], dst: [u8; 4], protocol: u8, payload: &[u8]) -> Vec<u8> {
    let mut packet = vec![0; 20];
    packet[0] = 0x45;
    put16(&mut packet, 2, (20 + payload.len()) as u16);
    packet[8] = 64;
    packet[9] = protocol;
    packet[12..16].copy_from_slice(&src);
    packet[16..20].copy_from_slice(&dst);
    let check = checksum(&packet);
    put16(&mut packet, 10, check);
    packet.extend_from_slice(payload);
    packet
}
