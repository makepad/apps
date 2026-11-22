use super::{f32_at, near, u32_at, Fault, Rule, Scene, SceneRecord};
use crate::format;

/// Parse and check a scene table against its layer count, height and screen half-extents in mm.
pub fn read_scene(bytes: &[u8], layers: u32, height: f64, half: [f64; 2]) -> Result<Scene, Fault> {
    let expected = 400u64 + 64 * layers as u64;
    if bytes.len() as u64 != expected {
        return Err(Fault::new(Rule::SceneSize, format!("{} bytes, expected {expected}", bytes.len())));
    }
    if !bytes.starts_with(format::SCENE_MAGIC) || bytes[format::SCENE_MAGIC.len()..16].iter().any(|b| *b != 0)
        || bytes[388..392] != format::SCENE_TABLE_START || bytes[bytes.len() - 4..] != format::SCENE_TABLE_END {
        return Err(Fault::new(Rule::SceneMarkers, format!("magic {:?}, start {:?}, end {:?}; expected {:?} padded to 16 bytes, {:?}, {:?}", &bytes[..16], &bytes[388..392], &bytes[bytes.len() - 4..], format::SCENE_MAGIC, format::SCENE_TABLE_START, format::SCENE_TABLE_END)));
    }
    let name = &bytes[16..80];
    let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
    if !name[..end].is_ascii() || name[end..].iter().any(|b| *b != 0) {
        return Err(Fault::new(Rule::SceneMarkers, "writer name must be NUL-padded ASCII"));
    }
    let counts = [u32_at(bytes, 100), u32_at(bytes, 392)];
    if counts != [layers; 2] {
        return Err(Fault::new(Rule::SceneCount, format!("counts {counts:?}, expected {layers} twice")));
    }
    let bounds = std::array::from_fn(|i| f32_at(bytes, 104 + i * 4));
    let records: Vec<_> = (0..layers as usize).map(|k| {
        let offset = 396 + 64 * k;
        SceneRecord {
            z: f32_at(bytes, offset), area: f32_at(bytes, offset + 4),
            bounds: std::array::from_fn(|i| f32_at(bytes, offset + 8 + i * 4)),
            contours: u32_at(bytes, offset + 24),
        }
    }).collect();
    for (k, record) in records.iter().enumerate() {
        let expected = (k as f64 + 0.5) * height;
        if !near(record.z as f64, expected, 1e-4) {
            return Err(Fault::new(Rule::SceneHeights, format!("layer {k}: z {}, expected {expected} mm", record.z)));
        }
    }
    check_box([bounds[0], bounds[1], bounds[3], bounds[4]], half).map_err(|e| e.at("parts"))?;
    for (k, record) in records.iter().enumerate() {
        check_box(record.bounds, half).map_err(|e| e.at(&format!("layer {k}")))?;
    }
    Ok(Scene { writer: String::from_utf8(name[..end].to_vec()).unwrap(), bounds, records })
}

fn check_box(bounds: [f32; 4], half: [f64; 2]) -> Result<(), Fault> {
    for axis in 0..2 {
        let lo = bounds[axis] as f64;
        let hi = bounds[axis + 2] as f64;
        if !lo.is_finite() || !hi.is_finite() || lo > hi || lo < -half[axis] - 1e-3 || hi > half[axis] + 1e-3 {
            return Err(Fault::new(Rule::SceneBounds, format!("box {bounds:?}, expected within half-extents {half:?} mm")));
        }
    }
    Ok(())
}
