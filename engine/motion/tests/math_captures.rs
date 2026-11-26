//! Math layers drawn on the CPU: the formulas' pieces filled (non-zero rule,
//! 4 sub-scanlines a pixel, exact horizontal coverage) over the document's
//! background and written as PNGs when MOTION_MATH_CAPTURE_DIR is set. A
//! picture of the geometry (outlines, placement, delimiter assembly, a part
//! coloured by its key at a time), not of the GPU renderer.

use makepad_motion::frame::{Item, ItemKind};
use makepad_motion::{geometry, MotionDoc};

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<f32>, // rgb
}

impl Canvas {
    fn new(w: usize, h: usize, bg: [f64; 4]) -> Self {
        let mut px = vec![0.0; w * h * 3];
        for p in px.chunks_mut(3) {
            p.copy_from_slice(&[bg[0] as f32, bg[1] as f32, bg[2] as f32]);
        }
        Canvas { w, h, px }
    }

    fn fill(&mut self, polys: &[Vec<[f64; 2]>], rgba: [f64; 4]) {
        const SUB: usize = 4;
        let (mut y0, mut y1) = (f64::MAX, f64::MIN);
        for p in polys.iter().flatten() {
            y0 = y0.min(p[1]);
            y1 = y1.max(p[1]);
        }
        if y0 > y1 {
            return;
        }
        let row0 = (y0.floor().max(0.0)) as usize;
        let row1 = (y1.ceil().min(self.h as f64)) as usize;
        for row in row0..row1 {
            let mut cover = vec![0.0f32; self.w];
            for s in 0..SUB {
                let y = row as f64 + (s as f64 + 0.5) / SUB as f64;
                let mut xs: Vec<(f64, i32)> = Vec::new();
                for poly in polys {
                    for i in 0..poly.len() {
                        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                        if (a[1] <= y) != (b[1] <= y) {
                            let x = a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                            xs.push((x, if b[1] > a[1] { 1 } else { -1 }));
                        }
                    }
                }
                xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                let mut wind = 0;
                for k in 0..xs.len().saturating_sub(1) {
                    wind += xs[k].1;
                    if wind != 0 {
                        let (xa, xb) = (xs[k].0.max(0.0), xs[k + 1].0.min(self.w as f64));
                        let mut x = xa;
                        while x < xb {
                            let px = x.floor() as usize;
                            let next = ((px + 1) as f64).min(xb);
                            if px < self.w {
                                cover[px] += ((next - x) / SUB as f64) as f32;
                            }
                            x = next;
                        }
                    }
                }
            }
            for (x, c) in cover.iter().enumerate() {
                if *c > 0.0 {
                    let a = (c.min(1.0)) * rgba[3] as f32;
                    let p = &mut self.px[(row * self.w + x) * 3..][..3];
                    for k in 0..3 {
                        p[k] = p[k] * (1.0 - a) + rgba[k] as f32 * a;
                    }
                }
            }
        }
    }

    fn painted(&self, bg: [f64; 4]) -> usize {
        self.px.chunks(3).filter(|p| (p[0] - bg[0] as f32).abs() > 0.02 || (p[1] - bg[1] as f32).abs() > 0.02 || (p[2] - bg[2] as f32).abs() > 0.02).count()
    }

