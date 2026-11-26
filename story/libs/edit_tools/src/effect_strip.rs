//! `effect-strip`: what an effect or transition looks like over time, on a
//! fixed test card, as grabbed frames (a contact sheet with
//! [`crate::grab::strip_picture`]). Three kinds of effect:
//!
//! - a bundled vj_fx preset (`vjfx/<name>`) or a library `VjEffect` (its
//!   source): an `Fx` over the card and a title, drawn by the cut
//!   renderer's engine on locked time; a transition (a preset, or a source
//!   that declares `engine: "transition"`) goes from card A to card B over
//!   the whole strip;
//! - a render-kit effect (`kit/<name>` with parameters): the kit's stack
//!   applied to card A at each time.
//!
//! In process, like [`crate::FrameGrabber`]: [`EffectStrip::new`], then
//! `handle_event` on every event and `draw` from the host's draw until it
//! is no longer busy.

use crate::grab::{FrameGrabber, GrabbedFrame};
use crate::image::{encode_png, Picture};
use crate::input::spread;
use makepad_story_edit_render::audio::{fps_ratio, frame_time};
use makepad_story_edit_render::engine::CutJob;
use makepad_story_edit_render::render::opaque_rgba;
use makepad_story_edit_render::LocalMedia;
use makepad_stage_render_kit::{Anim, Effect, EffectStack, FxContext, Painted, RenderKit, TextureLayer, Value};
use makepad_story_cut::{AssetRef, CutDoc};
use makepad_content_packs::effects as packs;
use makepad_widgets::*;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The effect a strip shows.
#[derive(Clone)]
pub enum EffectSpec {
    /// A bundled vj_fx preset, `vjfx/<name>` or `<name>`; a transition
    /// preset runs from card A to card B.
    Preset(String),
    /// A library `VjEffect`: its revision and its Splash source.
    Library { asset: AssetRef, source: String },
    /// A render-kit effect by name (`glow`, `kit/glow`) with parameters.
    Kit { name: String, params: Vec<(String, Anim)> },
}

impl EffectSpec {
    /// `kit/<name>` is a render-kit effect, anything else a vj_fx preset.
    pub fn parse(name: &str, params: Vec<(String, Anim)>) -> EffectSpec {
        match name.strip_prefix("kit/") {
            Some(kit) => EffectSpec::Kit { name: kit.to_string(), params },
            None => EffectSpec::Preset(name.to_string()),
        }
    }
}

/// How the strip is made.
#[derive(Clone, Debug)]
pub struct StripOptions {
    /// The card's size in pixels.
    pub size: (u32, u32),
    /// Seconds the effect runs over.
    pub duration: f64,
    pub fps: f64,
    /// Frames in the strip, spread over the duration.
    pub count: usize,
    /// Each frame's width in the strip.
    pub width: u32,
}

impl Default for StripOptions {
    fn default() -> Self {
        Self { size: (640, 360), duration: 4.0, fps: 30.0, count: 8, width: 320 }
    }
}

/// Test card A: a colour sweep with a grid, a ring and bars of fine detail.
/// Card B: a checkerboard with a disc. Straight RGBA, opaque.
pub fn test_card(which: u8, size: (u32, u32)) -> Picture {
    let (w, h) = (size.0.max(2), size.1.max(2));
    let mut picture = Picture::new(w, h, [0, 0, 0, 255]);
    let (cx, cy, r) = (w as f64 / 2.0, h as f64 / 2.0, h.min(w) as f64 * 0.3);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
            let d = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
            let rgb = if which == 0 {
                let grid = x % (w / 16).max(2) < 2 || y % (h / 9).max(2) < 2;
                let ring = (d - r).abs() < r * 0.06;
                let bars = fy > 0.86 && fy < 0.94 && (x / 3) % 2 == 0;
                if ring || bars {
                    [250, 250, 250]
                } else if grid {
                    [20, 20, 30]
                } else {
                    [(fx * 255.0) as u8, (fy * 200.0 + 30.0) as u8, ((1.0 - fx) * 200.0 + 40.0) as u8]
                }
            } else {
                let check = ((x / (w / 12).max(1)) + (y / (h / 7).max(1))) % 2 == 0;
                if d < r * 0.8 {
                    [230, 40, 60]
                } else if check {
                    [40, 70, 210]
                } else {
                    [235, 232, 215]
                }
            };
            let i = ((y * w + x) * 4) as usize;
            picture.rgba[i..i + 3].copy_from_slice(&rgb);
        }
    }
    picture
}

fn fixed_id(seed: u8) -> String {
    makepad_asset_data::AssetId::from_bytes([seed; 16]).to_string()
}

