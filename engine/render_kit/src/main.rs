//! `makepad-render-kit`: the kit on the real GPU, in its own (normally
//! hidden) window.
//!
//! ```text
//! MAKEPAD_HIDE_WINDOWS=1 makepad-render-kit test [--filter <name>] [--out <dir>]
//!     pixel tests: every effect on generated inputs, read back and checked
//!     against analytic or CPU reference values
//! MAKEPAD_HIDE_WINDOWS=1 makepad-render-kit bench [--frames 12] [--sizes 1920x1080,3840x2160] [--out table.md]
//!     GPU milliseconds per effect (command-buffer timestamps)
//! MAKEPAD_HIDE_WINDOWS=1 makepad-render-kit demo --out <dir> [--frames 8] [--size 960x540]
//!     the showcase scene (every effect) as frames and a contact strip PNG
//! MAKEPAD_HIDE_WINDOWS=1 makepad-render-kit strip --effect glow [--set radius=4..24]... [--input moving] [--times 0,0.5,1] --out s.png
//!     effect_strip: one effect over a test input at several times, as a PNG strip; without
//!     --set/--input/--times the catalogue's preview settings (`moving` = a disc on a curve and a
//!     travelling word, so time effects show motion); `a..b` ramps a value across the times
//! MAKEPAD_HIDE_WINDOWS=1 makepad-render-kit catalogue   the effect catalogue as JSON
//! MAKEPAD_HIDE_WINDOWS=1 makepad-render-kit doc         the authoring guide (Markdown)
//! ```

use makepad_stage_render_kit::catalog::EffectKind;
use makepad_stage_render_kit::shaders::KitBlend;
use makepad_stage_render_kit::{Anim, Camera, Effect, EffectStack, FnLayer, FxContext, Painted, RenderKit, ResolvedEffect, Value};
use makepad_widgets::*;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // A soft disc for demo layers: centre px, radius px, colour (straight).
    set_type_default() do #(DrawDisc::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let p = self.rect_pos + self.pos * self.rect_size
            let d = length(p - self.center) - self.radius
            let cov = clamp(0.5 - d, 0.0, 1.0)
            return vec4(self.fill.xyz * self.fill.w, self.fill.w) * cov
        }
    }

    // A solid rectangle: fill = colour (straight).
    set_type_default() do #(DrawBox::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            return vec4(self.fill.xyz * self.fill.w, self.fill.w)
        }
    }

    mod.widgets.KitHostBase = #(KitHost::register_widget(vm))
    mod.widgets.KitHost = set_type_default() do mod.widgets.KitHostBase{
        width: Fill
        height: Fill
        text +: {
            text_style: theme.font_bold{font_size: 64}
            color: #fff
        }
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Render kit"
                window.inner_size: vec2(480, 300)
                body +: {
                    host := mod.widgets.KitHost{}
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawDisc {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    center: Vec2f,
    #[live]
    radius: f32,
    #[live]
    fill: Vec4f,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawBox {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    fill: Vec4f,
}

// ---------------------------------------------------------------------------
// Images: premultiplied RGBA in 0..1.
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq)]
struct Image {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Image {
    fn new(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 4]) -> Self {
        let mut px = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                px.push(f(x, y));
            }
        }
        Self { w, h, px }
    }

    fn at(&self, x: usize, y: usize) -> [f32; 4] {
        self.px[y.min(self.h - 1) * self.w + x.min(self.w - 1)]
    }

    /// Quantised to 8 bits as the GPU sees it.
    fn texture(&self, cx: &mut Cx) -> Texture {
        let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        let data = self.px.iter().map(|p| (b(p[3]) << 24) | (b(p[0]) << 16) | (b(p[1]) << 8) | b(p[2])).collect();
        Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 { width: self.w, height: self.h, data: Some(data), updated: TextureUpdated::Full })
    }

    fn from_readback(result: &TextureReadback, data: &[u8]) -> Self {
        let (w, h) = (result.width, result.height);
        let px = data[..w * h * 4].chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]].map(|c| c as f32 / 255.0)).collect();
        Self { w, h, px }
    }

    fn rgba8(&self) -> Vec<u8> {
        // Unpremultiplied over a checker so transparency is visible.
        let mut out = Vec::with_capacity(self.w * self.h * 4);
        for y in 0..self.h {
            for x in 0..self.w {
                let p = self.at(x, y);
                let bg = if ((x / 16) + (y / 16)) % 2 == 0 { 0.16 } else { 0.24 };
                for c in 0..3 {
                    out.push(((p[c] + bg * (1.0 - p[3])).clamp(0.0, 1.0) * 255.0).round() as u8);
                }
                out.push(255);
            }
        }
        out
    }

    fn hash(&self) -> u64 {
        self.px.iter().flat_map(|p| p.iter()).fold(0xcbf2_9ce4_8422_2325u64, |h, v| (h ^ (v * 255.0).round() as u64).wrapping_mul(0x100_0000_01b3))
    }
}

fn write_png(path: &std::path::Path, w: usize, h: usize, rgba: &[u8]) -> Result<(), String> {
    use makepad_zune_png::makepad_zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions};
    let options = EncoderOptions::default().set_width(w).set_height(h).set_depth(BitDepth::Eight).set_colorspace(ColorSpace::RGBA);
    let mut out = Vec::new();
    makepad_zune_png::PngEncoder::new(rgba, options).encode(&mut out).map_err(|e| format!("png: {e:?}"))?;
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Test inputs
// ---------------------------------------------------------------------------

const W: usize = 512;
const H: usize = 256;

fn inputs_cpu() -> Vec<(&'static str, Image)> {
    let white = [1.0, 1.0, 1.0, 1.0];
    let clear = [0.0; 4];
    let in_square = |x: usize, y: usize| (160..352).contains(&x) && (64..192).contains(&y);
    vec![
        ("square", Image::new(W, H, |x, y| if in_square(x, y) { white } else { clear })),
        ("grey_square", Image::new(W, H, |x, y| if in_square(x, y) { [0.5, 0.5, 0.5, 1.0] } else { clear })),
        ("half", Image::new(W, H, |x, _| if x < W / 2 { white } else { clear })),
        ("line", Image::new(W, H, |x, _| if x == 256 { white } else { clear })),
        ("checker", Image::new(W, H, |x, y| if ((x / 16) + (y / 16)) % 2 == 0 { white } else { [0.0, 0.0, 0.0, 1.0] })),
        ("ramp", Image::new(W, H, |x, _| {
            let v = x as f32 / (W - 1) as f32;
            [v, v, v, 1.0]
        })),
        ("grey50", Image::new(W, H, |_, _| [0.5, 0.5, 0.5, 1.0])),
        ("grey25", Image::new(W, H, |_, _| [0.25, 0.25, 0.25, 1.0])),
        ("white", Image::new(W, H, |_, _| white)),
        ("red", Image::new(W, H, |_, _| [1.0, 0.0, 0.0, 1.0])),
        ("spot", Image::new(W, H, |x, y| if (246..266).contains(&x) && (118..138).contains(&y) { white } else { [0.0, 0.0, 0.0, 1.0] })),
        ("colors", Image::new(W, H, |x, _| match x * 4 / W {
            0 => [0.9, 0.2, 0.1, 1.0],
            1 => [0.1, 0.8, 0.3, 1.0],
            2 => [0.2, 0.3, 0.9, 1.0],
            _ => [0.6, 0.6, 0.2, 1.0],
        })),
        ("backdrop", Image::new(W, H, |x, _| if x < W / 2 { [0.2, 0.5, 0.8, 1.0] } else { [0.12, 0.3, 0.48, 0.6] })),
        ("source", Image::new(W, H, |_, _| [0.7 * 0.8, 0.3 * 0.8, 0.4 * 0.8, 0.8])),
    ]
}

struct Inputs {
    cpu: HashMap<&'static str, Image>,
    tex: HashMap<String, Texture>,
}

impl Inputs {
    fn t(&self, name: &str) -> &Texture {
        &self.tex[name]
    }
    fn i(&self, name: &str) -> &Image {
        &self.cpu[name]
    }
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

type Record = Box<dyn FnMut(&mut Cx2d, &mut RenderKit, &Inputs, &mut DrawBox) -> Texture>;
type Check = Box<dyn Fn(&Image, &Inputs, &RenderKit) -> Result<String, String>>;

struct Case {
    name: String,
    record: Record,
    check: Check,
    /// Record twice (two frames) and require identical pixels.
    twice: bool,
}

fn fx(kind: EffectKind) -> ResolvedEffect {
    ResolvedEffect::new(kind)
}

fn num(v: f64) -> Value {
    Value::Num(v)
}

fn ch(v: &str) -> Value {
    Value::Choice(v.into())
}

/// One stack on one input at t.
fn on(input: &'static str, t: f64, stack: Vec<ResolvedEffect>) -> Record {
    Box::new(move |cx, kit, inputs, _| {
        let ctx = FxContext::new(t, 30.0, &inputs.tex);
        kit.apply(cx, &stack, inputs.t(input), (W as u32, H as u32), &ctx).texture
    })
}

fn case(name: &str, record: Record, check: impl Fn(&Image, &Inputs, &RenderKit) -> Result<String, String> + 'static) -> Case {
    Case { name: name.into(), record, check: Box::new(check), twice: false }
}

fn close(what: &str, got: f32, want: f32, tol: f32) -> Result<(), String> {
    if (got - want).abs() <= tol {
        Ok(())
    } else {
        Err(format!("{what}: got {got:.4}, want {want:.4} ± {tol}"))
    }
}

fn close4(what: &str, got: [f32; 4], want: [f32; 4], tol: f32) -> Result<(), String> {
    for c in 0..4 {
        close(&format!("{what}[{}]", ["r", "g", "b", "a"][c]), got[c], want[c], tol)?;
    }
    Ok(())
}

fn max_diff(a: &Image, b: &Image) -> f32 {
    a.px.iter().zip(&b.px).map(|(p, q)| (0..4).map(|c| (p[c] - q[c]).abs()).fold(0.0, f32::max)).fold(0.0, f32::max)
}

fn phi(x: f64) -> f64 {
    // Standard normal CDF via erf (Abramowitz–Stegun 7.1.26).
    let z = x / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.3275911 * z.abs());
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * (-z * z).exp();
    0.5 * (1.0 + z.signum() * y)
}

