//! A part's colours: which colour each triangle of its print mesh has, and
//! how each colour prints. Every coloured source lands here: a distance
//! field's material slots, a sculpt's material table, a mesh's vertex
//! colours. Each colour names a print colour; the tool it prints in is set when a job maps
//! the print colours onto the printer ([`PartPaint::states`]), so a model evaluates without
//! the printer.

/// One colour of a part.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintColor {
    /// The design colour, linear RGB 0..1.
    pub rgb: [f32; 3],
    /// The print colour it prints as (`None`: the part's own).
    pub color: Option<String>,
    /// The job's tool (1-based) for that print colour, set when the job is mapped; `None`:
    /// the part's own tool.
    pub filament: Option<u32>,
    /// The nozzle (index into the machine's nozzles) of that tool, set with `filament`.
    pub nozzle: Option<usize>,
}

impl PaintColor {
    /// Put it on the job's tool and nozzle of its print colour (`tool_of`: a print colour's
    /// 1-based tool and nozzle index): the part's own colour, or one no tool takes, on none.
    pub fn map(&mut self, tool_of: &dyn Fn(&str) -> Option<(u32, usize)>) {
        let tool = self.color.as_deref().and_then(tool_of);
        self.filament = tool.map(|t| t.0);
        self.nozzle = tool.map(|t| t.1);
    }
}

/// The colours of one plate part.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PartPaint {
    /// Colour 0 is the part's own (its `filament`); the rest are painted.
    pub colors: Vec<PaintColor>,
    /// Per triangle of the part's print mesh: its colour.
    pub tri: Vec<u16>,
}

impl PartPaint {
    /// Whether any triangle has another colour than the part's own.
    pub fn painted(&self) -> bool {
        self.tri.iter().any(|&c| c != 0)
    }

    /// The tool (1-based) each colour prints in: colour 0 and colours without a mapped tool
    /// print as the part (`own`).
    pub fn filaments(&self, own: u32) -> Vec<u32> {
        self.colors
            .iter()
            .enumerate()
            .map(|(i, c)| if i == 0 { own } else { c.filament.filter(|&f| f >= 1).unwrap_or(own) })
            .collect()
    }

    /// The paint state per colour for the slicer: 0 where the colour prints
    /// as the part, N for filament N.
    pub fn states(&self, own: u32) -> Vec<u32> {
        self.filaments(own).into_iter().map(|f| if f == own { 0 } else { f }).collect()
    }

    /// From per-vertex colours (sRGB bytes; a triangle takes the colour two
    /// or three of its corners share, else its first corner's), quantised
    /// to 5 bits per channel and merged to at most `max_colors` by nearest
    /// pairs. `own` is the part's own colour (linear RGB): the colour nearest
    /// it becomes colour 0. `None` when the mesh is one colour.
    pub fn from_vertex_colors(triangles: &[[u32; 3]], colors: &[[u8; 4]], own: [f32; 3], max_colors: usize) -> Option<PartPaint> {
        let q = |c: [u8; 4]| [c[0] >> 3, c[1] >> 3, c[2] >> 3];
        let mut keys: Vec<[u8; 3]> = Vec::new();
        let mut tri_key: Vec<usize> = Vec::with_capacity(triangles.len());
        let mut count: Vec<usize> = Vec::new();
        for t in triangles {
            let c = t.map(|v| colors.get(v as usize).copied().map(q).unwrap_or([31; 3]));
            let k = if c[1] == c[2] { c[1] } else { c[0] };
            let i = match keys.iter().position(|x| *x == k) {
                Some(i) => i,
                None => {
                    keys.push(k);
                    count.push(0);
                    keys.len() - 1
                }
            };
            count[i] += 1;
            tri_key.push(i);
        }
        if keys.len() < 2 {
            return None;
        }
        let lin = |k: [u8; 3]| k.map(|v| srgb_to_linear((v as f32 * 8.0 + 4.0) / 255.0));
        // Clusters: weighted mean colour, members; merge the nearest pair
        // until few enough.
        let mut cl: Vec<([f32; 3], usize, Vec<usize>)> = keys.iter().enumerate().map(|(i, k)| (lin(*k), count[i], vec![i])).collect();
        let max_colors = max_colors.max(1);
        while cl.len() > max_colors {
            let mut best = (f32::MAX, 0, 1);
            for a in 0..cl.len() {
                for b in a + 1..cl.len() {
                    let d = dist(oklab(cl[a].0), oklab(cl[b].0));
                    if d < best.0 {
                        best = (d, a, b);
                    }
                }
            }
            let (_, a, b) = best;
            let mb = cl.remove(b);
            let ma = &mut cl[a];
            let (wa, wb) = (ma.1 as f32, mb.1 as f32);
            for c in 0..3 {
                ma.0[c] = (ma.0[c] * wa + mb.0[c] * wb) / (wa + wb);
            }
            ma.1 += mb.1;
            ma.2.extend(mb.2);
        }
        if cl.len() < 2 {
            return None;
        }
        let first = (0..cl.len()).min_by(|&a, &b| dist(oklab(cl[a].0), oklab(own)).total_cmp(&dist(oklab(cl[b].0), oklab(own)))).unwrap();
        cl.swap(0, first);
        let mut key_color = vec![0u16; keys.len()];
        for (ci, c) in cl.iter().enumerate() {
            for &k in &c.2 {
                key_color[k] = ci as u16;
            }
        }
        Some(PartPaint { colors: cl.iter().map(|c| PaintColor { rgb: c.0, color: None, filament: None, nozzle: None }).collect(), tri: tri_key.iter().map(|&k| key_color[k]).collect() })
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Linear RGB to OKLab.
pub fn oklab(c: [f32; 3]) -> [f32; 3] {
    let l = 0.4122214708 * c[0] + 0.5363325363 * c[1] + 0.0514459929 * c[2];
    let m = 0.2119034982 * c[0] + 0.6806995451 * c[1] + 0.1073969566 * c[2];
    let s = 0.0883024619 * c[0] + 0.2817188376 * c[1] + 0.6299787005 * c[2];
    let (l, m, s) = (l.max(0.0).cbrt(), m.max(0.0).cbrt(), s.max(0.0).cbrt());
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_print_in_their_mapped_tool_or_the_parts_own() {
        let p = PartPaint {
            colors: vec![PaintColor { rgb: [0.8; 3], color: None, filament: None, nozzle: None }, PaintColor { rgb: [0.9, 0.05, 0.05], color: Some("Red".into()), filament: None, nozzle: None }, PaintColor { rgb: [0.0, 0.0, 1.0], color: Some("Blue".into()), filament: Some(4), nozzle: None }],
            tri: vec![0, 1, 2],
        };
        assert_eq!(p.filaments(1), vec![1, 1, 4]);
        assert_eq!(p.states(4), vec![0, 0, 0]);
    }

    #[test]
    fn vertex_colours_become_part_colours() {
        let tris = [[0, 1, 2], [1, 3, 2], [3, 4, 2]];
        let red = [230, 20, 20, 255];
        let white = [240, 240, 240, 255];
        let near_red = [226, 22, 18, 255];
        let cols = [white, white, white, red, near_red];
        let p = PartPaint::from_vertex_colors(&tris, &cols, [0.9; 3], 2).unwrap();
        assert_eq!(p.colors.len(), 2);
        assert_eq!(p.tri, vec![0, 0, 1]);
        assert!(p.colors[1].rgb[0] > 0.5 && p.colors[1].rgb[1] < 0.1);
        assert!(PartPaint::from_vertex_colors(&tris, &[white; 5], [0.9; 3], 4).is_none());
    }
}
