//! Generated test images for the screen beneath a display window.

/// The image shown on a display, or a plain panel when off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenImageKind {
    Off,
    TestCard,
    ColourBars,
    GreyRamp,
    ZonePlate,
}

impl ScreenImageKind {
    /// The label shown in the image picker.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "Off (plain panel)",
            Self::TestCard => "Test card",
            Self::ColourBars => "Colour bars",
            Self::GreyRamp => "Grey ramp",
            Self::ZonePlate => "Zone plate",
        }
    }

    /// The key used to store the image choice.
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::TestCard => "testcard",
            Self::ColourBars => "bars",
            Self::GreyRamp => "ramp",
            Self::ZonePlate => "zoneplate",
        }
    }

    /// Reads a stored image key, returning none for an unknown key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "off" => Some(Self::Off),
            "testcard" => Some(Self::TestCard),
            "bars" => Some(Self::ColourBars),
            "ramp" => Some(Self::GreyRamp),
            "zoneplate" => Some(Self::ZonePlate),
            _ => None,
        }
    }
}

/// An opaque sRGB image in RGBA byte order, with row zero at the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenImage {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

/// Builds an image for a width / height aspect ratio, or none when off.
/// The aspect is clamped to 0.2..5, with 512 pixels on the short side.
/// There is no global cache: the caller keeps the image. Building one takes a few milliseconds.
pub fn screen_image(kind: ScreenImageKind, aspect: f64) -> Option<ScreenImage> {
    if kind == ScreenImageKind::Off {
        return None;
    }
    let (w, h) = image_size(aspect, 512);
    match kind {
        ScreenImageKind::Off => None,
        ScreenImageKind::TestCard => Some(make_test_card(w, h)),
        ScreenImageKind::ColourBars => Some(make_bars(w, h)),
        ScreenImageKind::GreyRamp => Some(make_ramp(w, h)),
        ScreenImageKind::ZonePlate => Some(make_zone(w, h)),
    }
}

fn image_size(aspect: f64, short_side: usize) -> (usize, usize) {
    let aspect = aspect.clamp(0.2, 5.0);
    let h = if aspect >= 1.0 {
        short_side
    } else {
        (short_side as f64 / aspect).round() as usize
    };
    let w = if aspect >= 1.0 {
        (short_side as f64 * aspect).round() as usize
    } else {
        short_side
    };
    (w, h)
}

type Colour = [f32; 3];

const BLACK: Colour = [0.0, 0.0, 0.0];
const WHITE: Colour = [1.0, 1.0, 1.0];

// An sRGB canvas (0..1), converted to bytes after drawing.
struct Canvas {
    w: usize,
    h: usize,
    px: Vec<Colour>,
}

impl Canvas {
    fn new(w: usize, h: usize, background: Colour) -> Self {
        Self {
            w,
            h,
            px: vec![background; w * h],
        }
    }