/// W3C blend of straight colours (the reference for the GPU modes).
fn blend_ref(mode: &str, cb: [f64; 3], cs: [f64; 3]) -> [f64; 3] {
    let lum = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let clip = |c: [f64; 3]| {
        let l = lum(c);
        let n = c[0].min(c[1]).min(c[2]);
        let x = c[0].max(c[1]).max(c[2]);
        let mut o = c;
        if n < 0.0 {
            o = o.map(|v| l + (v - l) * l / (l - n));
        }
        if x > 1.0 {
            o = o.map(|v| l + (v - l) * (1.0 - l) / (x - l));
        }
        o
    };
    let set_lum = |c: [f64; 3], l: f64| {
        let d = l - lum(c);
        clip(c.map(|v| v + d))
    };
    let sat = |c: [f64; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
    let set_sat = |c: [f64; 3], s: f64| {
        let n = c[0].min(c[1]).min(c[2]);
        let x = c[0].max(c[1]).max(c[2]);
        if x - n < 1e-9 { [0.0; 3] } else { c.map(|v| (v - n) * s / (x - n)) }
    };
    let sep = |f: &dyn Fn(f64, f64) -> f64| [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])];
    let hard = |b: f64, s: f64| if s <= 0.5 { b * 2.0 * s } else { 1.0 - (1.0 - b) * (1.0 - (2.0 * s - 1.0)) };
    let soft = |b: f64, s: f64| {
        if s <= 0.5 {
            b - (1.0 - 2.0 * s) * b * (1.0 - b)
        } else {
            let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
            b + (2.0 * s - 1.0) * (d - b)
        }
    };
    match mode {
        "multiply" => sep(&|b, s| b * s),
        "screen" => sep(&|b, s| b + s - b * s),
        "overlay" => sep(&|b, s| hard(s, b)),
        "darken" => sep(&|b, s| b.min(s)),
        "lighten" => sep(&|b, s| b.max(s)),
        "color_dodge" => sep(&|b, s| if b == 0.0 { 0.0 } else if s >= 1.0 { 1.0 } else { (b / (1.0 - s)).min(1.0) }),
        "color_burn" => sep(&|b, s| if b >= 1.0 { 1.0 } else if s <= 0.0 { 0.0 } else { 1.0 - ((1.0 - b) / s).min(1.0) }),
        "hard_light" => sep(&hard),
        "soft_light" => sep(&soft),
        "difference" => sep(&|b, s| (b - s).abs()),
        "exclusion" => sep(&|b, s| b + s - 2.0 * b * s),
        "add" => sep(&|b, s| (b + s).min(1.0)),
        "subtract" => sep(&|b, s| (b - s).max(0.0)),
        "divide" => sep(&|b, s| (b / s.max(1e-5)).min(1.0)),
        "hue" => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        "saturation" => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        "color" => set_lum(cs, lum(cb)),
        "luminosity" => set_lum(cb, lum(cs)),
        _ => cs,
    }
}

fn composite_ref(mode: &str, b: [f32; 4], s: [f32; 4]) -> [f32; 4] {
    let un = |p: [f32; 4]| [0, 1, 2].map(|c| if p[3] > 0.0 { p[c] as f64 / p[3] as f64 } else { 0.0 });
    let m = blend_ref(mode, un(b), un(s));
    let (ba, sa) = (b[3] as f64, s[3] as f64);
    let rgb = [0, 1, 2].map(|c| (s[c] as f64 * (1.0 - ba) + b[c] as f64 * (1.0 - sa) + sa * ba * m[c]) as f32);
    [rgb[0], rgb[1], rgb[2], (sa + ba * (1.0 - sa)) as f32]
}

/// A square of `size` px at x = x0 + speed·t (y centred), drawn by the host.
fn moving_square(draw: &mut DrawBox, x0: f64, speed: f64, size: f64) -> impl FnMut(&mut Cx2d, f64, (u32, u32)) + '_ {
    move |cx, t, frame| {
        draw.fill = vec4(1.0, 1.0, 1.0, 1.0);
        let y = frame.1 as f64 * 0.5 - size * 0.5;
        draw.draw_abs(cx, Rect { pos: dvec2(x0 + speed * t, y), size: dvec2(size, size) });
    }
}

/// A 17³ .cube LUT that swaps red and blue, in the temp dir.
fn write_swap_lut() -> PathBuf {
    let path = std::env::temp_dir().join("render_kit_swap.cube");
    let mut text = String::from("TITLE \"swap red and blue\"\nLUT_3D_SIZE 17\n");
    for b in 0..17 {
        for g in 0..17 {
            for r in 0..17 {
                text.push_str(&format!("{} {} {}\n", b as f64 / 16.0, g as f64 / 16.0, r as f64 / 16.0));
            }
        }
    }
    let _ = std::fs::write(&path, text);
    path
}

/// The strips' moving input at `t`: a disc on a Lissajous curve and a word
/// sliding across, drawn into the pass being drawn (512×256).
fn moving_centre(t: f64) -> [f64; 2] {
    [256.0 + 190.0 * (4.0 * t).sin(), 150.0 + 70.0 * (5.5 * t + 0.5).sin()]
}

fn draw_moving(cx: &mut Cx2d, t: f64, disc: &mut DrawDisc, text: &mut DrawText) {
    let [x, y] = moving_centre(t);
    let r = 22.0;
    disc.center = vec2(x as f32, y as f32);
    disc.radius = r as f32;
    disc.fill = vec4(1.0, 0.75, 0.25, 1.0);
    disc.draw_abs(cx, Rect { pos: dvec2(x - r - 1.0, y - r - 1.0), size: dvec2(2.0 * r + 2.0, 2.0 * r + 2.0) });
    text.text_style.font_size = 44.0;
    text.color = vec4(0.85, 0.95, 1.0, 1.0);
    let x = 20.0 + (t * 300.0).rem_euclid(460.0);
    text.draw_walk(cx, Walk::abs_rect(Rect { pos: dvec2(x, 4.0), size: dvec2(120.0, 60.0) }), Align { x: 0.0, y: 0.5 }, "KIT");
}

