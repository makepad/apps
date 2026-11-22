//! The print-time model (R1 §10.3): every motion becomes a block with a
//! trapezoidal speed profile; junction speeds follow per-axis jerk and the
//! firmware's centripetal rule for shallow turns; forward and reverse passes
//! make every block reachable. Dwell and machine macro time are explicit.

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// X, Y, Z, E
    pub max_speed: [f64; 4],
    pub max_accel: [f64; 4],
    pub max_jerk: [f64; 4],
    pub max_accel_extruding: f64,
    pub max_accel_retracting: f64,
    pub max_accel_travel: f64,
    pub min_extruding_rate: f64,
    pub min_travel_rate: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_speed: [500.0, 500.0, 12.0, 120.0],
            max_accel: [9000.0, 9000.0, 500.0, 10000.0],
            max_jerk: [10.0, 10.0, 0.2, 2.5],
            max_accel_extruding: 1500.0,
            max_accel_retracting: 1500.0,
            max_accel_travel: 1250.0,
            min_extruding_rate: 0.0,
            min_travel_rate: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Block {
    /// Axis deltas (mm): X, Y, Z, E.
    pub delta: [f64; 4],
    /// Path length used for timing (XYZ length, or |E| for E-only).
    pub dist: f64,
    pub nominal: f64,
    pub accel: f64,
    /// Planned speeds.
    pub v_entry: f64,
    pub v_exit: f64,
    pub v_cruise: f64,
    pub safe: f64,
    pub max_entry: f64,
    /// Must stop before this block (a synchronising command came first).
    pub stop_before: bool,
    pub time: f64,
    /// XY unit direction (0 when no XY motion).
    pub uxy: [f64; 2],
    pub xy_len: f64,
    /// For arcs: unit directions at the start and the end (else unused).
    pub arc: Option<([f64; 4], [f64; 4])>,
}

impl Block {
    pub fn unit(&self) -> [f64; 4] {
        if self.dist <= 0.0 {
            return [0.0; 4];
        }
        [self.delta[0] / self.dist, self.delta[1] / self.dist, self.delta[2] / self.dist, self.delta[3] / self.dist]
    }
    pub fn unit_in(&self) -> [f64; 4] {
        self.arc.map(|a| a.0).unwrap_or_else(|| self.unit())
    }
    pub fn unit_out(&self) -> [f64; 4] {
        self.arc.map(|a| a.1).unwrap_or_else(|| self.unit())
    }
}

/// Time of a trapezoid from v0 to v1 over d with accel a and cruise vc.
pub fn trapezoid(d: f64, v0: f64, vc: f64, v1: f64, a: f64) -> (f64, f64) {
    if d <= 0.0 {
        return (0.0, v0.min(v1));
    }
    if a <= 0.0 {
        return (d / vc.max(1e-9), vc);
    }
    let acc_d = ((vc * vc - v0 * v0) / (2.0 * a)).max(0.0);
    let dec_d = ((vc * vc - v1 * v1) / (2.0 * a)).max(0.0);
    if acc_d + dec_d <= d {
        let t = (vc - v0).max(0.0) / a + (vc - v1).max(0.0) / a + (d - acc_d - dec_d) / vc.max(1e-9);
        (t, vc)
    } else {
        // The ramps meet: peak speed where they cross.
        let vp2 = (2.0 * a * d + v0 * v0 + v1 * v1) / 2.0;
        let vp = vp2.max(v0 * v0).max(v1 * v1).sqrt();
        let t = (vp - v0).max(0.0) / a + (vp - v1).max(0.0) / a;
        (t, vp)
    }
}

