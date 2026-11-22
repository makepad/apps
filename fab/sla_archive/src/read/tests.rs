use super::*;

#[test]
fn run_examples_and_invalid_lengths() {
    let levels = decode_runs(&[0x00, 0x05, 0xf0, 0x03, 0x71, 0x00, 0x1f], 40).unwrap();
    assert_eq!(&levels[..5], &[0; 5], "first black span");
    assert_eq!(&levels[5..8], &[15; 3], "full span");
    assert_eq!(levels[8], 7, "grey pixel");
    assert!(levels[9..].iter().all(|v| *v == 0), "black span across row boundary");
    let row = decode_runs(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xf4, 0x03], 13312).unwrap();
    assert!(row.iter().all(|v| *v == 15), "full row split at 4095");
    let mut stream = Vec::new();
    for _ in 0..16644 { stream.extend_from_slice(&[0x0f, 0xff]); }
    stream.extend_from_slice(&[0x01, 0x04]);
    assert_eq!(stream.len(), 33290, "full screen stream length");
    let black = decode_runs(&stream, 13312 * 5120).unwrap();
    assert!(black == vec![0; 13312 * 5120], "all full screen pixels black");
    for (stream, pixels, property) in [
        (&[0x12][..], 1, "run past end"), (&[0x11][..], 2, "short stream"),
        (&[0, 0][..], 1, "zero two-byte count"), (&[0x70][..], 1, "zero one-byte count"),
        (&[0xf0][..], 1, "truncated run"),
    ] { assert!(decode_runs(stream, pixels).is_err(), "{property}"); }
    runs::check_area(&[0xf0, 2, 0x71], 3, [0.0168, 0.0248], (2.0 + 7.0 / 15.0) * 0.0168 * 0.0248).unwrap();
    assert_eq!(runs::check_area(&[0xf0, 2], 2, [1.0, 1.0], 1.0).unwrap_err().rule, Rule::LitArea);
}

fn put_u32(bytes: &mut [u8], at: usize, value: u32) { bytes[at..at + 4].copy_from_slice(&value.to_le_bytes()); }
fn put_float(bytes: &mut [u8], at: usize, value: f32) { put_u32(bytes, at, value.to_bits()); }
fn scene_bytes() -> Vec<u8> {
    let mut bytes = vec![0; 528];
    bytes[..format::SCENE_MAGIC.len()].copy_from_slice(format::SCENE_MAGIC);
    bytes[16..19].copy_from_slice(b"Fab");
    put_u32(&mut bytes, 80, 3);
    put_u32(&mut bytes, 84, 1);
    put_float(&mut bytes, 96, 1.0);
    put_u32(&mut bytes, 100, 2);
    for (i, value) in [-1.0, -2.0, 0.0, 1.0, 2.0, 0.1].into_iter().enumerate() { put_float(&mut bytes, 104 + i * 4, value); }
    bytes[388..392].copy_from_slice(&format::SCENE_TABLE_START);
    put_u32(&mut bytes, 392, 2);
    for (k, z) in [0.025, 0.075].into_iter().enumerate() {
        let at = 396 + 64 * k;
        put_float(&mut bytes, at, z);
        put_float(&mut bytes, at + 4, 8.0);
        for (i, value) in [-1.0, -2.0, 1.0, 2.0].into_iter().enumerate() { put_float(&mut bytes, at + 8 + i * 4, value); }
        put_u32(&mut bytes, at + 24, 1);
    }
    bytes[524..528].copy_from_slice(&format::SCENE_TABLE_END);
    bytes
}

#[test]
fn scene_values_and_faults() {
    let bytes = scene_bytes();
    let scene = read_scene(&bytes, 2, 0.05, [5.0, 5.0]).unwrap();
    assert_eq!(scene.writer, "Fab");
    assert_eq!(scene.bounds, [-1.0, -2.0, 0.0, 1.0, 2.0, 0.1]);
    assert_eq!(scene.records.len(), 2);
    for (k, r) in scene.records.iter().enumerate() {
        assert_eq!(r.z, [0.025, 0.075][k]);
        assert_eq!(r.area, 8.0);
        assert_eq!(r.bounds, [-1.0, -2.0, 1.0, 2.0]);
        assert_eq!(r.contours, 1);
    }
    assert_eq!(read_scene(&bytes[..527], 2, 0.05, [5.0, 5.0]).unwrap_err().rule, Rule::SceneSize);
    for (offset, replacement, rule) in [(0, 0, Rule::SceneMarkers), (524, 0, Rule::SceneMarkers), (100, 3, Rule::SceneCount), (392, 3, Rule::SceneCount)] {
        let mut bad = bytes.clone(); bad[offset] = replacement;
        assert_eq!(read_scene(&bad, 2, 0.05, [5.0, 5.0]).unwrap_err().rule, rule, "offset {offset}");
    }
    for (offset, value, rule) in [(396, 0.075, Rule::SceneHeights), (404, -6.0, Rule::SceneBounds), (104, -6.0, Rule::SceneBounds)] {
        let mut bad = bytes.clone(); put_float(&mut bad, offset, value);
        assert_eq!(read_scene(&bad, 2, 0.05, [5.0, 5.0]).unwrap_err().rule, rule, "offset {offset}");
    }
}