fn cases() -> Vec<Case> {
    use EffectKind::*;
    let mut out = Vec::new();

    // ── Blur ────────────────────────────────────────────────────────────
    for sigma in [2.0, 8.0, 40.0, 150.0] {
        let e = fx(GaussianBlur).with("radius", num(sigma)).with("edge", ch("extend"));
        out.push(case(&format!("gaussian_blur edge profile sigma {sigma}"), on("half", 0.0, vec![e]), move |img, _, _| {
            let mut worst: f32 = 0.0;
            let reach = (3.0 * sigma).min(250.0) as i64;
            for x in (256 - reach).max(0)..(256 + reach).min(W as i64) {
                let want = phi((256.0 - (x as f64 + 0.5)) / sigma) as f32;
                worst = worst.max((img.at(x as usize, 128)[3] - want).abs());
            }
            if worst > 0.012 {
                return Err(format!("alpha deviates from the gaussian edge by {worst:.4}"));
            }
            Ok(format!("max |alpha - Φ| = {worst:.4}"))
        }));
    }
    out.push(case(
        "gaussian_blur horizontal keeps vertical edges",
        on("square", 0.0, vec![fx(GaussianBlur).with("radius", num(12.0)).with("direction", ch("horizontal"))]),
        |img, _, _| {
            close("above the square", img.at(256, 62)[3], 0.0, 0.004)?;
            close("inside the square", img.at(256, 66)[3], 1.0, 0.004)?;
            close("blurred side edge", img.at(352, 128)[3], 0.5, 0.06)?;
            Ok("vertical edges sharp, sides blurred".into())
        },
    ));
    out.push(case("directional_blur smears a line over its length", on("line", 0.0, vec![fx(DirectionalBlur).with("length", num(10.0))]), |img, _, _| {
        let row: Vec<f32> = (0..W).map(|x| img.at(x, 128)[3]).collect();
        let sum: f32 = row.iter().sum();
        let lit = row.iter().filter(|a| **a > 0.004).count();
        // Each of ~10 pixels holds 0.1, quantised to 8 bits.
        close("energy", sum, 1.0, 0.03)?;
        if !(9..=12).contains(&lit) {
            return Err(format!("{lit} lit pixels, want about 10"));
        }
        Ok(format!("sum {sum:.3}, {lit} px wide"))
    }));
    for kind in [RadialBlur, ZoomBlur] {
        // Centred inside one checker square (256..272, 128..144).
        let centre = Value::Vec2([264.0 / W as f64, 136.0 / H as f64]);
        let e = if kind == RadialBlur { fx(kind).with("angle", num(20.0)) } else { fx(kind).with("amount", num(0.3)) }.with("center", centre);
        out.push(case(&format!("{} keeps its centre and blurs the edge", makepad_stage_render_kit::catalog::info(kind).name), on("checker", 0.0, vec![e]), |img, inputs, _| {
            close4("centre", img.at(264, 136), inputs.i("checker").at(264, 136), 0.02)?;
            let corner = img.at(20, 20)[0];
            if !(0.15..=0.85).contains(&corner) {
                return Err(format!("corner {corner:.3} is not blurred toward grey"));
            }
            Ok(format!("corner {corner:.3}"))
        }));
    }

    // ── Light ───────────────────────────────────────────────────────────
    out.push(case("glow lights around a bright spot", on("spot", 0.0, vec![fx(Glow).with("threshold", num(0.5)).with("radius", num(8.0)).with("intensity", num(2.0))]), |img, _, _| {
        close("spot", img.at(256, 128)[0], 1.0, 0.01)?;
        let near = img.at(272, 128)[0];
        let far = img.at(400, 128)[0];
        if near < 0.05 || far > 0.01 {
            return Err(format!("near {near:.3} (want > 0.05), far {far:.3} (want < 0.01)"));
        }
        Ok(format!("near {near:.3}, far {far:.3}"))
    }));
    out.push(case("bloom spreads wider than its core", on("square", 0.0, vec![fx(Bloom).with("threshold", num(0.5)).with("radius", num(64.0)).with("intensity", num(2.0))]), |img, _, _| {
        let near = img.at(360, 128)[0];
        let mid = img.at(410, 128)[0];
        if !(near > mid && mid > 0.02) {
            return Err(format!("near {near:.3}, mid {mid:.3}"));
        }
        Ok(format!("near {near:.3}, mid {mid:.3}"))
    }));
    out.push(case("halation tints highlights red", on("spot", 0.0, vec![fx(Halation).with("radius", num(6.0)).with("amount", num(2.0))]), |img, _, _| {
        let p = img.at(270, 128);
        if !(p[0] > 0.05 && p[0] > p[1] * 2.0 && p[0] > p[2] * 2.0) {
            return Err(format!("halo {p:?} is not red"));
        }
        Ok(format!("halo {:.3} {:.3} {:.3}", p[0], p[1], p[2]))
    }));

    // ── Styles ──────────────────────────────────────────────────────────
    let hard_shadow = fx(DropShadow).with("radius", num(0.0)).with("distance", num(20.0)).with("opacity", num(1.0));
    out.push(case("drop_shadow falls away from the light", on("square", 0.0, vec![hard_shadow]), |img, _, _| {
        close4("in the shadow", img.at(357, 150), [0.0, 0.0, 0.0, 1.0], 0.02)?;
        close4("the layer", img.at(165, 70), [1.0, 1.0, 1.0, 1.0], 0.01)?;
        close4("toward the light", img.at(150, 50), [0.0; 4], 0.01)?;
        close4("below the layer", img.at(200, 200), [0.0, 0.0, 0.0, 1.0], 0.02)?;
        Ok("offset (10, 17.3)".into())
    }));
    out.push(case("drop_shadow softens with radius", on("square", 0.0, vec![fx(DropShadow).with("radius", num(8.0)).with("distance", num(20.0)).with("opacity", num(1.0))]), |img, _, _| {
        let a = img.at(357, 150)[3];
        if !(0.2..0.95).contains(&a) {
            return Err(format!("edge alpha {a:.3}"));
        }
        Ok(format!("edge alpha {a:.3}"))
    }));
    out.push(case("inner_shadow darkens the lit-side inner edge", on("square", 0.0, vec![fx(InnerShadow).with("radius", num(0.0)).with("distance", num(10.0)).with("opacity", num(1.0))]), |img, _, _| {
        close4("upper-left inner edge", img.at(162, 128), [0.0, 0.0, 0.0, 1.0], 0.02)?;
        close4("centre", img.at(300, 128), [1.0; 4], 0.01)?;
        close4("lower-right inner edge", img.at(349, 189), [1.0; 4], 0.01)?;
        close4("outside", img.at(100, 128), [0.0; 4], 0.01)?;
        Ok("shadow on the upper-left inside".into())
    }));
    for (position, inside_px, outside_px) in [("outside", (355, [1.0, 0.0, 0.0, 1.0]), (359, [0.0; 4])), ("inside", (349, [1.0, 0.0, 0.0, 1.0]), (344, [1.0; 4])), ("center", (353, [1.0, 0.0, 0.0, 1.0]), (357, [0.0; 4]))] {
        let e = fx(Stroke).with("width", num(6.0)).with("color", Value::Color([1.0, 0.0, 0.0, 1.0])).with("position", ch(position));
        out.push(case(&format!("stroke {position} is exactly 6 px"), on("square", 0.0, vec![e]), move |img, _, _| {
            close4(&format!("x {}", inside_px.0), img.at(inside_px.0, 128), inside_px.1, 0.02)?;
            close4(&format!("x {}", outside_px.0), img.at(outside_px.0, 128), outside_px.1, 0.02)?;
            close4("centre", img.at(256, 128), [1.0; 4], 0.01)?;
            Ok("edges where they belong".into())
        }));
    }
    out.push(case("bevel lights the upper left and shades the lower right", on("grey_square", 0.0, vec![fx(Bevel).with("size", num(12.0)).with("depth", num(2.0))]), |img, _, _| {
        let lit = img.at(163, 67)[0];
        let shade = img.at(348, 188)[0];
        let flat = img.at(256, 128)[0];
        close("flat centre", flat, 0.5, 0.02)?;
        if !(lit > 0.56 && shade < 0.44) {
            return Err(format!("lit {lit:.3}, shade {shade:.3}"));
        }
        Ok(format!("lit {lit:.3}, flat {flat:.3}, shade {shade:.3}"))
    }));
    out.push(case("fill clips to the layer", on("square", 0.0, vec![fx(Fill).with("color", Value::Color([1.0, 0.0, 0.0, 1.0]))]), |img, _, _| {
        close4("inside", img.at(256, 128), [1.0, 0.0, 0.0, 1.0], 0.01)?;
        close4("outside", img.at(100, 128), [0.0; 4], 0.01)?;
        Ok("red inside".into())
    }));

    // ── Generators ──────────────────────────────────────────────────────
    let gradient = |kind: &str, repeat: &str, start: [f64; 2], end: [f64; 2]| {
        fx(Gradient).with("kind", ch(kind)).with("repeat", ch(repeat)).with("start", Value::Vec2(start)).with("end", Value::Vec2(end))
    };
    out.push(case("gradient linear is exact to the pixel", on("white", 0.0, vec![gradient("linear", "pad", [0.0, 0.5], [1.0, 0.5])]), |img, _, _| {
        let mut worst: f32 = 0.0;
        for x in (0..W).step_by(7) {
            let u = (x as f32 + 0.5) / W as f32;
            worst = worst.max((img.at(x, 100)[0] - u).abs());
        }
        close("worst", worst, 0.0, 2.5 / 255.0)?;
        Ok(format!("max error {:.2}/255", worst * 255.0))
    }));
    for (kind, x, y, want) in [("radial", 384usize, 128usize, 0.5f32), ("conic", 256, 250, 0.25), ("diamond", 320, 192, 0.5)] {
        out.push(case(&format!("gradient {kind}"), on("white", 0.0, vec![gradient(kind, "pad", [0.5, 0.5], [1.0, 0.5])]), move |img, _, _| {
            close(&format!("at ({x}, {y})"), img.at(x, y)[0], want, 0.02)?;
            Ok("on the ramp".into())
        }));
    }
    for (repeat, want) in [("repeat", 0.5f32), ("mirror", 0.5), ("pad", 1.0)] {
        out.push(case(&format!("gradient {repeat}"), on("white", 0.0, vec![gradient("linear", repeat, [0.0, 0.5], [0.5, 0.5])]), move |img, _, _| {
            close("x = 384 (u 1.5)", img.at(384, 64)[0], want, 0.02)?;
            Ok("wraps as told".into())
        }));
    }
    for kind in ["fbm", "cellular", "value", "perlin", "ridged", "turbulent"] {
        let e = fx(Noise).with("kind", ch(kind)).with("scale", num(40.0));
        let mut c = case(&format!("noise {kind} is deterministic and spread"), on("white", 1.25, vec![e]), |img, _, _| {
            let values: Vec<f32> = img.px.iter().map(|p| p[0]).collect();
            let mean = values.iter().sum::<f32>() / values.len() as f32;
            let std = (values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / values.len() as f32).sqrt();
            if !(0.15..0.85).contains(&mean) || std < 0.03 {
                return Err(format!("mean {mean:.3}, std {std:.3}"));
            }
            Ok(format!("mean {mean:.3}, std {std:.3}"))
        });
        c.twice = true;
        out.push(c);
    }
    out.push(case("noise evolves with time", Box::new(|cx, kit, inputs, _| {
        let e = fx(Noise).with("evolution", num(1.0));
        let ctx = FxContext::new(0.0, 30.0, &inputs.tex);
        let a = kit.apply(cx, &[e.clone()], inputs.t("white"), (W as u32, H as u32), &ctx).texture;
        let ctx = FxContext::new(0.5, 30.0, &inputs.tex);
        let b = kit.apply(cx, &[e], inputs.t("white"), (W as u32, H as u32), &ctx).texture;
        kit.composite(cx, &a, &b, KitBlend::Difference, 1.0, false, (W as u32, H as u32))
    }), |img, _, _| {
        let mean = img.px.iter().map(|p| p[0]).sum::<f32>() / img.px.len() as f32;
        if mean < 0.02 {
            return Err(format!("t 0 and t 0.5 differ by only {mean:.4}"));
        }
        Ok(format!("mean difference {mean:.3}"))
    }));
    let seg_line = Value::Points(vec![[50.0, 128.0], [450.0, 128.0]]);
    out.push(case("stroke_path trims and caps", on("white", 0.0, vec![fx(StrokePath).with("points", seg_line.clone()).with("width", num(10.0)).with("trim_end", num(0.5)).with("layer", ch("only"))]), |img, _, _| {
        close("on the line", img.at(150, 128)[3], 1.0, 0.01)?;
        close("beside the line", img.at(150, 134)[3], 0.0, 0.01)?;
        close("past the trim", img.at(300, 128)[3], 0.0, 0.01)?;
        close("round cap", img.at(253, 128)[3], 1.0, 0.01)?;
        Ok("50 → 250 px drawn".into())
    }));
    out.push(case("stroke_path tapers", on("white", 0.0, vec![fx(StrokePath).with("points", seg_line).with("width", num(20.0)).with("taper_start", num(0.5)).with("layer", ch("only"))]), |img, _, _| {
        let height = |x: usize| (0..H).filter(|y| img.at(x, *y)[3] > 0.5).count();
        let (thin, mid, full) = (height(90), height(200), height(400));
        if !(thin < mid && mid < full && (19..=21).contains(&full)) {
            return Err(format!("heights {thin}, {mid}, {full}"));
        }
        Ok(format!("heights {thin} < {mid} < {full}"))
    }));
    out.push(case("stroke_path draws SVG curves", on("white", 0.0, vec![fx(StrokePath).with("svg", Value::Text("M 60 200 C 60 40, 450 40, 450 200".into())).with("width", num(6.0)).with("layer", ch("only"))]), |img, _, _| {
        close("curve top (x 255, y 80)", (0..H).filter(|y| img.at(255, *y)[3] > 0.5).map(|y| y as f32).fold(f32::MAX, f32::min), 78.0, 4.0)?;
        Ok("cubic flattened".into())
    }));
    let mut c = case("particles are deterministic", on("white", 2.3, vec![fx(Particles).with("rate", num(300.0)).with("turbulence", num(20.0)).with("layer", ch("only")).with("shape", ch("spark"))]), |img, _, kit| {
        let lit = img.px.iter().filter(|p| p[3] > 0.05).count();
        if lit < 200 {
            return Err(format!("{lit} lit pixels"));
        }
        Ok(format!("{} particles, {lit} lit pixels", kit.stats.particles))
    });
    c.twice = true;
    out.push(c);
    out.push(case("particles from a layer are born inside its alpha", on("white", 2.0, vec![fx(Particles)
        .with("emitter", ch("layer"))
        .with("from", Value::Layer("square".into()))
        .with("size", Value::Vec2([1.0, 1.0]))
        .with("rate", num(2000.0))
        .with("life", num(1.0))
        .with("speed", num(0.0))
        .with("gravity", Value::Vec2([0.0, 0.0]))
        .with("particle_size", num(4.0))
        .with("size_end", num(4.0))
        .with("layer", ch("only"))]), |img, _, _| {
        let mut lit = 0;
        for y in 0..H {
            for x in 0..W {
                if img.at(x, y)[3] > 0.05 {
                    lit += 1;
                    if !((157..355).contains(&x) && (61..195).contains(&y)) {
                        return Err(format!("lit pixel ({x}, {y}) outside the emitter"));
                    }
                }
            }
        }
        if lit < 300 {
            return Err(format!("{lit} lit pixels"));
        }
        Ok(format!("{lit} lit pixels, all inside"))
    }));

    // ── Distortion ──────────────────────────────────────────────────────
    out.push(case("displace by mid grey is the identity", on("checker", 0.0, vec![fx(Displace).with("map", Value::Layer("grey50".into())).with("amount", Value::Vec2([30.0, 30.0]))]), |img, inputs, _| {
        let d = max_diff(img, inputs.i("checker"));
        close("max difference", d, 0.0, 1.5 / 255.0)?;
        Ok("unchanged".into())
    }));
    out.push(case("displace moves by the map", on("ramp", 0.0, vec![fx(Displace).with("map", Value::Layer("red".into())).with("amount", Value::Vec2([8.0, 8.0]))]), |img, inputs, _| {
        // Red 1.0 moves by +amount in x; green 0 by -amount in y (a
        // uniform ramp in x does not show it).
        close("x 200", img.at(200, 128)[0], inputs.i("ramp").at(208, 128)[0], 0.01)?;
        Ok("shifted 8 px".into())
    }));
    out.push(case("turbulence 0 is the identity", on("checker", 0.0, vec![fx(Turbulence).with("amount", num(0.0))]), |img, inputs, _| {
        close("max difference", max_diff(img, inputs.i("checker")), 0.0, 1.5 / 255.0)?;
        Ok("unchanged".into())
    }));
    out.push(case("turbulence moves pixels", on("checker", 0.7, vec![fx(Turbulence).with("amount", num(12.0))]), |img, inputs, _| {
        let d = img.px.iter().zip(&inputs.i("checker").px).filter(|(a, b)| (a[0] - b[0]).abs() > 0.1).count();
        if d < 1000 {
            return Err(format!("only {d} pixels changed"));
        }
        Ok(format!("{d} pixels moved"))
    }));
    out.push(case("ripple amplitude 0 is the identity", on("checker", 1.0, vec![fx(Ripple).with("amplitude", num(0.0))]), |img, inputs, _| {
        close("max difference", max_diff(img, inputs.i("checker")), 0.0, 1.5 / 255.0)?;
        Ok("unchanged".into())
    }));

    // ── Compositing ─────────────────────────────────────────────────────
    for (i, mode) in makepad_stage_render_kit::catalog::BLEND_MODES.iter().enumerate() {
        let mode = *mode;
        out.push(case(&format!("blend {mode} matches the W3C formula"), Box::new(move |cx, kit, inputs, _| {
            kit.composite(cx, inputs.t("backdrop"), inputs.t("source"), KitBlend::ALL[i], 1.0, false, (W as u32, H as u32))
        }), move |img, inputs, _| {
            for x in [100, 400] {
                let want = composite_ref(mode, inputs.i("backdrop").at(x, 10), inputs.i("source").at(x, 10));
                close4(&format!("x {x}"), img.at(x, 10), want, 2.5 / 255.0)?;
            }
            Ok("opaque and translucent backdrops".into())
        }));
    }
    out.push(case("blend effect uses the named layer", on("red", 0.0, vec![fx(Blend).with("layer", Value::Layer("grey50".into())).with("mode", ch("multiply"))]), |img, _, _| {
        close4("red × grey", img.at(10, 10), [0.5, 0.0, 0.0, 1.0], 0.01)?;
        Ok("multiplied".into())
    }));
    out.push(case("mask rect with a gaussian edge", on("white", 0.0, vec![fx(Mask).with("feather", num(10.0))]), |img, _, _| {
        close("centre", img.at(256, 128)[3], 1.0, 0.01)?;
        close("far outside", img.at(10, 10)[3], 0.0, 0.01)?;
        close("edge", img.at(128, 128)[3], 0.5, 0.05)?;
        close("one sigma in", img.at(138, 128)[3], phi(1.0) as f32, 0.05)?;
        Ok("erf edge".into())
    }));
    out.push(case("mask ellipse, inverted", on("white", 0.0, vec![fx(Mask).with("shape", ch("ellipse")).with("invert", Value::Bool(true))]), |img, _, _| {
        close("inside", img.at(256, 128 + 60)[3], 0.0, 0.01)?;
        close("outside", img.at(256, 128 + 70)[3], 1.0, 0.01)?;
        close("corner", img.at(5, 5)[3], 1.0, 0.01)?;
        Ok("inverted".into())
    }));
    out.push(case("matte by alpha", on("checker", 0.0, vec![fx(Matte).with("layer", Value::Layer("half".into()))]), |img, inputs, _| {
        close4("left", img.at(100, 100), inputs.i("checker").at(100, 100), 0.01)?;
        close4("right", img.at(400, 100), [0.0; 4], 0.01)?;
        Ok("half shown".into())
    }));
    out.push(case("matte by luma, inverted", on("white", 0.0, vec![fx(Matte).with("layer", Value::Layer("ramp".into())).with("mode", ch("luma")).with("invert", Value::Bool(true))]), |img, inputs, _| {
        close("x 128", img.at(128, 10)[3], 1.0 - inputs.i("ramp").at(128, 10)[0], 0.01)?;
        Ok("1 - luma".into())
    }));

    // ── 2.5D ────────────────────────────────────────────────────────────
    out.push(case("transform_3d identity is exact", on("checker", 0.0, vec![fx(Transform3d)]), |img, inputs, _| {
        close("max difference", max_diff(img, inputs.i("checker")), 0.0, 1.5 / 255.0)?;
        Ok("unchanged".into())
    }));
    out.push(case("transform_3d roll 180", on("ramp", 0.0, vec![fx(Transform3d).with("rotation", Value::Vec3([0.0, 0.0, 180.0]))]), |img, inputs, _| {
        close("x 100", img.at(100, 50)[0], inputs.i("ramp").at(W - 1 - 100, 0)[0], 0.01)?;
        Ok("flipped".into())
    }));
    out.push(case("transform_3d twice as far is half size", on("ramp", 0.0, vec![fx(Transform3d).with("position", Value::Vec3([0.0, 0.0, (H as f64 * 0.5) / (39.6f64.to_radians() * 0.5).tan()]))]), |img, inputs, _| {
        close("x 200", img.at(200, 100)[0], inputs.i("ramp").at(144, 0)[0], 0.02)?;
        close("outside", img.at(20, 100)[3], 0.0, 0.01)?;
        Ok("scaled about the centre".into())
    }));
    out.push(case("depth of field blurs away from the focus", Box::new(|cx, kit, inputs, _| {
        let camera = Camera::orbit([W as f64, H as f64], 39.6, [256.0, 128.0, 0.0], [0.0, 0.0], 0.0, 0.0, [0.0; 3], 0.0, 30.0);
        let mut ctx = FxContext::new(0.0, 30.0, &inputs.tex);
        ctx.camera = Some(&camera);
        let e = fx(Transform3d).with("rotation", Value::Vec3([55.0, 0.0, 0.0]));
        kit.apply(cx, &[e], inputs.t("checker"), (W as u32, H as u32), &ctx).texture
    }), |img, _, _| {
        // Contrast across a checker edge near the focus line vs far away.
        let contrast = |y: usize| (0..W - 1).map(|x| (img.at(x + 1, y)[0] - img.at(x, y)[0]).abs()).fold(0.0, f32::max);
        let (mid, top) = (contrast(128), contrast(40));
        if !(mid > top * 1.5) {
            return Err(format!("edge contrast at the focus {mid:.3}, away {top:.3}"));
        }
        Ok(format!("sharp {mid:.3} vs blurred {top:.3}"))
    }));

    // ── Time ────────────────────────────────────────────────────────────
    out.push(case("echo trails behind", Box::new(move |cx, kit, inputs, boxes| {
        let stack = EffectStack::new(vec![Effect::new(Echo).with("count", 2.0).with("delay", 0.25).with("decay", 0.5)]);
        let ctx = FxContext::new(1.0, 30.0, &inputs.tex);
        let mut layer = FnLayer { draw: moving_square(boxes, 96.0, 200.0, 8.0), placement: None };
        kit.apply_layer(cx, &stack, (W as u32, H as u32), &ctx, &mut layer).texture
    }), |img, _, _| {
        close("now", img.at(300, 128)[3], 1.0, 0.01)?;
        close("echo 1", img.at(250, 128)[3], 0.5, 0.01)?;
        close("echo 2", img.at(200, 128)[3], 0.25, 0.01)?;
        close("gap", img.at(275, 128)[3], 0.0, 0.01)?;
        Ok("1, 0.5, 0.25".into())
    }));
    let blur_case = |name: &str, velocity: bool, adaptive: bool| {
        case(name, Box::new(move |cx, kit, inputs, boxes| {
            let samples = if adaptive { 0.0 } else { 32.0 };
            let stack = EffectStack::new(vec![Effect::new(MotionBlur).with("samples", samples)]);
            let ctx = FxContext::new(0.5, 30.0, &inputs.tex);
            let placement: Box<dyn Fn(f64) -> [f64; 6]> = Box::new(|t| [1.0, 0.0, 0.0, 1.0, 600.0 * t, 0.0]);
            let mut layer = FnLayer { draw: moving_square(boxes, 96.0, 600.0, 8.0), placement: Some(placement) };
            let saved = kit.budget;
            if velocity {
                kit.budget = makepad_stage_render_kit::Budget::preview(0.001);
            }
            let out = kit.apply_layer(cx, &stack, (W as u32, H as u32), &ctx, &mut layer).texture;
            kit.budget = saved;
            out
        }), move |img, _, kit| {
            let row: Vec<f32> = (0..W).map(|x| img.at(x, 128)[3]).collect();
            let sum: f32 = row.iter().sum();
            let peak = row.iter().copied().fold(0.0, f32::max);
            let lit = row.iter().filter(|a| **a > 0.02).count();
            close("energy (8 px)", sum, 8.0, 0.35)?;
            // n discrete sub-frames 10/n px apart: an 8 px square covers
            // about 8·n/10 of them.
            let n = kit.stats.sub_frames.max(1) as f32;
            let (lo, hi) = if velocity { (0.68, 0.92) } else { ((8.0 * n / 10.0).floor() / n - 0.02, (8.0 * n / 10.0).ceil() / n + 0.02) };
            if !(lo..=hi).contains(&peak) {
                return Err(format!("plateau {peak:.3} outside {lo:.3}..{hi:.3} (8 px of a 10 px smear, {n} sub-frames)"));
            }
            if !(16..=20).contains(&lit) {
                return Err(format!("{lit} px lit, want 18"));
            }
            if adaptive && kit.stats.sub_frames != 11 {
                return Err(format!("{} sub-frames, want 11 for a 10 px move", kit.stats.sub_frames));
            }
            Ok(format!("sum {sum:.2}, plateau {peak:.3}, {lit} px, {} sub-frames", kit.stats.sub_frames))
        })
    };
    out.push(blur_case("motion_blur sub-frames smear exactly", false, false));
    out.push(blur_case("motion_blur adapts samples to motion", false, true));
    out.push(blur_case("motion_blur velocity fallback matches", true, false));

    // ── Colour ──────────────────────────────────────────────────────────
    let colour = |name: &str, input: &'static str, e: ResolvedEffect, x: usize, want: [f32; 4], tol: f32| {
        case(name, on(input, 0.0, vec![e]), move |img, _, _| {
            close4(&format!("x {x}"), img.at(x, 100), want, tol)?;
            Ok(format!("{:?}", img.at(x, 100).map(|v| (v * 1000.0).round() / 1000.0)))
        })
    };
    let lv = || fx(Levels).with("in_black", num(0.2)).with("in_white", num(0.8));
    out.push(colour("levels mid stays", "ramp", lv(), 255, [0.5, 0.5, 0.5, 1.0], 0.012));
    out.push(colour("levels black point", "ramp", lv(), 90, [0.0, 0.0, 0.0, 1.0], 0.012));
    out.push(colour("levels white point", "ramp", lv(), 420, [1.0, 1.0, 1.0, 1.0], 0.012));
    out.push(colour("hue +120 turns red green", "red", fx(HueSaturation).with("hue", num(120.0)), 10, [0.0, 1.0, 0.0, 1.0], 0.02));
    out.push(colour("saturation -1 greys", "red", fx(HueSaturation).with("saturation", num(-1.0)), 10, [0.5, 0.5, 0.5, 1.0], 0.02));
    out.push(colour("colorize", "grey50", fx(HueSaturation).with("colorize", Value::Bool(true)).with("colorize_hue", num(240.0)).with("colorize_saturation", num(1.0)), 10, [0.0, 0.0, 1.0, 1.0], 0.02));
    out.push(case("white_balance warms and keeps luma", on("grey50", 0.0, vec![fx(WhiteBalance).with("temperature", num(50.0))]), |img, _, _| {
        let p = img.at(10, 10);
        let luma = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
        close("luma", luma, 0.5, 0.01)?;
        if !(p[0] > p[1] && p[1] > p[2]) {
            return Err(format!("{p:?} is not warm"));
        }
        Ok(format!("{p:?}"))
    }));
    out.push(colour("exposure +1 doubles", "grey25", fx(Exposure).with("exposure", num(1.0)), 10, [0.5, 0.5, 0.5, 1.0], 0.012));
    out.push(colour("brightness +0.1", "grey50", fx(BrightnessContrast).with("brightness", num(0.1)), 10, [0.6, 0.6, 0.6, 1.0], 0.012));
    out.push(colour("tint maps black", "ramp", fx(Tint).with("black", Value::Color([1.0, 0.0, 0.0, 1.0])).with("white", Value::Color([0.0, 0.0, 1.0, 1.0])), 0, [1.0, 0.0, 0.0, 1.0], 0.012));
    out.push(colour("tint maps white", "ramp", fx(Tint).with("black", Value::Color([1.0, 0.0, 0.0, 1.0])).with("white", Value::Color([0.0, 0.0, 1.0, 1.0])), W - 1, [0.0, 0.0, 1.0, 1.0], 0.012));
    out.push(colour("invert", "grey25", fx(Invert), 10, [0.75, 0.75, 0.75, 1.0], 0.012));
    out.push(colour("vignette keeps the centre", "white", fx(Vignette).with("amount", num(1.0)), 256, [1.0; 4], 0.012));
    out.push(case("vignette darkens the corners", on("white", 0.0, vec![fx(Vignette).with("amount", num(1.0))]), |img, _, _| {
        let c = img.at(0, 0)[0];
        if c > 0.3 {
            return Err(format!("corner {c:.3}"));
        }
        Ok(format!("corner {c:.3}"))
    }));
    let mut c = case("grain keeps the mean", on("grey50", 0.4, vec![fx(Grain).with("amount", num(0.1))]), |img, _, _| {
        let n = img.px.len() as f32;
        let mean = img.px.iter().map(|p| p[0]).sum::<f32>() / n;
        let std = (img.px.iter().map(|p| (p[0] - mean).powi(2)).sum::<f32>() / n).sqrt();
        close("mean", mean, 0.5, 0.01)?;
        if std < 0.02 {
            return Err(format!("std {std:.4}"));
        }
        Ok(format!("mean {mean:.4}, std {std:.4}"))
    });
    c.twice = true;
    out.push(c);
    out.push(case("chromatic_aberration splits channels", on("line", 0.0, vec![fx(ChromaticAberration).with("mode", ch("linear")).with("amount", num(4.0))]), |img, _, _| {
        close("red at 252", img.at(252, 100)[0], 1.0, 0.02)?;
        close("green at 256", img.at(256, 100)[1], 1.0, 0.02)?;
        close("blue at 260", img.at(260, 100)[2], 1.0, 0.02)?;
        close("no red at 256", img.at(256, 100)[0], 0.0, 0.02)?;
        Ok("r −4, b +4".into())
    }));
    out.push(case("curves inverted master", on("ramp", 0.0, vec![fx(Curves).with("master", Value::Points(vec![[0.0, 1.0], [1.0, 0.0]]))]), |img, inputs, _| {
        for x in [0, 100, 300, W - 1] {
            close(&format!("x {x}"), img.at(x, 10)[0], 1.0 - inputs.i("ramp").at(x, 10)[0], 1.5 / 255.0)?;
        }
        Ok("1 - x".into())
    }));
    let lut_path = write_swap_lut();
    let lut_file = lut_path.display().to_string();
    out.push(case("lut from a .cube file", on("colors", 0.0, vec![fx(Lut).with("file", Value::Text(lut_file))]), |img, inputs, _| {
        for x in [50, 180, 300, 450] {
            let p = inputs.i("colors").at(x, 10);
            close4(&format!("x {x}"), img.at(x, 10), [p[2], p[1], p[0], 1.0], 2.0 / 255.0)?;
        }
        Ok("red and blue swapped".into())
    }));
    out.push(case("mix 0 is the input", on("spot", 0.0, vec![fx(Glow).with("mix", num(0.0))]), |img, inputs, _| {
        close("max difference", max_diff(img, inputs.i("spot")), 0.0, 1.0 / 255.0)?;
        Ok("unchanged".into())
    }));
    out
}

