use fab_elec_mcu_parts::screen::{screen_feed, ScreenFrame};

fn full(pixels: &[u16], inverted: bool) -> Vec<u32> {
    pixels.iter().map(|&pixel| {
        let pixel = if inverted { !pixel } else { pixel };
        let r = u32::from(pixel / 2048);
        let g = u32::from(pixel / 32 % 64);
        let b = u32::from(pixel % 32);
        u32::from_le_bytes([(b * 8 + b / 4) as u8, (g * 4 + g / 16) as u8,
            (r * 8 + r / 4) as u8, 255])
    }).collect()
}

fn check_mean(frame: &ScreenFrame, expected: &[u32], glass: [usize; 4]) {
    let [x, y, w, h] = glass;
    let mut direct = [0.0_f64; 3];
    if frame.on {
        for row in y..y + h {
            for pixel in &expected[row * frame.width + x..row * frame.width + x + w] {
                for (channel, shift) in [16, 8, 0].into_iter().enumerate() {
                    let srgb = f64::from((pixel >> shift) & 255) / 255.0;
                    direct[channel] += if srgb <= 0.04045 { srgb / 12.92 }
                        else { ((srgb + 0.055) / 1.055).powf(2.4) };
                }
            }
        }
    }
    for channel in 0..3 {
        assert!((f64::from(frame.mean[channel]) - direct[channel] / (w * h) as f64).abs() < 1e-6,
            "glass mean channel {channel} at generation {} must match direct sRGB decoding before brightness", frame.generation);
    }
}

#[test]
fn all_reader_interleavings_preserve_dirty_rows_and_inversion() {
    // Before/after each of three publishes: all 64 read/no-read interleavings,
    // under all eight inversion sequences, including three dirty-row-only writes.
    // A later metadata-only lap reuses stale slots, including one held by the reader.
    for case in 0..512 {
        let reads = case & 63;
        let inversions = case >> 6;
        // Exercise an offset glass and full-memory glass, including dirty rows
        // outside the cropped glass. Every slot keeps its own sums and versions.
        let glass = if case & 1 == 0 { [1, 1, 2, 2] } else { [0, 0, 4, 3] };
        let (mut writer, mut reader) = screen_feed(4, 3, glass);
        let mut pixels = vec![0xf800, 0x07e0, 0x001f, 0xffff, 0, 0x1234, 0xabcd, 0x5678,
            0x2945, 0x4567, 0x89ab, 0xcdef];
        assert!(writer.publish(1.0, &pixels, true, 37, false));
        assert_eq!(&reader.latest().bgra[..4], &[0xffff0000, 0xff00ff00, 0xff0000ff, 0xffffffff],
            "texture words put red at bits 16..23, blue at bits 0..7");
        let mut expected = full(&pixels, false);
        check_mean(reader.latest(), &expected, glass);
        for stage in 0..3 {
            if reads & (1 << (2 * stage)) != 0 {
                assert_eq!(reader.latest().bgra, expected, "read before publish: {reads}/{stage}");
                check_mean(reader.latest(), &expected, glass);
            }
            pixels[stage * 4 + 1] ^= 0x739d;
            writer.dirty_row(stage);
            let inverted = inversions & (1 << stage) != 0;
            assert!(writer.publish(stage as f64 + 2.0, &pixels, true, 37, inverted));
            expected = full(&pixels, inverted);
            if reads & (2 << (2 * stage)) != 0 {
                assert_eq!(reader.latest().bgra, expected, "read after publish: {reads}/{stage}");
                check_mean(reader.latest(), &expected, glass);
            }
        }
        assert_eq!(reader.latest().bgra, expected, "latest survives skipped frames: {reads}");
        check_mean(reader.latest(), &expected, glass);
        let inverted = inversions & 4 != 0;
        for t in 5..8 {
            writer.changed();
            let on = t != 5;
            assert!(writer.publish(f64::from(t), &pixels, on, t as u8, inverted));
            let frame = reader.latest();
            assert_eq!(frame.bgra, expected, "metadata publish fills this slot's stale rows: {reads}/{t}");
            assert_eq!((frame.t, frame.width, frame.height, frame.on, frame.brightness), (f64::from(t), 4, 3, on, t as u8));
            check_mean(frame, &expected, glass);
        }
        let generation = reader.latest().generation;
        assert!(!writer.publish(8.0, &pixels, true, 7, inverted), "unchanged publication does no work");
        assert_eq!(reader.latest().generation, generation, "reading never requests or fabricates a frame");
    }
}

