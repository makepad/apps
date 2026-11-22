//! The archive's scene table: a fixed header with the layer count and the parts' box, then one
//! 64-byte record per layer (its slicing plane, its cured area, its box and its contours), all
//! little-endian. Boxes are mm from the screen's centre, in the layer image's frame.

use fab_sla_archive::format::{SCENE_MAGIC, SCENE_TABLE_END, SCENE_TABLE_START};

/// What the scene table says of one layer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SceneLayer {
    /// Its cured area (mm²).
    pub area: f32,
    /// Its box: x min, y min, x max, y max (mm from the screen's centre); zeros when it lights
    /// nothing.
    pub bounds: [f32; 4],
    /// Its outlines, holes included.
    pub contours: u32,
}

/// The writer's name the table carries.
pub const WRITER: &str = "Fab";

/// The scene table of `layers` layers `layer_height` thick, the parts' box `parts` (x, y and z
/// min, then max).
pub fn scene_table(layer_height: f64, layers: &[SceneLayer], parts: [f32; 6]) -> Vec<u8> {
    let n = layers.len() as u32;
    let mut out = Vec::with_capacity(400 + 64 * layers.len());
    let field = |out: &mut Vec<u8>, bytes: &[u8], width: usize| {
        out.extend_from_slice(bytes);
        out.resize(out.len() + width - bytes.len(), 0);
    };
    field(&mut out, SCENE_MAGIC, 16);
    field(&mut out, WRITER.as_bytes(), 64);
    // a release build's table, version 1, an unknown 0, units in mm, an unknown 1.0
    for v in [3u32, 1, 0, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&1.0f32.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    for v in parts {
        out.extend_from_slice(&v.to_le_bytes());
    }
    // no flags, then 256 bytes of nothing
    field(&mut out, &0u32.to_le_bytes(), 4 + 256);
    out.extend_from_slice(&SCENE_TABLE_START);
    out.extend_from_slice(&n.to_le_bytes());
    for (k, l) in layers.iter().enumerate() {
        let z = ((k as f64 + 0.5) * layer_height) as f32;
        for v in [z, l.area].iter().chain(&l.bounds) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&l.contours.to_le_bytes());
        field(&mut out, &0.0f32.to_le_bytes(), 4 + 32);
    }
    out.extend_from_slice(&SCENE_TABLE_END);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of 2 layers, byte by byte at the layout's offsets: 400 + 64 N bytes, the magic,
    /// the counts, the parts' box, each record's plane at the layer's middle, area, box and
    /// contours, and the markers around the records.
    #[test]
    fn a_two_layer_scene_table_is_its_layout_byte_by_byte() {
        let layers = [SceneLayer { area: 12.5, bounds: [-1.0, -2.0, 3.0, 4.0], contours: 2 }, SceneLayer { area: 0.0, bounds: [0.0; 4], contours: 0 }];
        let t = scene_table(0.05, &layers, [-1.0, -2.0, 0.0, 3.0, 4.0, 0.1]);
        let u32_at = |at: usize| u32::from_le_bytes(t[at..at + 4].try_into().unwrap());
        let f32_at = |at: usize| f32::from_le_bytes(t[at..at + 4].try_into().unwrap());
        assert_eq!(t.len(), 400 + 64 * 2);
        assert_eq!(&t[..SCENE_MAGIC.len()], SCENE_MAGIC);
        assert!(t[SCENE_MAGIC.len()..16].iter().all(|b| *b == 0));
        assert_eq!(&t[16..19], b"Fab");
        assert!(t[19..80].iter().all(|b| *b == 0));
        assert_eq!((u32_at(80), u32_at(84), u32_at(88), u32_at(92), f32_at(96), u32_at(100)), (3, 1, 0, 0, 1.0, 2));
        assert_eq!((104..128).step_by(4).map(f32_at).collect::<Vec<_>>(), [-1.0, -2.0, 0.0, 3.0, 4.0, 0.1]);
        assert_eq!(u32_at(128), 0);
        assert!(t[132..388].iter().all(|b| *b == 0));
        assert_eq!((&t[388..392], u32_at(392)), (&SCENE_TABLE_START[..], 2));
        for (k, l) in layers.iter().enumerate() {
            let at = 396 + 64 * k;
            assert_eq!(f32_at(at), ((k as f64 + 0.5) * 0.05) as f32, "layer {k}'s plane");
            assert_eq!((f32_at(at + 4), [f32_at(at + 8), f32_at(at + 12), f32_at(at + 16), f32_at(at + 20)], u32_at(at + 24)), (l.area, l.bounds, l.contours), "layer {k}");
            assert!(t[at + 28..at + 64].iter().all(|b| *b == 0), "layer {k}'s tail");
        }
        assert_eq!(&t[396 + 128..], &SCENE_TABLE_END[..]);
    }
}
