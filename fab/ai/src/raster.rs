//! Triangles drawn on the CPU into a view's pixels: what covers each pixel, how far in front of the
//! camera the nearest is, and which way it faces. For the tools that read a view by numbers
//! (fab.compare's silhouettes, depths and points); the picture a person looks at is the
//! renderer's. Positions come in through [`ViewCamera::view_point`], the cameras fab.render draws
//! with.

use crate::camera::ViewCamera;

/// How far outside a triangle (a share of the way across it) a pixel's middle may be and still
/// be covered: one on the edge two triangles share is covered by both, never by neither.
const EDGE_SLACK: f64 = 1e-7;

/// A view's pixels, row by row from the top: the nearest of what was drawn, where anything was.
#[derive(Clone, Debug)]
pub struct DepthMap {
    pub width: usize,
    pub height: usize,
    /// mm in front of the camera along its middle ray; infinite where nothing was drawn.
    pub depth: Vec<f32>,
    /// The nearest triangle's unit normal, plate frame, either side; zero where nothing was drawn.
    pub normal: Vec<[f32; 3]>,
}

impl DepthMap {
    pub fn new(width: usize, height: usize) -> DepthMap {
        DepthMap { width, height, depth: vec![f32::INFINITY; width * height], normal: vec![[0.0; 3]; width * height] }
    }

    /// Something was drawn at the pixel.
    pub fn covered(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.height && self.depth[y * self.width + x].is_finite()
    }

    /// The pixel's depth (mm), where something was drawn.
    pub fn depth_at(&self, x: usize, y: usize) -> Option<f32> {
        self.covered(x, y).then(|| self.depth[y * self.width + x])
    }

    /// A triangle of plate points (mm) as `camera` sees it: drawn where it is nearer than what is
    /// there. One with a corner not in front of the camera is left out.
    pub fn triangle_mm(&mut self, camera: &ViewCamera, t: [[f64; 3]; 3]) {
        let (Some(a), Some(b), Some(c)) = (camera.view_point(t[0]), camera.view_point(t[1]), camera.view_point(t[2])) else { return };
        let (u, v) = (sub(t[1], t[0]), sub(t[2], t[0]));
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let normal = if l > 0.0 { n.map(|c| (c / l) as f32) } else { [0.0; 3] };
        self.triangle([a, b, c], normal);
    }

    /// A triangle in the view's terms: each corner [x, y] in pixels ((0, 0) the top left corner)
    /// and its depth in mm. Covers the pixels whose middles are inside it or on its edges, the
    /// depth taken across it as the camera sees it (its inverse is linear across the picture); one
    /// too small to hold a pixel's middle covers the pixel its middle is in. Worked in f64 from its
    /// own corner, so neighbours sharing an edge leave no crack between them.
    pub fn triangle(&mut self, v: [[f32; 3]; 3], normal: [f32; 3]) {
        let (lo_x, hi_x) = (v[0][0].min(v[1][0]).min(v[2][0]), v[0][0].max(v[1][0]).max(v[2][0]));
        let (lo_y, hi_y) = (v[0][1].min(v[1][1]).min(v[2][1]), v[0][1].max(v[1][1]).max(v[2][1]));
        if hi_x < 0.0 || hi_y < 0.0 || lo_x >= self.width as f32 || lo_y >= self.height as f32 || !(lo_x.is_finite() && hi_x.is_finite() && lo_y.is_finite() && hi_y.is_finite()) {
            return;
        }
        let o = [v[0][0] as f64, v[0][1] as f64];
        let c = v.map(|p| [p[0] as f64 - o[0], p[1] as f64 - o[1]]);
        let area = edge(c[0], c[1], c[2]);
        let (x0, x1) = ((lo_x - 0.5).ceil().max(0.0) as usize, ((hi_x - 0.5).floor() as isize).min(self.width as isize - 1));
        let (y0, y1) = ((lo_y - 0.5).ceil().max(0.0) as usize, ((hi_y - 0.5).floor() as isize).min(self.height as isize - 1));
        if area.abs() < 1e-12 || x1 < x0 as isize || y1 < y0 as isize {
            // no pixel's middle inside: the pixel of its middle
            let middle = [(v[0][0] + v[1][0] + v[2][0]) / 3.0, (v[0][1] + v[1][1] + v[2][1]) / 3.0, (v[0][2] + v[1][2] + v[2][2]) / 3.0];
            self.dot(middle, normal);
            return;
        }
        let inv = v.map(|p| 1.0 / p[2] as f64);
        for y in y0..=y1 as usize {
            for x in x0..=x1 as usize {
                let p = [x as f64 + 0.5 - o[0], y as f64 + 0.5 - o[1]];
                let w = [edge(c[1], c[2], p) / area, edge(c[2], c[0], p) / area, edge(c[0], c[1], p) / area];
                if w.iter().any(|&w| w < -EDGE_SLACK) {
                    continue;
                }
                let depth = 1.0 / (w[0] * inv[0] + w[1] * inv[1] + w[2] * inv[2]);
                self.put(x, y, depth as f32, normal);
            }
        }
    }