// ---------------------------------------------------------------------------
// The bench: GPU ms per effect at 1080p and 4K.
// ---------------------------------------------------------------------------

fn bench_stacks() -> Vec<(String, Vec<ResolvedEffect>)> {
    use EffectKind::*;
    let mut out: Vec<(String, Vec<ResolvedEffect>)> = Vec::new();
    for effect in makepad_stage_render_kit::EFFECTS {
        if effect.temporal {
            continue;
        }
        let mut e = fx(effect.kind);
        match effect.kind {
            Matte => e = e.with("layer", Value::Layer("input".into())),
            Blend => e = e.with("layer", Value::Layer("input".into())).with("mode", ch("soft_light")),
            Displace => e = e.with("map", Value::Layer("input".into())),
            Lut => e = e.with("file", Value::Text(std::env::temp_dir().join("render_kit_swap.cube").display().to_string())),
            Particles => e = e.with("rate", num(2000.0)),
            StrokePath => e = e.with("svg", Value::Text("M 100 800 C 400 100, 1400 100, 1800 800".into())).with("width", num(24.0)),
            _ => {}
        }
        out.push((effect.name.to_string(), vec![e]));
    }
    for radius in [32.0, 128.0] {
        out.push((format!("gaussian_blur radius {radius}"), vec![fx(GaussianBlur).with("radius", num(radius))]));
    }
    out.push(("stroke width 24".into(), vec![fx(Stroke).with("width", num(24.0))]));
    out
}

