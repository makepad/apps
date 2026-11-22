//! An ensemble member is the nominal circuit with each toleranced part value drawn once
//! per (member seed, part path), so it keeps its sample across edits and relowerings.
//! The knob moves a member's value by the same factor as the nominal value.

use crate::lower::Lowered;
use fab_elec_sim::circuit::{thermal_noise_source, Indep};
use fab_elec_sim::SimCircuit;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
pub struct TolSite {
    /// The part's index in the lowering input and value index.
    pub part: u32,
    pub path: String,
    pub device: String,
    pub key: String,
    /// The nominal value in ohms, farads or henries.
    pub nominal: f64,
    pub lo: f64,
    pub hi: f64,
}

/// A standard normal sample determined only by a member seed and part path.
pub fn draw(seed: u64, path: &str) -> f64 {
    let h = splitmix64(seed ^ fnv1a64(path.as_bytes()));
    let a = splitmix64(h);
    let b = splitmix64(a);
    let scale = 1.0 / ((1_u64 << 53) as f64);
    // Rounding the upper midpoint can produce 1.0; keep both uniforms strictly below it.
    let u1 = (((a >> 11) as f64 + 0.5) * scale).min(1.0 - f64::EPSILON / 2.0);
    let u2 = (((b >> 11) as f64 + 0.5) * scale).min(1.0 - f64::EPSILON / 2.0);
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

/// A resistor keeps its noise sequence across relowerings for the same member seed.
/// The fixed salt separates the noise seed from the tolerance draw's stream for the same name.
pub fn noise_seed(member_seed: u64, resistor: &str) -> u64 {
    splitmix64(member_seed ^ fnv1a64(resistor.as_bytes()) ^ 0x6e6f6973655f7365)
}

/// Draw one value per site, with the tolerance interval spanning six standard deviations.
pub fn member_values(sites: &[TolSite], seed: u64) -> Vec<f64> {
    sites.iter().map(|site| {
        let sigma = (site.hi - site.lo) / 6.0;
        (site.nominal + sigma * draw(seed, &site.path)).clamp(site.lo, site.hi)
    }).collect()
}

/// Clone the nominal circuit and apply paired site values to its R, C and L devices by name, and
/// to the parts a timing cell absorbed, in O(devices + sites x cells).
pub fn member_circuit(lowered: &Lowered, values: &[f64]) -> SimCircuit {
    let mut circuit = lowered.circuit.clone();
    let devices = &mut circuit.devices;
    let mut by_name: HashMap<&str, Slot> = HashMap::new();
    by_name.extend(devices.r.iter().enumerate().map(|(i, d)| (d.name.as_str(), Slot::R(i))));
    by_name.extend(devices.c.iter().enumerate().map(|(i, d)| (d.name.as_str(), Slot::C(i))));
    by_name.extend(devices.l.iter().enumerate().map(|(i, d)| (d.name.as_str(), Slot::L(i))));
    let slots: Vec<Option<Slot>> = lowered.tol.iter().map(|site| by_name.get(site.device.as_str()).copied()).collect();
    for ((slot, &value), site) in slots.into_iter().zip(values).zip(&lowered.tol) {
        match slot {
            Some(Slot::R(i)) => devices.r[i].r = value,
            Some(Slot::C(i)) => devices.c[i].c = value,
            Some(Slot::L(i)) => devices.l[i].l = value,
            // a part a timing cell absorbed (Logic) takes its draw in the cell
            None => {
                if let Some(cell) = devices.cell.iter_mut().find(|cell| cell.parts.iter().any(|p| p.0 == site.device) || cell.caps.iter().any(|k| k.0 == site.device)) {
                    cell.set_part(&site.device, value);
                }
            }
        }
    }
    circuit
}

/// Add thermal noise at each resistor's member resistance after its tolerance draw, except a
/// resistor that already makes its own (the design's `noise: thermal`).
/// `dt` is the member run's largest step; returns zero unless it is finite and positive.
pub fn member_noise(circuit: &mut SimCircuit, member_seed: u64, dt: f64) -> usize {
    if !dt.is_finite() || dt <= 0.0 {
        return 0;
    }
    // one pass over the resistors (O(resistors)), the same source `thermal_noise` adds
    let temp_c = circuit.temp_c;
    let devices = &mut circuit.devices;
    let before = devices.i.len();
    // a resistor whose own noise the design asked for (`noise: thermal`) keeps that one
    let own: HashSet<String> = devices.i.iter().filter_map(|i| i.name.strip_suffix("#noise").map(str::to_owned)).collect();
    for res in devices.r.iter().filter(|res| !own.contains(&res.name)) {
        if let Some(src) = thermal_noise_source(res.r, temp_c, dt, noise_seed(member_seed, &res.name)) {
            devices.i.push(Indep { name: format!("{}#noise", res.name), p: res.a, n: res.b, src });
        }
    }
    devices.i.len() - before
}

/// A passive device's place in `Devices`.
#[derive(Clone, Copy)]
enum Slot {
    R(usize),
    C(usize),
    L(usize),
}

/// Scale a nominal SetParam value by the member's drawn-to-nominal ratio, when defined.
pub fn member_param(sites: &[TolSite], values: &[f64], device: &str, key: &str, value: f64) -> f64 {
    for (site, &drawn) in sites.iter().zip(values) {
        if site.device == device && site.key == key && site.nominal != 0.0 {
            return value * drawn / site.nominal;
        }
    }
    value
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for &byte in bytes {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    hash
}
