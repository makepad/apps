use fab_elec_mcu::convert::{
    image, sound, Codec, Dither, Fit, ImageOptions, Rotate, SoundOptions, IMA_INDEX, IMA_STEP,
};

#[test]
fn firmware_image_pixels() {
    let corners = [
        255, 0, 0, 255, 0, 255, 0, 255,
        0, 0, 255, 255, 255, 255, 255, 255,
    ];
    let plain = ImageOptions {
        size: None,
        fit: Fit::Stretch,
        rotate: Rotate::R0,
        dither: Dither::None,
        background: [255, 0, 255],
    };
    let convert = |opts| image(&corners, 2, 2, &opts).expect("valid corner image");
    let (red, green, blue, white, background) = (0xf800, 0x07e0, 0x001f, 0xffff, 0xf81f);
    let original = convert(plain);
    assert_eq!((original.width, original.height), (2, 2), "native dimensions");
    assert_eq!(original.pixels, [red, green, blue, white], "native RGB565 in row order");

    let rotated = convert(ImageOptions { rotate: Rotate::R90, ..plain });
    assert_eq!(rotated.pixels, [blue, red, white, green], "clockwise corner mapping");
    let strip = image(&corners, 4, 1, &ImageOptions { rotate: Rotate::R270, ..plain }).unwrap();
    assert_eq!((strip.width, strip.height), (1, 4), "native size follows rotation");
    assert_eq!(strip.pixels, [white, blue, green, red], "counterclockwise strip order");

    let wide = ImageOptions { size: Some((5, 2)), fit: Fit::Contain, ..plain };
    let contained = convert(wide);
    assert_eq!((contained.width, contained.height), (5, 2), "letterboxed canvas size");
    assert_eq!(
        contained.pixels,
        [background, red, green, background, background, background, blue, white, background, background],
        "two content columns with the odd padding pixel on the right",
    );
    let tall = convert(ImageOptions { size: Some((2, 4)), ..wide });
    assert_eq!(
        tall.pixels, [background, background, red, green, blue, white, background, background],
        "top and bottom bars use the background",
    );
    let crop = ImageOptions { size: Some((2, 1)), fit: Fit::Cover, ..plain };
    let covered = image(&corners, 4, 1, &crop).unwrap();
    assert_eq!((covered.width, covered.height), (2, 1), "covered canvas size");
    assert_eq!(covered.pixels, [green, blue], "cover crops both outer pixels");
    assert_eq!(convert(crop).pixels, [0x8010, 0x87f0], "half-pixel centered vertical crop");

    let small = convert(ImageOptions { size: Some((1, 1)), ..plain });
    assert_eq!(small.pixels, [0x8410], "area mean of 127 and a half rounds to 128");
    let thirds = image(&corners[..12], 3, 1, &ImageOptions { size: Some((2, 1)), ..plain }).unwrap();
    assert_eq!(thirds.pixels, [0xaaa0, 0x02b5], "fractional area weights are two to one and one to two");
    let mixed = convert(ImageOptions { size: Some((1, 4)), ..plain });
    assert_eq!(mixed.pixels, [0x8400, 0x8408, 0x8417, 0x841f], "area across, bilinear down");
    let mixed = convert(ImageOptions { size: Some((4, 1)), ..plain });
    assert_eq!(mixed.pixels, [0x8010, 0x8210, 0x85f0, 0x87f0], "bilinear across, area down");
    let alpha = image(
        &[255, 255, 255, 0, 0, 15, 0, 128], 2, 1,
        &ImageOptions { background: [0, 0, 255], ..plain },
    ).unwrap();
    assert_eq!(alpha.pixels, [blue, 0x004f], "alpha composites and rounds before truncation");

    let grey = [128, 128, 128, 255].repeat(16 * 16);
    let flat = image(&grey, 16, 16, &plain).unwrap();
    assert!(flat.pixels.iter().all(|&p| p == 0x8410), "None leaves grey uniform");
    for dither in [Dither::Ordered, Dither::Diffusion] {
        let opts = ImageOptions { dither, ..plain };
        let tile = image(&grey, 16, 16, &opts).unwrap();
        let count = tile.pixels.len() as u32;
        for (shift, max, step) in [(11, 31u32, 8), (5, 63, 4), (0, 31, 8)] {
            let sum: u32 = tile.pixels.iter().map(|&p| (u32::from(p) >> shift) & max).sum();
            assert!(
                (sum * 255).abs_diff(128 * max * count) <= step * max * count,
                "{dither:?} channel at bit {shift} averages within one RGB565 step",
            );
        }
        assert_eq!(tile.pixels, image(&grey, 16, 16, &opts).unwrap().pixels, "{dither:?} is reproducible");
    }
    let near_step = [129, 128, 128, 255].repeat(16);
    let ordered = image(&near_step, 4, 4, &ImageOptions { dither: Dither::Ordered, ..plain }).unwrap();
    let raised: Vec<_> = ordered.pixels.iter().enumerate()
        .filter_map(|(i, &pixel)| (pixel == 0x8c10).then_some(i)).collect();
    assert_eq!(raised, [6, 12], "Bayer ranks 14 and 15 raise red by one level");
    let diffusion = image(&grey[..16], 2, 2, &ImageOptions { dither: Dither::Diffusion, ..plain }).unwrap();
    assert_eq!(diffusion.pixels, [0x8410, 0x7bef, 0x7bef, 0x8410], "diffusion mirrors the second row");

    for (w, h, reason) in [(0, 1, "non-zero"), (1, 0, "non-zero"), (8193, 1, "8192"), (1, 8193, "8192")] {
        let bad_source = image(&[], w, h, &plain).expect_err("invalid source size");
        assert!(bad_source.contains(reason), "source {w}x{h} explains the size refusal");
        let bad_output = image(&corners, 2, 2, &ImageOptions { size: Some((w, h)), ..plain })
            .expect_err("invalid output size");
        assert!(bad_output.contains(reason), "output {w}x{h} explains the size refusal");
    }
    assert!(image(&corners[..15], 2, 2, &plain).is_err(), "incomplete RGBA input is refused");
}