// ---------------------------------------------------------------------------
// The host widget
// ---------------------------------------------------------------------------

enum Command {
    Test { filter: Option<String>, out: PathBuf },
    Bench { frames: usize, sizes: Vec<(u32, u32)>, out: Option<PathBuf>, filter: Option<String> },
    Demo { out: PathBuf, frames: usize, size: (u32, u32) },
    /// `effect_strip`: one effect over a test input at several times.
    Strip { stack: EffectStack, input: String, times: Vec<f64>, out: PathBuf },
}

enum Stage {
    Idle,
    Recorded { texture: Texture, since: f64 },
    Reading { ticket: ReadbackTicket, since: f64 },
}

struct TestRun {
    cases: VecDeque<Case>,
    current: Option<Case>,
    first: Option<Image>,
    stage: Stage,
    passed: usize,
    failed: Vec<String>,
    out: PathBuf,
}

struct BenchRun {
    items: Vec<(String, Vec<ResolvedEffect>, (u32, u32))>,
    at: usize,
    frame: usize,
    frames: usize,
    input: Option<(Texture, (u32, u32))>,
    /// Per frame of the current item: GPU ms, filled as samples arrive.
    samples: HashMap<u64, f64>,
    rows: Vec<(String, (u32, u32), f64, f64, usize)>,
    out: Option<PathBuf>,
    waiting_since: f64,
}

