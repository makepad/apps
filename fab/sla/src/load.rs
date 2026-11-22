//! What the supports carry as the print hangs from the plate (R7 §3.11; ours: the reference
//! sizes nothing by load).
//!
//! Each layer cures onto the film and peels off it. The newest layer of each body (a connected
//! piece of the parts as printed so far) pulls with its area times the peel pressure, and the
//! heads on that body whose points are at or below the layer share the pull. A head's share is
//! the most area it ever takes ([`Bodies::shares`]). A tip, a pillar and a bridge each hold what
//! their section holds at the green resin's strength over the margin; a pillar pressed back down
//! onto the film buckles past its Euler load ([`Load`]). Weight never sizes anything: in the vat
//! a part nearly floats, and out of it the whole print weighs about a hundredth of one tip's peel.

use crate::grid::PixelGrid;
use crate::parts::{links, LayerParts};
use makepad_fab_settings::config::parse_num;
use makepad_fab_settings::Config;
use makepad_fab_slicer::par;
use std::f64::consts::PI;

/// What sizes the supports: the peel, the resin, and how much to spare.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Load {
    /// The peel's pull per area of the newest layer (MPa: N per mm²).
    pub peel_pressure: f64,
    /// The washed green resin's tensile strength and modulus (MPa).
    pub green_strength: f64,
    pub green_modulus: f64,
    /// How many times its load a section holds.
    pub margin: f64,
    /// A tip's least and most diameter (mm): the least is never under five pixels, and up to the
    /// most a tip still snaps off by hand.
    pub tip_min: f64,
    pub tip_max: f64,
    /// How many times its push back down a pillar holds before it buckles.
    pub buckling: f64,
}

/// Standard resin on an FEP film (R7 §3.11.7).
impl Default for Load {
    fn default() -> Self {
        Load { peel_pressure: 0.015, green_strength: 38.0, green_modulus: 1600.0, margin: 8.0, tip_min: 0.2, tip_max: 0.8, buckling: 3.0 }
    }
}

/// The profile keys and the units they are in, against the field each sets (MPa and mm here).
const KEYS: [(&str, f64); 7] = [
    ("fab_sla_peel_pressure", 0.001),
    ("fab_sla_green_strength", 1.0),
    ("fab_sla_green_modulus", 1000.0),
    ("fab_sla_load_margin", 1.0),
    ("fab_sla_tip_min", 1.0),
    ("fab_sla_tip_max", 1.0),
    ("fab_sla_buckling_factor", 1.0),
];

impl Load {
    /// The load a resin profile `cfg` sets: each key it leaves out at its default
    /// ([`Load::default`]); peel pressure in kPa, the modulus in GPa.
    pub fn from_config(cfg: &Config) -> Result<Load, String> {
        let mut load = Load::default();
        for (key, unit) in KEYS {
            let text = cfg.str(key).trim();
            if text.is_empty() {
                continue;
            }
            let v = parse_num(text).filter(|v| *v > 0.0).ok_or_else(|| format!("the resin profile: invalid {key} {text:?}"))? * unit;
            *load.field(key) = v;
        }
        if load.tip_min > load.tip_max {
            return Err(format!("the resin profile: its least tip {} mm is over its most {} mm", load.tip_min, load.tip_max));
        }
        Ok(load)
    }

    fn field(&mut self, key: &str) -> &mut f64 {
        match key {
            "fab_sla_peel_pressure" => &mut self.peel_pressure,
            "fab_sla_green_strength" => &mut self.green_strength,
            "fab_sla_green_modulus" => &mut self.green_modulus,
            "fab_sla_load_margin" => &mut self.margin,
            "fab_sla_tip_min" => &mut self.tip_min,
            "fab_sla_tip_max" => &mut self.tip_max,
            _ => &mut self.buckling,
        }
    }

    /// The least tip on `grid` (mm): the setting, and never under five pixels.
    pub fn least_tip(&self, grid: &PixelGrid) -> f64 {
        self.tip_min.max(5.0 * grid.pitch[0].max(grid.pitch[1]))
    }

