//! Host-side conversion of firmware images and sounds.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    Stretch,
    /// Preserve aspect ratio and fill the margins with the background.
    Contain,
    /// Preserve aspect ratio and crop around the image center.
    Cover,
}

/// Clockwise rotation applied before fitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotate {
    R0,
    R90,
    R180,
    R270,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dither {
    None,
    Ordered,
    Diffusion,
}

#[derive(Clone, Copy, Debug)]
pub struct ImageOptions {
    /// Output canvas in pixels. Use the rotated dimensions when omitted.
    pub size: Option<(u32, u32)>,
    pub fit: Fit,
    pub rotate: Rotate,
    pub dither: Dither,
    pub background: [u8; 3],
}

#[derive(Debug)]
pub struct Converted {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u16>,
}

fn check_size(kind: &str, width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!("{kind} dimensions must be non-zero, got {width}x{height}"));
    }
    if width > 8192 || height > 8192 {
        return Err(format!("{kind} dimensions exceed 8192 on a side, got {width}x{height}"));
    }
    Ok(())
}

struct Source<'a> {
    rgba: &'a [u8],
    width: u32,
    height: u32,
    rotate: Rotate,
    background: [u8; 3],
}

impl Source<'_> {
    fn rgb(&self, x: u32, y: u32) -> [u64; 3] {
        let (x, y) = match self.rotate {
            Rotate::R0 => (x, y),
            Rotate::R90 => (y, self.height - 1 - x),
            Rotate::R180 => (self.width - 1 - x, self.height - 1 - y),
            Rotate::R270 => (self.width - 1 - y, x),
        };
        let at = ((y * self.width + x) * 4) as usize;
        let alpha = u32::from(self.rgba[at + 3]);
        std::array::from_fn(|channel| {
            let color = u32::from(self.rgba[at + channel]);
            let background = u32::from(self.background[channel]);
            u64::from((color * alpha + background * (255 - alpha) + 127) / 255)
        })
    }
}

// Half-open interval in fixed-point source coordinates. Each source pixel spans `unit` ticks.
#[derive(Clone, Copy)]
struct Span {
    start: u32,
    end: u32,
    unit: u32,
}

impl Span {
    fn indices(self) -> std::ops::RangeInclusive<u32> {
        self.start / self.unit..=(self.end - 1) / self.unit
    }

    fn sum(self, mut sample: impl FnMut(u32) -> [u64; 3]) -> [u64; 3] {
        let mut total = [0; 3];
        for i in self.indices() {
            let coverage = self.end.min((i + 1) * self.unit) - self.start.max(i * self.unit);
            let rgb = sample(i);
            for c in 0..3 {
                total[c] += rgb[c] * u64::from(coverage);
            }
        }
        total
    }
}

// The source advances num/den pixels per output pixel. Doubled coordinates keep exact centers.
fn axis(source: u32, count: u32, num: u32, den: u32) -> Vec<Span> {
    let unit = 2 * den;
    let offset = source * den - count * num;
    // A shrinking pixel covers its whole footprint. A growing pixel blends adjacent centers.
    let radius = num.max(den);
    let last_start = source * unit - 2 * radius;
    (0..count)
        .map(|i| {
            let center = offset + (2 * i + 1) * num;
            let start = center.saturating_sub(radius).min(last_start);
            Span { start, end: start + 2 * radius, unit }
        })
        .collect()
}

fn fit_axes(w: u32, h: u32, out_w: u32, out_h: u32, fit: Fit) -> (Vec<Span>, Vec<Span>) {
    let width_limits_contain = out_w * h <= out_h * w;
    let (cols, rows) = match fit {
        Fit::Stretch => (out_w, out_h),
        Fit::Cover => {
            let (num, den) = if width_limits_contain { (h, out_h) } else { (w, out_w) };
            return (axis(w, out_w, num, den), axis(h, out_h, num, den));
        }
        // Round the contained side half up, keeping at least one pixel of the image.
        Fit::Contain if width_limits_contain => (out_w, ((h * out_w + w / 2) / w).max(1)),
        Fit::Contain => (((w * out_h + h / 2) / h).max(1), out_h),
    };
    (axis(w, cols, w, cols), axis(h, rows, h, rows))
}

// Filter the shrinking axis first in a mixed resize. Keep only scanlines of intermediate sums.
enum Scanlines {
    Horizontal { rows: [Vec<[u64; 3]>; 2], source_y: [Option<u32>; 2] },
    Vertical { first_x: u32, columns: Vec<[u64; 3]> },
}

