//! The Gerber layer writer (public layer format with attributes, X2 style).
//!
//! Output is deterministic: apertures are numbered in order of first use, attributes are written
//! only when they change, and no date is written. Coordinates are `4.n` in millimetres with
//! `n` from the options (6 is the nanometre grid: nothing is rounded).
//!
//! Aperture choice per shape: circle `C`, axis-aligned rectangle `R`, axis-aligned oblong `O`;
//! macros for what the standard apertures cannot say: `RECTR` (rotated rectangle), `OVALR`
//! (rotated oblong) and `ROUNDR` (rounded rectangle, rotated or not). Tracks are drawn with a
//! circular aperture, arcs with `G75` and `G02`/`G03`; polygons are regions (`G36`/`G37`), one per
//! polygon, its holes cut in by a zero-width bridge (`bridge`), never clear regions.

use crate::error::CamError;
use crate::image::{FileAttr, GItem, Geom, ObjAttrs, Polarity};
use crate::text::{deg_str, escape_attr, mm_str};
use fab_elec_core::geom::ArcGeom;
use fab_elec_core::{Angle, Contour, Nm, Seg, Shape, P};
use std::collections::HashMap;
use std::fmt::Write;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GerberOpts {
    /// Fraction digits of coordinates, 5 or 6 (6 is exact on the nm grid).
    pub decimals: u8,
    /// Attribute commands when on, `G04 #@! ` attribute comments when off (S46.R48).
    pub x2: bool,
    /// Object net, component and pin attributes, independently of X2 (S46.R48).
    pub netlist_attrs: bool,
    /// Replace macro flashes with regions, preserving their curved edges (S46.R48).
    pub no_aperture_macros: bool,
}

impl Default for GerberOpts {
    fn default() -> GerberOpts {
        GerberOpts { decimals: 6, x2: true, netlist_attrs: true, no_aperture_macros: false }
    }
}

/// The bodies of our three macros (the blocks after the name, joined by `*`): the reader
/// recognises its own by exact text and reads them back as the analytic shapes they came from.
pub(crate) const RECTR_BODY: &str = "21,1,$1,$2,0,0,$3";
pub(crate) const OVALR_BODY: &str = "21,1,$1-$2,$2,0,0,$3*1,1,$2,-($1-$2)/2,0,$3*1,1,$2,($1-$2)/2,0,$3";
pub(crate) const ROUNDR_BODY: &str = "21,1,$1-2x$3,$2,0,0,$4*21,1,$1,$2-2x$3,0,0,$4*1,1,2x$3,-($1/2-$3),-($2/2-$3),$4*1,1,2x$3,($1/2-$3),-($2/2-$3),$4*1,1,2x$3,($1/2-$3),($2/2-$3),$4*1,1,2x$3,-($1/2-$3),($2/2-$3),$4";
fn am(name: &str, body: &str) -> String {
    format!("%AM{name}*{body}*%\n")
}

struct Out<'a> {
    o: &'a GerberOpts,
    body: String,
    apertures: Vec<(String, Vec<String>)>,
    by_key: HashMap<(String, Vec<String>), u32>,
    macros: [bool; 3],
    cur_ap: u32,
    cur_pos: Option<P>,
    cur_pol: Polarity,
    mode: u8, // 0 unknown, 1 = G01, 2 = G02, 3 = G03
    cur: ObjAttrs,
    err: Option<CamError>,
}

