use crate::wire::{ethernet, put16, put32, transport_checksum, u16_at};
use crate::Gateway;

impl Gateway {
    pub fn learned_device_mac(&self) -> Option<[u8; 6]> { self.device_mac }

    pub(crate) fn arp(&mut self, mac: [u8; 6], bytes: &[u8]) {
        if bytes.len() < 28 || bytes[..6] != [0, 1, 8, 0, 6, 4] || bytes[8..14] != mac {
            self.stats.arp_malformed += 1; return;
        }
        if u16_at(bytes, 6) != 1 || bytes[24..28] != self.config.gateway_ip { self.stats.arp_ignored += 1; return; }
        let mut reply = vec![0, 1, 8, 0, 6, 4, 0, 2];
        reply.extend_from_slice(&self.config.gateway_mac);
        reply.extend_from_slice(&self.config.gateway_ip);
        reply.extend_from_slice(&mac);
        reply.extend_from_slice(&bytes[14..18]);
        self.stats.arp_replies += 1;
        self.send(ethernet(self.config.gateway_mac, mac, 0x0806, &reply));
    }

    pub(crate) fn udp(&mut self, mac: [u8; 6], src: [u8; 4], dst: [u8; 4], bytes: &[u8]) {
        if bytes.len() < 8 || usize::from(u16_at(bytes, 4)) != bytes.len() { self.stats.udp_malformed += 1; return; }
        if u16_at(bytes, 6) != 0 && transport_checksum(src, dst, 17, bytes) != 0 { self.stats.udp_checksum += 1; return; }
        let port = u16_at(bytes, 2);
        if dst != self.config.gateway_ip && !(port == 67 && dst == [255; 4]) { self.stats.ip_destination += 1; return; }
        match port {
            67 if u16_at(bytes, 0) == 68 => self.dhcp(mac, &bytes[8..]),
            53 if dst == self.config.gateway_ip => {
                if let Some(reply) = dns(&bytes[8..], &self.config.dns, &mut self.stats) {
                    self.send_udp(mac, src, 53, u16_at(bytes, 0), &reply);
                } else { self.stats.dns_malformed += 1; }
            }
            _ => self.stats.udp_port += 1,
        }
    }

    fn send_udp(&mut self, mac: [u8; 6], ip: [u8; 4], src_port: u16, dst_port: u16, data: &[u8]) {
        let mut bytes = vec![0; 8];
        put16(&mut bytes, 0, src_port);
        put16(&mut bytes, 2, dst_port);
        put16(&mut bytes, 4, (8 + data.len()) as u16);
        bytes.extend_from_slice(data);
        let check = transport_checksum(self.config.gateway_ip, ip, 17, &bytes);
        put16(&mut bytes, 6, if check == 0 { 0xffff } else { check });
        self.send_ip(mac, self.config.gateway_ip, ip, 17, &bytes);
    }

    fn dhcp(&mut self, mac: [u8; 6], data: &[u8]) {
        if data.len() < 240 || data[..3] != [1, 1, 6] || data[236..240] != [99, 130, 83, 99]
            || data[28..34] != mac {
            self.stats.dhcp_malformed += 1; return;
        }
        if data[24..28] != [0; 4] { self.stats.dhcp_unsupported += 1; return; }
        let mut options = DhcpOptions::default();
        if !options.parse(&data[240..])
            || (options.overload & 1 != 0 && !options.parse(&data[108..236]))
            || (options.overload & 2 != 0 && !options.parse(&data[44..108])) {
            self.stats.dhcp_malformed += 1; return;
        }
        if options.server.is_some_and(|ip| ip != self.config.gateway_ip) { self.stats.dhcp_other_server += 1; return; }
        let ciaddr: [u8; 4] = data[12..16].try_into().unwrap();
        let kind = match options.kind {
            Some(1) => { self.stats.dhcp_offers += 1; 2 }
            Some(3) => {
                let requested = options.requested.unwrap_or(ciaddr);
                if requested == self.config.device_ip {
                    self.stats.dhcp_acks += 1;
                    self.lease_until = Some(self.now.saturating_add(u64::from(self.config.lease_secs) * 1000));
                    5
                } else { self.stats.dhcp_naks += 1; 6 }
            }
            Some(7) if ciaddr == self.config.device_ip => {
                self.lease_until = None;
                self.stats.dhcp_releases += 1;
                return;
            }
            None => { self.stats.dhcp_malformed += 1; return; }
            _ => { self.stats.dhcp_unsupported += 1; return; }
        };
        let mut reply = vec![0; 240];
        reply[..3].copy_from_slice(&[2, 1, 6]);
        reply[4..8].copy_from_slice(&data[4..8]);
        reply[10..12].copy_from_slice(&data[10..12]);
        reply[28..44].copy_from_slice(&data[28..44]);
        reply[236..240].copy_from_slice(&[99, 130, 83, 99]);
        option(&mut reply, 53, &[kind]);
        option(&mut reply, 54, &self.config.gateway_ip);
        if kind != 6 {
            reply[12..16].copy_from_slice(&ciaddr);
            reply[16..20].copy_from_slice(&self.config.device_ip);
            option(&mut reply, 51, &self.config.lease_secs.to_be_bytes());
            option(&mut reply, 58, &(self.config.lease_secs / 2).to_be_bytes());
            option(&mut reply, 59, &((u64::from(self.config.lease_secs) * 7 / 8) as u32).to_be_bytes());
            let mask = if self.config.prefix_len == 0 { 0 } else { u32::MAX << (32 - self.config.prefix_len) };
            option(&mut reply, 1, &mask.to_be_bytes());
            option(&mut reply, 3, &self.config.gateway_ip);
            option(&mut reply, 6, &self.config.gateway_ip);
        }
        reply.push(255);
        reply.resize(reply.len().max(300), 0);
        let broadcast = kind == 6 || u16_at(data, 10) & 0x8000 != 0;
        let target = if broadcast { [255; 4] } else if ciaddr != [0; 4] { ciaddr } else { self.config.device_ip };
        self.send_udp(if broadcast { [255; 6] } else { mac }, target, 67, 68, &reply);
    }
}

