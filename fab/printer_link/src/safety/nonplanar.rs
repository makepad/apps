//! Non-planar moves: the head envelope of the installed machine and the 3D rules
//! (NONPLANAR-DESIGN §4).
//!
//! A non-planar move is a move of Fab's toolpath that changes Z while it pushes or
//! pulls filament. It is refused unless the installed profile carries a head envelope
//! that was measured on the user's own head ([`HeadEnvelope::usable`]). With one, the
//! move must keep the slope cap, the rise cap, the Z speed and the Z jerk, and its
//! head must clear everything printed so far. Every move at a height below printed
//! material, travel included, must clear it too.
//!
//! The head is checked against a height field of the printed material, rebuilt from the
//! file's own extrusions. Each part of the head is a box in the tip's frame: a footprint
//! and its lowest point above the tip. Above the tip the head is a height function, so
//! the highest printed point of each column decides. That makes the check exact for the
//! boxes, and the boxes cover the real shapes from below.

/// The v1 slope cap may not be set above this (gentle tops only).
pub const MAX_SLOPE_CAP: f64 = 20.0;
/// The own trail behind the tip (mm of path) that joins the height field only once the tip
/// has moved on: the tip shapes its fresh bead there. Sound within the slope cap: the trail
/// rises at most TRAIL x tan(20 deg) = 1.1 mm, and a cone of up to 60 deg half angle clears a
/// 20 deg trail beyond its tip zone.
pub const TRAIL: f64 = 3.0;
/// Sampling step along a move (mm).
const STEP: f64 = 0.25;
/// Height-field cell (mm) and the coarse cell factor for wide footprints.
const CELL: f64 = 0.1;
const COARSE: usize = 16;
/// A cell's half diagonal: a bead marks every cell it touches, a box reads every cell that
/// touches it.
pub const SLACK: f64 = 0.0708;

/// One part of the head, in the frame of the active nozzle's tip.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadPart {
    pub name: String,
    /// Footprint centre relative to the tip, and its half extents (mm).
    pub ox: f64,
    pub oy: f64,
    pub hx: f64,
    pub hy: f64,
    /// A square of this half extent left out of the footprint's centre: the cone is a stack of
    /// such rings, and inside the innermost hole the tip forms its bead (not checked).
    pub hole: f64,
    /// The part's lowest point above the tip (mm).
    pub e: f64,
    /// The clearance it must keep to printed material (mm).
    pub margin: f64,
}

/// The installed head geometry and the non-planar limits. It belongs to the user's
/// installed machine, never to a file.
#[derive(Clone, Debug, PartialEq)]
pub struct HeadEnvelope {
    /// Names this measurement. A job records it; a job checked against another one is stale.
    pub version: String,
    /// Measured on the user's own head. A placeholder envelope never allows a non-planar move.
    pub measured: bool,
    /// Steepest non-planar extrusion (degrees, at most [`MAX_SLOPE_CAP`]).
    pub max_slope_deg: f64,
    /// Most printed material above the tip within `rise_half` of it, during a non-planar extrusion (mm).
    pub max_rise: f64,
    pub rise_half: f64,
    /// Z speed (mm/s) and Z speed change at a junction (mm/s) of non-planar extrusion.
    pub z_speed: f64,
    pub z_jerk: f64,
    /// Per active extruder (0 = left, 1 = right): its head parts, the other nozzle included.
    pub parts: Vec<Vec<HeadPart>>,
}

fn part(name: &str, ox: f64, oy: f64, hx: f64, hy: f64, hole: f64, e: f64, margin: f64) -> HeadPart {
    HeadPart { name: name.into(), ox, oy, hx, hy, hole, e, margin }
}

/// A conical nozzle (tip flat radius, half angle from the axis, height up to the block) as
/// square rings around the tip zone (`flat + 0.6`: the flat and the bead it forms), each at the cone's height at its hole's
/// edge (the ring's lowest point) less 0.02 mm, with margins growing outward.
pub fn cone_rings(flat: f64, half_deg: f64, height: f64, ox: f64, lift: f64) -> Vec<HeadPart> {
    let rise = 1.0 / half_deg.to_radians().tan();
    let e = |r: f64| ((r - flat).max(0.0) * rise).min(height);
    let edges = [flat + 0.6, flat + 0.9, flat + 1.3, flat + 1.8, flat + 2.4, flat + 3.1, flat + 3.7, flat + 4.3];
    edges.windows(2).enumerate().map(|(i, w)| {
        let margin = [0.15, 0.2, 0.25, 0.3, 0.4, 0.5, 0.5][i];
        part(&format!("nozzle cone {}", i + 1), ox, 0.0, w[1], w[1], w[0], lift + (e(w[0]) - 0.02).max(margin), margin)
    }).collect()
}