    /// The section (mm²) that holds `share` mm² of peel in tension, at the margin.
    pub fn section_for(&self, share: f64) -> f64 {
        self.margin * self.peel_pressure * share / self.green_strength
    }

    /// The diameter (mm) of a round section of `area` mm².
    pub fn diameter(area: f64) -> f64 {
        (4.0 * area / PI).sqrt()
    }

    /// How much more section a strut leaning `lean` (radians from vertical) needs than one
    /// pulled along its axis: the vertical pull pulls it along and shears it across, by a von
    /// Mises estimate (the bend at its neck left out).
    pub fn leaning(lean: f64) -> f64 {
        (lean.cos().powi(2) + 3.0 * lean.sin().powi(2)).sqrt()
    }

    /// The tip (mm across) a head carrying `share` mm² needs, leaning `lean` from vertical.
    pub fn tip_for(&self, share: f64, lean: f64) -> f64 {
        Load::diameter(self.section_for(share) * Load::leaning(lean))
    }

    /// The share (mm²) a tip `d` mm across holds, leaning `lean`.
    pub fn tip_holds(&self, d: f64, lean: f64) -> f64 {
        PI * d * d / 4.0 / Load::leaning(lean) * self.green_strength / (self.margin * self.peel_pressure)
    }

    /// The pillar (mm across) that holds `shares` mm² hanging on it in tension, and that stands
    /// `length` mm unbraced under their push back down (R7 §3.11.4: the push about the peel, no
    /// margin but the buckling factor, Euler's load with pinned ends on the wet resin's modulus,
    /// half the washed one).
    pub fn pillar_for(&self, shares: f64, length: f64) -> f64 {
        let pull = Load::diameter(self.section_for(shares));
        let push = self.peel_pressure * shares;
        let wet = 0.5 * self.green_modulus;
        let buckle = (64.0 * self.buckling * push * length * length / (PI.powi(3) * wet)).powf(0.25);
        pull.max(buckle)
    }

    /// The bridge (mm across) that holds `share` mm² rising at `rise` (radians above the
    /// horizontal) to its pillar: the vertical pull along it over the sine.
    pub fn bridge_for(&self, share: f64, rise: f64) -> f64 {
        Load::diameter(self.section_for(share) / rise.sin().max(1e-6))
    }
}

/// The bodies of the parts' layers, as they join going up: each layer's parts, their areas, and
/// what each stands on in the layer below.
pub struct Bodies {
    /// Each layer's parts' areas (mm²).
    area: Vec<Vec<f64>>,
    /// Each layer's parts' parts below.
    on: Vec<Vec<Vec<u32>>>,
}

impl Bodies {
    pub fn new(cured: &[LayerParts], grid: &PixelGrid) -> Bodies {
        let px = grid.pixel_area();
        let area = cured.iter().map(|l| l.parts.iter().map(|p| p.pixels as f64 * px).collect()).collect();
        // each layer's links on its own, in parallel
        let on = par::map(cured.len(), |k| if k == 0 { vec![Vec::new(); cured[0].parts.len()] } else { links(&cured[k - 1], &cured[k]) });
        Bodies { area, on }
    }

