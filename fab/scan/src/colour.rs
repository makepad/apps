//! The cameras' colours: a keyframe's pictures as RGB, and what colour each shows a point of the
//! scan in, for how much it should be believed.

use crate::session::{camera_to_world, picture_intrinsics};
use makepad_desk_link::xr_capture::{decode_plane, encode_plane, CaptureCamera, CapturePlane, CaptureView};

/// One camera's picture of a keyframe, in colour.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    /// The camera's own values, 0..1 (its gamma is kept: the colours show as the cameras gave them).
    pub rgb: Vec<[f32; 3]>,
    /// fx, fy, cx, cy in pixels, centres at whole numbers.
    pub k: [f64; 4],
    /// The camera in the world, row-major: x right, y down, z forward.
    pub cam_to_world: [[f64; 4]; 4],
    /// How far the head's turn smears the picture during its exposure, in pixels.
    pub blur_px: f32,
}

/// A plane of `width` x `height` bytes, one byte a sample.
fn plane_bytes(plane: &CapturePlane, width: usize, height: usize) -> Result<Vec<u8>, String> {
    if plane.bytes_per_sample != 1 || plane.width as usize != width || plane.height as usize != height {
        return Err(format!("a picture's plane of {}x{} x{}, not {width}x{height} one byte a sample", plane.width, plane.height, plane.bytes_per_sample));
    }
    decode_plane(plane)
}

/// Bilinear at a real position of a plane, its edge held.
fn at(plane: &[u8], width: usize, height: usize, x: f32, y: f32) -> f32 {
    let (x, y) = (x.clamp(0.0, (width - 1) as f32), y.clamp(0.0, (height - 1) as f32));
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let v = |px: usize, py: usize| f32::from(plane[py * width + px]);
    (v(x0, y0) * (1.0 - tx) + v(x1, y0) * tx) * (1.0 - ty) + (v(x0, y1) * (1.0 - tx) + v(x1, y1) * tx) * ty
}

/// The mean luma of a colour (BT.601).
pub fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

impl Picture {
    /// The picture of a view: Y, U and V decoded (U and V at half size, sampled bilinearly) and
    /// turned to RGB with the BT.601 full-range matrix, which is what the headset's YUV_420_888
    /// pictures use. A view with only a Y plane is grey.
    pub fn of(view: &CaptureView, camera: &CaptureCamera) -> Result<Picture, String> {
        let lens = camera.lens_pose.as_ref().ok_or("the camera gives no lens pose")?;
        let y = view.planes.first().ok_or("a picture without planes")?;
        let (width, height) = (y.width as usize, y.height as usize);
        let luma = plane_bytes(y, width, height)?;
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let chroma = match (view.planes.get(1), view.planes.get(2)) {
            (Some(u), Some(v)) => Some((plane_bytes(u, cw, ch)?, plane_bytes(v, cw, ch)?)),
            _ => None,
        };
        let k = picture_intrinsics(camera)?;
        let rgb = (0..width * height)
            .map(|i| {
                let (x, y) = ((i % width) as f32, (i / width) as f32);
                let value = f32::from(luma[i]);
                match &chroma {
                    // (a chroma sample stands for the 2x2 pixels round it: its centre is between them)
                    Some((u, v)) => {
                        let (cx, cy) = ((x - 0.5) / 2.0, (y - 0.5) / 2.0);
                        let (u, v) = (at(u, cw, ch, cx, cy) - 128.0, at(v, cw, ch, cx, cy) - 128.0);
                        [value + 1.402 * v, value - 0.344_136 * u - 0.714_136 * v, value + 1.772 * u].map(|c| (c / 255.0).clamp(0.0, 1.0))
                    }
                    None => [value / 255.0; 3],
                }
            })
            .collect();
        let turn = view.angular_velocity.map(f64::from);
        let turn = (turn[0] * turn[0] + turn[1] * turn[1] + turn[2] * turn[2]).sqrt();
        Ok(Picture { width, height, rgb, k, cam_to_world: camera_to_world(&view.head, lens), blur_px: (turn * f64::from(view.exposure_ns) * 1e-9 * k[0]) as f32 })
    }