    fn set(&mut self, x: i32, y: i32, colour: Colour, alpha: f32) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 || alpha <= 0.0 {
            return;
        }
        let dest = &mut self.px[y as usize * self.w + x as usize];
        for i in 0..3 {
            dest[i] = dest[i] + (colour[i] - dest[i]) * alpha;
        }
    }

    // Fractional rectangle edges are antialiased, then multiplied by the clip coverage.
    fn rect_clipped(
        &mut self,
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        colour: Colour,
        clip: impl Fn(f64, f64) -> f64,
    ) {
        for y in (y0.floor() as i32).max(0)..(y1.ceil() as i32).min(self.h as i32) {
            for x in (x0.floor() as i32).max(0)..(x1.ceil() as i32).min(self.w as i32) {
                let cx = ((x as f64 + 1.0).min(x1) - (x as f64).max(x0)).clamp(0.0, 1.0);
                let cy = ((y as f64 + 1.0).min(y1) - (y as f64).max(y0)).clamp(0.0, 1.0);
                let alpha = clip(x as f64 + 0.5, y as f64 + 0.5);
                self.set(x, y, colour, (cx * cy * alpha) as f32);
            }
        }
    }

    fn rect(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, colour: Colour) {
        self.rect_clipped(x0, y0, x1, y1, colour, |_, _| 1.0);
    }

    fn into_image(self) -> ScreenImage {
        let mut rgba = vec![0; self.w * self.h * 4];
        for (colour, pixel) in self.px.into_iter().zip(rgba.chunks_exact_mut(4)) {
            for i in 0..3 {
                pixel[i] = (colour[i].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            pixel[3] = 255;
        }
        ScreenImage {
            w: self.w,
            h: self.h,
            rgba,
        }
    }
}

// A broadcast test card with a grid, border blocks, side columns and a patterned circle.
fn make_test_card(w: usize, h: usize) -> ScreenImage {
    let mut c = Canvas::new(w, h, [0.66, 0.66, 0.66]);
    let (w, h) = (w as f64, h as f64);
    let cell = h / 12.0;
    let line = 1.0_f64.max(h / 200.0);
    let cx = 0.5 * w;
    let cy = 0.5 * h;

    // Centred grid.
    let mut x = cx - (cx / cell).floor() * cell;
    while x < w {
        c.rect(x - line * 0.5, 0.0, x + line * 0.5, h, WHITE);
        x += cell;
    }
    let mut y = cy - (cy / cell).floor() * cell;
    while y < h {
        c.rect(0.0, y - line * 0.5, w, y + line * 0.5, WHITE);
        y += cell;
    }

    // Alternating black / white border blocks, half a cell deep.
    let b = cell * 0.45;
    let mut i = 0;
    while i as f64 * cell * 0.5 < w {
        let k = if i % 2 != 0 { WHITE } else { BLACK };
        c.rect(i as f64 * cell * 0.5, 0.0, (i + 1) as f64 * cell * 0.5, b, k);
        c.rect(i as f64 * cell * 0.5, h - b, (i + 1) as f64 * cell * 0.5, h, k);
        i += 1;
    }
    let mut i = 0;
    while i as f64 * cell * 0.5 < h {
        let k = if i % 2 != 0 { BLACK } else { WHITE };
        c.rect(0.0, i as f64 * cell * 0.5, b, (i + 1) as f64 * cell * 0.5, k);
        c.rect(w - b, i as f64 * cell * 0.5, w, (i + 1) as f64 * cell * 0.5, k);
        i += 1;
    }

    let radius = (0.44 * h).min(0.47 * w);
    // Side columns only fit when the screen is wider than the circle.
    if cx - radius > 2.2 * cell {
        let x0 = cx - radius - 1.9 * cell;
        let x1 = cx + radius + 0.9 * cell;
        c.rect(x0, cy - 4.0 * cell, x0 + cell, cy, [0.0, 0.58, 0.45]);
        c.rect(x0, cy, x0 + cell, cy + 4.0 * cell, [0.76, 0.24, 0.47]);
        c.rect(x0 + cell, cy - 4.0 * cell, x0 + 1.8 * cell, cy - 3.0 * cell, [0.10, 0.20, 0.70]);
        c.rect(x0 + cell, cy + 3.0 * cell, x0 + 1.8 * cell, cy + 4.0 * cell, [0.74, 0.30, 0.0]);
        c.rect(x1, cy - 4.0 * cell, x1 + cell, cy, [0.46, 0.73, 0.27]);
        c.rect(x1, cy, x1 + cell, cy + 4.0 * cell, [0.52, 0.52, 1.0]);
        c.rect(x1 - 0.8 * cell, cy - 4.0 * cell, x1, cy - 3.0 * cell, [0.10, 0.20, 0.70]);
        c.rect(x1 - 0.8 * cell, cy + 3.0 * cell, x1, cy + 4.0 * cell, [0.74, 0.30, 0.0]);
    }

    // All remaining shapes are clipped to the antialiased circle.
    let inside = |x: f64, y: f64| (radius - (x - cx).hypot(y - cy) + 0.5).clamp(0.0, 1.0);
    let top = cy - radius;
    let height = 2.0 * radius;
    let left = cx - radius;
    let width = 2.0 * radius;
    let band = |c: &mut Canvas, t0: f64, t1: f64, k| {
        c.rect_clipped(left, top + t0 * height, left + width, top + t1 * height, k, inside);
    };
    band(&mut c, 0.0, 1.0, WHITE);

    // Empty upper box.
    let bw = 0.62 * width;
    let bh = 0.12 * height;
    let bx = cx - 0.5 * bw;
    let by = top + 0.05 * height;
    c.rect_clipped(bx, by, bx + bw, by + bh, BLACK, inside);

    // Castellation.
    band(&mut c, 0.21, 0.30, BLACK);
    for i in 0..9 {
        if i % 2 != 0 {
            c.rect_clipped(left + width * i as f64 / 9.0, top + 0.21 * height,
                left + width * (i + 1) as f64 / 9.0, top + 0.26 * height, WHITE, inside);
        }
    }

    // Colour bars.
    let bars: [Colour; 6] = [
        [1.0, 1.0, 0.0], [0.0, 1.0, 1.0], [0.0, 1.0, 0.0],
        [1.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0],
    ];
    for (i, colour) in bars.into_iter().enumerate() {
        c.rect_clipped(left + width * i as f64 / 6.0, top + 0.30 * height,
            left + width * (i + 1) as f64 / 6.0, top + 0.44 * height, colour, inside);
    }

    // Tick row.
    band(&mut c, 0.44, 0.52, BLACK);
    c.rect_clipped(left, top + 0.48 * height - line * 0.5,
        left + width, top + 0.48 * height + line * 0.5, WHITE, inside);
    for i in 1..12 {
        let x = left + width * i as f64 / 12.0;
        c.rect_clipped(x - line * 0.5, top + 0.455 * height,
            x + line * 0.5, top + 0.505 * height, WHITE, inside);
    }

    // Frequency gratings: five groups, finer to the right.
    band(&mut c, 0.52, 0.66, BLACK);
    for g in 0..5 {
        let gx0 = left + width * (0.08 + 0.17 * g as f64);
        let gx1 = gx0 + width * 0.15;
        let period = 2.0_f64.max(width / (14.0 * (g + 1) as f64 * 1.6));
        let mut x = gx0;
        while x < gx1 {
            c.rect_clipped(x, top + 0.535 * height,
                x + period * 0.5, top + 0.645 * height, WHITE, inside);
            x += period;
        }
    }

    // Grey steps.
    for i in 0..6 {
        let v = i as f32 / 5.0;
        c.rect_clipped(left + width * i as f64 / 6.0, top + 0.66 * height,
            left + width * (i + 1) as f64 / 6.0, top + 0.77 * height, [v, v, v], inside);
    }

    // Clock / caption box.
    band(&mut c, 0.77, 0.86, BLACK);
    c.rect_clipped(cx - 0.22 * width, top + 0.79 * height,
        cx + 0.22 * width, top + 0.84 * height, WHITE, inside);

    // Yellow bottom with a red centre.
    band(&mut c, 0.86, 1.0, [1.0, 1.0, 0.0]);
    c.rect_clipped(cx - 0.05 * width, top + 0.86 * height,
        cx + 0.05 * width, top + height, [1.0, 0.0, 0.0], inside);

    // Centre cross.
    c.rect_clipped(cx - line * 0.5, top + 0.30 * height,
        cx + line * 0.5, top + 0.77 * height, WHITE, inside);
    c.rect_clipped(cx - 0.035 * width, top + 0.42 * height,
        cx + 0.035 * width, top + 0.62 * height, BLACK, inside);
    c.rect_clipped(cx - line * 0.5, top + 0.42 * height,
        cx + line * 0.5, top + 0.62 * height, WHITE, inside);
    c.into_image()
}

fn make_bars(w: usize, h: usize) -> ScreenImage {
    let mut c = Canvas::new(w, h, BLACK);
    let bars: [Colour; 7] = [
        [0.75, 0.75, 0.75], [0.75, 0.75, 0.0], [0.0, 0.75, 0.75],
        [0.0, 0.75, 0.0], [0.75, 0.0, 0.75], [0.75, 0.0, 0.0], [0.0, 0.0, 0.75],
    ];
    for (i, colour) in bars.into_iter().enumerate() {
        c.rect((w * i) as f64 / 7.0, 0.0, (w * (i + 1)) as f64 / 7.0, 0.67 * h as f64, colour);
    }
    let reverse: [Colour; 7] = [
        [0.0, 0.0, 0.75], BLACK, [0.75, 0.0, 0.75], BLACK,
        [0.0, 0.75, 0.75], BLACK, [0.75, 0.75, 0.75],
    ];
    for (i, colour) in reverse.into_iter().enumerate() {
        c.rect((w * i) as f64 / 7.0, 0.67 * h as f64,
            (w * (i + 1)) as f64 / 7.0, 0.75 * h as f64, colour);
    }
    let (w, h) = (w as f64, h as f64);
    c.rect(0.0, 0.75 * h, w * 0.18, h, [0.0, 0.13, 0.30]);
    c.rect(w * 0.18, 0.75 * h, w * 0.36, h, WHITE);
    c.rect(w * 0.36, 0.75 * h, w * 0.54, h, [0.20, 0.0, 0.42]);
    // PLUGE: below black, black, above black.
    for i in 0..3 {
        let v = if i == 0 { 0.0 } else if i == 1 { 0.035 } else { 0.075 };
        c.rect(w * (0.72 + 0.06 * i as f64), 0.75 * h,
            w * (0.78 + 0.06 * i as f64), h, [v, v, v]);
    }
    c.into_image()
}

fn make_ramp(w: usize, h: usize) -> ScreenImage {
    let mut c = Canvas::new(w, h, BLACK);
    for x in 0..w {
        let v = (x as f32 + 0.5) / w as f32;
        c.rect(x as f64, 0.0, x as f64 + 1.0, 0.5 * h as f64, [v, v, v]);
    }
    for i in 0..11 {
        let v = i as f32 / 10.0;
        c.rect((w * i) as f64 / 11.0, 0.5 * h as f64,
            (w * (i + 1)) as f64 / 11.0, h as f64, [v, v, v]);
    }
    c.into_image()
}

fn make_zone(w: usize, h: usize) -> ScreenImage {
    let mut c = Canvas::new(w, h, BLACK);
    let cx = 0.5 * w as f64;
    let cy = 0.5 * h as f64;
    let rmax = 0.5 * (w as f64).hypot(h as f64);
    let k = 3.14159265_f64 * 0.5 / rmax; // Half the Nyquist frequency at the corners.
    for y in 0..h {
        for x in 0..w {
            let dx = x as f64 + 0.5 - cx;
            let dy = y as f64 + 0.5 - cy;
            let v = 0.5 + 0.5 * (k * (dx * dx + dy * dy)).cos();
            c.px[y * w + x] = [v as f32, v as f32, v as f32];
        }
    }
    c.into_image()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(image: &ScreenImage, x: usize, y: usize) -> &[u8] {
        let offset = (y * image.w + x) * 4;
        &image.rgba[offset..offset + 4]
    }

    #[test]
    fn screen_sizes_and_choices() {
        for (aspect, expected) in [
            (0.1, (512, 2560)),
            (1.0, (512, 512)),
            (1.6, (819, 512)),
            (10.0, (2560, 512)),
        ] {
            assert_eq!(image_size(aspect, 512), expected, "screen size at aspect {aspect}");
        }
        assert!(screen_image(ScreenImageKind::Off, 1.0).is_none(), "off must not allocate an image");
        for (kind, name, key) in [
            (ScreenImageKind::Off, "Off (plain panel)", "off"),
            (ScreenImageKind::TestCard, "Test card", "testcard"),
            (ScreenImageKind::ColourBars, "Colour bars", "bars"),
            (ScreenImageKind::GreyRamp, "Grey ramp", "ramp"),
            (ScreenImageKind::ZonePlate, "Zone plate", "zoneplate"),
        ] {
            assert_eq!(kind.name(), name, "picker label for {kind:?}");
            assert_eq!(kind.key(), key, "stored key for {kind:?}");
            assert_eq!(ScreenImageKind::from_key(key), Some(kind), "key round trip for {kind:?}");
        }
        assert_eq!(ScreenImageKind::from_key("unknown"), None, "unknown image key");
    }

    #[test]
    fn screen_pixels() {
        let (w, h) = image_size(2.0, 32);
        assert_eq!((w, h), (64, 32), "small images preserve their aspect");
        let bars = make_bars(w, h);
        let ramp = make_ramp(w, h);
        let zone = make_zone(w, h);
        let card = make_test_card(w, h);
        for image in [&bars, &ramp, &zone, &card] {
            assert_eq!((image.w, image.h), (w, h), "makers preserve the requested size");
            assert_eq!(image.rgba.len(), w * h * 4, "RGBA buffer size");
            assert!(image.rgba.chunks_exact(4).all(|p| p[3] == 255), "all pixels are opaque");
        }

        assert_eq!(pixel(&bars, 0, 0), [191, 191, 191, 255], "first colour bar is 75 percent grey");
        assert_eq!(pixel(&bars, w - 1, 0), [0, 0, 191, 255], "last colour bar is 75 percent blue");

        // The upper middle row is in the continuous half; values sample pixel centres.
        let middle = (h - 1) / 2;
        assert_eq!(pixel(&ramp, 0, middle), [2, 2, 2, 255], "continuous ramp starts near black");
        assert_eq!(pixel(&ramp, w - 1, middle), [253, 253, 253, 255], "continuous ramp ends near white");
        for x in 1..w {
            let previous = pixel(&ramp, x - 1, middle);
            let current = pixel(&ramp, x, middle);
            assert!(previous[0] <= current[0], "ramp must increase at column {x}");
            assert_eq!(&current[..3], [current[0]; 3], "ramp must stay grey at column {x}");
        }
        assert_eq!(pixel(&ramp, 0, h - 1), [0, 0, 0, 255], "stepped ramp starts at black");
        assert_eq!(pixel(&ramp, w - 1, h - 1), [255, 255, 255, 255], "stepped ramp ends at white");
        assert_eq!(pixel(&zone, w / 2, h / 2), [255, 255, 255, 255], "zone plate centre is white");

        assert!(card.rgba.chunks_exact(4).any(|p| p != pixel(&card, 0, 0)), "test card is not uniform");
        // The upper box, caption box and bottom centre are symmetric within the circle.
        for (y, start) in [(5, 23), (24, 22), (27, 25)] {
            for x in start..w / 2 {
                assert_eq!(pixel(&card, x, y), pixel(&card, w - 1 - x, y),
                    "test card symmetry at ({x}, {y})");
            }
        }
    }
}