    /// Each head's share (mm²) and the layer where it takes the most, for heads on `at` (each a
    /// layer and a part of it): the sweep up the layers, the bodies joining as their parts stand
    /// on each other, each layer's area of a body shared by the heads on it so far.
    pub fn shares(&self, at: &[(u32, u32)]) -> Vec<(f64, u32)> {
        let start: Vec<usize> = self.area.iter().scan(0, |n, l| {
            let s = *n;
            *n += l.len();
            Some(s)
        }).collect();
        let nodes = start.last().map_or(0, |s| s + self.area.last().map_or(0, Vec::len));
        let mut up: Vec<u32> = (0..nodes as u32).collect();
        fn root(up: &mut [u32], mut a: u32) -> u32 {
            while up[a as usize] != a {
                up[a as usize] = up[up[a as usize] as usize];
                a = up[a as usize];
            }
            a
        }
        let mut heads = vec![0u32; nodes];
        let mut by_layer: Vec<Vec<usize>> = vec![Vec::new(); self.area.len()];
        for (h, &(k, _)) in at.iter().enumerate() {
            by_layer[k as usize].push(h);
        }
        let mut out = vec![(0.0, 0u32); at.len()];
        let mut live: Vec<usize> = Vec::new();
        let mut area = vec![0.0f64; nodes];
        for k in 0..self.area.len() {
            for (i, below) in self.on[k].iter().enumerate() {
                let a = (start[k] + i) as u32;
                for &b in below {
                    let (ra, rb) = (root(&mut up, a), root(&mut up, (start[k - 1] + b as usize) as u32));
                    if ra != rb {
                        up[rb as usize] = ra;
                        heads[ra as usize] += heads[rb as usize];
                    }
                }
            }
            for &h in &by_layer[k] {
                let r = root(&mut up, (start[k] + at[h].1 as usize) as u32);
                heads[r as usize] += 1;
                live.push(h);
            }
            let mut touched = Vec::new();
            for (i, a) in self.area[k].iter().enumerate() {
                let r = root(&mut up, (start[k] + i) as u32) as usize;
                if area[r] == 0.0 {
                    touched.push(r);
                }
                area[r] += a;
            }
            for &h in &live {
                let (l, p) = at[h];
                let r = root(&mut up, (start[l as usize] + p as usize) as u32) as usize;
                if area[r] > 0.0 {
                    let share = area[r] / heads[r] as f64;
                    if share > out[h].0 {
                        out[h] = (share, k as u32);
                    }
                }
            }
            for r in touched {
                area[r] = 0.0;
            }
        }
        out
    }
}