    /// This picture with `factor` times fewer pixels a side, as `GrayView::downsampled` does: each new pixel the mean of
    /// its block, the intrinsics moving with its centre.
    pub fn downsampled(&self, factor: usize) -> Result<Picture, String> {
        if factor == 0 || factor > self.width || factor > self.height {
            return Err("the factor must be positive and fit the image".into());
        }
        let (width, height) = (self.width / factor, self.height / factor);
        let rgb = (0..width * height)
            .map(|i| {
                let (x, y) = (i % width * factor, i / width * factor);
                let mut sum = [0.0f32; 3];
                for row in y..y + factor {
                    for c in &self.rgb[row * self.width + x..row * self.width + x + factor] {
                        for k in 0..3 {
                            sum[k] += c[k];
                        }
                    }
                }
                sum.map(|v| v / (factor * factor) as f32)
            })
            .collect();
        let scale = factor as f64;
        let shift = (scale - 1.0) / 2.0;
        let k = [self.k[0] / scale, self.k[1] / scale, (self.k[2] - shift) / scale, (self.k[3] - shift) / scale];
        Ok(Picture { width, height, rgb, k, cam_to_world: self.cam_to_world, blur_px: self.blur_px / factor as f32 })
    }

    /// The colour of a pixel (a whole position inside the picture).
    pub fn pixel(&self, x: usize, y: usize) -> Option<[f32; 3]> {
        (x < self.width && y < self.height).then(|| self.rgb[y * self.width + x])
    }

    /// The colour where `point` shows, bilinear, and how far to believe it: the cosine of the angle between `normal` and
    /// the ray back to the camera (0 for a surface turned away), over 1 plus the smear squared. None behind the camera or
    /// outside the picture.
    pub fn see(&self, point: [f32; 3], normal: [f32; 3]) -> Option<([f32; 3], f32)> {
        let m = &self.cam_to_world;
        let d: [f64; 3] = std::array::from_fn(|i| f64::from(point[i]) - m[i][3]);
        let p: [f64; 3] = std::array::from_fn(|a| m[0][a] * d[0] + m[1][a] * d[1] + m[2][a] * d[2]);
        if p[2] <= 1e-6 {
            return None;
        }
        let (x, y) = (self.k[0] * p[0] / p[2] + self.k[2], self.k[1] * p[1] / p[2] + self.k[3]);
        if !(x >= 0.0 && y >= 0.0 && x <= (self.width - 1) as f64 && y <= (self.height - 1) as f64) {
            return None;
        }
        let (x, y) = (x as f32, y as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (tx, ty) = (x - x0 as f32, y - y0 as f32);
        let colour: [f32; 3] = std::array::from_fn(|c| {
            let v = |px: usize, py: usize| self.rgb[py * self.width + px][c];
            (v(x0, y0) * (1.0 - tx) + v(x1, y0) * tx) * (1.0 - ty) + (v(x0, y1) * (1.0 - tx) + v(x1, y1) * tx) * ty
        });
        let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let cosine = -(0..3).map(|i| f64::from(normal[i]) * d[i]).sum::<f64>() / length;
        let blur = f64::from(self.blur_px);
        Some((colour, (cosine.max(0.0) / (1.0 + blur * blur)) as f32))
    }
}

/// Y, U and V planes (I420, BT.601 full range) of an RGB picture (0..1 a channel), U and V the mean of each 2x2 block.
pub fn i420_planes(width: usize, height: usize, rgb: &[[f32; 3]]) -> Vec<CapturePlane> {
    let byte = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    let y: Vec<u8> = rgb.iter().map(|c| byte(255.0 * luma(*c))).collect();
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let (mut u, mut v) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
    for cy in 0..ch {
        for cx in 0..cw {
            let (mut sum, mut count) = ([0.0f32; 3], 0.0);
            for py in 2 * cy..(2 * cy + 2).min(height) {
                for px in 2 * cx..(2 * cx + 2).min(width) {
                    let c = rgb[py * width + px];
                    for k in 0..3 {
                        sum[k] += c[k];
                    }
                    count += 1.0;
                }
            }
            let [r, g, b] = sum.map(|s| 255.0 * s / count);
            u.push(byte(-0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0));
            v.push(byte(0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0));
        }
    }
    vec![encode_plane(width as u32, height as u32, 1, &y), encode_plane(cw as u32, ch as u32, 1, &u), encode_plane(cw as u32, ch as u32, 1, &v)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_desk_link::xr_capture::{CaptureLensPose, CapturePose};

    const W: usize = 32;
    const H: usize = 24;

    fn camera() -> CaptureCamera {
        CaptureCamera {
            size: [W as u32, H as u32],
            intrinsics: Some([20.0, 20.0, 16.0, 12.0, 0.0]),
            array: [0, 0, W as i32, H as i32],
            active: [0, 0, W as i32, H as i32],
            lens_pose: Some(CaptureLensPose { rotation: [0.0, 0.0, 0.0, 1.0], translation: [0.0; 3], reference: 1 }),
            ..Default::default()
        }
    }

    fn view(rgb: &[[f32; 3]], turn: f32, exposure_ns: u32) -> CaptureView {
        CaptureView { head: CapturePose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.0; 3] }, angular_velocity: [turn, 0.0, 0.0], exposure_ns, planes: i420_planes(W, H, rgb), ..Default::default() }
    }

