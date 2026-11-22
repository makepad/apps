//! IPC-D-356A test points (S46.1): 80-column ASCII records, decimils and degrees.
//! Coordinates are relative to the caller's drill/place origin, with file Y pointing up.

use crate::error::CamError;
use fab_elec_board::{Board, RuleSet, Tent};
use fab_elec_circuit::Circuit;
use fab_elec_core::{Angle, LayerId, LayerSet, NetId, Nm, PadIx, Side, P};
use fab_elec_parts::{PadKind, PadShape};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct IpcOpts {
    /// Board file name without its folder or extension.
    pub job: String,
    /// Drill/place (auxiliary) origin. The board model does not yet store this setting.
    pub origin: P,
    /// GUI option only; job/command-line exports leave this false (S46.R1/R2).
    pub skip_unconnected: bool,
}

/// Nanometres in board axes, relative to the auxiliary origin. The IPC range
/// (999999 decimils) exceeds `Nm`, so reading it must not narrow to 32 bits.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct IpcPosition {
    pub x: i64,
    pub y: i64,
}

impl IpcPosition {
    /// Restore the board position when it fits the board model's coordinate range.
    pub fn board_at(self, origin: P) -> Option<P> {
        Some(P::new(
            Nm::try_from(self.x.checked_add(origin.x as i64)?).ok()?,
            Nm::try_from(self.y.checked_add(origin.y as i64)?).ok()?,
        ))
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IpcPoint {
    /// Empty for N/C; otherwise the canonical name or the sanitized NNAME tail.
    pub net: String,
    pub net_aliased: bool,
    pub refdes: String,
    pub pin: String,
    pub at: IpcPosition,
    pub drill: Nm,
    pub plated: bool,
    /// 0 = both surfaces; otherwise a physical copper layer number. A file alone
    /// cannot distinguish back-only from inner-only access without the board stack.
    pub access: u8,
    pub layer_span: Option<(u8, u8)>,
    pub midpoint: bool,
    /// Unrotated sizes; the second is zero for round features or absent sizes.
    pub size: (Nm, Nm),
    pub rot_deg: u16,
    /// True means covered by solder mask, false means an opening.
    pub mask_top: bool,
    pub mask_bottom: bool,
    pub smd: bool,
}

impl IpcPoint {
    /// Match against a source board net using S46.R26, including alias tail truncation.
    pub fn matches_net(&self, source: &str) -> bool {
        if self.net_aliased {
            self.net == name_tail(source)
        } else if self.net.is_empty() {
            source.is_empty()
        } else {
            !source.is_empty() && self.net == canonical(source)
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct IpcFile {
    pub job: String,
    pub points: Vec<IpcPoint>,
}

/// Write every copper pad and via, including unconnected points. Standalone board
/// holes are represented as unnumbered through pads after the footprint pads.
/// Work is per export: O(points log points) for aliases and the stable signal sort.
/// The circuit is unused: the board owns the manufacturing geometry and nets.
pub fn write_ipc356(board: &Board, _circuit: &Circuit, opts: &IpcOpts) -> Vec<u8> {
    let layers = board.stackup.copper();
    let copper = LayerSet::of(&layers);
    let count = layers.len() as u8;
    let physical = |layer| layers.iter().position(|l| *l == layer).map(|i| (i + 1) as u8)
        .expect("IPC point must use a copper layer in the stackup");
    let net_name = |id: Option<NetId>| {
        id.and_then(|id| board.nets.get(id.ix())).map_or("", |net| net.name.as_str()).to_owned()
    };
    let position = |at: P| IpcPosition {
        x: at.x as i64 - opts.origin.x as i64,
        y: at.y as i64 - opts.origin.y as i64,
    };
    let mut points = Vec::new();
    // S46.R21: collect vias first; the later signal-name sort is stable.
    for via in &board.vias {
        let a = physical(via.span.0);
        let b = physical(via.span.1);
        let span = (a.min(b), a.max(b));
        let access = if span == (1, count) { 0 } else if span.0 == 1 { 1 }
            else if span.1 == count { count } else { span.0 };
        let default_drill = || {
            let class = board.nets.get(via.net.ix()).map_or(fab_elec_core::ClassId(0), |net| net.class);
            board.classes.iter().find(|c| c.id == class).and_then(|c| c.rules.via)
                .or(board.rules.base.via).or(RuleSet::standard().via).map_or(0, |v| v.drill)
        };
        // Via currently stores one diameter and an explicit tenting value, not a
        // per-layer padstack or inherited board tenting setting (S46.R15/R18).
        points.push(IpcPoint {
            net: net_name(Some(via.net)), net_aliased: false, refdes: "VIA".into(), pin: String::new(),
            at: position(via.at), drill: if via.drill > 0 { via.drill } else { default_drill() },
            plated: true, access, layer_span: (access != 0).then_some(span), midpoint: true,
            size: (via.pad, 0), rot_deg: 0,
            mask_top: matches!(via.tent, Tent::Both | Tent::Front),
            mask_bottom: matches!(via.tent, Tent::Both | Tent::Back), smd: false,
        });
    }
    for placed in &board.placed {
        for (i, pad) in placed.footprint.pads.iter().enumerate() {
            let ix = PadIx(i as u16);
            let on = placed.pad_layers(ix, copper, count);
            if on.is_empty() { continue; }
            let net = net_name(placed.pad_nets.get(i).copied().flatten());
            if opts.skip_unconnected && net.is_empty() { continue; }
            let access = match (on.has(LayerId::F_CU), on.has(LayerId::B_CU)) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => count,
                (false, false) => physical(*layers.iter().find(|l| on.has(**l)).expect("nonempty copper")),
            };
            // Stacks are in footprint coordinates. The placed component's outer
            // side maps back to the stack's front even when the component is flipped.
            let outer = placed.fp_layer(LayerId::F_CU.on(placed.side), count);
            let shape = if outer == LayerId::B_CU { pad.stack.back.as_ref().unwrap_or(&pad.stack.front) }
                else { &pad.stack.front };
            let rot = placed.rot.add(if placed.side == Side::Bottom { pad.rot.neg() } else { pad.rot });
            points.push(IpcPoint {
                net, net_aliased: false, refdes: placed.refdes.clone(), pin: pad.no.clone(),
                at: position(placed.pad_at(ix).expect("existing pad")),
                drill: pad.drill.as_ref().map_or(0, |d| d.size.x.min(d.size.y)),
                plated: pad.kind != PadKind::Npth, access, layer_span: None, midpoint: pad.no.is_empty(),
                size: pad_size(shape), rot_deg: rotation(rot),
                mask_top: !pad.stack.tech.has(placed.fp_layer(LayerId::F_MASK, count)),
                mask_bottom: !pad.stack.tech.has(placed.fp_layer(LayerId::B_MASK, count)),
                smd: matches!(pad.kind, PadKind::Smd | PadKind::Edge),
            });
        }
    }
    for hole in &board.holes {
        let net = net_name(hole.net);
        if opts.skip_unconnected && net.is_empty() { continue; }
        points.push(IpcPoint {
            net, net_aliased: false, refdes: hole.name.clone(), pin: String::new(), at: position(hole.at),
            drill: hole.drill, plated: hole.plated, access: 0, layer_span: None, midpoint: true,
            size: (hole.pad.unwrap_or(hole.drill), 0), rot_deg: 0,
            // Standalone holes have no mask-layer membership in the board model.
            mask_top: true, mask_bottom: true, smd: false,
        });
    }

    let names: BTreeSet<_> = points.iter().filter(|p| !p.net.is_empty()).map(|p| p.net.clone()).collect();
    let mut occurrences = BTreeMap::<String, usize>::new();
    for name in &names { *occurrences.entry(canonical(name)).or_default() += 1; }
    let mut reserved: BTreeSet<_> = occurrences.keys().cloned().collect();
    reserved.insert("N/C".into());
    let mut signals = BTreeMap::new();
    let mut aliases = Vec::new();
    let mut next = 0;
    for name in names {
        let canon = canonical(&name);
        let signal = if canon.len() <= 14 && canon != "N/C" && occurrences[&canon] == 1 { canon }
            else {
                let alias = loop {
                    let candidate = alias_name(next);
                    next += 1;
                    if reserved.insert(candidate.clone()) { break candidate; }
                };
                aliases.push((alias.clone(), name_tail(&name)));
                alias
            };
        signals.insert(name, signal);
    }
    for point in &mut points {
        point.net = if point.net.is_empty() { "N/C".into() } else { signals[&point.net].clone() };
    }
    points.sort_by(|a, b| a.net.cmp(&b.net));

    let mut out = Vec::new();
    header(&mut out, &format!("P  JOB   {}", sanitize(&opts.job)));
    for text in ["P  CODE  00", "P  UNITS CUST 0", "P  arrayDim   N", "P  VER   IPC-D-356A", "P  IMAGE PRIMARY"] {
        header(&mut out, text);
    }
    for (alias, tail) in aliases { header(&mut out, &format!("P  NNAME{alias} {tail}")); }
    for point in &points {
        let code = if point.layer_span.is_some() { "307" } else if point.smd { "327" }
            else if point.plated { "317" } else { "367" };
        write_point(&mut out, point, code);
        if point.layer_span.is_some_and(|(a, b)| a == 1 || b == count) {
            write_point(&mut out, point, "027");
        }
    }
    header(&mut out, "999");
    out
}

fn pad_size(shape: &PadShape) -> (Nm, Nm) {
    match shape {
        PadShape::Circle { d } => (*d, 0),
        PadShape::Rect { size } | PadShape::RoundRect { size, .. } | PadShape::Chamfer { size, .. }
        | PadShape::Oval { size } | PadShape::Trapezoid { size, .. } => (size.x, size.y),
        PadShape::Custom { anchor, .. } => pad_size(anchor),
    }
}

fn rotation(angle: Angle) -> u16 {
    let neg = -angle.udeg() / 1_000_000;
    (if neg < 0 { neg + 360 } else { neg }) as u16
}

fn sanitize(text: &str) -> String {
    text.chars().map(|c| if ('!'..='~').contains(&c) { c } else { '?' }).collect()
}

fn canonical(text: &str) -> String { sanitize(text).to_ascii_uppercase() }

fn name_tail(text: &str) -> String {
    let name = sanitize(text);
    name[name.len().saturating_sub(58)..].into()
}

fn alias_name(mut index: usize) -> String {
    assert!(index < 36usize.pow(4), "IPC-D-356 aliases exhausted");
    let mut alias = *b"M0000";
    for digit in alias[1..].iter_mut().rev() {
        *digit = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"[index % 36];
        index /= 36;
    }
    String::from_utf8(alias.to_vec()).expect("ASCII alias")
}

fn header(out: &mut Vec<u8>, text: &str) {
    let mut line = [b' '; 80];
    field(&mut line[..72], text);
    emit(out, &line);
}

fn field(dest: &mut [u8], text: &str) {
    let n = dest.len().min(text.len());
    dest[..n].copy_from_slice(&text.as_bytes()[..n]);
}

fn emit(out: &mut Vec<u8>, line: &[u8; 80]) {
    out.extend_from_slice(line);
    out.push(b'\n');
}

fn number(dest: &mut [u8], value: i64, step: i64, signed: bool) {
    let digits = dest.len() - usize::from(signed);
    let limit = 10u64.pow(digits as u32) - 1;
    let rounded = ((value.unsigned_abs() + step as u64 / 2) / step as u64).min(limit);
    let magnitude = if signed || value >= 0 { rounded } else { 0 };
    let start = usize::from(signed);
    if signed { dest[0] = if value < 0 && magnitude != 0 { b'-' } else { b'+' }; }
    dest[start..].copy_from_slice(format!("{magnitude:0digits$}").as_bytes());
}

fn write_point(out: &mut Vec<u8>, point: &IpcPoint, code: &str) {
    let mut line = [b' '; 80];
    field(&mut line[..3], code);
    field(&mut line[3..17], &point.net);
    field(&mut line[20..26], &sanitize(&point.refdes));
    if !point.pin.is_empty() {
        line[26] = b'-';
        field(&mut line[27..31], &sanitize(&point.pin));
    }
    if code != "027" {
        if point.midpoint { line[31] = b'M'; }
        if point.drill > 0 {
            line[32] = b'D';
            number(&mut line[33..37], point.drill as i64, 2540, false);
            line[37] = if point.plated { b'P' } else { b'U' };
        }
    }
    line[38] = b'A';
    number(&mut line[39..41], point.access as i64, 1, false);
    line[41] = b'X';
    number(&mut line[42..49], point.at.x, 2540, true);
    line[49] = b'Y';
    number(&mut line[50..57], -point.at.y, 2540, true);
    if code != "307" {
        line[57] = b'X';
        number(&mut line[58..62], point.size.0 as i64, 2540, false);
        line[62] = b'Y';
        number(&mut line[63..67], point.size.1 as i64, 2540, false);
        line[67] = b'R';
        number(&mut line[68..71], point.rot_deg as i64, 1, false);
    }
    line[72] = b'S';
    line[73] = b'0' + u8::from(point.mask_top) + 2 * u8::from(point.mask_bottom);
    if code == "307" {
        let (a, b) = point.layer_span.expect("blind/buried span");
        line[74] = b'L';
        number(&mut line[75..77], a as i64, 1, false);
        line[77] = b'L';
        number(&mut line[78..80], b as i64, 1, false);
    }
    emit(out, &line);
}

/// Read S46.R23-R27. Missing UNITS means decimils. Positions are returned in
/// board axes relative to the auxiliary origin; use `at.board_at(origin)` to restore it.
pub fn read_ipc356(bytes: &[u8]) -> Result<IpcFile, CamError> {
    let mut file = IpcFile::default();
    let mut aliases = BTreeMap::<String, String>::new();
    let mut pending = None;
    let mut last_line = 0;
    for (i, raw) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
        let at = i + 1;
        last_line = at;
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let mut line = [b' '; 80];
        let n = raw.len().min(80);
        line[..n].copy_from_slice(&raw[..n]);
        if &line[..3] == b"999" {
            for point in &mut file.points {
                if point.net == "N/C" { point.net.clear(); }
                else if let Some(full) = aliases.get(&point.net) {
                    point.net = full.clone();
                    point.net_aliased = true;
                } else { point.net.make_ascii_uppercase(); }
            }
            return Ok(file);
        }
        match line[0] {
            b'C' => { pending = None; }
            b'P' => {
                pending = None;
                if line[3..8].eq_ignore_ascii_case(b"NNAME") {
                    let alias = text(&line[8..13]);
                    let name = text(&line[14..80]);
                    if alias.len() != 5 || name.is_empty() || aliases.contains_key(&alias) {
                        return Err(CamError::parse(at, "invalid or duplicate NNAME"));
                    }
                    aliases.insert(alias, name);
                    continue;
                }
                let parameter = String::from_utf8_lossy(&line[3..]);
                let (key, value) = parameter.split_once(' ').unwrap_or((&parameter, ""));
                let value = value.trim();
                match key.to_ascii_uppercase().as_str() {
                    "JOB" => file.job = value.into(),
                    "UNITS" if value != "CUST 0" => {
                        return Err(CamError::unsupported(at, format!("IPC-D-356 UNITS {value}")));
                    }
                    // CODE, arrayDim, VER, IMAGE and other parameters need no state.
                    _ => {}
                }
            }
            b'3' | b'0' => {
                let code = text(&line[..3]);
                if !matches!(code.as_str(), "317" | "327" | "367" | "307" | "027") {
                    return Err(CamError::unsupported(at, format!("IPC-D-356 record {code}")));
                }
                let point = read_point(&line, &code, at)?;
                if code == "027" {
                    let index = pending.take().ok_or_else(|| CamError::parse(at, "027 without preceding 307"))?;
                    let parent: &mut IpcPoint = &mut file.points[index];
                    if point.net != parent.net || point.refdes != parent.refdes || point.pin != parent.pin
                        || point.at != parent.at || point.access != parent.access
                        || point.mask_top != parent.mask_top || point.mask_bottom != parent.mask_bottom
                        || point.midpoint || point.drill != 0 {
                        return Err(CamError::parse(at, "027 does not match preceding 307"));
                    }
                    parent.size = point.size;
                    parent.rot_deg = point.rot_deg;
                } else {
                    pending = (code == "307").then_some(file.points.len());
                    file.points.push(point);
                }
            }
            _ => return Err(CamError::unsupported(at, format!("IPC-D-356 record {}", text(&line[..3])))),
        }
    }
    Err(CamError::parse(last_line.max(1), "missing 999 terminator"))
}

fn text(bytes: &[u8]) -> String { String::from_utf8_lossy(bytes).trim_end_matches(' ').into() }

fn marker(line: &[u8; 80], column: usize, expected: u8, at: usize) -> Result<(), CamError> {
    if line[column - 1] == expected { Ok(()) }
    else { Err(CamError::parse(at, format!("expected {} in column {column}", expected as char))) }
}

fn parse_number(bytes: &[u8], signed: bool, at: usize) -> Result<i64, CamError> {
    let start = usize::from(signed);
    if (signed && !matches!(bytes.first(), Some(b'+') | Some(b'-')))
        || !bytes[start..].iter().all(u8::is_ascii_digit) {
        return Err(CamError::parse(at, format!("invalid numeric field {:?}", String::from_utf8_lossy(bytes))));
    }
    String::from_utf8_lossy(bytes).parse().map_err(|_| CamError::parse(at, "numeric field out of range"))
}

fn read_point(line: &[u8; 80], code: &str, at: usize) -> Result<IpcPoint, CamError> {
    for (column, expected) in [(39, b'A'), (42, b'X'), (50, b'Y'), (73, b'S')] {
        marker(line, column, expected, at)?;
    }
    let net = text(&line[3..17]);
    if net.is_empty() { return Err(CamError::parse(at, "empty signal name")); }
    let midpoint = match line[31] {
        b'M' => true, b' ' => false,
        _ => return Err(CamError::parse(at, "invalid midpoint flag")),
    };
    let (drill, plated) = match line[32] {
        b' ' if line[33..38].iter().all(|b| *b == b' ') => (0, code != "367"),
        b'D' if matches!(line[37], b'P' | b'U') => ((parse_number(&line[33..37], false, at)? * 2540) as Nm, line[37] == b'P'),
        _ => return Err(CamError::parse(at, "invalid hole fields")),
    };
    let (size, rot_deg, layer_span) = if code == "307" {
        if line[57..71].iter().any(|b| *b != b' ') { return Err(CamError::parse(at, "307 has size fields")); }
        marker(line, 75, b'L', at)?;
        marker(line, 78, b'L', at)?;
        let a = parse_number(&line[75..77], false, at)? as u8;
        let b = parse_number(&line[78..80], false, at)? as u8;
        if a == 0 || a > b { return Err(CamError::parse(at, "invalid copper layer span")); }
        ((0, 0), 0, Some((a, b)))
    } else {
        for (column, expected) in [(58, b'X'), (63, b'Y'), (68, b'R')] { marker(line, column, expected, at)?; }
        let rot = parse_number(&line[68..71], false, at)?;
        if rot >= 360 { return Err(CamError::parse(at, "invalid rotation")); }
        ((
            (parse_number(&line[58..62], false, at)? * 2540) as Nm,
            (parse_number(&line[63..67], false, at)? * 2540) as Nm,
        ), rot as u16, None)
    };
    let mask = match line[73] {
        b'0'..=b'3' => line[73] - b'0',
        _ => return Err(CamError::parse(at, "invalid solder mask code")),
    };
    Ok(IpcPoint {
        net, net_aliased: false, refdes: text(&line[20..26]), pin: text(&line[27..31]),
        at: IpcPosition {
            x: parse_number(&line[42..49], true, at)? * 2540,
            y: -parse_number(&line[50..57], true, at)? * 2540,
        },
        drill, plated, access: parse_number(&line[39..41], false, at)? as u8, layer_span, midpoint,
        size, rot_deg, mask_top: mask & 1 != 0, mask_bottom: mask & 2 != 0, smd: code == "327",
    })
}
