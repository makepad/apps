use fab_elec_mcu_netgw::wire::{checksum, transport_checksum};

pub const DEVICE_MAC: [u8; 6] = [2, 0, 0, 0, 0, 2];
pub const GATEWAY_MAC: [u8; 6] = [2, 0, 0, 0, 0, 1];
pub const DEVICE_IP: [u8; 4] = [192, 0, 2, 2];
pub const GATEWAY_IP: [u8; 4] = [192, 0, 2, 1];
pub const ENDPOINT_IP: [u8; 4] = [192, 0, 2, 80];
pub const PORT: u16 = 8080;

pub fn read16(bytes: &[u8], at: usize) -> u16 { u16::from_be_bytes(bytes[at..at + 2].try_into().unwrap()) }
pub fn read32(bytes: &[u8], at: usize) -> u32 { u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) }
pub fn write16(bytes: &mut [u8], at: usize, value: u16) { bytes[at..at + 2].copy_from_slice(&value.to_be_bytes()); }
pub fn write32(bytes: &mut [u8], at: usize, value: u32) { bytes[at..at + 4].copy_from_slice(&value.to_be_bytes()); }

pub fn ethernet(kind: u16, body: &[u8], broadcast: bool) -> Vec<u8> {
    let mut out = if broadcast { vec![255; 6] } else { GATEWAY_MAC.to_vec() };
    out.extend_from_slice(&DEVICE_MAC);
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(body);
    out
}

pub fn ip(src: [u8; 4], dst: [u8; 4], proto: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0; 20];
    out[0] = 0x45;
    out[8] = 64;
    out[9] = proto;
    write16(&mut out, 2, (20 + body.len()) as u16);
    out[12..16].copy_from_slice(&src);
    out[16..20].copy_from_slice(&dst);
    let check = checksum(&out);
    write16(&mut out, 10, check);
    out.extend_from_slice(body);
    ethernet(0x0800, &out, dst == [255; 4])
}

pub fn udp(src: [u8; 4], dst: [u8; 4], ports: (u16, u16), body: &[u8]) -> Vec<u8> {
    let mut out = vec![0; 8];
    write16(&mut out, 0, ports.0);
    write16(&mut out, 2, ports.1);
    write16(&mut out, 4, (8 + body.len()) as u16);
    out.extend_from_slice(body);
    let check = transport_checksum(src, dst, 17, &out);
    write16(&mut out, 6, if check == 0 { 65535 } else { check });
    ip(src, dst, 17, &out)
}

pub fn arp() -> Vec<u8> {
    let mut body = vec![0, 1, 8, 0, 6, 4, 0, 1];
    body.extend_from_slice(&DEVICE_MAC);
    body.extend_from_slice(&DEVICE_IP);
    body.extend_from_slice(&[0; 6]);
    body.extend_from_slice(&GATEWAY_IP);
    ethernet(0x0806, &body, true)
}

pub fn ping() -> Vec<u8> {
    let mut body = vec![8, 0, 0, 0, 0, 7, 0, 1, 9, 8, 7];
    let check = checksum(&body);
    write16(&mut body, 2, check);
    ip(DEVICE_IP, GATEWAY_IP, 1, &body)
}

pub fn dhcp(kind: u8, requested: Option<[u8; 4]>, ciaddr: [u8; 4]) -> Vec<u8> {
    let mut body = vec![0; 240];
    body[..3].copy_from_slice(&[1, 1, 6]);
    write32(&mut body, 4, 0x10203040);
    write16(&mut body, 10, 0x8000);
    body[12..16].copy_from_slice(&ciaddr);
    body[28..34].copy_from_slice(&DEVICE_MAC);
    body[236..240].copy_from_slice(&[99, 130, 83, 99]);
    body.extend_from_slice(&[53, 1, kind]);
    if let Some(requested) = requested {
        body.extend_from_slice(&[50, 4]);
        body.extend_from_slice(&requested);
        body.extend_from_slice(&[54, 4]);
        body.extend_from_slice(&GATEWAY_IP);
    }
    body.push(255);
    udp(ciaddr, [255; 4], (68, 67), &body)
}