struct DemoRun {
    out: PathBuf,
    /// Where the contact strip goes.
    strip_path: PathBuf,
    /// `effect_strip`: one stack over a test input instead of the showcase.
    effect: Option<(EffectStack, String)>,
    times: Vec<f64>,
    frames: usize,
    size: (u32, u32),
    at: usize,
    stage: Stage,
    shots: Vec<Image>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct KitHost {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawQuad,
    #[live]
    text: DrawText,
    #[live]
    disc: DrawDisc,
    #[live]
    boxes: DrawBox,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    readback_wait_logged: bool,
    #[rust]
    kit: Option<RenderKit>,
    #[rust]
    inputs: Option<Inputs>,
    #[rust]
    test: Option<TestRun>,
    #[rust]
    bench: Option<BenchRun>,
    #[rust]
    demo: Option<DemoRun>,
}

const CASE_TIMEOUT: f64 = 20.0;

fn finish(result: Result<String, String>) -> ! {
    match result {
        Ok(report) => {
            println!("{report}");
            std::process::exit(0)
        }
        Err(error) => {
            eprintln!("render-kit: FAILED: {error}");
            std::process::exit(1)
        }
    }
}

impl KitHost {
    fn start(&mut self, cx: &mut Cx, command: Command) {
        let mut kit = RenderKit::new(cx);
        let cpu: HashMap<&'static str, Image> = inputs_cpu().into_iter().collect();
        let tex = cpu.iter().map(|(name, image)| (name.to_string(), image.texture(cx))).collect();
        match command {
            Command::Test { filter, out } => {
                let _ = std::fs::create_dir_all(&out);
                let cases: VecDeque<Case> = cases().into_iter().filter(|c| filter.as_ref().is_none_or(|f| c.name.contains(f.as_str()))).collect();
                println!("render-kit: {} pixel tests", cases.len());
                self.test = Some(TestRun { cases, current: None, first: None, stage: Stage::Idle, passed: 0, failed: Vec::new(), out });
            }
            Command::Bench { frames, sizes, out, filter } => {
                kit.set_gpu_timing(true);
                let mut items = Vec::new();
                for size in sizes {
                    for (name, stack) in bench_stacks().into_iter().filter(|(n, _)| filter.as_ref().is_none_or(|f| n.contains(f.as_str()))) {
                        items.push((name, stack, size));
                    }
                }
                self.bench = Some(BenchRun { items, at: 0, frame: 0, frames, input: None, samples: HashMap::new(), rows: Vec::new(), out, waiting_since: 0.0 });
            }
            Command::Demo { out, frames, size } => {
                let _ = std::fs::create_dir_all(&out);
                let strip_path = out.join("strip.png");
                self.demo = Some(DemoRun { out, strip_path, effect: None, times: Vec::new(), frames, size, at: 0, stage: Stage::Idle, shots: Vec::new() });
            }
            Command::Strip { stack, input, times, out } => {
                let frames_dir = std::env::temp_dir().join("render_kit_strip_frames");
                let _ = std::fs::create_dir_all(&frames_dir);
                let frames = times.len();
                self.demo = Some(DemoRun { out: frames_dir, strip_path: out, effect: Some((stack, input)), times, frames, size: (W as u32, H as u32), at: 0, stage: Stage::Idle, shots: Vec::new() });
            }
        }
        self.inputs = Some(Inputs { cpu, tex });
        self.kit = Some(kit);
        self.next_frame = cx.new_next_frame();
    }

    fn draw_test(&mut self, cx: &mut Cx2d) {
        let (Some(run), Some(kit), Some(inputs)) = (&mut self.test, &mut self.kit, &self.inputs) else { return };
        match &run.stage {
            Stage::Idle => {
                if run.current.is_none() {
                    run.current = run.cases.pop_front();
                    run.first = None;
                }
                let Some(case) = &mut run.current else {
                    let report = format!("render-kit: {} passed, {} failed", run.passed, run.failed.len());
                    if run.failed.is_empty() {
                        finish(Ok(report));
                    }
                    finish(Err(format!("{report}\n  {}", run.failed.join("\n  "))));
                };
                kit.begin_frame(cx);
                let texture = (case.record)(cx, kit, inputs, &mut self.boxes);
                // A result that is an input itself (an identity) is not a
                // render target; copy it into one to read it back.
                let texture = kit.copy(cx, &texture, (W as u32, H as u32));
                run.stage = Stage::Recorded { texture, since: Cx::monotonic_now() };
            }
            Stage::Recorded { texture, since } => {
                if !self.readback_wait_logged && Cx::monotonic_now() - since > CASE_TIMEOUT {
                    let states: Vec<String> = kit.passes().iter().map(|&id| format!("{:?}:{}{}", id, if cx.passes[id].paint_dirty { "dirty" } else { "clean" }, cx.passes[id].painted_serial)).collect();
                    finish(Err(format!("{}: never painted; passes {}", run.current.as_ref().unwrap().name, states.join(" "))));
                }
                match kit.painted(cx) {
                    Painted::Whole => match texture.read_back(cx, ReadbackRequest::default()) {
                        Ok(ticket) => {
                            self.readback_wait_logged = false;
                            run.stage = Stage::Reading { ticket, since: Cx::monotonic_now() };
                        }
                        Err(ReadbackError::Backpressure) => {
                            if !self.readback_wait_logged {
                                log!("render-kit: context readback request limit reached; rendered frame waits");
                                self.readback_wait_logged = true;
                            }
                        }
                        Err(error) => finish(Err(format!("readback: {error}"))),
                    },
                    Painted::Partial => {
                        self.readback_wait_logged = false;
                        println!("  (a draw was dropped for a shader not yet compiling; recording again)");
                        run.stage = Stage::Idle;
                    }
                    Painted::Painting => kit.reattach(cx),
                }
            }
            Stage::Reading { since, .. } => {
                if Cx::monotonic_now() - since > CASE_TIMEOUT {
                    finish(Err(format!("{}: no readback", run.current.as_ref().unwrap().name)));
                }
            }
        }
    }

    fn readback_test(&mut self, cx: &mut Cx) {
        let (Some(run), Some(kit), Some(inputs)) = (&mut self.test, &self.kit, &self.inputs) else { return };
        let Stage::Reading { ticket, .. } = run.stage else { return };
        for result in cx.try_take_texture_readbacks_for(&[ticket]) {
            let data = result.data.as_ref().unwrap_or_else(|e| finish(Err(format!("readback: {e}"))));
            let image = Image::from_readback(&result, data);
            let case = run.current.as_ref().unwrap();
            if case.twice && run.first.is_none() {
                run.first = Some(image);
                run.stage = Stage::Idle;
                return;
            }
            let mut verdict = (case.check)(&image, inputs, kit);
            if let (true, Some(first)) = (case.twice, &run.first) {
                if first != &image {
                    verdict = Err(format!("two renders differ (hash {:016x} vs {:016x}, max {:.4})", first.hash(), image.hash(), max_diff(first, &image)));
                } else if let Ok(ok) = &verdict {
                    verdict = Ok(format!("{ok}; identical twice ({:016x})", image.hash()));
                }
            }
            match verdict {
                Ok(note) => {
                    run.passed += 1;
                    println!("  ok   {} — {note}", case.name);
                }
                Err(error) => {
                    let file = run.out.join(format!("{}.png", case.name.replace(|c: char| !c.is_ascii_alphanumeric(), "_")));
                    let _ = write_png(&file, image.w, image.h, &image.rgba8());
                    println!("  FAIL {} — {error} ({})", case.name, file.display());
                    run.failed.push(format!("{}: {error}", case.name));
                }
            }
            run.current = None;
            run.stage = Stage::Idle;
        }
    }