    fn png(&self) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut c = 0xffff_ffffu32;
            for &b in data {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
            out.extend((data.len() as u32).to_be_bytes());
            let mut body = tag.to_vec();
            body.extend(data);
            out.extend(&body);
            out.extend(crc(&body).to_be_bytes());
        }
        let mut raw = Vec::with_capacity((self.w * 3 + 1) * self.h);
        for row in 0..self.h {
            raw.push(0);
            for v in &self.px[row * self.w * 3..(row + 1) * self.w * 3] {
                raw.push((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
        }
        // zlib, stored blocks
        let mut z = vec![0x78, 0x01];
        for (i, block) in raw.chunks(65535).enumerate() {
            let last = (i + 1) * 65535 >= raw.len();
            z.push(last as u8);
            z.extend((block.len() as u16).to_le_bytes());
            z.extend((!(block.len() as u16)).to_le_bytes());
            z.extend(block);
        }
        let (mut s1, mut s2) = (1u32, 0u32);
        for &b in &raw {
            s1 = (s1 + b as u32) % 65521;
            s2 = (s2 + s1) % 65521;
        }
        z.extend(((s2 << 16) | s1).to_be_bytes());
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut ihdr = Vec::new();
        ihdr.extend((self.w as u32).to_be_bytes());
        ihdr.extend((self.h as u32).to_be_bytes());
        ihdr.extend([8, 2, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }
}

fn paint(canvas: &mut Canvas, item: &Item, opacity: f64) {
    let opacity = opacity * item.opacity;
    match &item.kind {
        ItemKind::Group { children, .. } => {
            for child in children {
                paint(canvas, child, opacity);
            }
        }
        ItemKind::Path { path, fill: Some(fill), .. } => {
            let Some(b) = geometry::bounds(path) else { return };
            let m = item.transform([b[2], b[3]]);
            let polys: Vec<Vec<[f64; 2]>> = geometry::flatten(path, 0.05).into_iter().map(|(pts, _)| pts.into_iter().map(|p| m.apply(p)).collect()).collect();
            canvas.fill(&polys, [fill[0], fill[1], fill[2], fill[3] * opacity]);
        }
        _ => {}
    }
}

fn render(source: &str, t: f64) -> (Canvas, [f64; 4]) {
    let doc = load(source);
    let frame = doc.eval(t);
    let bg = frame.background.unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let mut canvas = Canvas::new(frame.size[0] as usize, frame.size[1] as usize, bg);
    for item in &frame.items {
        paint(&mut canvas, item, 1.0);
    }
    (canvas, bg)
}

fn doc(layer: &str) -> String {
    format!("use mod.motion.*; Motion{{size: vec2(1280, 480) duration: 2.0 background: #101418 layers: [{layer}]}}")
}

fn cases() -> Vec<(&'static str, String, f64)> {
    vec![
        (
            "fourier_series",
            doc(r#"Math{tex: "S_N(x) = \\frac{4}{\\pi}\\sum_{k=1}^{N} \\frac{\\sin((2k-1)x)}{\\id{den}{2k-1}}" size: 56 color: #f0ebe0 at: vec2(120, 90)
                parts: {den: {color: tween(#f0ebe0, #ff6a00, 0.2, 1.0, @linear)}}}"#),
            1.2,
        ),
        (
            "fourier_transform",
            doc(r#"Math{tex: "\\hat f(\\xi) = \\int_{-\\infty}^{\\infty} f(x)\\, \\id{kernel}{e^{-2\\pi i x \\xi}}\\, dx" size: 56 color: #f0ebe0 at: vec2(120, 120)
                parts: {kernel: {color: #3fb6ff}}}"#),
            1.0,
        ),
        (
            "matrix",
            doc(r#"Math{tex: "A = \\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}, \\quad \\det A = ad - bc" size: 64 color: #f0ebe0 at: vec2(160, 100)
                chars: {b: {color: #ff6a00} c: {color: #ff6a00}}}"#),
            1.0,
        ),
        (
            "radical",
            doc(r#"Math{tex: "x = \\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}, \\quad \\sqrt[3]{\\frac{1}{x}}" size: 56 color: #f0ebe0 at: vec2(120, 160)
                paths: {"0/num/2": {color: #ff6a00}}}"#),
            1.0,
        ),
        (
            "delimiters_and_cases",
            doc(r#"Math{tex: "\\left[ \\frac{\\frac{a}{b}}{\\frac{c}{d}} \\right] \\quad f(x) = \\begin{cases} x^2 & x \\ge 0 \\\\ -x & \\text{else} \\end{cases}" size: 52 color: #f0ebe0 at: vec2(100, 70)}"#),
            0.5,
        ),
        (
            "aligned",
            doc(r#"Math{tex: "\\begin{aligned} (a+b)^2 &= a^2 + 2ab + b^2 \\\\ &\\ge 4ab \\end{aligned}" size: 56 color: #f0ebe0 at: vec2(160, 80)}"#),
            0.5,
        ),
    ]
}

#[test]
fn formulas_draw_and_write_captures() {
    let dir = std::env::var("MOTION_MATH_CAPTURE_DIR").ok();
    for (name, source, t) in cases() {
        let (canvas, bg) = render(&source, t);
        let painted = canvas.painted(bg);
        assert!(painted > 3000, "{name}: only {painted} pixels drawn");
        if let Some(dir) = &dir {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(format!("{dir}/math_{name}.png"), canvas.png()).unwrap();
        }
    }
    // a part keyed by its id changes colour over time and nothing else moves
    let (_, source, _) = cases().into_iter().next().unwrap();
    let (early, bg) = render(&source, 0.0);
    let (late, _) = render(&source, 1.5);
    assert!(early.painted(bg).abs_diff(late.painted(bg)) < 50, "same shapes");
    assert!(early.px != late.px, "the part's colour changed");
}