fn option(out: &mut Vec<u8>, kind: u8, data: &[u8]) {
    out.extend_from_slice(&[kind, data.len() as u8]);
    out.extend_from_slice(data);
}

#[derive(Default)]
struct DhcpOptions {
    kind: Option<u8>, requested: Option<[u8; 4]>, server: Option<[u8; 4]>, overload: u8,
}

impl DhcpOptions {
    fn parse(&mut self, mut data: &[u8]) -> bool {
        while let Some((&kind, rest)) = data.split_first() {
            data = rest;
            if kind == 255 { return true; }
            if kind == 0 { continue; }
            let Some((&len, rest)) = data.split_first() else { return false; };
            if rest.len() < usize::from(len) { return false; }
            let value = &rest[..usize::from(len)];
            data = &rest[usize::from(len)..];
            match kind {
                53 if len == 1 && self.kind.is_none() => self.kind = Some(value[0]),
                50 if len == 4 && self.requested.is_none() => self.requested = Some(value.try_into().unwrap()),
                54 if len == 4 && self.server.is_none() => self.server = Some(value.try_into().unwrap()),
                52 if len == 1 && self.overload == 0 && matches!(value[0], 1..=3) => self.overload = value[0],
                50 | 52 | 53 | 54 => return false,
                _ => {},
            }
        }
        false
    }
}

// Compression pointers must point backwards; the visited set also catches
// cycles involving an earlier label followed by a pointer back to that label.
fn name(data: &[u8], start: usize) -> Option<(Vec<Vec<u8>>, usize)> {
    let mut labels = Vec::new();
    let mut at = start;
    let mut end = None;
    let mut size = 1;
    let mut seen = vec![false; data.len()];
    loop {
        let len = *data.get(at)?;
        if seen[at] { return None; }
        seen[at] = true;
        if len & 0xc0 == 0xc0 {
            let target = (usize::from(len & 63) << 8) | usize::from(*data.get(at + 1)?);
            if target < 12 || target >= at { return None; }
            end.get_or_insert(at + 2);
            at = target;
        } else if len & 0xc0 != 0 { return None; }
        else if len == 0 { return Some((labels, end.unwrap_or(at + 1))); }
        else {
            let next = at + 1 + usize::from(len);
            let label = data.get(at + 1..next)?.to_vec();
            size += label.len() + 1;
            if size > 255 { return None; }
            labels.push(label);
            at = next;
        }
    }
}

fn dns(data: &[u8], configured: &[(String, [u8; 4])], stats: &mut crate::Stats) -> Option<Vec<u8>> {
    if data.len() < 12 || u16_at(data, 2) & !0x0130 != 0 || u16_at(data, 4) != 1
        || u16_at(data, 6) != 0 || u16_at(data, 8) != 0 { return None; }
    let (labels, end) = name(data, 12)?;
    let question = data.get(end..end + 4)?;
    let mut at = end + 4;
    for _ in 0..u16_at(data, 10) {
        let (_, next) = name(data, at)?;
        let rr = data.get(next..next + 10)?;
        at = next.checked_add(10 + usize::from(u16_at(rr, 8)))?;
        data.get(..at)?;
    }
    if at != data.len() { return None; }
    let query = labels.join(&b'.');
    let found = configured.iter().find(|(n, _)| n.trim_end_matches('.').as_bytes().eq_ignore_ascii_case(&query));
    let answer = found.is_some() && u16_at(question, 0) == 1 && matches!(u16_at(question, 2), 1 | 255);
    let mut out = vec![0; 12];
    out[..2].copy_from_slice(&data[..2]);
    put16(&mut out, 2, 0x8400 | (u16_at(data, 2) & 0x0100) | if found.is_some() { 0 } else { 3 });
    put16(&mut out, 4, 1);
    put16(&mut out, 6, u16::from(answer));
    for label in labels { out.push(label.len() as u8); out.extend_from_slice(&label); }
    out.push(0);
    out.extend_from_slice(question);
    if answer {
        stats.dns_answers += 1;
        let offset = out.len();
        out.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 0, 0, 4]);
        put32(&mut out, offset + 6, 60);
        out.extend_from_slice(&found.unwrap().1);
    } else if found.is_some() { stats.dns_no_data += 1; }
    else { stats.dns_nxdomain += 1; }
    Some(out)
}