impl HeadEnvelope {
    /// A two-nozzle head of the default family, from the profile's numbers and assumptions: the cone (40 deg half
    /// angle, 0.5 mm flat) as three rings around the soft tip zone, the profile's 4 mm block
    /// height, footprints larger than assumed. It is not measured, so it never allows a
    /// non-planar move: it is what slicing and preview run on until the user measures the head.
    pub fn placeholder_two_nozzle() -> Self {
        let side = |dx: f64, other_e: f64| {
            let mut v = cone_rings(0.5, 40.0, 4.0, 0.0, 0.0);
            v.extend([
                part("heater block", 0.0, 0.0, 12.0, 14.0, 0.0, 4.0, 1.0),
                part("cooling duct", 0.0, 0.0, 20.0, 20.0, 0.0, 6.0, 1.0),
                part("head body", dx / 2.0, 0.0, 47.5, 40.0, 0.0, 12.0, 1.5),
                part("other nozzle", dx, 0.0, 4.5, 4.5, 0.0, other_e, 1.0),
                part("other heater block", dx, 0.0, 12.0, 14.0, 0.0, other_e + 4.0, 1.0),
            ]);
            v
        };
        HeadEnvelope {
            version: "placeholder-h2d".into(),
            measured: false,
            max_slope_deg: 15.0,
            max_rise: 2.0,
            rise_half: 15.0,
            z_speed: 20.0,
            z_jerk: 2.0,
            // Left active: the right tip ~5 mm higher (derived), taken as 4.5. Right active: the
            // raised left tip's height is unknown, taken as 2.
            parts: vec![side(25.0, 4.5), side(-25.0, 2.0)],
        }
    }

    /// Whether this envelope may allow non-planar moves, and why not.
    pub fn usable(&self) -> Result<(), String> {
        if !self.measured { return Err("the head geometry is not measured".into()); }
        if !(self.max_slope_deg > 0.0 && self.max_slope_deg <= MAX_SLOPE_CAP) { return Err(format!("slope cap {} deg is outside 0-{MAX_SLOPE_CAP}", self.max_slope_deg)); }
        if !(self.max_rise >= 0.0 && self.rise_half > 0.0 && self.z_speed > 0.0 && self.z_jerk > 0.0) { return Err("a limit is missing or not positive".into()); }
        if self.parts.is_empty() || self.parts.iter().any(Vec::is_empty) { return Err("a nozzle has no head parts".into()); }
        for p in self.parts.iter().flatten() {
            // A part that would touch planar layers printed below the tip is not a valid head.
            if !(p.hx > 0.0 && p.hy > 0.0 && p.hole >= 0.0 && p.hole < p.hx.min(p.hy) && p.margin > 0.0 && p.e >= p.margin) { return Err(format!("head part {} is invalid (it needs e >= margin > 0, a hole inside the footprint)", p.name)); }
        }
        // The tip zone (inside every hole) must be small: the tip's own bead only.
        for side in &self.parts {
            let tip = side.iter().filter(|p| p.ox == 0.0 && p.oy == 0.0).map(|p| p.hole).fold(f64::MAX, f64::min);
            if tip == f64::MAX || tip > 1.2 { return Err("the tip zone (the smallest hole at the tip) is missing or over 1.2 mm".into()); }
        }
        Ok(())
    }

