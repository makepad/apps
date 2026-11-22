//! A scan as it is captured: what each record says, where each keyframe's cameras stood, and the
//! headset's own depth image of each keyframe fused into one surface, so the scan shows from its
//! first keyframe and grows as the wearer walks. The surface is coarse (the headset's depth is):
//! the reconstruction from the pictures replaces it where it has run.
//!
//! World: the headset's tracking space, y against gravity, metres. A depth image is the runtime's:
//! per eye a pose and a field of view (the tangents' angles: left, right, up, down), 16-bit
//! values of a projection with `near` and `far` (`far` not past `near`: infinite), row 0 at the
//! bottom, the view looking along its -z.

use crate::colour::{luma, Picture};
use crate::session::{camera_to_world, rotation_of};
use makepad_desk_link::xr_capture::{decode_plane, CaptureCamera, CaptureDepth, CaptureKeyframe, CaptureRecord};
use std::collections::HashMap;

/// The surface's cells, metres.
pub const VOXEL: f32 = 0.01;
/// How far each side of a depth sample its ray writes distances, in cells.
const BAND: i32 = 3;
/// The nearest and farthest depth the surface is made of, metres: nearer is the wearer's own
/// hands, farther the headset's depth is coarser than the room's shapes.
const NEAREST: f32 = 0.15;
const FARTHEST: f32 = 3.0;
/// The most a cell's mean weighs: it keeps following what the newest keyframes say.
const MOST_WEIGHT: f32 = 32.0;
/// Within this of where a keyframe was taken from the surface is given at its cells' own width,
/// metres: an arm's reach and the table in front.
const NEAR: f32 = 1.2;
/// The widest the surface's cells are given: this many doublings of their width.
const WIDEST: i32 = 6;

#[derive(Clone, Copy)]
struct Cell {
    /// The signed distance to the surface along the rays that saw it, metres: positive in front.
    distance: f32,
    weight: f32,
    /// The keyframe that saw it first.
    first: u32,
    /// What the keyframes' pictures showed here: the sum of the colours, each times its weight, and the weights.
    colour: [f32; 3],
    colour_weight: f32,
}

/// Fewest samples a keyframe needs in cells already coloured for its exposure to be matched to them.
const GAIN_SAMPLES: usize = 200;
/// The most a keyframe's exposure is matched by.
const GAIN: std::ops::RangeInclusive<f32> = 0.5..=2.0;
/// What a cell no picture saw shows as.
const UNSEEN: [f32; 3] = [0.5; 3];

/// A point of the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Surfel {
    pub point: [f32; 3],
    /// Unit, out of the surface.
    pub normal: [f32; 3],
    /// The keyframe that saw it first.
    pub first: u32,
    /// The width of the square it stands for, metres.
    pub size: f32,
    /// The pictures' colour there, 0..1: the mean over what saw it; mid grey where none did.
    pub colour: [f32; 3],
}

/// The surface as points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Surface {
    pub points: Vec<Surfel>,
}

/// Where a keyframe was taken from.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveKeyframe {
    pub index: u32,
    pub head: [f32; 3],
    /// Each picture's camera in the world, row-major: its x right, y down, z forward.
    pub cameras: Vec<[[f32; 4]; 3]>,
    /// The headset's depth came with it.
    pub depth: bool,
    /// How far its depth lay from the surface the keyframes before it made, millimetres (the
    /// mean, over its samples that fell in cells seen before, of what the cell held against what
    /// the sample writes there); None where too few did.
    pub agreement_mm: Option<f32>,
}

/// What a record was.
#[derive(Clone, Debug, PartialEq)]
pub enum Taken {
    Cameras(usize),
    Status { state: u8, text: String },
    Keyframe(LiveKeyframe),
    Preview,
}

#[derive(Default)]
pub struct LiveScan {
    cameras: Vec<CaptureCamera>,
    cells: HashMap<[i32; 3], Cell>,
    keyframes: u32,
    /// Where each keyframe was taken from.
    heads: Vec<[f32; 3]>,
}

