//! Strict GNU ar framing; the symbol index is deliberately not used for extraction.

use crate::{bytes, Error, Location};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Member<'a> {
    pub name: String,
    pub data: &'a [u8],
}

fn number(field: &[u8], radix: u32, optional: bool, at: &Location, what: &str) -> Result<usize, Error> {
    let value = std::str::from_utf8(field).map_err(|_| at.error(format!("invalid {what}")))?.trim_matches(' ');
    if optional && value.is_empty() { return Ok(0); }
    if value.is_empty() || !value.chars().all(|c| c.is_digit(radix)) {
        return Err(at.error(format!("invalid {what}: {value:?}")));
    }
    usize::from_str_radix(value, radix).map_err(|_| at.error(format!("{what} overflow: {value}")))
}

fn name(value: &[u8], at: &Location) -> Result<String, Error> {
    let value = std::str::from_utf8(value).map_err(|_| at.error("non-UTF-8 member name"))?;
    if value.is_empty() || value.chars().any(|c| c.is_control()) || value.ends_with('/') {
        return Err(at.error(format!("malformed member name {value:?}")));
    }
    Ok(value.into())
}

fn name_table(table: &[u8], at: &Location) -> Result<BTreeMap<usize, String>, Error> {
    let mut names = BTreeMap::new();
    let mut start = 0;
    while start < table.len() {
        let tail = &table[start..];
        // GNU ar may count its final alignment newline inside the // member size.
        if tail == b"\n" && start % 2 == 1 { break; }
        let length = tail.windows(2).position(|w| w == b"/\n")
            .ok_or_else(|| at.error("unterminated long name"))?;
        names.insert(start, name(&tail[..length], at)?);
        start += length + 2; // bounded by the terminator found in table
    }
    Ok(names)
}

fn resolve_name(token: &str, names: &BTreeMap<usize, String>, at: &Location) -> Result<String, Error> {
    if let Some(offset) = token.strip_prefix('/') {
        let offset = number(offset.as_bytes(), 10, false, at, "long-name offset")?;
        names.get(&offset).cloned().ok_or_else(|| at.error(format!("invalid long-name offset {offset}")))
    } else {
        let short = token.strip_suffix('/').ok_or_else(|| at.error("short name lacks trailing /"))?;
        if short.contains('/') || short.contains(' ') { return Err(at.error("malformed short name")); }
        name(short.as_bytes(), at)
    }
}

pub fn parse<'a>(archive: &str, data: &'a [u8]) -> Result<Vec<Member<'a>>, Error> {
    let header = Location::new(archive, "<archive>", "<ar-header>");
    if data.starts_with(b"!<thin>\n") {
        return Err(header.error("thin archive refused"));
    }
    if bytes(data, 0, 8, &header)? != b"!<arch>\n" {
        return Err(header.error("invalid GNU ar magic"));
    }
    let mut raw = Vec::new();
    let mut long_names: Option<BTreeMap<usize, String>> = None;
    let mut pos = 8usize;
    while pos < data.len() {
        let at = Location::new(archive, &format!("<header at 0x{pos:x}>"), "<ar-header>");
        let h = bytes(data, pos, 60, &at)?;
        let token = std::str::from_utf8(&h[..16]).map_err(|_| at.error("invalid name field"))?.trim_end_matches(' ');
        let mut at = Location::new(archive, token, "<ar-member>");
        if token.starts_with("#1/") { return Err(at.error("BSD #1/ member name refused")); }
        if token != "/" && token != "//" {
            if let Some(names) = &long_names { at.member = resolve_name(token, names, &at)?; }
            else if !token.starts_with('/') { at.member = resolve_name(token, &BTreeMap::new(), &at)?; }
        }
        if &h[58..60] != b"`\n" { return Err(at.error("malformed ar header trailer")); }
        number(&h[16..28], 10, true, &at, "timestamp")?;
        number(&h[28..34], 10, true, &at, "uid")?;
        number(&h[34..40], 10, true, &at, "gid")?;
        number(&h[40..48], 8, true, &at, "mode")?;
        let size = number(&h[48..58], 10, false, &at, "member size")?;
        let start = pos.checked_add(60).ok_or_else(|| at.error("header offset overflow"))?;
        let body = bytes(data, start, size, &at)?;
        match token {
            "/" => {},
            "//" => {
                if long_names.is_some() { return Err(at.error("duplicate // name table")); }
                long_names = Some(name_table(body, &at)?);
            }
            _ => raw.push((token.to_string(), body)),
        }
        pos = start.checked_add(size).ok_or_else(|| at.error("member end overflow"))?;
        if size % 2 != 0 {
            if bytes(data, pos, 1, &at)? != b"\n" { return Err(at.error("invalid ar padding byte")); }
            pos = pos.checked_add(1).ok_or_else(|| at.error("padding offset overflow"))?;
        }
    }
    let names = long_names.unwrap_or_default();
    raw.into_iter().map(|(token, body)| {
        let at = Location::new(archive, &token, "<ar-member>");
        let resolved = resolve_name(&token, &names, &at)?;
        Ok(Member { name: resolved, data: body })
    }).collect()
}