pub fn write_gerber(items: &[GItem], file_attrs: &[FileAttr], opts: &GerberOpts) -> Result<Vec<u8>, CamError> {
    crate::gerber_export::validate_precision(opts.decimals)?;
    let mut w = Out {
        o: opts,
        body: String::new(),
        apertures: Vec::new(),
        by_key: HashMap::new(),
        macros: [false; 3],
        cur_ap: 0,
        cur_pos: None,
        cur_pol: Polarity::Dark,
        mode: 0,
        cur: ObjAttrs::default(),
        err: None,
    };
    for it in items {
        w.item(it);
        if let Some(e) = w.err.take() {
            return Err(e);
        }
    }
    let mut s = String::new();
    s.push_str("G04 Makepad Fab*\n");
    attribute(&mut s, opts.x2, "TF.GenerationSoftware,Makepad,Fab");
    for (k, v) in file_attrs {
        // S46.R66 / main ruling: even caller-supplied dates are omitted.
        if k == "CreationDate" {
            continue;
        }
        let mut text = format!("TF.{}", escape_attr(k));
        for f in v {
            text.push(',');
            text.push_str(&escape_attr(f));
        }
        attribute(&mut s, opts.x2, &text);
    }
    let _ = writeln!(s, "%FSLAX4{d}Y4{d}*%", d = opts.decimals);
    s.push_str("%MOMM*%\n");
    s.push_str("%LPD*%\nG75*\n");
    if w.macros[0] {
        s.push_str(&am("RECTR", RECTR_BODY));
    }
    if w.macros[1] {
        s.push_str(&am("OVALR", OVALR_BODY));
    }
    if w.macros[2] {
        s.push_str(&am("ROUNDR", ROUNDR_BODY));
    }
    for (i, (def, func)) in w.apertures.iter().enumerate() {
        if !func.is_empty() {
            let mut text = String::from("TA.AperFunction");
            for f in func {
                text.push(',');
                text.push_str(&escape_attr(f));
            }
            attribute(&mut s, opts.x2, &text);
        }
        let _ = writeln!(s, "%ADD{}{}*%", 10 + i, def);
        if !func.is_empty() {
            attribute(&mut s, opts.x2, "TD.AperFunction");
        }
    }
    s.push_str(&w.body);
    s.push_str("M02*\n");
    Ok(s.into_bytes())
}

fn attribute(out: &mut String, x2: bool, text: &str) {
    if x2 {
        let _ = writeln!(out, "%{text}*%");
    } else {
        let _ = writeln!(out, "G04 #@! {text}*");
    }
}