fn fixed_rev(bytes: &[u8]) -> String {
    let digest = makepad_asset_data::BlobId::hash_of(bytes);
    format!("arev_{}", digest.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Whether an effect document is a two-deck transition: it declares
/// `engine: "transition"` on a line of its own, as documents write it.
fn source_is_transition(source: &str) -> bool {
    source.lines().any(|line| line.trim().strip_prefix("engine:").is_some_and(|v| v.trim().trim_end_matches(',').trim() == "\"transition\""))
}

/// The cut that shows a vj_fx effect (or transition) over the test cards,
/// with the media table that resolves it. The cards are written as PNG
/// into `dir`.
pub fn effect_edit(spec: &EffectSpec, options: &StripOptions, dir: &Path) -> Result<(CutDoc, LocalMedia), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut media = LocalMedia::new();
    let mut cards = Vec::new();
    for which in 0..2u8 {
        let png = encode_png(&test_card(which, options.size))?;
        let path = dir.join(format!("card_{}_{}x{}.png", (b'a' + which) as char, options.size.0, options.size.1));
        std::fs::write(&path, &png).map_err(|e| format!("{}: {e}", path.display()))?;
        let id = fixed_id(0xc0 + which);
        media.insert(id.clone(), path);
        cards.push(format!("asset(\"{id}\", \"{}\")", fixed_rev(&png)));
    }
    let d = options.duration;
    let (effect, transition) = match spec {
        EffectSpec::Preset(name) => {
            let bare = name.strip_prefix("vjfx/").unwrap_or(name);
            if packs::resolve(bare).is_none() {
                let near = crate::inspect::suggest(bare, false);
                let hint = if near.is_empty() { String::new() } else { format!("; did you mean {}?", near.iter().map(|n| format!("vjfx/{n}")).collect::<Vec<_>>().join(", ")) };
                return Err(format!("there is no bundled effect `{name}`{hint} (render-kit effects are `kit/<name>`)"));
            }
            (format!("\"vjfx/{bare}\""), packs::is_transition(bare))
        }
        EffectSpec::Library { asset, source } => {
            media.insert_effect(asset.id.clone(), source.clone());
            (format!("asset(\"{}\", \"{}\")", asset.id, asset.rev), source_is_transition(source))
        }
        EffectSpec::Kit { .. } => return Err("a render-kit effect is drawn by the kit, not as a cut".into()),
    };
    let nodes = if transition {
        format!(
            "let a = Still{{src: {a} span: [0.0, {d}] fit: @fill}}\n\
             let b = Still{{src: {b} span: [0.0, {d}] fit: @fill track: 1}}\n\
             let fx = Transition{{a: a.out(@frame) b: b.out(@frame) effect: {effect} span: [0.0, {d}]}}\n\
             let out = CutOutput{{frame: fx.out(@frame)}}\n",
            a = cards[0],
            b = cards[1]
        )
    } else {
        format!(
            "let a = Still{{src: {a} span: [0.0, {d}] fit: @fill}}\n\
             let title = Title{{text: \"Aa 0123\" span: [0.0, {d}] anchor: vec2(0.5, 0.5) font_size: {font} track: 1}}\n\
             let s = Stack{{layers: [a.out(@frame), title.out(@frame)]}}\n\
             let fx = Fx{{input: s.out(@frame) effect: {effect} span: [0.0, {d}]}}\n\
             let out = CutOutput{{frame: fx.out(@frame)}}\n",
            a = cards[0],
            font = (options.size.1 as f64 * 0.12).round().max(8.0)
        )
    };
    let list = if transition { "a, b, fx, out" } else { "a, title, s, fx, out" };
    let source = format!(
        "use mod.flow.*\nuse mod.cut.*\n\n{nodes}\nFlow{{\n    kind: @cut\n    label: \"effect strip\"\n    output: {{width: {} height: {} fps: {}}}\n    duration: {d}\n    bpm: 120\n    {list}\n}}\n",
        options.size.0, options.size.1, options.fps
    );
    let doc = CutDoc::load(&source).map_err(|diags| diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("; "))?;
    Ok((doc, media))
}

/// Parse `name=value` into a kit parameter: numbers, `x,y` / `x,y,z`
/// vectors, `#rrggbb[aa]` colours, `true`/`false`, `@choice`, else text.
pub fn kit_param(pair: &str) -> Result<(String, Anim), String> {
    let (name, value) = pair.split_once('=').ok_or_else(|| format!("--param {pair}: expected <name>=<value>"))?;
    let value = value.trim();
    let numbers: Option<Vec<f64>> = value.split(',').map(|v| v.trim().parse::<f64>().ok()).collect();
    let parsed = match numbers.as_deref() {
        Some([v]) => Value::Num(*v),
        Some([x, y]) => Value::Vec2([*x, *y]),
        Some([x, y, z]) => Value::Vec3([*x, *y, *z]),
        _ if value == "true" || value == "false" => Value::Bool(value == "true"),
        _ if value.starts_with('#') => Value::Text(value.to_string()),
        _ if value.starts_with('@') => Value::Choice(value.trim_start_matches('@').to_string()),
        _ => Value::Text(value.to_string()),
    };
    Ok((name.trim().to_string(), Anim::Const(parsed)))
}

enum KitStep {
    Recording,
    Painting { texture: Texture, retries: u32 },
    Reading { ticket: ReadbackTicket },
}

/// A render-kit stack over card A at each time.
struct KitRun {
    kit: RenderKit,
    stack: EffectStack,
    card: Texture,
    size: (u32, u32),
    fps: f64,
    todo: VecDeque<(u64, f64)>,
    width: u32,
    step: KitStep,
    readback_wait_logged: bool,
    layers: HashMap<String, Texture>,
}

enum Driver {
    Edit(FrameGrabber),
    Kit(KitRun),
}

pub struct EffectStrip {
    driver: Driver,
    frames: Vec<GrabbedFrame>,
    result: Option<Result<(), String>>,
    /// What building a kit effect warned about.
    pub warnings: Vec<String>,
}

/// Times spread over the duration, on the strip's frame grid.
fn strip_times(options: &StripOptions) -> Vec<f64> {
    spread(0.0, options.duration, options.count.max(1), options.fps, options.duration)
}

impl EffectStrip {
    /// Start a strip of `spec`. `dir` holds the test cards (written once per
    /// size). Fails at once for an unknown effect or bad parameters.
    pub fn new(cx: &mut Cx, spec: &EffectSpec, options: &StripOptions, dir: &Path) -> Result<Self, String> {
        let times = strip_times(options);
        match spec {
            EffectSpec::Kit { name, params } => {
                let (effect, warnings) = Effect::build(name, params.iter().cloned()).map_err(|d| d.errors.join("; "))?;
                let card = test_card(0, options.size);
                let data = card.rgba.chunks_exact(4).map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect();
                let texture = Texture::new_with_format(
                    cx,
                    TextureFormat::VecBGRAu8_32 { width: options.size.0 as usize, height: options.size.1 as usize, data: Some(data), updated: TextureUpdated::Full },
                );
                let ratio = fps_ratio(options.fps);
                let todo = times.iter().map(|&t| (crate::input::frame_of(t, options.fps, options.duration), t)).map(|(n, _)| (n, frame_time(n, ratio))).collect();
                let run = KitRun {
                    kit: RenderKit::new(cx),
                    stack: EffectStack::new(vec![effect]),
                    card: texture,
                    size: options.size,
                    fps: options.fps,
                    todo,
                    width: options.width,
                    step: KitStep::Recording,
                    readback_wait_logged: false,
                    layers: HashMap::new(),
                };
                Ok(Self { driver: Driver::Kit(run), frames: Vec::new(), result: None, warnings })
            }
            _ => {
                let (doc, media) = effect_edit(spec, options, dir)?;
                let job = CutJob { doc: Arc::new(doc), marks: HashMap::new(), resolver: Box::new(media), audio: false };
                Ok(Self { driver: Driver::Edit(FrameGrabber::new(cx, job, &times, options.width)), frames: Vec::new(), result: None, warnings: Vec::new() })
            }
        }
    }

    pub fn busy(&self) -> bool {
        match &self.driver {
            Driver::Edit(grabber) => grabber.busy(),
            Driver::Kit(_) => self.result.is_none(),
        }
    }

    /// The frames, or why there are none, once done.
    pub fn result(&self) -> Option<Result<&[GrabbedFrame], &str>> {
        match &self.driver {
            Driver::Edit(grabber) => grabber.result(),
            Driver::Kit(_) => match &self.result {
                Some(Ok(())) => Some(Ok(&self.frames)),
                Some(Err(error)) => Some(Err(error)),
                None => None,
            },
        }
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        let run = match &mut self.driver {
            Driver::Edit(grabber) => return grabber.handle_event(cx, event),
            Driver::Kit(run) => run,
        };
        if self.result.is_some() {
            return;
        }
        let KitStep::Reading { ticket } = &run.step else { return };
        let ticket = *ticket;
        let Some(readback) = cx.try_take_texture_readbacks_for(&[ticket]).into_iter().next() else { return };
        let data = match readback.data {
            Ok(data) => data,
            Err(error) => {
                self.result = Some(Err(format!("readback: {error}")));
                return;
            }
        };
        let mut rgba = Vec::new();
        opaque_rgba(&data, &mut rgba);
        let (n, t) = run.todo.pop_front().unwrap_or((0, 0.0));
        let exposure = crate::exposure::Exposure::of_rgba(&rgba);
        match makepad_story_edit_render::thumbnail::scale(&rgba, readback.width as u32, readback.height as u32, run.width) {
            Ok((rgba, width, height)) => self.frames.push(GrabbedFrame { frame: n, t, picture: Picture { width, height, rgba }, exposure, coverage: Vec::new() }),
            Err(error) => {
                self.result = Some(Err(error));
                return;
            }
        }
        run.step = KitStep::Recording;
        if run.todo.is_empty() {
            self.result = Some(Ok(()));
        }
    }

    /// Record, wait for the paint, read back. From the host's draw.
    pub fn draw(&mut self, cx: &mut Cx2d) {
        let run = match &mut self.driver {
            Driver::Edit(grabber) => return grabber.draw(cx),
            Driver::Kit(run) => run,
        };
        if self.result.is_some() {
            return;
        }
        match &mut run.step {
            KitStep::Recording => {
                let Some(&(_, t)) = run.todo.front() else { return };
                run.kit.begin_frame(cx);
                let ctx = FxContext::new(t, run.fps, &run.layers);
                let applied = run.kit.apply_layer(cx, &run.stack, run.size, &ctx, &mut TextureLayer(run.card.clone()));
                run.step = KitStep::Painting { texture: applied.texture, retries: 0 };
            }
            KitStep::Painting { texture, retries } => match run.kit.painted(cx) {
                Painted::Painting => run.kit.reattach(cx),
                Painted::Partial if *retries < 60 => {
                    *retries += 1;
                    run.step = KitStep::Recording;
                }
                Painted::Partial => self.result = Some(Err("the effect's shaders did not compile".into())),
                Painted::Whole => match texture.read_back(cx, ReadbackRequest::default()) {
                    Ok(ticket) => {
                        run.readback_wait_logged = false;
                        run.step = KitStep::Reading { ticket };
                    }
                    Err(ReadbackError::Backpressure) => {
                        if !run.readback_wait_logged {
                            log!("effect strip: context readback request limit reached; effect frame waits");
                            run.readback_wait_logged = true;
                        }
                    }
                    Err(error) => self.result = Some(Err(format!("readback: {error}"))),
                },
            },
            KitStep::Reading { .. } => {}
        }
    }
}

/// Where test cards are kept: the system temp folder.
pub fn default_card_dir() -> PathBuf {
    std::env::temp_dir().join("makepad-edit-tools-cards")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_edits_load_for_effects_and_transitions() {
        let dir = std::env::temp_dir().join(format!("edit_tools_cards_{}", std::process::id()));
        let options = StripOptions::default();
        let library = |seed: u8, engine: &str| EffectSpec::Library {
            asset: AssetRef { id: fixed_id(seed), rev: fixed_rev(&[seed]) },
            source: format!("{{\n    engine: \"{engine}\"\n}}"),
        };
        let (doc, _) = effect_edit(&library(0xe0, "screen"), &options, &dir).unwrap();
        assert!(doc.node("fx").is_some() && doc.node("title").is_some());
        let (doc, _) = effect_edit(&library(0xe1, "transition"), &options, &dir).unwrap();
        assert!(matches!(doc.node("fx").unwrap().kind, makepad_story_cut::CutNodeKind::Transition { .. }));
        // A preset is an installed effect pack's; none is installed here.
        assert!(effect_edit(&EffectSpec::Preset("vjfx/nope".into()), &options, &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kit_params_parse_by_shape() {
        let value = |pair: &str| match kit_param(pair).unwrap().1 {
            Anim::Const(value) => value,
            _ => panic!("{pair} is not a constant"),
        };
        assert_eq!(value("radius=12"), Value::Num(12.0));
        assert_eq!(value("offset=3,4"), Value::Vec2([3.0, 4.0]));
        assert_eq!(value("color=#ff0000"), Value::Text("#ff0000".into()));
        assert_eq!(value("mode=@screen"), Value::Choice("screen".into()));
        assert_eq!(value("on=true"), Value::Bool(true));
        assert!(kit_param("nothing").is_err());
        // The kit's own diagnostics name the fix.
        let error = Effect::build("glw", Vec::<(String, Anim)>::new()).err().unwrap();
        assert!(error.errors[0].contains("glow"), "{:?}", error.errors);
    }

    #[test]
    fn the_cards_differ() {
        let (a, b) = (test_card(0, (64, 36)), test_card(1, (64, 36)));
        assert_eq!(a.rgba.len(), 64 * 36 * 4);
        assert_ne!(a.rgba, b.rgba);
    }
}