const WAV_RATE: u32 = 16000;

fn wav(tag: u16, channels: u16, bits: u16, extensible: bool, data: &[u8]) -> Vec<u8> {
    let align = channels * (bits / 8);
    let mut fmt = Vec::new();
    let format = if extensible { 0xfffe } else { tag };
    fmt.extend_from_slice(&format.to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&WAV_RATE.to_le_bytes());
    fmt.extend_from_slice(&(WAV_RATE * u32::from(align)).to_le_bytes());
    fmt.extend_from_slice(&align.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if extensible {
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(&0u32.to_le_bytes());
        fmt.extend_from_slice(&u32::from(tag).to_le_bytes());
        fmt.extend_from_slice(&[0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]);
    }
    let mut riff = b"RIFF\0\0\0\0WAVE".to_vec();
    for (id, payload) in [(b"fmt ", fmt.as_slice()), (b"JUNK", &[7][..]), (b"data", data)] {
        riff.extend_from_slice(id);
        riff.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        riff.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            riff.push(0);
        }
    }
    let size = (riff.len() - 8) as u32;
    riff[4..8].copy_from_slice(&size.to_le_bytes());
    riff
}

fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn decode_ima(bytes: &[u8], count: usize) -> Vec<i16> {
    let mut decoded = Vec::new();
    let mut index = 0;
    for (number, block) in bytes.chunks(256).enumerate() {
        let mut predictor = i32::from(i16::from_le_bytes([block[0], block[1]]));
        assert_eq!(i32::from(block[2]), index, "block {number} carries the previous step index");
        assert_eq!(block[3], 0, "block {number} reserved header byte is zero");
        decoded.push(predictor as i16);
        for &byte in &block[4..] {
            for code in [byte & 15, byte >> 4] {
                if decoded.len() == count {
                    return decoded;
                }
                let step = IMA_STEP[index as usize];
                let mut delta = step >> 3;
                if code & 1 != 0 { delta += step >> 2; }
                if code & 2 != 0 { delta += step >> 1; }
                if code & 4 != 0 { delta += step; }
                predictor += if code & 8 == 0 { delta } else { -delta };
                predictor = predictor.clamp(-32768, 32767);
                decoded.push(predictor as i16);
                index = (index + IMA_INDEX[usize::from(code)]).clamp(0, 88);
            }
        }
    }
    decoded
}