/// What a point of the surface gathers from the cells and points it stands for: sums, and what they count.
#[derive(Default)]
struct Gather {
    point: [f32; 3],
    count: f32,
    slope: [f32; 3],
    first: u32,
    colour: [f32; 3],
    weight: f32,
}

impl Gather {
    /// The point of the means; the normal is the slope's sum, not yet of length one.
    fn surfel(&self, size: f32) -> Surfel {
        let colour = if self.weight > 0.0 { self.colour.map(|v| v / self.weight) } else { UNSEEN };
        Surfel { point: self.point.map(|v| v / self.count), normal: self.slope, first: self.first, size, colour }
    }
}

fn cell_of(p: [f32; 3]) -> [i32; 3] {
    p.map(|v| (v / VOXEL).floor() as i32)
}

/// The depth along the view of a 16-bit value of the runtime's projection.
fn metres(value: u16, near: f32, far: f32) -> Option<f32> {
    if value == 0 || value == u16::MAX {
        return None;
    }
    let ndc = f32::from(value) / f32::from(u16::MAX) * 2.0 - 1.0;
    let z = if far > near { 2.0 * far * near / ((far + near) - ndc * (far - near)) } else { 2.0 * near / (1.0 - ndc) };
    z.is_finite().then_some(z)
}

impl LiveScan {
    /// The surface's cells: what it holds.
    pub fn cells(&self) -> usize {
        self.cells.len()
    }

    /// Takes a record in its order of arrival.
    pub fn take(&mut self, record: &CaptureRecord) -> Result<Taken, String> {
        match record {
            CaptureRecord::Cameras(list) => {
                self.cameras = list.clone();
                Ok(Taken::Cameras(list.len()))
            }
            CaptureRecord::Status { state, text } => Ok(Taken::Status { state: *state, text: text.clone() }),
            CaptureRecord::Preview { .. } => Ok(Taken::Preview),
            CaptureRecord::Keyframe(keyframe) => self.keyframe(keyframe).map(Taken::Keyframe),
        }
    }

    fn keyframe(&mut self, keyframe: &CaptureKeyframe) -> Result<LiveKeyframe, String> {
        let index = self.keyframes;
        self.keyframes += 1;
        let cameras = keyframe
            .views
            .iter()
            .map(|view| {
                let lens = self.cameras.get(view.camera as usize).and_then(|c| c.lens_pose.as_ref());
                let m = match lens {
                    Some(lens) => camera_to_world(&view.head, lens),
                    // (no lens pose: the head itself, looking along its -z)
                    None => {
                        let h = rotation_of(view.head.orientation);
                        let p = view.head.position.map(f64::from);
                        let mut m = [[0.0; 4]; 4];
                        for i in 0..3 {
                            m[i] = [h[i][0], -h[i][1], -h[i][2], p[i]];
                        }
                        m
                    }
                };
                std::array::from_fn(|i| m[i].map(|v| v as f32))
            })
            .collect();
        let head = match (keyframe.views.first(), &keyframe.depth) {
            (Some(view), _) => view.head.position,
            (None, Some(depth)) => depth.views[0].pose.position,
            (None, None) => [0.0; 3],
        };
        self.heads.push(head);
        // (a picture that cannot be read as one gives no colour: the depth still fuses)
        let pictures: Vec<Picture> = keyframe.views.iter().filter_map(|view| Picture::of(view, self.cameras.get(view.camera as usize)?).ok()).collect();
        let agreement_mm = match &keyframe.depth {
            Some(depth) => self.fuse(depth, index, &pictures)?,
            None => None,
        };
        Ok(LiveKeyframe { index, head, cameras, depth: keyframe.depth.is_some(), agreement_mm })
    }

