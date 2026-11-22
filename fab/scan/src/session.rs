//! A capture session's records (`fab/app/src/scan.rs` keeps them) read back as posed views.
//!
//! The views count as `makepad_ai_geometry::views` does: x right, y down, z forward, pixel centres at
//! whole numbers. The head is the runtime's (x right, y up, z back, local space: y against gravity).
//! Camera2 gives a lens by the rotation R from its reference to the camera and its centre t there:
//! the camera's axes in the head's are the rows of R, its centre t.

use makepad_ai_geometry::views::GrayView;
use makepad_desk_link::xr_capture::{decode_plane, CaptureCamera, CaptureLensPose, CapturePose, CaptureRecord, CaptureView};
use std::path::Path;

/// One camera's picture of a keyframe.
pub struct SessionView {
    pub view: GrayView,
    pub keyframe: usize,
    pub camera: u8,
    pub velocity: [f64; 3],
    pub angular_velocity: [f64; 3],
    pub exposure_ns: u32,
    pub rolling_skew_ns: u32,
}

pub struct Session {
    pub views: Vec<SessionView>,
    /// Keyframes in the folder (with every one of them read or not).
    pub keyframes: usize,
    /// Records that did not decode (one cut short by the end of the session).
    pub unreadable: usize,
}

/// The rotation of a unit quaternion (x, y, z, w), row-major.
pub fn rotation_of(q: [f32; 4]) -> [[f64; 3]; 3] {
    let [x, y, z, w] = q.map(f64::from);
    let n = (x * x + y * y + z * z + w * w).sqrt();
    let [x, y, z, w] = [x / n, y / n, z / n, w / n];
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

/// The camera in tracking space, row-major: head times the lens in the head.
pub fn camera_to_world(head: &CapturePose, lens: &CaptureLensPose) -> [[f64; 4]; 4] {
    let h = rotation_of(head.orientation);
    let l = rotation_of(lens.rotation);
    let t = lens.translation.map(f64::from);
    let mut m = [[0.0; 4]; 4];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = (0..3).map(|k| h[i][k] * l[j][k]).sum();
        }
        m[i][3] = f64::from(head.position[i]) + (0..3).map(|k| h[i][k] * t[k]).sum::<f64>();
    }
    m[3][3] = 1.0;
    m
}

/// fx, fy, cx, cy in the pictures' pixels, centres at whole numbers. Camera2 counts in the
/// pre-correction array, from its corner; the pictures show the active array scaled to cover
/// their size, centred.
pub fn picture_intrinsics(camera: &CaptureCamera) -> Result<[f64; 4], String> {
    let [fx, fy, cx, cy, skew] = camera.intrinsics.ok_or("the camera gives no intrinsics")?.map(f64::from);
    if skew.abs() > 1e-3 * fx {
        return Err(format!("a camera with skew {skew}"));
    }
    if camera.distortion.is_some() {
        return Err("a camera with lens distortion: its pictures are not undistorted here".into());
    }
    let (active, array) = (camera.active.map(f64::from), camera.array.map(f64::from));
    let size = camera.size.map(f64::from);
    if active[2] <= 0.0 || active[3] <= 0.0 || size[0] <= 0.0 || size[1] <= 0.0 {
        return Err("a camera without an active array or a picture size".into());
    }
    let scale = (size[0] / active[2]).max(size[1] / active[3]);
    let picture = |c: f64, at: usize| (c - (active[at] - array[at])) * scale - (active[at + 2] * scale - size[at]) / 2.0 - 0.5;
    Ok([fx * scale, fy * scale, picture(cx, 0), picture(cy, 1)])
}

