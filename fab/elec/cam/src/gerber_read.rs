//! The Gerber layer reader: the subset our writers and the common plot outputs use.
//!
//! Reads units (`MO`), the coordinate format (`FS`, leading zeros omitted), standard apertures
//! `C`, `R`, `O` (no holes), aperture macros with expressions and the primitives 1, 4, 5, 20,
//! 21, linear and multi-quadrant circular interpolation (`G01`, `G02`, `G03`, `G75`), regions,
//! layer polarity and the file, aperture and object attributes. Anything else is an
//! `Unsupported` error naming the construct: a reader that skips geometry would let a wrong
//! file validate.

use crate::error::CamError;
use crate::image::{FileAttr, GItem, Geom, GerberImage, ObjAttrs, Polarity};
use crate::text::unescape_attr;
use fab_elec_core::geom::ArcGeom;
use fab_elec_core::{Angle, Contour, Nm, Path, PolySet, Seg, Shape, P};
use std::collections::HashMap;

/// Chord error when macro curves are turned into polygons: 0.2 micrometre.
const MACRO_ERR: Nm = 200;

#[derive(Clone, Debug)]
enum Ap {
    Circle(Nm),
    Rect(Nm, Nm),
    Oval(Nm, Nm),
    Macro(String, Vec<f64>),
}

struct Dict {
    ap: Ap,
    func: Vec<String>,
}

#[derive(Clone, Debug)]
enum Stmt {
    Assign(u32, String),
    Prim(Vec<String>),
}

enum Tok<'a> {
    Ext(&'a str, usize),
    Word(&'a str, usize),
}

fn tokenize(text: &str) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let s = i + 1;
                let e = text[s..].find('%').map(|k| s + k).unwrap_or(b.len());
                out.push(Tok::Ext(&text[s..e], s));
                i = (e + 1).min(b.len());
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                let e = text[i..].find('*').map(|k| i + k).unwrap_or(b.len());
                out.push(Tok::Word(text[i..e].trim(), i));
                i = (e + 1).min(b.len());
            }
        }
    }
    out
}

struct R<'a> {
    text: &'a str,
    unit_nm: f64,
    dec: u8,
    fmt_seen: bool,
    aps: HashMap<u32, Dict>,
    ap_cache: HashMap<u32, PolySet>,
    macros: HashMap<String, Vec<Stmt>>,
    /// The text of each macro's body, to recognise our own.
    macro_src: HashMap<String, String>,
    ap_func: Vec<String>,
    cur_ap: u32,
    pos: P,
    mode: u8,
    pol: Polarity,
    obj: ObjAttrs,
    region: Option<Vec<Contour>>,
    reg_open: Option<(P, Vec<Seg>)>,
    items: Vec<GItem>,
    file_attrs: Vec<FileAttr>,
    inch: bool,
    ended: bool,
}

pub fn read_gerber(bytes: &[u8]) -> Result<GerberImage, CamError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CamError::parse(1, "not text"))?;
    let mut r = R {
        text,
        unit_nm: 1e6,
        dec: 6,
        fmt_seen: false,
        aps: HashMap::new(),
        ap_cache: HashMap::new(),
        macros: HashMap::new(),
        macro_src: HashMap::new(),
        ap_func: Vec::new(),
        cur_ap: 0,
        pos: P::ZERO,
        mode: 1,
        pol: Polarity::Dark,
        obj: ObjAttrs::default(),
        region: None,
        reg_open: None,
        items: Vec::new(),
        file_attrs: Vec::new(),
        inch: false,
        ended: false,
    };
    for t in tokenize(text) {
        if r.ended {
            break;
        }
        match t {
            Tok::Ext(s, at) => r.ext(s, at)?,
            Tok::Word(s, at) => r.word(s, at)?,
        }
    }
    if !r.ended {
        return Err(CamError::parse(r.line(text.len()), "no end of file (M02)"));
    }
    if !r.fmt_seen {
        return Err(CamError::parse(1, "no coordinate format (FS)"));
    }
    if r.region.is_some() {
        return Err(CamError::parse(r.line(text.len()), "region not closed (G37)"));
    }
    Ok(GerberImage { file_attrs: r.file_attrs, items: r.items, decimals: r.dec, inch: r.inch })
}