    /// A depth image into the surface, coloured by the keyframe's pictures (a depth sample is seen by the pictures of its
    /// own keyframe, a few centimetres from the depth's eye: that is what keeps what is behind a surface from colouring it).
    /// Returns how far it lay from the surface before it.
    fn fuse(&mut self, depth: &CaptureDepth, keyframe: u32, pictures: &[Picture]) -> Result<Option<f32>, String> {
        let layers = depth.layers.max(1) as usize;
        let (width, height) = (depth.texels.width as usize, depth.texels.height as usize / layers);
        if depth.texels.bytes_per_sample != 2 || width == 0 || height == 0 {
            return Err("a depth image that is not 16 bits a sample".into());
        }
        let bytes = decode_plane(&depth.texels)?;
        let (mut off, mut measured) = (0.0f64, 0usize);
        let mut samples: Vec<([f32; 3], [f32; 3])> = Vec::new();
        for (eye, view) in depth.views.iter().enumerate().take(layers) {
            let r = rotation_of(view.pose.orientation);
            let at = view.pose.position;
            let [left, right, up, down] = view.fov.map(f32::tan);
            let layer = &bytes[eye * width * height * 2..(eye + 1) * width * height * 2];
            for y in 0..height {
                for x in 0..width {
                    let i = (y * width + x) * 2;
                    let Some(z) = metres(u16::from_le_bytes([layer[i], layer[i + 1]]), depth.near, depth.far) else { continue };
                    let ndc = [(x as f32 + 0.5) / width as f32 * 2.0 - 1.0, (y as f32 + 0.5) / height as f32 * 2.0 - 1.0];
                    let v = [0.5 * z * (ndc[0] * (right - left) + right + left), 0.5 * z * (ndc[1] * (up - down) + up + down), -z];
                    let reach = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                    if !(NEAREST..=FARTHEST).contains(&reach) {
                        continue;
                    }
                    let turned: [f32; 3] = std::array::from_fn(|k| (r[k][0] * f64::from(v[0]) + r[k][1] * f64::from(v[1]) + r[k][2] * f64::from(v[2])) as f32);
                    let point: [f32; 3] = std::array::from_fn(|k| at[k] + turned[k]);
                    samples.push((point, turned.map(|c| c / reach)));
                }
            }
        }
        // each sample's colour: the pictures' mean, each weighed by how squarely it looks at the surface (taken as facing the
        // depth's eye) and by how sharp it is, and the weights' sum
        let colours: Vec<Option<([f32; 3], f32)>> = samples
            .iter()
            .map(|(point, ray)| {
                let (mut sum, mut weight) = ([0.0f32; 3], 0.0f32);
                for (colour, w) in pictures.iter().filter_map(|p| p.see(*point, ray.map(|c| -c))) {
                    for k in 0..3 {
                        sum[k] += colour[k] * w;
                    }
                    weight += w;
                }
                (weight > 0.0).then(|| (sum.map(|v| v / weight), weight))
            })
            .collect();
        // against the surface as it stood, before any of this image is in it; and how bright this keyframe's pictures
        // are against what the cells hold
        let mut ratios: Vec<f32> = Vec::new();
        for ((point, ray), colour) in samples.iter().zip(&colours) {
            let key = cell_of(*point);
            if let Some(cell) = self.cells.get(&key) {
                let distance: f32 = (0..3).map(|k| (point[k] - (key[k] as f32 + 0.5) * VOXEL) * ray[k]).sum();
                off += f64::from((cell.distance - distance).abs());
                measured += 1;
                if let Some((colour, _)) = colour.filter(|_| cell.colour_weight > 0.0) {
                    let (held, seen) = (luma(cell.colour.map(|v| v / cell.colour_weight)), luma(colour));
                    if seen > 1e-3 && held > 1e-3 {
                        ratios.push(held / seen);
                    }
                }
            }
        }
        let gain = if ratios.len() < GAIN_SAMPLES {
            1.0
        } else {
            let middle = ratios.len() / 2;
            ratios.select_nth_unstable_by(middle, f32::total_cmp);
            ratios[middle].clamp(*GAIN.start(), *GAIN.end())
        };
        for ((point, ray), colour) in samples.iter().zip(&colours) {
            for step in -BAND..=BAND {
                let key = cell_of(std::array::from_fn(|k| point[k] + ray[k] * step as f32 * VOXEL));
                let centre: [f32; 3] = std::array::from_fn(|k| (key[k] as f32 + 0.5) * VOXEL);
                let distance: f32 = (0..3).map(|k| (point[k] - centre[k]) * ray[k]).sum();
                if distance.abs() > BAND as f32 * VOXEL {
                    continue;
                }
                let cell = self.cells.entry(key).or_insert(Cell { distance: 0.0, weight: 0.0, first: keyframe, colour: [0.0; 3], colour_weight: 0.0 });
                cell.distance = (cell.distance * cell.weight + distance) / (cell.weight + 1.0);
                cell.weight = (cell.weight + 1.0).min(MOST_WEIGHT);
                // the colour goes to the cells the distance does, the nearer to the surface the more: the samples lie a
                // little more than a cell apart, so the cell a sample lies in is not the only one at the surface
                if let Some((colour, w)) = colour {
                    let w = w * (1.0 - distance.abs() / ((BAND + 1) as f32 * VOXEL));
                    for k in 0..3 {
                        cell.colour[k] += (colour[k] * gain).min(1.0) * w;
                    }
                    cell.colour_weight += w;
                }
            }
        }
        Ok((measured >= 100).then(|| (1000.0 * off / measured as f64) as f32))
    }