/// Make a new block from axis deltas and the commanded feedrate (mm/s).
pub fn make_block(lim: &Limits, delta: [f64; 4], feed: f64, accel_print: f64, accel_travel: f64, accel_retract: f64) -> Option<Block> {
    let xyz = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
    let dist = if xyz > 1e-9 { xyz } else { delta[3].abs() };
    if dist <= 1e-9 {
        return None;
    }
    let extruding = delta[3].abs() > 1e-9 && xyz > 1e-9;
    let e_only = xyz <= 1e-9;
    let mut v = feed;
    if extruding || e_only {
        v = v.max(lim.min_extruding_rate);
    } else {
        v = v.max(lim.min_travel_rate);
    }
    let mut accel = if e_only {
        accel_retract.min(lim.max_accel_retracting.max(1.0))
    } else if extruding {
        accel_print.min(lim.max_accel_extruding.max(1.0))
    } else {
        accel_travel.min(lim.max_accel_travel.max(1.0))
    };
    // Per-axis speed and acceleration limits.
    for a in 0..4 {
        let ua = (delta[a] / dist).abs();
        if ua > 1e-12 {
            if v * ua > lim.max_speed[a] {
                v = lim.max_speed[a] / ua;
            }
            if accel * ua > lim.max_accel[a] {
                accel = lim.max_accel[a] / ua;
            }
        }
    }
    let mut safe = v;
    for a in 0..4 {
        let ua = (delta[a] / dist).abs();
        if ua > 1e-12 && safe * ua > lim.max_jerk[a] {
            safe = lim.max_jerk[a] / ua;
        }
    }
    let xy_len = (delta[0] * delta[0] + delta[1] * delta[1]).sqrt();
    let uxy = if xy_len > 1e-9 { [delta[0] / xy_len, delta[1] / xy_len] } else { [0.0, 0.0] };
    Some(Block { delta, dist, nominal: v, accel, safe: safe.min(v), max_entry: safe.min(v), uxy, xy_len, arc: None, ..Default::default() })
}

/// Junction limit between two consecutive blocks.
fn junction(lim: &Limits, prev: &Block, cur: &mut Block) {
    if cur.stop_before {
        cur.max_entry = 0.0;
        return;
    }
    let up = prev.unit_out();
    let uc = cur.unit_in();
    let mut vj = prev.nominal.min(cur.nominal);
    for a in 0..4 {
        let d = (uc[a] - up[a]).abs();
        if d > 1e-12 {
            vj = vj.min(lim.max_jerk[a] / d);
        }
    }
    let thr = vj * 0.99;
    if prev.safe > thr && cur.safe > thr {
        vj = cur.safe;
    }
    // Centripetal rule for shallow XY turns (between line segments; an arc
    // is one block whose ends are tangent to its neighbours).
    if prev.xy_len > 0.0 && cur.xy_len > 0.0 && prev.arc.is_none() && cur.arc.is_none() {
        let dx = cur.uxy[0] - prev.uxy[0];
        let dy = cur.uxy[1] - prev.uxy[1];
        let du = (dx * dx + dy * dy).sqrt();
        if du > 1e-5 && du < 0.5 {
            // |u1 - u2| = 2 sin(θ/2)
            let half_sin = du / 2.0;
            let r = (cur.xy_len / 2.0) / half_sin;
            let vc = (cur.accel * r).sqrt();
            if vc < cur.nominal {
                cur.nominal = vc;
                vj = vj.min(vc);
            }
        }
    }
    cur.max_entry = vj.min(cur.nominal);
}

/// Plan speeds and times for a run of blocks that starts and ends at rest.
pub fn plan(lim: &Limits, blocks: &mut [Block]) {
    let n = blocks.len();
    if n == 0 {
        return;
    }
    blocks[0].max_entry = blocks[0].safe.min(blocks[0].nominal);
    if blocks[0].stop_before {
        blocks[0].max_entry = 0.0;
    }
    for i in 1..n {
        let (a, b) = blocks.split_at_mut(i);
        junction(lim, &a[i - 1], &mut b[0]);
    }
    // Reverse pass: each block's entry must allow stopping at the end.
    let mut next_entry = blocks[n - 1].safe.min(blocks[n - 1].nominal);
    let mut last = true;
    for i in (0..n).rev() {
        let b = &mut blocks[i];
        let exit = if last { b.safe.min(b.nominal) } else { next_entry };
        last = false;
        b.v_exit = exit;
        let reach = (exit * exit + 2.0 * b.accel * b.dist).sqrt();
        b.v_entry = b.max_entry.min(reach);
        next_entry = b.v_entry;
    }
    // Forward pass.
    for i in 0..n {
        let reach = (blocks[i].v_entry * blocks[i].v_entry + 2.0 * blocks[i].accel * blocks[i].dist).sqrt();
        let exit = if i + 1 < n { blocks[i + 1].v_entry.min(reach) } else { blocks[i].v_exit.min(reach) };
        blocks[i].v_exit = exit;
        if i + 1 < n {
            blocks[i + 1].v_entry = exit;
        }
    }
    for b in blocks.iter_mut() {
        let (t, vc) = trapezoid(b.dist, b.v_entry, b.nominal, b.v_exit, b.accel);
        b.time = t;
        b.v_cruise = vc;
    }
}