fn edge_bytes(hole: bool) -> (Vec<u8>, SceneRecord) {
    let outer = [
        ([-2.0, -2.0], [-2.0, 2.0], 0),
        ([2.0, -2.0], [2.0, 2.0], 1),
        ([-2.0, -2.0], [2.0, -2.0], 1),
        ([2.0, 2.0], [-2.0, 2.0], 1),
    ];
    let inner = [
        ([1.0, -1.0], [-1.0, -1.0], 1),
        ([1.0, -1.0], [1.0, 1.0], 0),
        ([-1.0, -1.0], [-1.0, 1.0], 1),
        ([-1.0, 1.0], [1.0, 1.0], 1),
    ];
    let segments: Vec<_> = if hole { outer[..3].iter().chain(inner.iter()).chain(outer[3..].iter()).copied().collect() } else { outer.to_vec() };
    let contours = if hole { 2 } else { 1 };
    let area = if hole { 12.0 } else { 16.0 };
    let mut bytes = vec![0; 56 + 17 * segments.len()];
    bytes[..4].copy_from_slice(&format::VECTOR_START);
    put_float(&mut bytes, 4, area * 2.0);
    for (i, v) in [-2.0, -2.0, 2.0, 2.0].into_iter().enumerate() { put_float(&mut bytes, 8 + 4 * i, v); }
    put_u32(&mut bytes, 28, contours);
    bytes[32..36].copy_from_slice(&format::VECTOR_SEGMENTS_START);
    put_u32(&mut bytes, 40, segments.len() as u32);
    put_u32(&mut bytes, 44, 1);
    for (k, (a, b, flag)) in segments.into_iter().enumerate() {
        for (i, v) in [a[0], a[1], b[0], b[1]].into_iter().enumerate() { put_float(&mut bytes, 48 + 17 * k + 4 * i, v); }
        bytes[48 + 17 * k + 16] = flag;
    }
    let tail = bytes.len() - 8;
    bytes[tail..tail + 4].copy_from_slice(&format::VECTOR_SEGMENTS_END);
    bytes[tail + 4..].copy_from_slice(&format::VECTOR_END);
    (bytes, SceneRecord { z: 0.025, area, bounds: [-2.0, -2.0, 2.0, 2.0], contours })
}

#[test]
fn edge_tables_and_faults() {
    for hole in [false, true] {
        let (bytes, record) = edge_bytes(hole);
        let edges = read_vector(&bytes, 100000, &record).unwrap();
        assert_eq!(edges.len(), if hole { 8 } else { 4 });
        assert_eq!(u32_at(&bytes, 28), if hole { 2 } else { 1 });
        assert_eq!(f32_at(&bytes, 4), if hole { 24.0 } else { 32.0 });
    }
    let (bytes, record) = edge_bytes(false);
    let check = |bad: &[u8], rule| assert_eq!(read_vector(bad, 100000, &record).unwrap_err().rule, rule);
    let mut bad = bytes.clone(); bad[64] = 2; check(&bad, Rule::SegmentOrder);
    let mut bad = bytes.clone();
    for i in 0..17 { bad.swap(48 + i, 99 + i); }
    check(&bad, Rule::SegmentOrder);
    let mut bad = bytes.clone(); put_float(&mut bad, 48, -1.0); check(&bad, Rule::Closed);
    let mut bad = bytes.clone();
    put_float(&mut bad, 48, -1.9995); put_float(&mut bad, 82, -1.9995);
    check(&bad, Rule::Rounding);
    let mut bad = bytes.clone();
    for k in 0..4 {
        let at = 48 + 17 * k;
        if f32_at(&bad, at + 4) == f32_at(&bad, at + 12) {
            for i in 0..8 { bad.swap(at + i, at + 8 + i); }
        } else { bad[at + 16] ^= 1; }
    }
    check(&bad, Rule::Winding);
    let mut bad = bytes.clone(); put_float(&mut bad, 4, 31.0); check(&bad, Rule::VectorArea);
    let mut bad = bytes.clone(); put_u32(&mut bad, 28, 2); check(&bad, Rule::VectorContours);
    check(&bytes[..bytes.len() - 1], Rule::VectorSize);
    let mut bad = bytes.clone(); bad[0] = 0; check(&bad, Rule::VectorMarkers);
    assert_eq!(read_vector(&bytes, 3, &record).unwrap_err().rule, Rule::VectorCapacity);
    let mut bad = bytes.clone(); put_float(&mut bad, 8, -3.0); check(&bad, Rule::VectorBounds);
}