    /// The surface as at most about `most` points: where the distance changes sign between a
    /// cell and its next, one point a cell near the wearer, wider cells farther off.
    pub fn surface(&self, most: usize) -> Surface {
        let at = |key: [i32; 3]| self.cells.get(&key).map(|c| c.distance);
        let mut found: HashMap<[i32; 3], Gather> = HashMap::new();
        for (key, cell) in &self.cells {
            for axis in 0..3 {
                let mut next = *key;
                next[axis] += 1;
                let Some(other) = self.cells.get(&next) else { continue };
                if (cell.distance > 0.0) == (other.distance > 0.0) {
                    continue;
                }
                let t = cell.distance / (cell.distance - other.distance);
                let mut point: [f32; 3] = std::array::from_fn(|k| (key[k] as f32 + 0.5) * VOXEL);
                point[axis] += t * VOXEL;
                // out of the surface: up the distance, by what the cells around say
                let slope: [f32; 3] = std::array::from_fn(|k| {
                    let (mut low, mut high) = (*key, *key);
                    low[k] -= 1;
                    high[k] += 1;
                    match (at(low), at(high)) {
                        (Some(a), Some(b)) => 0.5 * (b - a),
                        (Some(a), None) => cell.distance - a,
                        (None, Some(b)) => b - cell.distance,
                        (None, None) => 0.0,
                    }
                });
                let entry = found.entry(*key).or_default();
                for k in 0..3 {
                    entry.point[k] += point[k];
                    entry.slope[k] += slope[k];
                    entry.colour[k] += cell.colour[k] + other.colour[k];
                }
                entry.count += 1.0;
                entry.weight += cell.colour_weight + other.colour_weight;
                entry.first = cell.first.min(other.first);
            }
        }
        // (in the cells' order: a map's own order is not one, and sums follow their order)
        let mut fine: Vec<([i32; 3], Surfel, f32)> = found.into_iter().map(|(key, g)| (key, g.surfel(VOXEL), g.weight)).collect();
        fine.sort_by_key(|(key, _, _)| *key);
        // how far each point is from the nearest place a keyframe was taken from
        let reach: Vec<f32> = fine
            .iter()
            .map(|(_, s, _)| self.heads.iter().map(|h| (0..3).map(|k| (s.point[k] - h[k]).powi(2)).sum::<f32>()).fold(f32::INFINITY, f32::min).sqrt())
            .collect();
        // Near the wearer the surface keeps its cells; each doubling of the distance past that it
        // is given in cells of twice the width: the headset's depth is coarser there, and what is
        // scanned is near.
        // Past `most`, the distances that count as near shrink until it fits.
        let mut near = NEAR;
        let mut points = loop {
            let mut wider: HashMap<(u8, [i32; 3]), Gather> = HashMap::new();
            for ((_, s, weight), reach) in fine.iter().zip(&reach) {
                let level = if *reach < near { 0u8 } else { (((reach / near).log2().floor() as i32) + 1).clamp(1, WIDEST) as u8 };
                let width = VOXEL * f32::from(1u8 << level);
                let entry = wider.entry((level, s.point.map(|v| (v / width).floor() as i32))).or_default();
                for k in 0..3 {
                    entry.point[k] += s.point[k];
                    entry.slope[k] += s.normal[k];
                    entry.colour[k] += s.colour[k] * weight;
                }
                entry.count += 1.0;
                entry.weight += weight;
                // (the newest of what it stands for: new surface shows as new)
                entry.first = entry.first.max(s.first);
            }
            if wider.len() <= most.max(1) || near < 1e-4 {
                let mut list: Vec<_> = wider.into_iter().collect();
                list.sort_by_key(|(key, _)| *key);
                break list.into_iter().map(|((level, _), g)| g.surfel(VOXEL * f32::from(1u8 << level))).collect::<Vec<Surfel>>();
            }
            near *= 0.7;
        };
        for s in &mut points {
            let length = (s.normal[0] * s.normal[0] + s.normal[1] * s.normal[1] + s.normal[2] * s.normal[2]).sqrt();
            s.normal = if length > 1e-9 { s.normal.map(|v| v / length) } else { [0.0, 1.0, 0.0] };
        }
        Surface { points }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colour::i420_planes;
    use makepad_desk_link::xr_capture::{encode_plane, CaptureEye, CaptureLensPose, CapturePlane, CapturePose, CaptureView};

    /// The floor's colour and the step's.
    const FLOOR: [f32; 3] = [0.6, 0.3, 0.2];
    const STEP: [f32; 3] = [0.2, 0.4, 0.6];
    const SIZE: usize = 96;
    const NEAR: f32 = 0.1;
    const HALF_FOV: f32 = 0.6;

    /// The depth image of the floor y = 0 and a 20 cm step on it (x > 0.3), from a head at
    /// `position` looking straight down with its up along the world's -z, as the runtime gives it.
    fn looking_down(position: [f32; 3]) -> CaptureDepth {
        // a quarter turn about x: the view's -z is the world's -y, its y the world's -z
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let pose = CapturePose { orientation: [-half, 0.0, 0.0, half], position };
        let fov = [-HALF_FOV, HALF_FOV, HALF_FOV, -HALF_FOV];
        let mut texels = Vec::new();
        for y in 0..SIZE {
            for x in 0..SIZE {
                let ndc = [(x as f32 + 0.5) / SIZE as f32 * 2.0 - 1.0, (y as f32 + 0.5) / SIZE as f32 * 2.0 - 1.0];
                let ray = [ndc[0] * HALF_FOV.tan(), ndc[1] * HALF_FOV.tan()];
                // the step's top where the ray reaches it, else its side, else the floor
                let floor = position[1];
                let z = if position[0] + ray[0] * (floor - 0.2) > 0.3 {
                    floor - 0.2
                } else if position[0] + ray[0] * floor > 0.3 {
                    (0.3 - position[0]) / ray[0]
                } else {
                    floor
                };
                let value = (1.0 - NEAR / z) * f32::from(u16::MAX);
                texels.extend((value.round() as u16).to_le_bytes());
            }
        }
        CaptureDepth { time_ns: 0, near: NEAR, far: 0.0, views: [CaptureEye { pose, fov }; 2], layers: 1, texels: encode_plane(SIZE as u32, SIZE as u32, 2, &texels) }
    }

    /// The picture of the same scene from the camera on the head at `position` (the test's lens: 3 cm right and 6 cm ahead
    /// of the head, looking down as the depth does), the floor and the step in their colours times `brightness`.
    fn looking_down_in_colour(position: [f32; 3], brightness: f32) -> Vec<CapturePlane> {
        let focal = SIZE as f32 / 2.0 / HALF_FOV.tan();
        let eye = [position[0] + 0.03, position[1], position[2] - 0.06];
        let rgb: Vec<[f32; 3]> = (0..SIZE * SIZE)
            .map(|i| {
                // the picture's x is the world's x, its y the world's z, its forward the world's -y
                let ray_x = ((i % SIZE) as f32 - (SIZE as f32 - 1.0) / 2.0) / focal;
                let reaches = |height: f32| eye[0] + ray_x * (eye[1] - height) > 0.3;
                let colour = if reaches(0.2) || reaches(0.0) { STEP } else { FLOOR };
                colour.map(|v| v * brightness)
            })
            .collect();
        i420_planes(SIZE, SIZE, &rgb)
    }

    fn keyframe(index: u32, position: [f32; 3], brightness: f32) -> CaptureRecord {
        let head = CapturePose { orientation: [0.0, 0.0, 0.0, 1.0], position };
        let planes = looking_down_in_colour(position, brightness);
        CaptureRecord::Keyframe(CaptureKeyframe { index, views: vec![CaptureView { camera: 0, head, planes, ..Default::default() }], depth: Some(looking_down(position)) })
    }

    /// The floor's surfels (away from the step and the depth's edge) are the floor's colour, the step's the step's, within `bar` a channel.
    fn assert_coloured(scan: &LiveScan, bar: f32, when: &str) {
        let (mut floor, mut step) = (0, 0);
        for s in &scan.surface(usize::MAX).points {
            let truth = if s.point[1].abs() < VOXEL && s.point[0] > -0.4 && s.point[0] < 0.25 && s.point[2].abs() < 0.4 {
                floor += 1;
                FLOOR
            } else if (s.point[1] - 0.2).abs() < VOXEL && s.point[0] > 0.35 && s.point[0] < 0.45 && s.point[2].abs() < 0.3 {
                step += 1;
                STEP
            } else {
                continue;
            };
            let worst = (0..3).map(|c| (s.colour[c] - truth[c]).abs()).fold(0.0, f32::max);
            assert!(worst <= bar, "{when}: a surfel at {:?} is {:?}, {worst:.3} a channel off {truth:?}", s.point, s.colour);
        }
        assert!(floor > 1000 && step > 200, "{when}: the floor and the step were checked on {floor} and {step} surfels");
    }

    #[test]
    fn a_scan_grows_from_its_first_keyframe_where_the_headset_saw_it() {
        let mut scan = LiveScan::default();
        let lens = CaptureLensPose { rotation: [-std::f32::consts::FRAC_1_SQRT_2, 0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2], translation: [0.03, 0.0, -0.06], reference: 1 };
        let camera = CaptureCamera {
            size: [SIZE as u32; 2],
            intrinsics: Some([SIZE as f32 / 2.0 / HALF_FOV.tan(), SIZE as f32 / 2.0 / HALF_FOV.tan(), SIZE as f32 / 2.0, SIZE as f32 / 2.0, 0.0]),
            array: [0, 0, SIZE as i32, SIZE as i32],
            active: [0, 0, SIZE as i32, SIZE as i32],
            lens_pose: Some(lens),
            ..Default::default()
        };
        assert_eq!(scan.take(&CaptureRecord::Cameras(vec![camera])).unwrap(), Taken::Cameras(1));
        assert_eq!(scan.take(&CaptureRecord::Status { state: 2, text: "on".into() }).unwrap(), Taken::Status { state: 2, text: "on".into() });
        let Taken::Keyframe(first) = scan.take(&keyframe(0, [0.0, 1.0, 0.0], 1.0)).unwrap() else { panic!("a keyframe") };
        assert!(first.depth && first.agreement_mm.is_none() && first.head == [0.0, 1.0, 0.0], "the first keyframe has nothing before it: {first:?}");
        let camera = first.cameras[0];
        assert!((camera[0][3] - 0.03).abs() < 1e-6 && (camera[2][3] + 0.06).abs() < 1e-6, "the camera stands where its lens is on the head: {camera:?}");
        let one = scan.surface(usize::MAX);
        assert!(one.points.len() > 2000, "the first keyframe alone shows a surface: {}", one.points.len());
        assert!(one.points.iter().any(|s| s.size == VOXEL) && one.points.iter().any(|s| s.size > VOXEL), "its cells are kept near the wearer and widened farther off");
        assert!(one.points.iter().filter(|s| s.point[0].hypot(s.point[2]) < 0.4).all(|s| s.size == VOXEL), "under the wearer the surface keeps its cells");
        // the floor at y = 0 and the step's top at y = 0.2, each within a cell, facing up
        let (mut floor, mut step, mut other) = (0, 0, 0);
        for s in &one.points {
            let wall = (s.point[0] - 0.3).abs() < 3.0 * VOXEL;
            if s.point[1].abs() < VOXEL && s.point[0] < 0.3 {
                floor += 1;
                assert!(s.normal[1] > 0.9, "the floor faces up: {s:?}");
            } else if (s.point[1] - 0.2).abs() < VOXEL && s.point[0] > 0.3 {
                step += 1;
            } else if !wall {
                other += 1;
            }
        }
        assert!(floor > 1500 && step > 300, "the floor and the step are where they are: {floor} {step}");
        assert!(other * 50 < one.points.len(), "points off the floor and the step: {other} of {}", one.points.len());
        // a second keyframe half a metre on: it agrees with the first where they overlap, and adds its own
        let Taken::Keyframe(second) = scan.take(&keyframe(1, [-0.5, 1.0, 0.0], 1.0)).unwrap() else { panic!("a keyframe") };
        assert!(second.agreement_mm.is_some_and(|mm| mm < 3.0), "a second look at the same floor agrees with the first: {:?}", second.agreement_mm);
        let two = scan.surface(usize::MAX);
        assert!(two.points.len() > one.points.len() + 1000, "the surface grew: {} to {}", one.points.len(), two.points.len());
        assert!(two.points.iter().any(|s| s.first == 1 && s.point[0] < -0.7), "what only the second saw is marked as its own");
        assert!(two.points.iter().filter(|s| s.point[0] > 0.2).all(|s| s.first == 0), "what the second did not reach stays the first's");
        assert_coloured(&scan, 0.03, "after two keyframes");
        assert!(one.points.iter().chain(&two.points).all(|s| s.colour.iter().all(|c| (0.0..=1.0).contains(c))), "a colour is out of 0..1");
        // fewer points asked: wider cells, never more than asked, the same surface
        let few = scan.surface(800);
        assert!(few.points.len() <= 800 && few.points.iter().all(|s| s.size > VOXEL), "{}", few.points.len());
        assert!(few.points.iter().all(|s| s.point[1] > -VOXEL && s.point[1] < 0.2 + VOXEL));
        assert_eq!(scan.surface(800), few, "the same surface gives the same points");
        // a keyframe whose pictures came out 1.5 times brighter (the camera's exposure): the colours stay what they were
        scan.take(&keyframe(2, [-0.25, 1.0, 0.0], 1.5)).unwrap();
        assert_coloured(&scan, 0.05, "after a brighter keyframe");
        // a keyframe with no depth is still a keyframe
        let bare = CaptureRecord::Keyframe(CaptureKeyframe { index: 3, views: Vec::new(), depth: None });
        assert!(matches!(scan.take(&bare).unwrap(), Taken::Keyframe(LiveKeyframe { index: 3, depth: false, .. })));
    }
}