    /// One point in the view's terms ([x, y] pixels, depth mm): the pixel it is in.
    pub fn dot(&mut self, p: [f32; 3], normal: [f32; 3]) {
        if p[0] >= 0.0 && p[1] >= 0.0 && (p[0] as usize) < self.width && (p[1] as usize) < self.height {
            self.put(p[0] as usize, p[1] as usize, p[2], normal);
        }
    }

    /// A disc `radius` pixels round a point in the view's terms, at its depth: what a sample of a
    /// surface covers when the samples are that far apart.
    pub fn disc(&mut self, p: [f32; 3], radius: f32, normal: [f32; 3]) {
        let r2 = radius * radius;
        let (x0, x1) = ((p[0] - radius).floor().max(0.0), (p[0] + radius).ceil().min(self.width as f32 - 1.0));
        let (y0, y1) = ((p[1] - radius).floor().max(0.0), (p[1] + radius).ceil().min(self.height as f32 - 1.0));
        if x1 < x0 || y1 < y0 {
            return;
        }
        for y in y0 as usize..=y1 as usize {
            for x in x0 as usize..=x1 as usize {
                let (dx, dy) = (x as f32 + 0.5 - p[0], y as f32 + 0.5 - p[1]);
                if dx * dx + dy * dy <= r2 {
                    self.put(x, y, p[2], normal);
                }
            }
        }
        self.dot(p, normal);
    }

    fn put(&mut self, x: usize, y: usize, depth: f32, normal: [f32; 3]) {
        let i = y * self.width + x;
        if depth > 0.0 && depth < self.depth[i] {
            self.depth[i] = depth;
            self.normal[i] = normal;
        }
    }
}

/// Twice the signed area of a, b, p in the picture's plane.
fn edge(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiles on the pixel grid far from the view's corner, each a fan of four triangles round its
    /// middle as a traced tile is drawn: together they cover every pixel they span, no crack
    /// between them.
    #[test]
    fn tiles_on_the_pixel_grid_leave_no_crack() {
        let mut map = DepthMap::new(1024, 1024);
        let (from, step, count) = (700.5f32, 3.0f32, 60);
        for ty in 0..count {
            for tx in 0..count {
                let (x, y) = (from + tx as f32 * step, from - 600.0 + ty as f32 * step);
                let corners = [[x, y, 50.0], [x + step, y, 50.2], [x + step, y + step, 50.4], [x, y + step, 50.1]];
                let middle = [x + step * 0.5, y + step * 0.5, 50.2];
                for k in 0..4 {
                    map.triangle([middle, corners[k], corners[(k + 1) % 4]], [0.0, 0.0, 1.0]);
                }
            }
        }
        let span = (from as usize)..(from + step * count as f32) as usize;
        let rows = (from as usize - 600)..(from - 600.0 + step * count as f32) as usize;
        let missed: Vec<(usize, usize)> = rows.clone().flat_map(|y| span.clone().map(move |x| (x, y))).filter(|&(x, y)| !map.covered(x, y)).collect();
        assert!(missed.is_empty(), "{} pixels left uncovered, the first {:?}", missed.len(), &missed[..missed.len().min(8)]);
    }
}