    /// Reads `key = value` lines; `part = <extruder> <name> <ox> <oy> <hx> <hy> <e> <margin> [<hole>]`
    /// (the name without spaces). Unknown keys are errors.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut h = HeadEnvelope { version: String::new(), measured: false, max_slope_deg: 0.0, max_rise: -1.0, rise_half: 0.0, z_speed: 0.0, z_jerk: 0.0, parts: vec![] };
        for (i, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() { continue; }
            let Some((k, v)) = line.split_once('=') else { return Err(format!("line {}: expected key = value", i + 1)) };
            let (k, v) = (k.trim(), v.trim());
            let num = |s: &str| s.parse::<f64>().map_err(|_| format!("line {}: {s} is not a number", i + 1));
            match k {
                "version" => h.version = v.to_string(),
                "measured" => h.measured = v == "true",
                "max_slope_deg" => h.max_slope_deg = num(v)?,
                "max_rise" => h.max_rise = num(v)?,
                "rise_half" => h.rise_half = num(v)?,
                "z_speed" => h.z_speed = num(v)?,
                "z_jerk" => h.z_jerk = num(v)?,
                "part" => {
                    let w: Vec<&str> = v.split_whitespace().collect();
                    if w.len() != 8 && w.len() != 9 { return Err(format!("line {}: part needs 8 or 9 fields", i + 1)); }
                    let ext: usize = w[0].parse().map_err(|_| format!("line {}: bad extruder", i + 1))?;
                    while h.parts.len() <= ext { h.parts.push(vec![]); }
                    let hole = if w.len() == 9 { num(w[8])? } else { 0.0 };
                    h.parts[ext].push(part(w[1], num(w[2])?, num(w[3])?, num(w[4])?, num(w[5])?, hole, num(w[6])?, num(w[7])?));
                }
                _ => return Err(format!("line {}: unknown key {k}", i + 1)),
            }
        }
        if h.version.is_empty() { return Err("the envelope has no version".into()); }
        Ok(h)
    }
}

/// The highest printed point per column, from the file's extrusions.
pub(super) struct HeightMap {
    x0: f64,
    y0: f64,
    nx: usize,
    ny: usize,
    fine: Vec<f32>,
    cnx: usize,
    coarse: Vec<f32>,
}

impl HeightMap {
    pub(super) fn new(lo: [f64; 2], hi: [f64; 2]) -> Option<Self> {
        let (x0, y0) = (lo[0] - 1.0, lo[1] - 1.0);
        let nx = super::paths::sample_count(hi[0] - x0 + 1.0, CELL, 1, hi[0] - lo[0] + 2.0)?;
        let ny = super::paths::sample_count(hi[1] - y0 + 1.0, CELL, 1, hi[1] - lo[1] + 2.0)?;
        let (cnx, cny) = (nx.div_ceil(COARSE), ny.div_ceil(COARSE));
        let zeros = |count: usize| -> Option<Vec<f32>> {
            let mut values = Vec::new();
            values.try_reserve_exact(count).ok()?;
            values.resize(count, 0.0);
            Some(values)
        };
        Some(HeightMap { x0, y0, nx, ny, fine: zeros(nx.checked_mul(ny)?)?, cnx, coarse: zeros(cnx.checked_mul(cny)?)? })
    }

    fn cell(&self, x: f64, y: f64) -> (i64, i64) { (((x - self.x0) / CELL).floor() as i64, ((y - self.y0) / CELL).floor() as i64) }

    /// Raises every column a bead of half width `half` touches to the segment's height there.
    pub(super) fn stamp(&mut self, a: [f64; 3], b: [f64; 3], half: f64) {
        let r = half + SLACK;
        let (i0, j0) = self.cell(a[0].min(b[0]) - r, a[1].min(b[1]) - r);
        let (i1, j1) = self.cell(a[0].max(b[0]) + r, a[1].max(b[1]) + r);
        let ab = [b[0] - a[0], b[1] - a[1]];
        let ll = ab[0] * ab[0] + ab[1] * ab[1];
        for j in j0.max(0)..=j1.min(self.ny as i64 - 1) {
            for i in i0.max(0)..=i1.min(self.nx as i64 - 1) {
                let (x, y) = (self.x0 + (i as f64 + 0.5) * CELL, self.y0 + (j as f64 + 0.5) * CELL);
                let t = if ll > 1e-12 { (((x - a[0]) * ab[0] + (y - a[1]) * ab[1]) / ll).clamp(0.0, 1.0) } else { 0.0 };
                if (x - a[0] - t * ab[0]).hypot(y - a[1] - t * ab[1]) > r { continue; }
                // The height a cell's width further along, so a sloped bead is never under-stated.
                let slope = if ll > 1e-12 { (b[2] - a[2]).abs() / ll.sqrt() } else { f64::INFINITY };
                let z = (a[2] + t * (b[2] - a[2]) + slope * CELL).min(a[2].max(b[2])) as f32;
                let k = j as usize * self.nx + i as usize;
                if z > self.fine[k] {
                    self.fine[k] = z;
                    let c = (j as usize / COARSE) * self.cnx + i as usize / COARSE;
                    if z > self.coarse[c] { self.coarse[c] = z; }
                }
            }
        }
    }