    fn draw_bench(&mut self, cx: &mut Cx2d) {
        let (Some(run), Some(kit), Some(inputs)) = (&mut self.bench, &mut self.kit, &self.inputs) else { return };
        // Drain timing samples of every recorded frame.
        for (tag, ms) in kit.take_gpu_ms(cx) {
            *run.samples.entry(tag).or_insert(0.0) += ms;
        }
        if run.at >= run.items.len() {
            let mut table = String::from("| effect | size | GPU ms (min) | GPU ms (median) | passes |\n|---|---|---|---|---|\n");
            for (name, size, min, ms, passes) in &run.rows {
                table.push_str(&format!("| {name} | {}x{} | {min:.3} | {ms:.3} | {passes} |\n", size.0, size.1));
            }
            if let Some(out) = &run.out {
                let _ = std::fs::write(out, &table);
            }
            finish(Ok(table));
        }
        let (name, stack, size) = run.items[run.at].clone();
        if run.frame < run.frames {
            if run.input.as_ref().is_none_or(|(_, s)| *s != size) {
                // A busy, full-frame input: the checker scaled up with colour.
                let image = Image::new(size.0 as usize, size.1 as usize, |x, y| {
                    let c = ((x / 48) + (y / 48)) % 2 == 0;
                    let v = if c { 0.9 } else { 0.1 };
                    [v, (x as f32 / size.0 as f32), (y as f32 / size.1 as f32), 1.0]
                });
                run.input = Some((image.texture(cx), size));
            }
            let input = run.input.as_ref().unwrap().0.clone();
            kit.begin_frame(cx);
            let mut layers = inputs.tex.clone();
            layers.insert("input".into(), input.clone());
            let ctx = FxContext::new(run.frame as f64 / 30.0, 30.0, &layers);
            let _ = kit.apply(cx, &stack, &input, size, &ctx);
            if run.frame == 0 {
                run.samples.clear();
            }
            run.frame += 1;
            run.waiting_since = Cx::monotonic_now();
            return;
        }
        // All frames recorded: wait for their samples (or give up).
        let tags: Vec<f64> = run.samples.values().copied().collect();
        if tags.len() + 1 >= run.frames || Cx::monotonic_now() - run.waiting_since > 3.0 {
            let mut ms = tags;
            ms.sort_by(f64::total_cmp);
            // The first frames compile pipelines; the median ignores them.
            let median = ms.get(ms.len() / 2).copied().unwrap_or(f64::NAN);
            let min = ms.first().copied().unwrap_or(f64::NAN);
            println!("  {name} {}x{}: min {min:.3} median {median:.3} ms ({} samples, {} passes)", size.0, size.1, ms.len(), kit.stats.passes);
            run.rows.push((name, size, min, median, kit.stats.passes));
            run.at += 1;
            run.frame = 0;
            run.samples.clear();
        } else {
            kit.reattach(cx);
        }
    }

    fn draw_demo(&mut self, cx: &mut Cx2d) {
        let Some(run) = &mut self.demo else { return };
        let (Some(kit), Some(inputs)) = (&mut self.kit, &self.inputs) else { return };
        match &run.stage {
            Stage::Idle => {
                if run.at >= run.frames {
                    let result = write_strip(run);
                    finish(result);
                }
                let t = run.times.get(run.at).copied().unwrap_or_else(|| 6.0 * run.at as f64 / (run.frames.max(2) - 1) as f64);
                kit.begin_frame(cx);
                let texture = match &run.effect {
                    Some((stack, input)) => {
                        let ctx = FxContext::new(t, 30.0, &inputs.tex);
                        let out = if input == "moving" {
                            let (disc, text) = (&mut self.disc, &mut self.text);
                            let mut layer = FnLayer {
                                draw: |cx: &mut Cx2d, s: f64, _size: (u32, u32)| draw_moving(cx, s, disc, text),
                                placement: Some(Box::new(|s| {
                                    let [x, y] = moving_centre(s);
                                    [1.0, 0.0, 0.0, 1.0, x, y]
                                })),
                            };
                            kit.apply_layer(cx, stack, run.size, &ctx, &mut layer).texture
                        } else {
                            let mut layer = makepad_stage_render_kit::TextureLayer(inputs.t(input).clone());
                            kit.apply_layer(cx, stack, run.size, &ctx, &mut layer).texture
                        };
                        kit.copy(cx, &out, run.size)
                    }
                    None => demo_frame(cx, kit, inputs, run.size, t, &mut self.text, &mut self.disc),
                };
                run.stage = Stage::Recorded { texture, since: Cx::monotonic_now() };
            }
            Stage::Recorded { texture, since } => {
                if !self.readback_wait_logged && Cx::monotonic_now() - since > CASE_TIMEOUT {
                    let states: Vec<String> = kit.passes().iter().map(|&id| format!("{:?}:{}:{}{}", id, cx.passes[id].debug_name, if cx.passes[id].paint_dirty { "dirty" } else { "clean" }, cx.passes[id].painted_serial)).collect();
                    finish(Err(format!("demo frame {} never painted; pending {} skips {} reasons {:?} repaint {}; passes {}", run.at, cx.pipelines_pending(), cx.pipeline_skips(), cx.draw_skip_reasons(), cx.repaint_id(), states.into_iter().take(12).collect::<Vec<_>>().join(" "))));
                }
                match kit.painted(cx) {
                    Painted::Whole => match texture.read_back(cx, ReadbackRequest::default()) {
                        Ok(ticket) => {
                            self.readback_wait_logged = false;
                            run.stage = Stage::Reading { ticket, since: Cx::monotonic_now() };
                        }
                        Err(ReadbackError::Backpressure) => {
                            if !self.readback_wait_logged {
                                log!("render-kit: context readback request limit reached; rendered frame waits");
                                self.readback_wait_logged = true;
                            }
                        }
                        Err(error) => finish(Err(format!("readback: {error}"))),
                    },
                    Painted::Partial => {
                        self.readback_wait_logged = false;
                        println!("  (a draw was dropped for a shader not yet compiling; recording again)");
                        run.stage = Stage::Idle;
                    }
                    Painted::Painting => kit.reattach(cx),
                }
            }
            Stage::Reading { .. } => {}
        }
    }