impl R<'_> {
    fn line(&self, at: usize) -> usize {
        1 + self.text[..at.min(self.text.len())].bytes().filter(|&c| c == b'\n').count()
    }
    fn err(&self, at: usize, m: impl Into<String>) -> CamError {
        CamError::parse(self.line(at), m)
    }
    fn unsup(&self, at: usize, m: impl Into<String>) -> CamError {
        CamError::unsupported(self.line(at), m)
    }
    fn len(&self, v: f64) -> Nm {
        (v * self.unit_nm).round() as Nm
    }

    fn coord(&self, v: i64) -> Nm {
        (v as f64 * self.unit_nm / 10f64.powi(self.dec as i32)).round() as Nm
    }

    fn ext(&mut self, s: &str, at: usize) -> Result<(), CamError> {
        let blocks: Vec<&str> = s.split('*').map(|b| b.trim()).filter(|b| !b.is_empty()).collect();
        let Some(first) = blocks.first().copied() else { return Ok(()) };
        if let Some(rest) = first.strip_prefix("FS") {
            let rest = rest.strip_prefix('L').ok_or_else(|| self.unsup(at, "FS: trailing-zero omission"))?;
            let rest = rest.strip_prefix('A').ok_or_else(|| self.unsup(at, "FS: incremental coordinates"))?;
            let rest = rest.strip_prefix('X').ok_or_else(|| self.err(at, "FS"))?;
            let b: Vec<char> = rest.chars().collect();
            if b.len() < 5 || b[2] != 'Y' {
                return Err(self.err(at, "FS"));
            }
            let dec = b[1].to_digit(10).ok_or_else(|| self.err(at, "FS"))?;
            let dec_y = b[4].to_digit(10).ok_or_else(|| self.err(at, "FS"))?;
            if dec != dec_y {
                return Err(self.unsup(at, "FS: different X and Y formats"));
            }
            self.dec = dec as u8;
            self.fmt_seen = true;
        } else if let Some(u) = first.strip_prefix("MO") {
            match u {
                "MM" => (self.unit_nm, self.inch) = (1e6, false),
                "IN" => (self.unit_nm, self.inch) = (25.4e6, true),
                _ => return Err(self.err(at, "MO")),
            }
        } else if let Some(p) = first.strip_prefix("LP") {
            self.pol = match p {
                "D" => Polarity::Dark,
                "C" => Polarity::Clear,
                _ => return Err(self.err(at, "LP")),
            };
        } else if first.starts_with("LR") || first.starts_with("LS") || first.starts_with("LM") {
            return Err(self.unsup(at, "aperture transformation (LR, LS, LM)"));
        } else if first.starts_with("AB") || first.starts_with("SR") {
            return Err(self.unsup(at, "block aperture or step and repeat"));
        } else if let Some(name) = first.strip_prefix("AM") {
            let mut stmts = Vec::new();
            for b in &blocks[1..] {
                if b.starts_with('0') && b[1..].starts_with(' ') || *b == "0" {
                    continue;
                }
                if let Some(eq) = b.find('=') {
                    if let Some(n) = b[..eq].strip_prefix('$').and_then(|n| n.trim().parse().ok()) {
                        stmts.push(Stmt::Assign(n, b[eq + 1..].to_string()));
                        continue;
                    }
                }
                stmts.push(Stmt::Prim(b.split(',').map(|x| x.trim().to_string()).collect()));
            }
            self.macro_src.insert(name.to_string(), blocks[1..].join("*"));
            self.macros.insert(name.to_string(), stmts);
        } else if let Some(d) = first.strip_prefix("ADD") {
            self.aperture(d, at)?;
        } else if let Some(a) = first.strip_prefix("TF.") {
            let (k, v) = split_attr(a);
            self.file_attrs.retain(|(kk, _)| *kk != k);
            self.file_attrs.push((k, v));
        } else if let Some(a) = first.strip_prefix("TA.") {
            let (k, v) = split_attr(a);
            if k == "AperFunction" {
                self.ap_func = v;
            }
        } else if let Some(a) = first.strip_prefix("TO.") {
            let (k, v) = split_attr(a);
            match k.as_str() {
                "N" => self.obj.net = v.into_iter().next(),
                "P" if v.len() >= 2 => self.obj.pin = Some((v[0].clone(), v[1].clone())),
                "C" => self.obj.component = v.into_iter().next(),
                _ => {}
            }
        } else if let Some(a) = first.strip_prefix("TD") {
            match a {
                "" => {
                    self.ap_func.clear();
                    self.obj = ObjAttrs::default();
                }
                ".AperFunction" => self.ap_func.clear(),
                ".N" => self.obj.net = None,
                ".P" => self.obj.pin = None,
                ".C" => self.obj.component = None,
                _ => {}
            }
        }
        Ok(())
    }

    fn aperture(&mut self, d: &str, at: usize) -> Result<(), CamError> {
        let nd = d.chars().take_while(|c| c.is_ascii_digit()).count();
        let code: u32 = d[..nd].parse().map_err(|_| self.err(at, "ADD: aperture number"))?;
        if code < 10 {
            return Err(self.err(at, "ADD: aperture numbers start at 10"));
        }
        let rest = &d[nd..];
        let (name, params) = match rest.split_once(',') {
            Some((n, p)) => (n, p),
            None => (rest, ""),
        };
        let nums: Result<Vec<f64>, _> = if params.is_empty() { Ok(vec![]) } else { params.split('X').map(|x| x.trim().parse::<f64>()).collect() };
        let nums = nums.map_err(|_| self.err(at, "ADD: parameters"))?;
        let ap = match (name, nums.len()) {
            ("C", 1) => Ap::Circle(self.len(nums[0])),
            ("R", 2) => Ap::Rect(self.len(nums[0]), self.len(nums[1])),
            ("O", 2) => Ap::Oval(self.len(nums[0]), self.len(nums[1])),
            ("C" | "R" | "O", _) => return Err(self.unsup(at, "aperture with a hole")),
            ("P", _) => return Err(self.unsup(at, "polygon aperture")),
            (m, _) if self.macros.contains_key(m) => Ap::Macro(m.to_string(), nums),
            (m, _) => return Err(self.err(at, format!("unknown aperture {m}"))),
        };
        self.aps.insert(code, Dict { ap, func: self.ap_func.clone() });
        Ok(())
    }

    fn word(&mut self, w: &str, at: usize) -> Result<(), CamError> {
        if w.starts_with("G04") {
            return Ok(());
        }
        let mut toks: Vec<(char, i64)> = Vec::new();
        let cs: Vec<char> = w.chars().collect();
        let mut i = 0;
        while i < cs.len() {
            let l = cs[i];
            if !l.is_ascii_alphabetic() {
                return Err(self.err(at, format!("unexpected {l:?}")));
            }
            let mut j = i + 1;
            if j < cs.len() && (cs[j] == '-' || cs[j] == '+') {
                j += 1;
            }
            while j < cs.len() && cs[j].is_ascii_digit() {
                j += 1;
            }
            let s: String = cs[i + 1..j].iter().collect();
            let v: i64 = s.parse().map_err(|_| self.err(at, format!("number after {l}")))?;
            toks.push((l.to_ascii_uppercase(), v));
            i = j;
        }
        let (mut x, mut y, mut ci, mut cj, mut d) = (None, None, 0i64, 0i64, None);
        for &(l, v) in &toks {
            match l {
                'G' => match v {
                    1 => self.mode = 1,
                    2 => self.mode = 2,
                    3 => self.mode = 3,
                    75 => {}
                    74 => return Err(self.unsup(at, "single-quadrant arcs (G74)")),
                    36 => {
                        self.region = Some(Vec::new());
                        self.reg_open = None;
                    }
                    37 => self.end_region(at)?,
                    70 => (self.unit_nm, self.inch) = (25.4e6, true),
                    71 => (self.unit_nm, self.inch) = (1e6, false),
                    90 => {}
                    91 => return Err(self.unsup(at, "incremental coordinates (G91)")),
                    _ => return Err(self.unsup(at, format!("G{v:02}"))),
                },
                'X' => x = Some(self.coord(v)),
                'Y' => y = Some(self.coord(v)),
                'I' => ci = self.coord(v) as i64,
                'J' => cj = self.coord(v) as i64,
                'D' => d = Some(v),
                'M' => match v {
                    2 => self.ended = true,
                    0 => self.ended = true,
                    _ => return Err(self.unsup(at, format!("M{v:02}"))),
                },
                _ => return Err(self.err(at, format!("unknown command {l}"))),
            }
        }
        let to = P::new(x.unwrap_or(self.pos.x), y.unwrap_or(self.pos.y));
        match d {
            None => {
                if x.is_some() || y.is_some() {
                    // a coordinate without D: the modal operation of the old formats
                    return Err(self.unsup(at, "coordinate without D01, D02 or D03"));
                }
            }
            Some(n) if n >= 10 => {
                if !self.aps.contains_key(&(n as u32)) {
                    return Err(self.err(at, format!("aperture D{n} is not defined")));
                }
                self.cur_ap = n as u32;
            }
            Some(2) => {
                if self.region.is_some() {
                    self.close_open_contour();
                    self.reg_open = Some((to, Vec::new()));
                }
                self.pos = to;
            }
            Some(1) => self.interpolate(to, P::new(ci as Nm, cj as Nm), at)?,
            Some(3) => {
                if self.region.is_some() {
                    return Err(self.err(at, "flash inside a region"));
                }
                self.flash(to, at)?;
                self.pos = to;
            }
            Some(n) => return Err(self.err(at, format!("D{n:02}"))),
        }
        Ok(())
    }

    fn close_open_contour(&mut self) {
        if let (Some(reg), Some((start, mut segs))) = (self.region.as_mut(), self.reg_open.take()) {
            if !segs.is_empty() {
                if segs.last().map(|s| s.end()) != Some(start) {
                    segs.push(Seg::Line(start));
                }
                reg.push(Contour { path: Path { start, segs } });
            }
        }
    }

    fn end_region(&mut self, at: usize) -> Result<(), CamError> {
        if self.region.is_none() {
            return Err(self.err(at, "G37 without G36"));
        }
        self.close_open_contour();
        let attrs = ObjAttrs { aper_function: Vec::new(), ..self.obj.clone() };
        let Some(contours) = self.region.take() else { return Err(self.err(at, "G37 without G36")) };
        for c in contours {
            self.items.push(GItem { polarity: self.pol, geom: Geom::Region(c), attrs: attrs.clone() });
        }
        Ok(())
    }

    fn obj_attrs(&self) -> ObjAttrs {
        let func = self.aps.get(&self.cur_ap).map(|d| d.func.clone()).unwrap_or_default();
        ObjAttrs { aper_function: func, ..self.obj.clone() }
    }

    fn interpolate(&mut self, to: P, ij: P, at: usize) -> Result<(), CamError> {
        let from = self.pos;
        let mut arcs: Vec<(P, P)> = Vec::new(); // (mid, end), for an arc mode
        if self.mode != 1 {
            let c = (from.x as f64 + ij.x as f64, from.y as f64 + ij.y as f64);
            let r0 = ((from.x as f64 - c.0).powi(2) + (from.y as f64 - c.1).powi(2)).sqrt();
            let r1 = ((to.x as f64 - c.0).powi(2) + (to.y as f64 - c.1).powi(2)).sqrt();
            if r0 < 1.0 {
                return Err(self.err(at, "arc with no radius"));
            }
            if (r0 - r1).abs() > r0.max(1000.0) * 1e-3 + 2000.0 {
                return Err(self.err(at, "arc whose end is not on its circle"));
            }
            let r = (r0 + r1) / 2.0;
            let a0 = (from.y as f64 - c.1).atan2(from.x as f64 - c.0);
            let a1 = (to.y as f64 - c.1).atan2(to.x as f64 - c.0);
            let tau = std::f64::consts::TAU;
            let ccw = self.mode == 3;
            let mut sweep = if ccw { (a1 - a0).rem_euclid(tau) } else { -((a0 - a1).rem_euclid(tau)) };
            let pt = |a: f64| P::from_f(c.0 + r * a.cos(), c.1 + r * a.sin());
            if from == to {
                // G75 with the same point: the full circle, as two half arcs
                sweep = if ccw { tau } else { -tau };
                arcs.push((pt(a0 + sweep / 4.0), pt(a0 + sweep / 2.0)));
                arcs.push((pt(a0 + sweep * 0.75), to));
            } else {
                arcs.push((pt(a0 + sweep / 2.0), to));
            }
        }
        if let Some(reg) = self.region.as_ref() {
            let _ = reg;
            let Some((_, segs)) = self.reg_open.as_mut() else {
                return Err(self.err(at, "region contour starts without D02"));
            };
            if arcs.is_empty() {
                segs.push(Seg::Line(to));
            } else {
                for (mid, end) in arcs {
                    segs.push(Seg::Arc { mid, end });
                }
            }
            self.pos = to;
            return Ok(());
        }
        let Some(ap) = self.aps.get(&self.cur_ap) else { return Err(self.err(at, "draw with no aperture")) };
        let Ap::Circle(w) = ap.ap else { return Err(self.unsup(at, "draw with a non-circular aperture")) };
        let attrs = self.obj_attrs();
        if arcs.is_empty() {
            self.items.push(GItem { polarity: self.pol, geom: Geom::Shape(Shape::Segment { a: from, b: to, w }), attrs });
        } else {
            let mut a = from;
            for (mid, end) in arcs {
                if ArcGeom::new(a, mid, end).is_some() {
                    self.items.push(GItem { polarity: self.pol, geom: Geom::Shape(Shape::Arc { a, mid, b: end, w }), attrs: attrs.clone() });
                } else {
                    self.items.push(GItem { polarity: self.pol, geom: Geom::Shape(Shape::Segment { a, b: end, w }), attrs: attrs.clone() });
                }
                a = end;
            }
        }
        self.pos = to;
        Ok(())
    }

    fn flash(&mut self, at_p: P, at: usize) -> Result<(), CamError> {
        let Some(d) = self.aps.get(&self.cur_ap) else { return Err(self.err(at, "flash with no aperture")) };
        let ap = d.ap.clone();
        let shape = match ap {
            Ap::Circle(d) => Shape::Circle { c: at_p, r: (d as f64 / 2.0).round() as Nm },
            Ap::Rect(w, h) => Shape::Rect { c: at_p, half: P::new(w / 2, h / 2), rot: Angle::ZERO, corner: 0 },
            Ap::Oval(w, h) => Shape::Oval { c: at_p, half: P::new(w / 2, h / 2), rot: Angle::ZERO },
            Ap::Macro(name, params) if self.own_macro(&name, params.len()) => {
                // our own macros (checked by their text) are the analytic shapes they were made from
                let v = |i: usize| self.len(params[i]);
                let rot = |i: usize| Angle::deg(params[i]);
                match name.as_str() {
                    "RECTR" => Shape::Rect { c: at_p, half: P::new(v(0) / 2, v(1) / 2), rot: rot(2), corner: 0 },
                    "OVALR" => Shape::Oval { c: at_p, half: P::new(v(0) / 2, v(1) / 2), rot: rot(2) },
                    _ => Shape::Rect { c: at_p, half: P::new(v(0) / 2, v(1) / 2), rot: rot(3), corner: v(2) },
                }
            }
            Ap::Macro(name, params) => {
                if !self.ap_cache.contains_key(&self.cur_ap) {
                    let set = self.eval_macro(&name, &params, at)?;
                    self.ap_cache.insert(self.cur_ap, set);
                }
                Shape::Poly(self.ap_cache[&self.cur_ap].translated(at_p))
            }
        };
        let attrs = self.obj_attrs();
        self.items.push(GItem { polarity: self.pol, geom: Geom::Shape(shape), attrs });
        Ok(())
    }

    fn own_macro(&self, name: &str, params: usize) -> bool {
        use crate::gerber_write::{OVALR_BODY, RECTR_BODY, ROUNDR_BODY};
        let (body, n) = match name {
            "RECTR" => (RECTR_BODY, 3),
            "OVALR" => (OVALR_BODY, 3),
            "ROUNDR" => (ROUNDR_BODY, 4),
            _ => return false,
        };
        params == n && self.macro_src.get(name).map(|s| s.as_str()) == Some(body)
    }

    fn eval_macro(&self, name: &str, params: &[f64], at: usize) -> Result<PolySet, CamError> {
        let stmts = &self.macros[name];
        let mut vars: HashMap<u32, f64> = HashMap::new();
        for (i, p) in params.iter().enumerate() {
            vars.insert(i as u32 + 1, *p);
        }
        let s = self.unit_nm;
        let mut acc = PolySet::new();
        for st in stmts {
            match st {
                Stmt::Assign(n, e) => {
                    let v = eval(e, &vars).map_err(|m| self.err(at, m))?;
                    vars.insert(*n, v);
                }
                Stmt::Prim(f) => {
                    let v: Result<Vec<f64>, String> = f.iter().map(|e| eval(e, &vars)).collect();
                    let v = v.map_err(|m| self.err(at, m))?;
                    let code = v[0] as i64;
                    let need = |n: usize| if v.len() >= n { Ok(()) } else { Err(self.err(at, format!("macro primitive {code}: too few parameters"))) };
                    let exp = |k: usize| v.get(k).copied().unwrap_or(1.0) != 0.0;
                    let (ring, expose): (Vec<(f64, f64)>, bool) = match code {
                        1 => {
                            need(5)?;
                            let rot = v.get(5).copied().unwrap_or(0.0);
                            let (cx, cy) = rotate((v[3], v[4]), rot);
                            (circle_ring(cx * s, cy * s, v[2] * s / 2.0), exp(1))
                        }
                        20 => {
                            need(7)?;
                            let (w, x1, y1, x2, y2, rot) = (v[2], v[3], v[4], v[5], v[6], v.get(7).copied().unwrap_or(0.0));
                            let (dx, dy) = (x2 - x1, y2 - y1);
                            let l = (dx * dx + dy * dy).sqrt().max(1e-12);
                            let (nx, ny) = (-dy / l * w / 2.0, dx / l * w / 2.0);
                            let pts = [(x1 + nx, y1 + ny), (x2 + nx, y2 + ny), (x2 - nx, y2 - ny), (x1 - nx, y1 - ny)];
                            (pts.iter().map(|&p| scale(rotate(p, rot), s)).collect(), exp(1))
                        }
                        21 => {
                            need(6)?;
                            let (w, h, cx, cy, rot) = (v[2], v[3], v[4], v[5], v.get(6).copied().unwrap_or(0.0));
                            let pts = [(cx - w / 2.0, cy - h / 2.0), (cx + w / 2.0, cy - h / 2.0), (cx + w / 2.0, cy + h / 2.0), (cx - w / 2.0, cy + h / 2.0)];
                            (pts.iter().map(|&p| scale(rotate(p, rot), s)).collect(), exp(1))
                        }
                        4 => {
                            need(3)?;
                            let n = v[2] as usize + 1;
                            need(3 + 2 * n)?;
                            let rot = v.get(3 + 2 * n).copied().unwrap_or(0.0);
                            ((0..n).map(|i| scale(rotate((v[3 + 2 * i], v[4 + 2 * i]), rot), s)).collect(), exp(1))
                        }
                        5 => {
                            need(6)?;
                            let (n, cx, cy, dia, rot) = (v[2] as usize, v[3], v[4], v[5], v.get(6).copied().unwrap_or(0.0));
                            let (cx, cy) = rotate((cx, cy), rot);
                            let ring = (0..n.max(3))
                                .map(|k| {
                                    let a = (rot + 360.0 * k as f64 / n.max(3) as f64).to_radians();
                                    ((cx + dia / 2.0 * a.cos()) * s, (cy + dia / 2.0 * a.sin()) * s)
                                })
                                .collect();
                            (ring, exp(1))
                        }
                        _ => return Err(self.unsup(at, format!("macro primitive {code}"))),
                    };
                    let ring: Vec<P> = ring.into_iter().map(|(x, y)| P::from_f(x, y)).collect();
                    let set = PolySet::from_ring_nonzero(ring);
                    acc = if expose { acc.union(&set) } else { acc.difference(&set) };
                }
            }
        }
        Ok(acc)
    }
}