    /// The highest printed point over a rectangle (every cell that touches it).
    pub(super) fn max_rect(&self, xa: f64, ya: f64, xb: f64, yb: f64) -> f64 {
        let (i0, j0) = self.cell(xa - SLACK, ya - SLACK);
        let (i1, j1) = self.cell(xb + SLACK, yb + SLACK);
        let (i0, j0) = (i0.max(0), j0.max(0));
        let (i1, j1) = (i1.min(self.nx as i64 - 1), j1.min(self.ny as i64 - 1));
        if i0 > i1 || j0 > j1 { return 0.0; }
        let mut m = 0f32;
        if (i1 - i0) as usize > 4 * COARSE || (j1 - j0) as usize > 4 * COARSE {
            // Wide footprints read the coarse cells that cover them (never lower than the fine ones).
            for cj in j0 as usize / COARSE..=j1 as usize / COARSE {
                for ci in i0 as usize / COARSE..=i1 as usize / COARSE { m = m.max(self.coarse[cj * self.cnx + ci]); }
            }
        } else {
            for j in j0..=j1 { for i in i0..=i1 { m = m.max(self.fine[j as usize * self.nx + i as usize]); } }
        }
        m as f64
    }
}

/// The tip's path of a move, as points every <= [`STEP`] mm. Arcs (G2/G3 with I/J) are
/// followed with Z changing evenly (the spiral lift's helix); a full circle when the end
/// equals the start.
pub(super) fn path(from: [f64; 3], to: [f64; 3], arc: Option<([f64; 2], bool)>, bound: f64) -> Option<Vec<[f64; 3]>> {
    let lerp = |t: f64, xy: [f64; 2]| [xy[0], xy[1], from[2] + t * (to[2] - from[2])];
    match arc {
        None => {
            let l = ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2) + (to[2] - from[2]).powi(2)).sqrt();
            let n = super::paths::sample_count(l, STEP, 1, bound)?;
            super::paths::sampled(n, |i| { let t = i as f64 / n as f64; lerp(t, [from[0] + t * (to[0] - from[0]), from[1] + t * (to[1] - from[1])]) })
        }
        Some((c, clockwise)) => {
            let r = (from[0] - c[0]).hypot(from[1] - c[1]);
            let a0 = (from[1] - c[1]).atan2(from[0] - c[0]);
            let a1 = (to[1] - c[1]).atan2(to[0] - c[0]);
            let tau = std::f64::consts::TAU;
            let mut sweep = if clockwise { a0 - a1 } else { a1 - a0 };
            while sweep <= 1e-9 { sweep += tau; }
            let n = super::paths::sample_count(sweep * r, STEP, 4, bound)?;
            super::paths::sampled(n, |i| {
                let t = i as f64 / n as f64;
                let a = if clockwise { a0 - t * sweep } else { a0 + t * sweep };
                lerp(t, [c[0] + r * a.cos(), c[1] + r * a.sin()])
            })
        }
    }
}

/// The lowest clearance of the head over `pts` (tip positions), and the part that has it,
/// for the parts that come within their margin; `None` when every part keeps its margin.
pub(super) fn clearance(map: &HeightMap, parts: &[HeadPart], pts: &[[f64; 3]]) -> (f64, Option<(String, f64)>) {
    let mut least = f64::MAX;
    let mut worst: Option<(String, f64)> = None;
    for p in pts {
        for h in parts {
            let (cx, cy) = (p[0] + h.ox, p[1] + h.oy);
            let top = if h.hole > 0.0 {
                let q = h.hole;
                map.max_rect(cx - h.hx, cy - h.hy, cx + h.hx, cy - q)
                    .max(map.max_rect(cx - h.hx, cy + q, cx + h.hx, cy + h.hy))
                    .max(map.max_rect(cx - h.hx, cy - q, cx - q, cy + q))
                    .max(map.max_rect(cx + q, cy - q, cx + h.hx, cy + q))
            } else { map.max_rect(cx - h.hx, cy - h.hy, cx + h.hx, cy + h.hy) };
            let c = p[2] + h.e - top;
            least = least.min(c);
            if c < h.margin && worst.as_ref().is_none_or(|w| c - h.margin < w.1) { worst = Some((h.name.clone(), c - h.margin)); }
        }
    }
    (least, worst)
}