    fn readback_demo(&mut self, cx: &mut Cx) {
        let Some(run) = &mut self.demo else { return };
        let Stage::Reading { ticket, .. } = run.stage else { return };
        for result in cx.try_take_texture_readbacks_for(&[ticket]) {
            let data = result.data.as_ref().unwrap_or_else(|e| finish(Err(format!("readback: {e}"))));
            let image = Image::from_readback(&result, data);
            let path = run.out.join(format!("frame_{:02}.png", run.at));
            if let Err(e) = write_png(&path, image.w, image.h, &image.rgba8()) {
                finish(Err(e));
            }
            println!("  demo frame {} -> {}", run.at, path.display());
            run.shots.push(image);
            run.at += 1;
            run.stage = Stage::Idle;
        }
    }
}

fn write_strip(run: &DemoRun) -> Result<String, String> {
    let cols = 4.min(run.shots.len()).max(1);
    let rows = run.shots.len().div_ceil(cols);
    let (w, h) = (run.size.0 as usize, run.size.1 as usize);
    let (sw, sh) = (cols * w, rows * h);
    let mut strip = vec![0u8; sw * sh * 4];
    for (i, shot) in run.shots.iter().enumerate() {
        let (ox, oy) = ((i % cols) * w, (i / cols) * h);
        let rgba = shot.rgba8();
        for y in 0..h {
            let dst = ((oy + y) * sw + ox) * 4;
            strip[dst..dst + w * 4].copy_from_slice(&rgba[y * w * 4..(y + 1) * w * 4]);
        }
    }
    let path = run.strip_path.clone();
    write_png(&path, sw, sh, &strip)?;
    Ok(format!("render-kit: demo strip {}x{} ({} frames) -> {}", sw, sh, run.shots.len(), path.display()))
}

/// The showcase scene at `t`: every family of effects in one comp.
fn demo_frame(cx: &mut Cx2d, kit: &mut RenderKit, inputs: &Inputs, size: (u32, u32), t: f64, text: &mut DrawText, disc: &mut DrawDisc) -> Texture {
    use EffectKind::*;
    let (w, h) = (size.0 as f64, size.1 as f64);
    let s = w / 960.0;
    let layers = &inputs.tex;
    let clear = inputs.t("white");
    // Background: a conic multi-stop gradient, fbm noise in soft light,
    // a slow ripple, then the grade (curves, hue, white balance).
    let background = vec![
        fx(Gradient)
            .with("kind", ch("conic"))
            .with("start", Value::Vec2([0.5, 0.55]))
            .with("end", Value::Vec2([0.5 + (t * 0.3).cos(), 0.55 + (t * 0.3).sin()]))
            .with("stops", Value::Stops(vec![(0.0, [0.05, 0.07, 0.18, 1.0]), (0.35, [0.25, 0.06, 0.35, 1.0]), (0.7, [0.02, 0.25, 0.35, 1.0]), (1.0, [0.05, 0.07, 0.18, 1.0])])),
        fx(Noise).with("scale", num(180.0 * s)).with("evolution", num(0.4)).with("blend", ch("soft_light")).with("opacity", num(0.8)),
        fx(Ripple).with("wavelength", num(90.0 * s)).with("amplitude", num(6.0 * s)).with("speed", num(60.0 * s)),
        fx(Curves).with("master", Value::Points(vec![[0.0, 0.03], [0.5, 0.45], [1.0, 1.0]])),
        fx(Vignette).with("amount", num(0.6)),
    ];
    let ctx = FxContext::new(t, 30.0, layers);
    let bg = kit.apply(cx, &background, clear, size, &ctx).texture;

    // The title: drawn by the host, flipped in on a 3D plane through an
    // orbiting camera with depth of field, with stroke, bevel, drop shadow
    // and glow, motion-blurred as it moves, revealed by a feathered mask.
    let enter = (t / 1.5).clamp(0.0, 1.0);
    let ease = 1.0 - (1.0 - enter).powi(3);
    let camera = Camera::orbit([w, h], 39.6, [w * 0.5, h * 0.5, 0.0], [12.0 * (t * 0.5).sin(), 6.0], 0.0, 0.0, [0.0; 3], 0.0, 18.0 * s);
    let mut ctx3 = FxContext::new(t, 30.0, layers);
    ctx3.camera = Some(&camera);
    let title_stack = EffectStack::new(vec![
        Effect::new(Stroke).with("width", 3.0 * s).with("color", Value::Color([1.0, 0.55, 0.2, 1.0])),
        Effect::new(Bevel).with("size", 6.0 * s).with("depth", 2.0),
        Effect::new(Transform3d)
            .with("rotation", Anim::keys(vec![
                makepad_stage_render_kit::Key { t: 0.0, v: Value::Vec3([70.0, -30.0, 0.0]), ease: makepad_stage_render_kit::Ease::Linear },
                makepad_stage_render_kit::Key { t: 1.5, v: Value::Vec3([0.0, 0.0, 0.0]), ease: makepad_stage_render_kit::Ease::OutBack },
                makepad_stage_render_kit::Key { t: 6.0, v: Value::Vec3([0.0, 25.0, 0.0]), ease: makepad_stage_render_kit::Ease::InOutCubic },
            ]).unwrap())
            .with("position", Value::Vec3([0.0, 0.0, 200.0 * s * (1.0 - ease)])),
        Effect::new(MotionBlur).with("samples", 8.0),
        Effect::new(DropShadow).with("distance", 10.0 * s).with("radius", 8.0 * s),
        Effect::new(Glow).with("threshold", 0.8).with("radius", 10.0 * s).with("intensity", 0.35),
        Effect::new(Mask).with("size", Value::Vec2([2.0 * enter.max(0.05), 1.0])).with("feather", 40.0 * s),
    ]);
    let font = 96.0 * s;
    let mut title_layer = FnLayer {
        draw: |cx: &mut Cx2d, _t: f64, (fw, fh): (u32, u32)| {
            text.text_style.font_size = font as f32;
            text.color = vec4(1.0, 1.0, 1.0, 1.0);
            let walk = Walk::abs_rect(Rect { pos: dvec2(0.0, fh as f64 * 0.5 - font), size: dvec2(fw as f64, font * 2.0) });
            text.draw_walk(cx, walk, Align { x: 0.5, y: 0.5 }, "RENDER KIT");
        },
        placement: None,
    };
    let title = kit.apply_layer(cx, &title_stack, size, &ctx3, &mut title_layer).texture;

    // A comet: a disc on a curve, with echo trails and motion blur.
    let comet_stack = EffectStack::new(vec![Effect::new(MotionBlur).with("samples", 12.0), Effect::new(Echo).with("count", 8.0).with("delay", 0.06).with("decay", 0.7).with("mode", Value::Choice("add".into()))]);
    let mut comet = FnLayer {
        draw: |cx: &mut Cx2d, t: f64, (fw, fh): (u32, u32)| {
            let (fw, fh) = (fw as f64, fh as f64);
            let x = fw * (0.1 + 0.8 * ((t * 0.9).sin() * 0.5 + 0.5));
            let y = fh * (0.78 + 0.1 * (t * 1.7).cos());
            let r = 12.0 * s;
            disc.center = vec2(x as f32, y as f32);
            disc.radius = r as f32;
            disc.fill = vec4(0.4, 0.9, 1.0, 1.0);
            disc.draw_abs(cx, Rect { pos: dvec2(x - r - 1.0, y - r - 1.0), size: dvec2(2.0 * r + 2.0, 2.0 * r + 2.0) });
        },
        placement: None,
    };
    let comet_tex = kit.apply_layer(cx, &comet_stack, size, &ctx, &mut comet).texture;

    // Particles born inside the title, a tapered path drawing on, bloom.
    let mut named = layers.clone();
    named.insert("title".into(), title.clone());
    let ctxn = FxContext::new(t, 30.0, &named);
    let top = vec![
        fx(Particles)
            .with("emitter", ch("layer"))
            .with("from", Value::Layer("title".into()))
            .with("size", Value::Vec2([0.8, 0.3]))
            .with("rate", num(1500.0))
            .with("start", num(1.0))
            .with("life", num(1.4))
            .with("speed", num(40.0 * s))
            .with("spread", num(360.0))
            .with("gravity", Value::Vec2([0.0, -40.0 * s]))
            .with("particle_size", num(4.0 * s))
            .with("size_end", num(0.5))
            .with("turbulence", num(12.0 * s)),
        fx(StrokePath)
            .with("svg", Value::Text(format!("M {} {} C {} {}, {} {}, {} {}", 80.0 * s, 470.0 * s, 300.0 * s, 330.0 * s, 660.0 * s, 610.0 * s, 880.0 * s, 470.0 * s)))
            .with("width", num(10.0 * s))
            .with("taper_start", num(0.3))
            .with("taper_end", num(0.3))
            .with("trim_end", num(((t - 0.5) / 2.5).clamp(0.0, 1.0)))
            .with("color", Value::Color([1.0, 0.8, 0.3, 1.0])),
    ];
    let title_fx = kit.apply(cx, &top, &title, size, &ctxn).texture;
    let comp = kit.composite(cx, &bg, &comet_tex, KitBlend::Screen, 1.0, false, size);
    let comp = kit.composite(cx, &comp, &title_fx, KitBlend::Normal, 1.0, false, size);
    // Finishing: bloom, halation, chromatic aberration, grain, a turbulent
    // shimmer in the last seconds.
    let finish = vec![
        fx(Bloom).with("threshold", num(0.8)).with("radius", num(40.0 * s)).with("intensity", num(0.35)),
        fx(Halation).with("radius", num(6.0 * s)).with("amount", num(0.4)),
        fx(Turbulence).with("amount", num(6.0 * s * ((t - 4.5) / 1.5).clamp(0.0, 1.0))).with("size", num(80.0 * s)),
        fx(ChromaticAberration).with("amount", num(3.0 * s)),
        fx(HueSaturation).with("saturation", num(0.15)),
        fx(Grain).with("amount", num(0.05)),
    ];
    kit.apply(cx, &finish, &comp, size, &ctx).texture
}

impl Widget for KitHost {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        self.readback_test(cx);
        self.readback_demo(cx);
        if self.next_frame.is_event(event).is_some() {
            self.draw_bg.redraw(cx);
            self.next_frame = cx.new_next_frame();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.draw_bg.draw_walk(cx, walk);
        self.draw_test(cx);
        self.draw_bench(cx);
        self.draw_demo(cx);
        DrawStep::done()
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
}

fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (w, h) = text.split_once('x').ok_or_else(|| format!("`{text}` is not WxH"))?;
    Ok((w.parse().map_err(|_| format!("`{text}`"))?, h.parse().map_err(|_| format!("`{text}`"))?))
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    let args: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--remote") && *a != "--stdin-loop").collect();
    let value = |name: &str| args.iter().position(|a| *a == name).and_then(|i| args.get(i + 1)).map(|v| v.to_string());
    let usage = "usage: makepad-render-kit test [--filter <name>] [--out <dir>] | bench [--frames n] [--sizes WxH,...] [--out table.md] | demo --out <dir> [--frames n] [--size WxH] | strip --effect <name> [--set p=v]... [--input checker] [--times 0,0.5,1] --out <strip.png> | catalogue | doc";
    match args.first().map(|a| a.as_str()) {
        Some("test") => Ok(Command::Test { filter: value("--filter"), out: value("--out").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("render_kit_test")) }),
        Some("bench") => Ok(Command::Bench {
            frames: value("--frames").and_then(|v| v.parse().ok()).unwrap_or(12),
            sizes: value("--sizes").unwrap_or_else(|| "1920x1080,3840x2160".into()).split(',').map(parse_size).collect::<Result<_, _>>()?,
            out: value("--out").map(PathBuf::from),
            filter: value("--filter"),
        }),
        Some("demo") => Ok(Command::Demo {
            out: PathBuf::from(value("--out").ok_or(usage)?),
            frames: value("--frames").and_then(|v| v.parse().ok()).unwrap_or(8),
            size: parse_size(&value("--size").unwrap_or_else(|| "960x540".into()))?,
        }),
        Some("strip") => {
            let name = value("--effect").ok_or(usage)?;
            let preset = makepad_stage_render_kit::strip_preset(&name);
            let times: Vec<f64> = match value("--times") {
                Some(t) => t.split(',').map(|t| t.trim().parse::<f64>().map_err(|_| format!("--times: `{t}`"))).collect::<Result<_, _>>()?,
                None => preset.times.to_vec(),
            };
            let given: Vec<(String, String)> = args
                .windows(2)
                .filter(|w| *w[0] == "--set")
                .map(|w| w[1].split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).ok_or_else(|| format!("--set {}: expected name=value", w[1])))
                .collect::<Result<_, String>>()?;
            // No --set: the catalogue's preview settings.
            let sets: Vec<(String, String)> = if given.is_empty() { preset.set.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() } else { given };
            let (t0, t1) = (times.first().copied().unwrap_or(0.0), times.last().copied().unwrap_or(0.0));
            let mut params: Vec<(String, Anim)> = Vec::new();
            for (k, v) in sets {
                let parse = makepad_stage_render_kit::catalogue::parse_value;
                // `from..to` ramps linearly across the strip's times.
                let anim = match v.split_once("..") {
                    Some((a, b)) if t1 > t0 => Anim::keys(vec![
                        makepad_stage_render_kit::Key { t: t0, v: parse(a), ease: makepad_stage_render_kit::Ease::Linear },
                        makepad_stage_render_kit::Key { t: t1, v: parse(b), ease: makepad_stage_render_kit::Ease::Linear },
                    ])
                    .map_err(|e| format!("--set {k}: {e}"))?,
                    Some((a, _)) => Anim::Const(parse(a)),
                    None => Anim::Const(parse(&v)),
                };
                params.push((k, anim));
            }
            if name.trim_start_matches('@') == "lut" && !params.iter().any(|(k, _)| k == "file") {
                params.push(("file".into(), Anim::Const(Value::Text(write_swap_lut().display().to_string()))));
            }
            let (effect, warnings) = Effect::build(&name, params).map_err(|d| d.errors.join("\n"))?;
            for w in warnings {
                eprintln!("render-kit: {w}");
            }
            let input = value("--input").unwrap_or_else(|| preset.input.to_string());
            if !makepad_stage_render_kit::catalogue::STRIP_INPUTS.contains(&input.as_str()) {
                return Err(format!("--input {input}: one of {}", makepad_stage_render_kit::catalogue::STRIP_INPUTS.join(", ")));
            }
            Ok(Command::Strip { stack: EffectStack::new(vec![effect]), input, times, out: PathBuf::from(value("--out").ok_or(usage)?) })
        }
        Some("catalogue") => {
            println!("{}", makepad_stage_render_kit::catalogue_json());
            std::process::exit(0)
        }
        Some("doc") => {
            print!("{}", makepad_stage_render_kit::guide());
            std::process::exit(0)
        }
        _ => Err(usage.into()),
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        let args: Vec<String> = std::env::args().collect();
        let command = parse_args(&args).unwrap_or_else(|e| finish(Err(e)));
        if let Some(mut host) = self.ui.widget(cx, ids!(host)).borrow_mut::<KitHost>() {
            host.start(cx, command);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::script_mod(vm);
        makepad_stage_render_kit::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