fn split_attr(a: &str) -> FileAttr {
    let mut it = a.split(',').map(unescape_attr);
    let k = it.next().unwrap_or_default();
    (k, it.collect())
}

fn rotate(p: (f64, f64), deg: f64) -> (f64, f64) {
    if deg == 0.0 {
        return p;
    }
    let (s, c) = deg.to_radians().sin_cos();
    (p.0 * c - p.1 * s, p.0 * s + p.1 * c)
}

fn scale(p: (f64, f64), s: f64) -> (f64, f64) {
    (p.0 * s, p.1 * s)
}

fn circle_ring(cx: f64, cy: f64, r: f64) -> Vec<(f64, f64)> {
    let e = (MACRO_ERR as f64).min(r.max(1.0));
    let step = 2.0 * (1.0 - e / r.max(1.0)).clamp(-1.0, 1.0).acos();
    let n = ((std::f64::consts::TAU / step.max(1e-3)).ceil() as usize).clamp(8, 4096);
    (0..n).map(|k| { let a = std::f64::consts::TAU * k as f64 / n as f64; (cx + r * a.cos(), cy + r * a.sin()) }).collect()
}

/// A macro expression: `+ - x / ( )`, numbers and `$n`.
fn eval(e: &str, vars: &HashMap<u32, f64>) -> Result<f64, String> {
    let cs: Vec<char> = e.chars().filter(|c| !c.is_whitespace()).collect();
    let mut i = 0;
    let v = expr(&cs, &mut i, vars)?;
    if i != cs.len() {
        return Err(format!("macro expression {e:?}"));
    }
    Ok(v)
}