impl Scanlines {
    fn new(xs: &[Span], y: Span) -> Self {
        // A span one source pixel wide touches at most two consecutive source rows.
        if y.end - y.start == y.unit {
            Self::Horizontal {
                rows: std::array::from_fn(|_| vec![[0; 3]; xs.len()]),
                source_y: [None; 2],
            }
        } else {
            let first_x = xs[0].start / xs[0].unit;
            let last = xs[xs.len() - 1];
            let last_x = (last.end - 1) / last.unit;
            Self::Vertical { first_x, columns: vec![[0; 3]; (last_x - first_x + 1) as usize] }
        }
    }

    fn row(&mut self, source: &Source<'_>, xs: &[Span], y: Span, output: &mut [[u8; 3]]) {
        // Each axis has constant total coverage. Round half up once after both passes.
        let weight = u64::from(xs[0].end - xs[0].start) * u64::from(y.end - y.start);
        let finish = |sum: [u64; 3]| sum.map(|c| ((c + weight / 2) / weight) as u8);
        match self {
            Self::Horizontal { rows, source_y } => {
                for sy in y.indices() {
                    let slot = (sy % 2) as usize;
                    if source_y[slot] != Some(sy) {
                        for (sum, &x) in rows[slot].iter_mut().zip(xs) {
                            *sum = x.sum(|sx| source.rgb(sx, sy));
                        }
                        source_y[slot] = Some(sy);
                    }
                }
                for (x, rgb) in output.iter_mut().enumerate() {
                    *rgb = finish(y.sum(|sy| rows[(sy % 2) as usize][x]));
                }
            }
            Self::Vertical { first_x, columns } => {
                let first_x = *first_x;
                for (i, sum) in columns.iter_mut().enumerate() {
                    *sum = y.sum(|sy| source.rgb(first_x + i as u32, sy));
                }
                for (rgb, &x) in output.iter_mut().zip(xs) {
                    *rgb = finish(x.sum(|sx| columns[(sx - first_x) as usize]));
                }
            }
        }
    }
}

// None truncates the low 3/2/3 bits, as in the firmware asset decoder.
fn truncate([r, g, b]: [u8; 3]) -> u16 {
    (u16::from(r >> 3) << 11) | (u16::from(g >> 2) << 5) | u16::from(b >> 3)
}

// Ordered adds floor(Bayer rank * channel step / 16), saturating before truncation.
fn ordered([r, g, b]: [u8; 3], x: usize, y: usize) -> u16 {
    const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let rank = BAYER[y % 4][x % 4];
    truncate([r.saturating_add(rank / 2), g.saturating_add(rank / 4), b.saturating_add(rank / 2)])
}

// Diffusion sends 7/3/5/1 parts of nearest-level error forward and to the next row, mirrored on odd rows.
fn diffuse(rgb: [u8; 3], x: usize, reverse: bool, row: &mut [[i32; 3]], next: &mut [[i32; 3]]) -> u16 {
    let here = x + 1;
    let (ahead, behind) = if reverse { (here - 1, here + 1) } else { (here + 1, here - 1) };
    let mut code = [0u16; 3];
    for (c, max) in [31, 63, 31].into_iter().enumerate() {
        // In these units adjacent levels are exactly 255 apart. Both divisions round half up.
        let correction = (row[here][c] + 8).div_euclid(16);
        let value = (i32::from(rgb[c]) * max + correction).clamp(0, 255 * max);
        let level = (value + 127) / 255;
        let error = value - level * 255;
        row[ahead][c] += error * 7;
        next[behind][c] += error * 3;
        next[here][c] += error * 5;
        next[ahead][c] += error;
        code[c] = level as u16;
    }
    (code[0] << 11) | (code[1] << 5) | code[2]
}