pub fn dhcp_option(body: &[u8], option: u8) -> Option<&[u8]> {
    let mut at = 240;
    while at < body.len() {
        let kind = body[at];
        at += 1;
        if kind == 255 { break; }
        if kind == 0 { continue; }
        let len = usize::from(*body.get(at)?);
        at += 1;
        let value = body.get(at..at + len)?;
        if kind == option { return Some(value); }
        at += len;
    }
    None
}

pub fn dns(name: &str, kind: u16) -> Vec<u8> {
    let mut body = vec![0; 12];
    write16(&mut body, 0, 0x1234);
    write16(&mut body, 2, 0x0100);
    write16(&mut body, 4, 1);
    for label in name.split('.') { body.push(label.len() as u8); body.extend_from_slice(label.as_bytes()); }
    body.push(0);
    body.extend_from_slice(&kind.to_be_bytes());
    body.extend_from_slice(&1u16.to_be_bytes());
    udp(DEVICE_IP, GATEWAY_IP, (53000, 53), &body)
}

pub fn payload(frame: &[u8], proto: u8) -> Option<&[u8]> {
    if frame.len() < 34 || read16(frame, 12) != 0x0800 || frame[23] != proto { return None; }
    let ip = &frame[14..];
    assert_eq!(ip[0], 0x45, "gateway sends IPv4 without options");
    assert_eq!(checksum(&ip[..20]), 0, "gateway IPv4 checksum is valid");
    let end = usize::from(read16(ip, 2));
    assert!(end <= ip.len(), "gateway IP length fits Ethernet payload");
    let body = &ip[20..end];
    if matches!(proto, 6 | 17) {
        assert_eq!(transport_checksum(ip[12..16].try_into().unwrap(), ip[16..20].try_into().unwrap(), proto, body), 0,
            "gateway transport checksum is valid");
    }
    Some(body)
}

pub fn udp_payload(frame: &[u8], port: u16) -> Option<&[u8]> {
    let data = payload(frame, 17)?;
    if read16(data, 0) != port { return None; }
    assert_eq!(usize::from(read16(data, 4)), data.len(), "UDP length matches IP payload");
    Some(&data[8..])
}

pub fn tcp(port: u16, remote: u16, seq: u32, ack: u32, flags: u8, window: u16, data: &[u8]) -> Vec<u8> {
    let mut body = vec![0; if flags & 2 != 0 { 24 } else { 20 }];
    write16(&mut body, 0, port);
    write16(&mut body, 2, remote);
    write32(&mut body, 4, seq);
    write32(&mut body, 8, ack);
    body[12] = ((body.len() / 4) as u8) << 4;
    body[13] = flags;
    write16(&mut body, 14, window);
    // Deliberately different from the gateway MSS; both directions test it.
    if flags & 2 != 0 { body[20..24].copy_from_slice(&[2, 4, 4, 176]); }
    body.extend_from_slice(data);
    let check = transport_checksum(DEVICE_IP, ENDPOINT_IP, 6, &body);
    write16(&mut body, 16, check);
    ip(DEVICE_IP, ENDPOINT_IP, 6, &body)
}

pub fn ws(opcode: u8, fin: bool, masked: bool, data: &[u8]) -> Vec<u8> {
    let mut frame = vec![opcode | if fin { 128 } else { 0 }];
    let bit = if masked { 128 } else { 0 };
    if data.len() < 126 { frame.push(bit | data.len() as u8); }
    else if data.len() <= 65535 { frame.push(bit | 126); frame.extend_from_slice(&(data.len() as u16).to_be_bytes()); }
    else { frame.push(bit | 127); frame.extend_from_slice(&(data.len() as u64).to_be_bytes()); }
    if masked {
        let mask = [0x37, 0xfa, 0x21, 0x3d];
        frame.extend_from_slice(&mask);
        frame.extend(data.iter().enumerate().map(|(i, &b)| b ^ mask[i & 3]));
    } else { frame.extend_from_slice(data); }
    frame
}

pub fn upgrade() -> Vec<u8> {
    b"GET /voice?stream=1 HTTP/1.1\r\nHost: 192.0.2.80\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n".to_vec()
}