fn expr(c: &[char], i: &mut usize, v: &HashMap<u32, f64>) -> Result<f64, String> {
    let mut a = term(c, i, v)?;
    while *i < c.len() && (c[*i] == '+' || c[*i] == '-') {
        let op = c[*i];
        *i += 1;
        let b = term(c, i, v)?;
        a = if op == '+' { a + b } else { a - b };
    }
    Ok(a)
}

fn term(c: &[char], i: &mut usize, v: &HashMap<u32, f64>) -> Result<f64, String> {
    let mut a = factor(c, i, v)?;
    while *i < c.len() && matches!(c[*i], 'x' | 'X' | '/') {
        let op = c[*i];
        *i += 1;
        let b = factor(c, i, v)?;
        a = if op == '/' { a / b } else { a * b };
    }
    Ok(a)
}

fn factor(c: &[char], i: &mut usize, v: &HashMap<u32, f64>) -> Result<f64, String> {
    let Some(&ch) = c.get(*i) else { return Err("macro expression ends early".into()) };
    match ch {
        '+' => {
            *i += 1;
            factor(c, i, v)
        }
        '-' => {
            *i += 1;
            Ok(-factor(c, i, v)?)
        }
        '(' => {
            *i += 1;
            let x = expr(c, i, v)?;
            if c.get(*i) != Some(&')') {
                return Err("macro expression: missing )".into());
            }
            *i += 1;
            Ok(x)
        }
        '$' => {
            let s = *i + 1;
            let mut e = s;
            while e < c.len() && c[e].is_ascii_digit() {
                e += 1;
            }
            let n: u32 = c[s..e].iter().collect::<String>().parse().map_err(|_| "macro variable")?;
            *i = e;
            v.get(&n).copied().ok_or_else(|| format!("macro variable ${n} is not set"))
        }
        _ => {
            let s = *i;
            let mut e = s;
            while e < c.len() && (c[e].is_ascii_digit() || c[e] == '.') {
                e += 1;
            }
            *i = e;
            c[s..e].iter().collect::<String>().parse().map_err(|_| format!("macro number at {ch:?}"))
        }
    }
}