pub fn session_view(view: &CaptureView, camera: &CaptureCamera, keyframe: usize) -> Result<SessionView, String> {
    let lens = camera.lens_pose.as_ref().ok_or("the camera gives no lens pose")?;
    let y = view.planes.first().ok_or("a picture without planes")?;
    if y.bytes_per_sample != 1 {
        return Err("a picture whose luma is not one byte a sample".into());
    }
    let (width, height) = (y.width as usize, y.height as usize);
    let pixels = decode_plane(y)?.into_iter().map(|v| f32::from(v) / 255.0).collect();
    let view_of = GrayView { width, height, pixels, k: picture_intrinsics(camera)?, cam_to_world: camera_to_world(&view.head, lens) };
    Ok(SessionView {
        view: view_of,
        keyframe,
        camera: view.camera,
        velocity: view.velocity.map(f64::from),
        angular_velocity: view.angular_velocity.map(f64::from),
        exposure_ns: view.exposure_ns,
        rolling_skew_ns: view.rolling_skew_ns,
    })
}

/// Every `every`th keyframe of the session in `folder`, in the order they arrived.
pub fn read_session(folder: &Path, every: usize) -> Result<Session, String> {
    let mut names: Vec<_> = std::fs::read_dir(folder)
        .map_err(|e| format!("the session folder: {}", e.kind()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("record-") && name.ends_with(".bin"))
        .collect();
    names.sort();
    let mut cameras: Vec<CaptureCamera> = Vec::new();
    let (mut views, mut keyframes, mut unreadable) = (Vec::new(), 0, 0);
    for name in names {
        let bytes = std::fs::read(folder.join(&name)).map_err(|e| format!("{name}: {}", e.kind()))?;
        match CaptureRecord::decode(&bytes) {
            Err(_) => unreadable += 1,
            Ok(CaptureRecord::Cameras(list)) => cameras = list,
            Ok(CaptureRecord::Keyframe(keyframe)) => {
                keyframes += 1;
                if (keyframes - 1) % every.max(1) != 0 {
                    continue;
                }
                for view in &keyframe.views {
                    let camera = cameras.get(view.camera as usize).ok_or("a picture of a camera not listed")?;
                    views.push(session_view(view, camera, keyframes - 1)?);
                }
            }
            Ok(_) => {}
        }
    }
    Ok(Session { views, keyframes, unreadable })
}

fn centre_of(view: &GrayView) -> [f64; 3] {
    std::array::from_fn(|i| view.cam_to_world[i][3])
}

fn axis_of(view: &GrayView) -> [f64; 3] {
    std::array::from_fn(|i| view.cam_to_world[i][2])
}

/// Where the views' optical axes come nearest together: what the wearer kept mid-view.
pub fn looked_at(views: &[GrayView]) -> Result<[f64; 3], String> {
    let (mut a, mut b) = ([[0.0; 3]; 3], [0.0; 3]);
    for view in views {
        let (c, d) = (centre_of(view), axis_of(view));
        for i in 0..3 {
            for j in 0..3 {
                let m = f64::from(u8::from(i == j)) - d[i] * d[j];
                a[i][j] += m;
                b[i] += m * c[j];
            }
        }
    }
    let det = |m: &[[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let whole = det(&a);
    if whole.abs() < 1e-9 {
        return Err("the views' axes do not cross".into());
    }
    Ok(std::array::from_fn(|at| {
        let mut m = a;
        for (row, value) in m.iter_mut().zip(b) {
            row[at] = value;
        }
        det(&m) / whole
    }))
}

/// The triangulation angle a view pair is best at, degrees.
const BEST_ANGLE: f64 = 12.0;
const ANGLE_RANGE: std::ops::Range<f64> = 2.0..35.0;

/// For each view up to `count` others to match it against: those that see `target` from an angle
/// nearest [`BEST_ANGLE`].
pub fn neighbours(views: &[GrayView], target: [f64; 3], count: usize) -> Vec<Vec<usize>> {
    let towards = |view: &GrayView| -> [f64; 3] {
        let c = centre_of(view);
        let d = [target[0] - c[0], target[1] - c[1], target[2] - c[2]];
        let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        d.map(|v| v / n)
    };
    let sees = |view: &GrayView| {
        let (c, m) = (centre_of(view), &view.cam_to_world);
        let d = [target[0] - c[0], target[1] - c[1], target[2] - c[2]];
        let p: [f64; 3] = std::array::from_fn(|i| (0..3).map(|k| m[k][i] * d[k]).sum());
        let (x, y) = (view.k[0] * p[0] / p[2] + view.k[2], view.k[1] * p[1] / p[2] + view.k[3]);
        let (w, h) = (view.width as f64, view.height as f64);
        p[2] > 0.0 && x > 0.1 * w && x < 0.9 * w && y > 0.1 * h && y < 0.9 * h
    };
    let rays: Vec<[f64; 3]> = views.iter().map(towards).collect();
    let seen: Vec<bool> = views.iter().map(sees).collect();
    (0..views.len())
        .map(|r| {
            let mut scored: Vec<(f64, usize)> = (0..views.len())
                .filter(|&s| s != r && seen[s])
                .filter_map(|s| {
                    let cosine: f64 = (0..3).map(|i| rays[r][i] * rays[s][i]).sum();
                    let angle = cosine.clamp(-1.0, 1.0).acos().to_degrees();
                    ANGLE_RANGE.contains(&angle).then_some(((angle - BEST_ANGLE).abs(), s))
                })
                .collect();
            scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            scored.truncate(count);
            let mut list: Vec<usize> = scored.into_iter().map(|(_, s)| s).collect();
            list.sort_unstable();
            list
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_desk_link::xr_capture::{encode_plane, CaptureKeyframe};

    const TILT: [f32; 4] = [-0.995_184_7, 0.0, 0.0, 0.098_017_14];

    fn camera() -> CaptureCamera {
        CaptureCamera {
            position: 0,
            size: [64, 48],
            intrinsics: Some([50.0, 50.0, 32.25, 24.25, 0.0]),
            array: [0, 0, 64, 48],
            active: [0, 0, 64, 48],
            distortion: None,
            lens_pose: Some(CaptureLensPose { rotation: TILT, translation: [-0.03, -0.02, -0.06], reference: 1 }),
        }
    }

    #[test]
    fn a_lens_on_the_head_looks_forward_and_down() {
        let head = CapturePose { orientation: [0.0, 0.0, 0.0, 1.0], position: [1.0, 2.0, 3.0] };
        let m = camera_to_world(&head, camera().lens_pose.as_ref().unwrap());
        // ahead of the head (its -z), left of it, a little below
        assert!((m[0][3] - 0.97).abs() < 1e-6 && (m[1][3] - 1.98).abs() < 1e-6 && (m[2][3] - 2.94).abs() < 1e-6);
        // the optical axis: forward and tilted down by the lens's 11.25 degrees; the picture's
        // right is the head's, its down is the head's down
        let axis = [m[0][2], m[1][2], m[2][2]];
        assert!(axis[0].abs() < 1e-6 && (axis[1] + 11.25_f64.to_radians().sin()).abs() < 1e-6 && axis[2] < -0.98, "{axis:?}");
        assert!(m[0][0] > 0.999 && m[1][1] < -0.98);
        // a turned head turns the camera with it: 90 degrees about y, the head's forward is -x
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let turned = CapturePose { orientation: [0.0, half, 0.0, half], position: [0.0; 3] };
        let t = camera_to_world(&turned, camera().lens_pose.as_ref().unwrap());
        assert!(t[0][2] < -0.98 && t[2][2].abs() < 1e-6, "{t:?}");
        assert!((t[2][3] - 0.03).abs() < 1e-6 && (t[0][3] + 0.06).abs() < 1e-6, "{t:?}");
    }

    #[test]
    fn intrinsics_reach_the_pictures_pixels() {
        assert_eq!(picture_intrinsics(&camera()).unwrap(), [50.0, 50.0, 31.75, 23.75]);
        // a 16:9 picture of half the size of a 4:3 active array that starts 10 pixels in: scaled
        // to cover, cut to the middle row band
        let c = CaptureCamera { size: [640, 360], array: [0, 0, 1300, 980], active: [10, 10, 1280, 960], intrinsics: Some([1000.0, 1000.0, 650.0, 490.0, 0.0]), ..camera() };
        assert_eq!(picture_intrinsics(&c).unwrap(), [500.0, 500.0, 319.5, 179.5]);
        assert!(picture_intrinsics(&CaptureCamera { distortion: Some([0.1; 5]), ..camera() }).is_err());
        assert!(picture_intrinsics(&CaptureCamera { intrinsics: None, ..camera() }).is_err());
    }

    #[test]
    fn a_session_folder_reads_back_as_posed_views() {
        let dir = std::env::temp_dir().join(format!("fab-scan-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let luma: Vec<u8> = (0..64 * 48).map(|i| (i % 251) as u8).collect();
        let keyframe = |index: u32| CaptureRecord::Keyframe(CaptureKeyframe {
            index,
            views: vec![CaptureView {
                camera: 0,
                head: CapturePose { orientation: [0.0, 0.0, 0.0, 1.0], position: [0.0, 0.0, index as f32] },
                velocity: [0.5, 0.0, 0.0],
                planes: vec![encode_plane(64, 48, 1, &luma)],
                ..Default::default()
            }],
            depth: None,
        });
        let records = [CaptureRecord::Cameras(vec![camera()]), keyframe(0), keyframe(1), keyframe(2)];
        for (n, record) in records.iter().enumerate() {
            std::fs::write(dir.join(format!("record-{n:06}.bin")), record.encode()).unwrap();
        }
        std::fs::write(dir.join("record-000004.bin"), [1, 2, 3]).unwrap();
        std::fs::write(dir.join("other.txt"), "not a record").unwrap();
        let session = read_session(&dir, 2).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((session.keyframes, session.unreadable, session.views.len()), (3, 1, 2));
        let (first, second) = (&session.views[0], &session.views[1]);
        assert_eq!((first.keyframe, second.keyframe, first.velocity[0]), (0, 2, 0.5));
        assert_eq!((first.view.width, first.view.height), (64, 48));
        assert_eq!(first.view.pixels[250], 250.0 / 255.0);
        assert!((second.view.cam_to_world[2][3] - first.view.cam_to_world[2][3] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn neighbours_are_the_views_at_a_good_angle() {
        // a ring around the origin, all looking at it, at 10 degree steps
        let at = |i: usize| {
            let a = (i as f64 * 10.0).to_radians();
            let c = [0.5 * a.sin(), 0.0, 0.5 * a.cos()];
            let (x, z) = ([a.cos(), 0.0, -a.sin()], [-a.sin(), 0.0, -a.cos()]);
            let y = [0.0, -1.0, 0.0];
            GrayView {
                width: 64,
                height: 64,
                pixels: vec![0.5; 64 * 64],
                k: [50.0, 50.0, 31.5, 31.5],
                cam_to_world: [[x[0], y[0], z[0], c[0]], [x[1], y[1], z[1], c[1]], [x[2], y[2], z[2], c[2]], [0.0, 0.0, 0.0, 1.0]],
            }
        };
        let views: Vec<GrayView> = (0..36).map(at).collect();
        let target = looked_at(&views).unwrap();
        assert!(target.iter().all(|v| v.abs() < 1e-9), "{target:?}");
        let lists = neighbours(&views, target, 4);
        assert_eq!(lists[0], vec![1, 2, 34, 35]);
        assert_eq!(lists[18], vec![16, 17, 19, 20]);
        // one too far round is no use: nothing within 35 degrees but itself
        let apart: Vec<GrayView> = [0, 9, 18, 27].map(at).into_iter().collect();
        assert!(neighbours(&apart, [0.0; 3], 4).iter().all(|list| list.is_empty()));
    }
}