/// Convert tightly packed straight-alpha RGBA to row-major RGB565. Each side must be 1..=8192.
pub fn image(
    rgba: &[u8],
    width: u32,
    height: u32,
    options: &ImageOptions,
) -> Result<Converted, String> {
    check_size("source", width, height)?;
    let (w, h) = match options.rotate {
        Rotate::R0 | Rotate::R180 => (width, height),
        Rotate::R90 | Rotate::R270 => (height, width),
    };
    let (out_w, out_h) = options.size.unwrap_or((w, h));
    check_size("output", out_w, out_h)?;
    let expected = (width * height * 4) as usize;
    if rgba.len() != expected {
        return Err(format!("expected {expected} RGBA bytes for {width}x{height}, got {}", rgba.len()));
    }
    let source = Source { rgba, width, height, rotate: options.rotate, background: options.background };
    let (xs, ys) = fit_axes(w, h, out_w, out_h, options.fit);
    let mut filter = Scanlines::new(&xs, ys[0]);
    // With an odd number of padding pixels, the extra pixel is on the right or bottom.
    let left = (out_w as usize - xs.len()) / 2;
    let top = (out_h as usize - ys.len()) / 2;
    let mut colors = vec![options.background; out_w as usize];
    let error_width = if options.dither == Dither::Diffusion { out_w as usize + 2 } else { 0 };
    let mut errors = vec![[0; 3]; error_width];
    let mut next = vec![[0; 3]; error_width];
    let mut pixels = Vec::new();
    let count = (out_w * out_h) as usize;
    pixels.try_reserve_exact(count).map_err(|_| format!("cannot allocate {count} RGB565 pixels"))?;
    pixels.resize(count, 0);
    for (y, row) in pixels.chunks_exact_mut(out_w as usize).enumerate() {
        let content = &mut colors[left..left + xs.len()];
        if (top..top + ys.len()).contains(&y) {
            filter.row(&source, &xs, ys[y - top], content);
        } else {
            content.fill(options.background);
        }
        match options.dither {
            Dither::None => {
                for (pixel, &rgb) in row.iter_mut().zip(&colors) {
                    *pixel = truncate(rgb);
                }
            }
            Dither::Ordered => {
                for (x, pixel) in row.iter_mut().enumerate() {
                    *pixel = ordered(colors[x], x, y);
                }
            }
            Dither::Diffusion => {
                let reverse = y % 2 == 1;
                for i in 0..row.len() {
                    let x = if reverse { row.len() - 1 - i } else { i };
                    row[x] = diffuse(colors[x], x, reverse, &mut errors, &mut next);
                }
                std::mem::swap(&mut errors, &mut next);
                next.fill([0; 3]);
            }
        }
    }
    Ok(Converted { width: out_w, height: out_h, pixels })
}

/// Whether a file is a picture [`decode_picture`] reads, by its bytes (else its name's extension).
pub fn is_picture(name: &str, bytes: &[u8]) -> bool {
    makepad_image_decode::detect_image_format_from_path_and_data(std::path::Path::new(name), bytes).is_some()
}

/// A picture file's pixels as RGBA: (width, height, bytes row by row from the top).
pub fn decode_picture(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let (width, height, rgba) = makepad_image_decode::decode_rgba(bytes).map_err(|e| format!("cannot decode: {e:?}"))?;
    let side = |n: usize| u32::try_from(n).map_err(|_| format!("{width}x{height} is too large"));
    Ok((side(width)?, side(height)?, rgba))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Pcm16,
    ImaAdpcm,
}

#[derive(Clone, Copy, Debug)]
pub struct SoundOptions {
    pub rate: u32,
    pub codec: Codec,
}

#[derive(Debug)]
pub struct ConvertedSound {
    pub rate: u32,
    pub samples: u32,
    pub codec: Codec,
    pub bytes: Vec<u8>,
}

fn wav_chunks(wav: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let header = wav.get(..12).ok_or("truncated RIFF/WAVE header")?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Err("expected a little-endian RIFF/WAVE file".into());
    }
    let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let body = wav[8..].get(..size).ok_or("truncated RIFF/WAVE body")?;
    let mut chunks = body.get(4..).ok_or("RIFF size excludes WAVE identifier")?;
    let (mut fmt, mut data) = (None, None);
    while !chunks.is_empty() {
        let header = chunks.get(..8).ok_or("truncated WAV chunk header")?;
        let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let (payload, tail) = chunks[8..].split_at_checked(size).ok_or("truncated WAV chunk")?;
        match &header[..4] {
            b"fmt " if fmt.is_none() => fmt = Some(payload),
            b"data" if data.is_none() => data = Some(payload),
            _ => {}
        }
        chunks = tail.get(size % 2..).ok_or("truncated WAV chunk padding")?;
    }
    Ok((fmt.ok_or("missing WAV fmt chunk")?, data.ok_or("missing WAV data chunk")?))
}

enum WavEncoding {
    Pcm8,
    Pcm16,
    Pcm24,
    Pcm32,
    Float32,
}

struct WavFormat {
    encoding: WavEncoding,
    rate: u32,
    channels: u16,
    sample_bytes: usize,
}