#[test]
fn firmware_sound_samples() {
    let pcm = SoundOptions { rate: WAV_RATE, codec: Codec::Pcm16 };
    let raw = pcm_bytes(&[i16::MIN, -0x1234, 1, 0x4567, i16::MAX]);
    let native = wav(1, 1, 16, false, &raw);
    let original = sound(&native, &pcm).unwrap();
    assert_eq!((original.rate, original.samples, original.codec), (WAV_RATE, 5, Codec::Pcm16),
        "native PCM metadata describes the mono output");
    assert_eq!(original.bytes, raw, "same-rate PCM16 preserves every little-endian byte");

    let signal = [-32768i16, -16384, 0, 8192, 16384];
    let pcm8: Vec<_> = signal.iter().map(|&s| ((s >> 8) + 128) as u8).collect();
    let pcm24: Vec<_> = signal.iter().flat_map(|&s| {
        let [lo, hi] = s.to_le_bytes();
        [0xff, lo, hi]
    }).collect();
    let pcm32: Vec<_> = signal.iter().flat_map(|&s| {
        ((i32::from(s) << 16) | 0xffff).to_le_bytes()
    }).collect();
    // IEEE binary32 encodings of -1, -1/2, 0, 1/4 and 1/2.
    let float: Vec<_> = [0xbf80_0000u32, 0xbf00_0000, 0, 0x3e80_0000, 0x3f00_0000]
        .into_iter().flat_map(u32::to_le_bytes).collect();
    for (tag, bits, extensible, data) in [
        (1, 8, false, &pcm8), (1, 24, false, &pcm24), (1, 32, false, &pcm32),
        (3, 32, false, &float), (1, 24, true, &pcm24), (3, 32, true, &float),
    ] {
        let result = sound(&wav(tag, 1, bits, extensible, data), &pcm).unwrap();
        assert_eq!(result.bytes.len(), 2 * signal.len(), "{bits}-bit format {tag} preserves frame count");
        for (pair, &expected) in result.bytes.chunks_exact(2).zip(&signal) {
            let actual = i16::from_le_bytes([pair[0], pair[1]]);
            assert!(actual.abs_diff(expected) <= 1,
                "{bits}-bit format {tag} (extensible={extensible}) differs by at most one PCM step");
        }
    }
    let peaks: Vec<_> = [0xc000_0000u32, 0xbf00_0000, 0x3f00_0000, 0x4000_0000]
        .into_iter().flat_map(u32::to_le_bytes).collect();
    assert_eq!(sound(&wav(3, 1, 32, false, &peaks), &pcm).unwrap().bytes,
        pcm_bytes(&[-32767, -16384, 16384, 32767]), "float clamps peaks and rounds signed ties away");

    let stereo = wav(1, 2, 16, false, &pcm_bytes(&[12345, -12345, -32767, 32767]));
    assert_eq!(sound(&stereo, &pcm).unwrap().bytes, pcm_bytes(&[0, 0]), "opposite stereo cancels");
    let ties = [1, 1, 1, 1, 0, 0, 0, 0, -1, -1, -1, -1, 0, 0, 0, 0];
    assert_eq!(sound(&wav(1, 8, 16, false, &pcm_bytes(&ties)), &pcm).unwrap().bytes,
        pcm_bytes(&[1, -1]), "eight-channel means round both half ties away from zero");

    let ramp = wav(1, 1, 16, false, &pcm_bytes(&[-4, -1, 2, 5, -7]));
    let at_rate = |rate| sound(&ramp, &SoundOptions { rate, ..pcm }).unwrap();
    let half = at_rate(WAV_RATE / 2);
    assert_eq!((half.rate, half.samples), (8000, 3), "halving keeps ceil(n/2) samples");
    assert_eq!(half.bytes, pcm_bytes(&[-2, 4, -7]), "pair means round half up and keep the tail");
    assert_eq!(at_rate(12000).bytes, pcm_bytes(&[-3, 1, 4, -7]), "fractional areas weight both edges");
    let double = at_rate(WAV_RATE * 2);
    assert_eq!(double.samples, 10, "doubling emits twice as many samples");
    assert_eq!(double.bytes, pcm_bytes(&[-4, -2, -1, 1, 2, 4, 5, -1, -7, -7]),
        "doubling interpolates signed midpoints half up and holds the last sample");
    assert_eq!(at_rate(24000).bytes, pcm_bytes(&[-4, -2, 0, 2, 4, 1, -7, -7]),
        "fractional interpolation carries its phase across source frames");

    let ima = SoundOptions { codec: Codec::ImaAdpcm, ..pcm };
    let steps = wav(1, 1, 16, false, &pcm_bytes(&[0, 11, -19]));
    assert_eq!(sound(&steps, &ima).unwrap().bytes, [0, 0, 0, 0, 0xf7],
        "IMA packs the positive full step first in the low nibble");
    // A quarter of a 64-sample sine period at amplitude 8192.
    let quarter = [0i16, 803, 1598, 2378, 3135, 3862, 4551, 5197, 5793,
        6333, 6811, 7225, 7568, 7839, 8035, 8153, 8192];
    let sine: Vec<_> = (16usize..16 + 1012).map(|phase| {
        let i = phase % 16;
        match phase / 16 % 4 {
            0 => quarter[i],
            1 => quarter[16 - i],
            2 => -quarter[i],
            _ => -quarter[16 - i],
        }
    }).collect();
    let wave = wav(1, 1, 16, false, &pcm_bytes(&sine));
    let encoded = sound(&wave, &ima).unwrap();
    assert_eq!((encoded.codec, encoded.samples), (Codec::ImaAdpcm, 1012), "IMA reports true sample count");
    assert_eq!(encoded.bytes.len(), 2 * 256 + 4 + 1, "505 samples per full block, two in the last");
    assert_eq!(encoded.bytes.last().unwrap() >> 4, 0, "odd final code leaves a zero high nibble");
    let decoded = decode_ima(&encoded.bytes, sine.len());
    assert_eq!(decoded.len(), sine.len(), "IMA decoder recovers exactly the true sample count");
    for start in (0..sine.len()).step_by(505) {
        assert_eq!(decoded[start], sine[start], "block predictor stores sample {start} exactly");
    }
    assert!(decoded.iter().zip(&sine).all(|(&a, &b)| u32::from(a.abs_diff(b)) * 100 <= 3 * 32767),
        "IMA sine reconstruction stays within three percent of full scale");
    assert_eq!(sound(&wave, &ima).unwrap().bytes, encoded.bytes, "IMA bytes are reproducible");

    for rate in [7999, 48001] {
        let err = sound(&native, &SoundOptions { rate, ..pcm }).unwrap_err();
        assert!(err.contains("8000..=48000"), "out-of-range rate {rate} explains the accepted range");
    }
    let mut missing = native.clone();
    let data_header = missing.len() - raw.len() - 8;
    missing[data_header..data_header + 4].copy_from_slice(b"skip");
    let mut bad_align = native.clone();
    bad_align[32..34].copy_from_slice(&1u16.to_le_bytes());
    for (bad, reason) in [
        (missing, "missing WAV data"), (bad_align, "block align"),
        (native[..native.len() - 1].to_vec(), "truncated"),
        (wav(1, 1, 16, false, &[0]), "truncated WAV sample frame"),
        (wav(1, 1, 16, false, &[]), "zero samples"),
    ] {
        assert!(sound(&bad, &pcm).unwrap_err().contains(reason), "malformed WAV explains {reason}");
    }
}