impl Out<'_> {
    fn unit(&self) -> f64 {
        10f64.powi(6 - self.o.decimals as i32)
    }
    fn c(&self, v: Nm) -> i64 {
        (v as f64 / self.unit()).round() as i64
    }
    fn xy(&self, p: P) -> String {
        format!("X{}Y{}", self.c(p.x), self.c(p.y))
    }

    fn aperture(&mut self, def: String, func: &[String]) -> u32 {
        let key = (def, func.to_vec());
        if let Some(&n) = self.by_key.get(&key) {
            return n;
        }
        let n = 10 + self.apertures.len() as u32;
        self.apertures.push(key.clone());
        self.by_key.insert(key, n);
        n
    }

    fn select(&mut self, n: u32) {
        if self.cur_ap != n {
            let _ = writeln!(self.body, "D{n}*");
            self.cur_ap = n;
        }
    }

    fn polarity(&mut self, p: Polarity) {
        if self.cur_pol != p {
            self.body.push_str(if p == Polarity::Dark { "%LPD*%\n" } else { "%LPC*%\n" });
            self.cur_pol = p;
        }
    }

    fn obj_attrs(&mut self, a: &ObjAttrs) {
        if !self.o.netlist_attrs {
            return;
        }
        if a.net != self.cur.net {
            match &a.net {
                Some(n) => {
                    attribute(&mut self.body, self.o.x2, &format!("TO.N,{}", escape_attr(n)));
                }
                None => attribute(&mut self.body, self.o.x2, "TD.N"),
            }
            self.cur.net = a.net.clone();
        }
        if a.pin != self.cur.pin {
            match &a.pin {
                Some((r, p)) => {
                    attribute(&mut self.body, self.o.x2, &format!("TO.P,{},{}", escape_attr(r), escape_attr(p)));
                }
                None => attribute(&mut self.body, self.o.x2, "TD.P"),
            }
            self.cur.pin = a.pin.clone();
        }
        if a.component != self.cur.component {
            match &a.component {
                Some(c) => {
                    attribute(&mut self.body, self.o.x2, &format!("TO.C,{}", escape_attr(c)));
                }
                None => attribute(&mut self.body, self.o.x2, "TD.C"),
            }
            self.cur.component = a.component.clone();
        }
    }

    fn mode(&mut self, m: u8) {
        if self.mode != m {
            self.body.push_str(["", "G01*\n", "G02*\n", "G03*\n"][m as usize]);
            self.mode = m;
        }
    }

    fn move_to(&mut self, p: P) {
        if self.cur_pos != Some(p) {
            let _ = writeln!(self.body, "{}D02*", self.xy(p));
            self.cur_pos = Some(p);
        }
    }

    fn line_to(&mut self, p: P) {
        self.mode(1);
        let _ = writeln!(self.body, "{}D01*", self.xy(p));
        self.cur_pos = Some(p);
    }

    /// An arc from the current point through `mid` to `b` (never collinear).
    fn arc_to(&mut self, a: P, mid: P, b: P) {
        let Some(g) = ArcGeom::new(a, mid, b) else {
            self.line_to(b);
            return;
        };
        self.mode(if g.sweep > 0.0 { 3 } else { 2 });
        let (i, j) = (self.c((g.center.0 - a.x as f64).round() as Nm), self.c((g.center.1 - a.y as f64).round() as Nm));
        let _ = writeln!(self.body, "X{}Y{}I{}J{}D01*", self.c(b.x), self.c(b.y), i, j);
        self.cur_pos = Some(b);
    }

    fn item(&mut self, it: &GItem) {
        self.polarity(it.polarity);
        match &it.geom {
            Geom::Shape(s) => self.shape(s, &it.attrs),
            Geom::Region(c) => {
                self.obj_attrs(&it.attrs);
                self.region(c);
            }
        }
    }

    fn region(&mut self, c: &Contour) {
        self.body.push_str("G36*\n");
        self.cur_pos = None;
        self.move_to(c.path.start);
        let mut from = c.path.start;
        for s in &c.path.segs {
            match *s {
                Seg::Line(p) => self.line_to(p),
                Seg::Arc { mid, end } => self.arc_to(from, mid, end),
            }
            from = s.end();
        }
        if from != c.path.start {
            self.line_to(c.path.start);
        }
        self.body.push_str("G37*\n");
        self.cur_pos = None;
    }

    fn flash(&mut self, def: String, at: P, a: &ObjAttrs) {
        let n = self.aperture(def, &a.aper_function);
        self.select(n);
        self.obj_attrs(a);
        let _ = writeln!(self.body, "{}D03*", self.xy(at));
        self.cur_pos = Some(at);
    }

    fn draw_ap(&mut self, w: Nm, a: &ObjAttrs) {
        let n = self.aperture(format!("C,{}", mm_str(w as i64)), &a.aper_function);
        self.select(n);
        self.obj_attrs(a);
    }

    fn pad_region(&mut self, c: P, half: P, rot: Angle, radius: Nm, a: &ObjAttrs) {
        let contour = Contour::rect(P::new(-half.x, -half.y), half, radius);
        let contour = Contour::new(contour.path.mapped(|p| p.rotated(rot).add(c)));
        // Select a standard aperture for the region's function; it does not stroke its edge.
        self.draw_ap(1, a);
        self.region(&contour);
    }

    fn shape(&mut self, s: &Shape, a: &ObjAttrs) {
        match s {
            Shape::Circle { c, r } => self.flash(format!("C,{}", mm_str(2 * *r as i64)), *c, a),
            Shape::Segment { a: p, b, w } => {
                if p == b {
                    self.flash(format!("C,{}", mm_str(*w as i64)), *p, a);
                } else {
                    self.draw_ap(*w, a);
                    self.move_to(*p);
                    self.line_to(*b);
                }
            }
            Shape::Arc { a: p, mid, b, w } => {
                if ArcGeom::new(*p, *mid, *b).is_none() {
                    return self.shape(&Shape::Segment { a: *p, b: *b, w: *w }, a);
                }
                self.draw_ap(*w, a);
                self.move_to(*p);
                self.arc_to(*p, *mid, *b);
            }
            Shape::Rect { c, half, rot, corner } => {
                let corner = (*corner).max(0);
                if corner >= half.x.min(half.y) && corner > 0 {
                    return self.shape(&Shape::Oval { c: *c, half: *half, rot: *rot }, a);
                }
                if self.o.no_aperture_macros && (corner > 0 || rot.quarter_turns().is_none()) {
                    return self.pad_region(*c, *half, *rot, corner, a);
                }
                let (w, h) = (2 * half.x as i64, 2 * half.y as i64);
                let def = if corner > 0 {
                    self.macros[2] = true;
                    format!("ROUNDR,{}X{}X{}X{}", mm_str(w), mm_str(h), mm_str(corner as i64), deg_str(rot.udeg()))
                } else {
                    match rot.quarter_turns().map(|q| q % 4) {
                        Some(0) | Some(2) => format!("R,{}X{}", mm_str(w), mm_str(h)),
                        Some(_) => format!("R,{}X{}", mm_str(h), mm_str(w)),
                        None => {
                            self.macros[0] = true;
                            format!("RECTR,{}X{}X{}", mm_str(w), mm_str(h), deg_str(rot.udeg()))
                        }
                    }
                };
                self.flash(def, *c, a);
            }
            Shape::Oval { c, half, rot } => {
                if half.x == half.y {
                    return self.flash(format!("C,{}", mm_str(2 * half.x as i64)), *c, a);
                }
                if self.o.no_aperture_macros && rot.quarter_turns().is_none() {
                    return self.pad_region(*c, *half, *rot, half.x.min(half.y), a);
                }
                let (w, h) = (2 * half.x as i64, 2 * half.y as i64);
                let def = match rot.quarter_turns().map(|q| q % 4) {
                    Some(0) | Some(2) => format!("O,{}X{}", mm_str(w), mm_str(h)),
                    Some(_) => format!("O,{}X{}", mm_str(h), mm_str(w)),
                    None => {
                        self.macros[1] = true;
                        // the long axis along x: turn a tall oblong a quarter
                        let (l, d, r) = if w >= h { (w, h, *rot) } else { (h, w, rot.add(Angle::QUARTER)) };
                        format!("OVALR,{}X{}X{}", mm_str(l), mm_str(d), deg_str(r.udeg()))
                    }
                };
                self.flash(def, *c, a);
            }
            Shape::Poly(set) => {
                for poly in set.polygons() {
                    let ring = match bridge(poly) {
                        Ok(r) => r,
                        Err(at) => {
                            // never write the polygon solid over a hole: the copper in it would short
                            let net = a.net.as_deref().map(|n| format!(" of net {n}")).unwrap_or_default();
                            let msg = format!("a hole of a polygon{net} cannot be joined to its outline, at ({:.3}, {:.3}) mm", fab_elec_core::to_mm(at.x), fab_elec_core::to_mm(at.y));
                            eprintln!("cam: {msg}");
                            self.err = Some(CamError::Geometry(msg));
                            return;
                        }
                    };
                    let contour = Contour::new(ring_path(&ring));
                    self.item(&GItem { polarity: self.cur_pol, geom: Geom::Region(contour), attrs: a.clone() });
                }
            }
        }
    }
}