impl WavFormat {
    fn parse(fmt: &[u8]) -> Result<Self, String> {
        if fmt.len() < 16 {
            return Err("truncated WAV fmt chunk".into());
        }
        let mut tag = u16::from_le_bytes([fmt[0], fmt[1]]);
        let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
        let rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
        let align = u16::from_le_bytes([fmt[12], fmt[13]]);
        let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
        if tag == 0xfffe {
            if fmt.len() < 40 {
                return Err("truncated extensible WAV fmt chunk".into());
            }
            let extension = usize::from(u16::from_le_bytes([fmt[16], fmt[17]]));
            if extension < 22 || extension > fmt.len() - 18 {
                return Err("invalid extensible WAV fmt extension size".into());
            }
            tag = u16::from_le_bytes([fmt[24], fmt[25]]);
        }
        let encoding = match (tag, bits) {
            (1, 8) => WavEncoding::Pcm8,
            (1, 16) => WavEncoding::Pcm16,
            (1, 24) => WavEncoding::Pcm24,
            (1, 32) => WavEncoding::Pcm32,
            (3, 32) => WavEncoding::Float32,
            _ => return Err(format!("unsupported WAV format {tag:#x} with {bits} bits per sample")),
        };
        if !(1..=8).contains(&channels) {
            return Err(format!("WAV channel count must be 1..=8, got {channels}"));
        }
        if rate == 0 {
            return Err("WAV source rate must be non-zero".into());
        }
        let expected = channels * (bits / 8);
        if align != expected {
            return Err(format!("WAV block align is {align}, expected {expected}"));
        }
        Ok(Self { encoding, rate, channels, sample_bytes: usize::from(bits / 8) })
    }

    fn mono(&self, frame: &[u8]) -> i16 {
        let sum: i32 = frame.chunks_exact(self.sample_bytes).map(|s| match self.encoding {
            WavEncoding::Pcm8 => (i32::from(s[0]) - 128) << 8,
            WavEncoding::Pcm16 => i32::from(i16::from_le_bytes([s[0], s[1]])),
            WavEncoding::Pcm24 => i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 16,
            WavEncoding::Pcm32 => i32::from_le_bytes(s.try_into().unwrap()) >> 16,
            WavEncoding::Float32 => i32::from(float_to_i16(f32::from_le_bytes(s.try_into().unwrap()))),
        }).sum();
        let channels = i32::from(self.channels);
        // Integer channel mean with ties away from zero.
        ((sum + sum.signum() * (channels / 2)) / channels) as i16
    }
}