    #[test]
    fn a_picture_reads_back_in_colour_and_weighs_what_it_sees() {
        // two colours, the left and right halves
        let (a, b) = ([0.8, 0.3, 0.2], [0.1, 0.5, 0.9]);
        let rgb: Vec<[f32; 3]> = (0..W * H).map(|i| if i % W < W / 2 { a } else { b }).collect();
        let picture = Picture::of(&view(&rgb, 0.0, 0), &camera()).unwrap();
        let mut worst = 0.0f32;
        for y in 0..H {
            for x in (0..W).filter(|x| !(W / 2 - 3..W / 2 + 3).contains(x)) {
                let truth = rgb[y * W + x];
                let got = picture.pixel(x, y).unwrap();
                for c in 0..3 {
                    worst = worst.max((got[c] - truth[c]).abs());
                }
            }
        }
        assert!(worst < 2.0 / 255.0, "a pixel away from a chroma edge is {:.1}/255 off its colour", worst * 255.0);
        let grey = view(&rgb, 0.0, 0);
        let only_luma = CaptureView { planes: grey.planes[..1].to_vec(), ..grey };
        let plain = Picture::of(&only_luma, &camera()).unwrap();
        assert!(plain.rgb.iter().all(|c| c[0] == c[1] && c[1] == c[2]), "a picture with only a Y plane is grey");

        // a point straight ahead, one seen at 60 degrees, one behind, one outside
        let ahead = [0.0, 0.0, 1.0];
        let (_, front) = picture.see(ahead, [0.0, 0.0, -1.0]).unwrap();
        assert!(front > 0.99, "a point seen face on weighs {front}");
        let (_, slant) = picture.see(ahead, [0.866_025_4, 0.0, -0.5]).unwrap();
        assert!((slant - 0.5).abs() < 0.01, "a point seen at 60 degrees weighs {slant}, not about half");
        assert!(picture.see([0.0, 0.0, -1.0], [0.0, 0.0, 1.0]).is_none(), "a point behind the camera is not seen");
        assert!(picture.see([5.0, 0.0, 1.0], [0.0, 0.0, -1.0]).is_none(), "a point outside the picture is not seen");
        let (_, back) = picture.see(ahead, [0.0, 0.0, 1.0]).unwrap();
        assert_eq!(back, 0.0, "a surface turned away weighs nothing");
        // a left point reads the left colour, a right point the right
        let left = picture.see([-0.4, 0.0, 1.0], [0.0, 0.0, -1.0]).unwrap().0;
        let right = picture.see([0.4, 0.0, 1.0], [0.0, 0.0, -1.0]).unwrap().0;
        assert!((0..3).all(|c| (left[c] - a[c]).abs() < 3.0 / 255.0 && (right[c] - b[c]).abs() < 3.0 / 255.0), "left {left:?} right {right:?}");
        // a blurred view (10 rad/s for 20 ms is 4 px here) weighs less
        let blurred = Picture::of(&view(&rgb, 10.0, 20_000_000), &camera()).unwrap();
        let (_, smeared) = blurred.see(ahead, [0.0, 0.0, -1.0]).unwrap();
        assert!(smeared < 0.2 * front, "a smeared picture weighs {smeared} against {front}");
        // fewer pixels: the mean of each block
        let small = picture.downsampled(2).unwrap();
        assert!((small.pixel(2, 3).unwrap()[0] - a[0]).abs() < 3.0 / 255.0 && (small.width, small.height) == (W / 2, H / 2), "a downsampled picture keeps its colours");
    }
}