fn ring_path(r: &[P]) -> fab_elec_core::Path {
    let mut p = fab_elec_core::Path::new(r[0]);
    for q in &r[1..] {
        p = p.line(*q);
    }
    p
}

/// One ring for a polygon with holes: each hole is joined to the outline by a zero-width cut
/// (a bridge walked there and back), holes in order of their rightmost vertex, from the right.
/// The bridge runs from the hole's rightmost vertex along +x to the nearest outline edge and to
/// the edge end that is visible from it (the reflex vertex inside the triangle with the least
/// angle when something is in the way): the ear-clipping hole elimination. Where that vertex
/// appears more than once (a bridge already ends there) the copy whose interior sector holds
/// the direction to the hole is the one cut in, so a cut never crosses an older one. The result
/// keeps the outer orientation and every vertex of the polygon. `Err` carries the position of a
/// hole that cannot be joined: the caller must fail, never write the polygon solid.
pub fn bridge(poly: &fab_elec_core::Polygon) -> Result<Vec<P>, P> {
    let mut ring = poly.outer.clone();
    let mut holes: Vec<&Vec<P>> = poly.holes.iter().filter(|h| h.len() >= 3).collect();
    holes.sort_by_key(|h| std::cmp::Reverse(h.iter().map(|p| (p.x, -p.y)).max().unwrap()));
    for h in holes {
        let mi = (0..h.len()).max_by_key(|&i| (h[i].x, -h[i].y)).unwrap();
        let m = h[mi];
        let Some(pi) = visible_vertex(&ring, m) else { return Err(m) };
        let mut out: Vec<P> = Vec::with_capacity(ring.len() + h.len() + 2);
        out.extend_from_slice(&ring[..=pi]);
        for k in 0..=h.len() {
            out.push(h[(mi + k) % h.len()]);
        }
        out.push(ring[pi]);
        out.extend_from_slice(&ring[pi + 1..]);
        ring = out;
    }
    Ok(ring)
}