/// The part of `cured` a point at bed `xy` on layer `k` is on: the part under it, else the
/// nearest part of that layer (none on an empty layer).
pub fn part_of(cured: &[LayerParts], grid: &PixelGrid, k: u32, xy: [f64; 2]) -> Option<u32> {
    let l = cured.get(k as usize)?;
    let p = grid.to_pixels(xy);
    let (x, y) = (p[0].max(0.0) as u32, p[1].max(0.0) as u32);
    l.part_at(x, y).or_else(|| {
        let gap = |r: &crate::grid::PixelRect| {
            let dx = (r.x0 as f64 - p[0]).max(p[0] - r.x1 as f64).max(0.0);
            let dy = (r.y0 as f64 - p[1]).max(p[1] - r.y1 as f64).max(0.0);
            dx.hypot(dy)
        };
        (0..l.parts.len()).min_by(|&a, &b| gap(&l.parts[a].rect).total_cmp(&gap(&l.parts[b].rect))).map(|i| i as u32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::LayerStack;
    use crate::parts::cured;
    use crate::printer::simulated;
    use crate::section::Section;
    use crate::tests::field;
    use makepad_csg_sdf::{SdfNode, SdfOp};
    use std::sync::Arc;

    /// The sizes reproduce the reference's fixed ones at R7 §3.11's worked loads: a 0.38 mm tip
    /// for a 37 mm² share and 0.43 mm for 46 mm²; a 1 mm pillar under six heads; a 1 mm pillar
    /// standing 15.3 mm unbraced under one head, a 30 mm one needing 1.4 mm; a head leaning 45°
    /// needing 1.41 times the section. What a tip holds is what needs it.
    #[test]
    fn the_sizes_are_the_references_at_its_loads() {
        let l = Load::default();
        let near = |a: f64, b: f64, tol: f64| (a - b).abs() < tol;
        assert!(near(l.tip_for(37.0, 0.0), 0.38, 0.01), "{}", l.tip_for(37.0, 0.0));
        assert!(near(l.tip_for(46.0, 0.0), 0.43, 0.01), "{}", l.tip_for(46.0, 0.0));
        assert!(near(Load::leaning(PI / 4.0), 2f64.sqrt(), 1e-9));
        assert!(near(l.tip_holds(l.tip_for(37.0, 0.3), 0.3), 37.0, 1e-9));
        assert!(near(l.pillar_for(6.0 * 37.0, 0.0), 0.94, 0.01), "{}", l.pillar_for(222.0, 0.0));
        assert!(near(l.pillar_for(37.0, 15.3), 1.0, 0.01), "{}", l.pillar_for(37.0, 15.3));
        assert!(near(l.pillar_for(37.0, 30.0), 1.4, 0.01), "{}", l.pillar_for(37.0, 30.0));
        assert!(near(l.bridge_for(37.0, PI / 4.0), 0.46, 0.01), "{}", l.bridge_for(37.0, PI / 4.0));
        // the least tip is five pixels where they are wider than the setting
        let p = simulated();
        assert!(near(l.least_tip(&crate::grid::PixelGrid::of(&p.screen)), 0.25, 1e-9));
    }

    /// A profile's keys set the load in its units (kPa, MPa, GPa, mm); the keys it leaves out
    /// keep the defaults, and a bad value is named.
    #[test]
    fn a_profile_sets_the_load() {
        let mut cfg = crate::printer::simulated_config();
        assert_eq!(Load::from_config(&cfg), Ok(Load::default()));
        cfg.set_str("fab_sla_peel_pressure", "12");
        cfg.set_str("fab_sla_green_modulus", "2.2");
        cfg.set_str("fab_sla_tip_max", "1.5");
        let l = Load::from_config(&cfg).unwrap();
        assert_eq!((l.peel_pressure, l.green_modulus, l.tip_max, l.green_strength), (0.012, 2200.0, 1.5, 38.0));
        cfg.set_str("fab_sla_green_strength", "-3");
        assert!(Load::from_config(&cfg).unwrap_err().contains("fab_sla_green_strength"));
    }

    fn at(x: f64, y: f64, z: f64, size: [f64; 3]) -> SdfNode {
        SdfNode::Transform { at: [x, y, z], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size, radius: 0.0 }) }
    }

    /// A 2 mm stem 10 mm tall under a 20 mm square top (model Y is the plate's Z), or two stems
    /// under one top: the parts' layers and their bodies.
    fn tee(stems: &[f64]) -> (LayerStack, Vec<LayerParts>) {
        let mut of: Vec<SdfNode> = stems.iter().map(|&x| at(x, 5.0, 0.0, [2.0, 10.0, 2.0])).collect();
        of.push(at(0.0, 11.0, 0.0, [20.0, 2.0, 20.0]));
        let part: Arc<dyn Section> = Arc::new(field(SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of }, [40.0, 40.0, 0.0], ([29.9, 29.9, -0.1], [50.1, 50.1, 12.1])));
        let stack = LayerStack::new(vec![part], &simulated());
        let layers = cured(&stack);
        (stack, layers)
    }

    /// A head under the stem carries the top's whole area once the top cures; a head on the
    /// top's first layer halves it for both from there; two stems' heads pool their bodies'
    /// areas where the top joins them.
    #[test]
    fn heads_share_their_bodies_newest_layer() {
        let (stack, layers) = tee(&[0.0]);
        let b = Bodies::new(&layers, &stack.grid);
        let on = |k: u32, xy: [f64; 2]| (k, part_of(&layers, &stack.grid, k, xy).expect("a part there"));
        let top = stack.layers.at(10.01);
        let one = b.shares(&[on(0, [40.0, 40.0])]);
        assert!((one[0].0 - 400.0).abs() < 2.0 && one[0].1 >= top, "{one:?}");
        let two = b.shares(&[on(0, [40.0, 40.0]), on(top, [48.0, 48.0])]);
        assert!(two.iter().all(|s| (s.0 - 200.0).abs() < 1.0), "{two:?}");
        let (stack, layers) = tee(&[-5.0, 5.0]);
        let b = Bodies::new(&layers, &stack.grid);
        let on = |k: u32, xy: [f64; 2]| (k, part_of(&layers, &stack.grid, k, xy).expect("a part there"));
        let pooled = b.shares(&[on(0, [35.0, 40.0]), on(0, [45.0, 40.0])]);
        assert!(pooled.iter().all(|s| (s.0 - 200.0).abs() < 1.0), "{pooled:?}");
    }
}