/// Full-scale float audio to signed PCM. The integer cast maps NaN to silence.
pub fn float_to_i16(value: f32) -> i16 {
    (value.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

fn weighted_sample(sum: i64, weight: i64) -> i16 {
    // Round half up, including negative ties, as in image's fixed-point filters.
    let sample = (sum + weight / 2).div_euclid(weight);
    sample.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

/// Resamples mono PCM on an integer time grid. Each input spans `rate` ticks and each output
/// spans `source_rate` ticks. The fractional period carries across calls.
pub struct Resampler {
    unit: i64,
    step: i64,
    phase: i64,
    sum: i64,
    left: Option<i16>,
}

impl Resampler {
    pub fn new(source_rate: u32, rate: u32) -> Self {
        assert!(source_rate > 0 && rate > 0, "sound rates must be non-zero");
        Self { unit: i64::from(rate), step: i64::from(source_rate), phase: 0, sum: 0, left: None }
    }

    /// Feeds one signed PCM sample, keeping the unfinished output period for the next call.
    pub fn push(&mut self, sample: i16, mut emit: impl FnMut(i16)) {
        if self.unit == self.step {
            emit(sample);
        } else if self.unit > self.step {
            if let Some(left) = self.left.replace(sample) { self.interpolate(left, sample, emit); }
        } else {
            let take = self.unit.min(self.step - self.phase);
            self.sum += i64::from(sample) * take;
            self.phase += take;
            if self.phase == self.step {
                emit(weighted_sample(self.sum, self.step));
                self.phase = self.unit - take;
                self.sum = i64::from(sample) * self.phase;
            }
        }
    }

    fn interpolate(&mut self, left: i16, right: i16, mut emit: impl FnMut(i16)) {
        while self.phase < self.unit {
            let sum = i64::from(left) * (self.unit - self.phase) + i64::from(right) * self.phase;
            emit(weighted_sample(sum, self.unit));
            self.phase += self.step;
        }
        self.phase -= self.unit;
    }

    /// Emits the fractional last period, or the held last sample when interpolating.
    pub fn finish(&mut self, mut emit: impl FnMut(i16)) {
        if let Some(last) = self.left.take() {
            self.interpolate(last, last, emit);
        } else if self.unit <= self.step && self.phase > 0 {
            emit(weighted_sample(self.sum, self.phase));
        }
        self.phase = 0;
        self.sum = 0;
    }
}

fn resample(source: impl Iterator<Item = i16>, source_rate: u32, rate: u32, mut emit: impl FnMut(i16)) {
    let mut resampler = Resampler::new(source_rate, rate);
    for sample in source { resampler.push(sample, &mut emit); }
    resampler.finish(emit);
}

// IMA Digital Audio Focus and Technical Working Groups' recommendation (1992).
pub const IMA_STEP: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31,
    34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143,
    157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544,
    598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878,
    2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818,
    18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
pub const IMA_INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

const IMA_BLOCK_SAMPLES: u32 = 505;

#[derive(Default)]
struct ImaEncoder {
    predictor: i32,
    index: i32,
    block_pos: u32,
}

impl ImaEncoder {
    fn push(&mut self, bytes: &mut Vec<u8>, sample: i16) {
        if self.block_pos == 0 {
            // Each block seeds the predictor with its first PCM sample and carries the index.
            self.predictor = i32::from(sample);
            bytes.extend_from_slice(&sample.to_le_bytes());
            bytes.extend_from_slice(&[self.index as u8, 0]);
        } else {
            let step = IMA_STEP[self.index as usize];
            let delta = i32::from(sample) - self.predictor;
            let mut code = if delta < 0 { 8 } else { 0 };
            let mut remainder = delta.abs();
            let mut change = step >> 3;
            for (bit, weight) in [(4, step), (2, step >> 1), (1, step >> 2)] {
                if remainder >= weight {
                    code |= bit;
                    remainder -= weight;
                    change += weight;
                }
            }
            self.predictor += if delta < 0 { -change } else { change };
            self.predictor = self.predictor.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
            self.index = (self.index + IMA_INDEX[usize::from(code)]).clamp(0, 88);
            if self.block_pos % 2 == 1 {
                bytes.push(code);
            } else {
                *bytes.last_mut().unwrap() |= code << 4;
            }
        }
        self.block_pos = (self.block_pos + 1) % IMA_BLOCK_SAMPLES;
    }
}

/// Convert RIFF/WAVE samples to mono at the firmware playback rate.
pub fn sound(wav: &[u8], options: &SoundOptions) -> Result<ConvertedSound, String> {
    let SoundOptions { rate, codec } = *options;
    if !(8000..=48000).contains(&rate) {
        return Err(format!("output rate must be 8000..=48000 Hz, got {rate}"));
    }
    let (fmt, data) = wav_chunks(wav)?;
    let format = WavFormat::parse(fmt)?;
    let frames = data.chunks_exact(usize::from(format.channels) * format.sample_bytes);
    if !frames.remainder().is_empty() {
        return Err("truncated WAV sample frame in data chunk".into());
    }
    if frames.len() == 0 {
        return Err("WAV data contains zero samples".into());
    }
    let count = (frames.len() as u64 * u64::from(rate)).div_ceil(u64::from(format.rate));
    let samples = u32::try_from(count).map_err(|_| "output sample count exceeds u32")?;
    let size = match codec {
        Codec::Pcm16 => count * 2,
        Codec::ImaAdpcm => {
            let blocks = count.div_ceil(u64::from(IMA_BLOCK_SAMPLES));
            4 * blocks + (count - blocks).div_ceil(2)
        }
    };
    let mut bytes = Vec::new();
    let size = usize::try_from(size).map_err(|_| "output byte count exceeds addressable space")?;
    bytes.try_reserve_exact(size).map_err(|_| format!("cannot allocate {size} sound bytes"))?;
    let mono = frames.map(|frame| format.mono(frame));
    match codec {
        Codec::Pcm16 => resample(mono, format.rate, rate, |s| bytes.extend_from_slice(&s.to_le_bytes())),
        Codec::ImaAdpcm => {
            let mut encoder = ImaEncoder::default();
            resample(mono, format.rate, rate, |s| encoder.push(&mut bytes, s));
        }
    }
    Ok(ConvertedSound { rate, samples, codec, bytes })
}