/// Whether the direction from vertex `k` to `m` lies in the interior of the ring there.
fn locally_inside(ring: &[P], k: usize, m: P) -> bool {
    use fab_elec_core::geom::orient;
    let n = ring.len();
    let (p, v, nx) = (ring[(k + n - 1) % n], ring[k], ring[(k + 1) % n]);
    let o = orient(p, v, nx);
    if o > 0 {
        orient(v, nx, m) >= 0 && orient(v, m, p) >= 0
    } else if o < 0 {
        !(orient(v, p, m) > 0 && orient(v, m, nx) > 0)
    } else {
        orient(v, nx, m) >= 0
    }
}

fn visible_vertex(ring: &[P], m: P) -> Option<usize> {
    let n = ring.len();
    // nearest edge crossing the ray from m towards +x
    let mut best: Option<(f64, usize)> = None; // (x of the hit, index of the edge start)
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if a.y == b.y || (a.y.min(b.y) > m.y) || (a.y.max(b.y) < m.y) {
            continue;
        }
        let x = a.x as f64 + (m.y - a.y) as f64 * (b.x - a.x) as f64 / (b.y - a.y) as f64;
        if x >= m.x as f64 && best.map_or(true, |(bx, _)| x < bx) {
            best = Some((x, i));
        }
    }
    let (ix, i) = best?;
    let (a, b) = (ring[i], ring[(i + 1) % n]);
    let (pi, p) = if a.x >= b.x { (i, a) } else { ((i + 1) % n, b) };
    let hit = P::from_f(ix, m.y as f64);
    let mut pick = pi;
    if hit != p {
        // a reflex vertex inside the triangle m, hit, p is in the way: the one at the least angle
        let inside = |q: P| {
            let o1 = fab_elec_core::geom::orient(m, hit, q);
            let o2 = fab_elec_core::geom::orient(hit, p, q);
            let o3 = fab_elec_core::geom::orient(p, m, q);
            (o1 >= 0 && o2 >= 0 && o3 >= 0) || (o1 <= 0 && o2 <= 0 && o3 <= 0)
        };
        let mut best_cos = f64::MIN;
        for k in 0..n {
            let q = ring[k];
            if q == p || !inside(q) {
                continue;
            }
            let (prev, next) = (ring[(k + n - 1) % n], ring[(k + 1) % n]);
            if fab_elec_core::geom::orient(prev, q, next) >= 0 {
                continue; // convex: it cannot hide the vertex
            }
            let (dx, dy) = ((q.x - m.x) as f64, (q.y - m.y) as f64);
            let cos = dx / (dx * dx + dy * dy).sqrt().max(1e-9);
            if cos > best_cos {
                best_cos = cos;
                pick = k;
            }
        }
    }
    // the same point may be in the ring twice (an earlier bridge ends there): take the copy
    // whose interior sector holds the way to m
    let at = ring[pick];
    (0..n).filter(|&k| ring[k] == at).find(|&k| locally_inside(ring, k, m)).or(Some(pick))
}
