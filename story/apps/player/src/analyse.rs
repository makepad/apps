//! `--analyse <dir>`: what the web export needs to know about the film,
//! learned by playing it (native only).
//!
//! The player steps through every frame of the film on locked time
//! (`1 / fps` apart, waiting at each until the plate is ready and its
//! frame drawn exact), with the recorders on: every kernel compiled
//! (compute's `record_compiled`), every glyph laid out (the text layouter
//! and 3D type), and, where the platform has it, every draw shader built.
//! Its own loader and controls draw too, so their shaders and glyphs are
//! among them. Then it writes into `<dir>` (the files
//! `makepad-story-web-export`'s `analyse` module reads) and quits:
//!
//! - `glyphs.txt`: `<16 hex FNV-1a 64 of a font file> <glyph id> ...`
//! - `kernels.wasm` + `kernels.keys`: one module of every kernel
//!   (`makepad_script_compute::kernel::wasm_module`, single-threaded
//!   memory, `relaxed_fma` off) and per kernel its program key and entry
//!   (`simd` when four wide; `scalar` is then `u32::MAX`, no entry)
//! - kernel blobs `audio.wasm` + `audio.keys`: one module of every audio
//!   program compiled (`makepad_script_audio_aot::wasm::module`, the same
//!   target) and their program keys in export order, when the film plays
//!   any
//! - `texts.txt`: per font, every row of text it laid out and in how many
//!   frames (`<font hash>\t<frames>\t<text, \n escaped>`): what the export
//!   reads to tell static text from counters, lyrics and open text
//! - `coverage.txt`: what the documents declared (`<family>\t<spec>`)
//! - `features.txt`, `summary.txt`, `diagnostics.txt`, then `done`
//!
//! `--check-glyphs <n>` instead draws `n` random times of a film (one with
//! subset fonts) and writes `check.txt`: `missing=<glyphs shaped to
//! .notdef>`; the export runs it to prove its subsets miss nothing.
//!
//! Progress goes to stdout as `analyse <t> <duration>` lines.

use crate::film::FilmRuntime;
use makepad_script_compute::kernel::{self, Kernel, KernelRef};
use makepad_script_compute::wasm::Target;
use makepad_story_film_pack::KernelEntry;
use makepad_widgets::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The blank list of the UI kit's unused Splash (see `finish`), in the
/// analysis directory.
pub const SPLASH_BLANK: &str = "splash_blank.bin";

/// How long one frame may wait for its plate before the analysis gives up
/// on it (a document that never loads is an error, not a hang).
const FRAME_WAIT: f64 = 120.0;

/// What the player's draw at [`Analyser::time`] did: drew a frame, drew it
/// exact (every glyph and shader ready), and painted what it recorded.
#[derive(Clone, Copy, Debug)]
pub struct Drawn {
    pub drawn: bool,
    pub exact: bool,
    pub painted: bool,
}

pub struct Analyser {
    dir: PathBuf,
    /// The frame being made (its index; its time is `i / fps`).
    i: usize,
    fps: f64,
    duration: f64,
    started: bool,
    waiting_since: f64,
    slowest: (f64, f64),
    kernels: BTreeMap<u64, kernel::Recorded>,
    glyphs: BTreeMap<String, BTreeSet<u16>>,
    /// Per font: each text it laid out, and in how many frames.
    texts: BTreeMap<String, BTreeMap<String, u32>>,
    /// Per font: every set of variation axis values it was read at.
    instances: BTreeMap<String, BTreeSet<String>>,
    coverage: BTreeSet<(String, String)>,
    /// `--check-glyphs`: the random times to draw (the analysis steps every
    /// frame instead when None).
    check: Option<Vec<f64>>,
    check_n: usize,
    /// Glyphs shaped to `.notdef` so far (the layouter's count).
    missing: usize,
    /// The shader pack recorded (written at the end).
    shaders: Vec<u8>,
    /// 3D type was built.
    text3d: bool,
    diagnostics: String,
    /// The script modules registered and used (the web optimiser drops the
    /// registration of the unused ones).
    modules: Option<makepad_widgets::makepad_platform::makepad_script_census::ModuleUse>,
    pub finished: bool,
}

fn fnv_hex(bytes: &[u8]) -> String {
    let h = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    format!("{h:016x}")
}

/// Before any shader is applied: under `--analyse`, every draw shader the
/// app builds from now on is also compiled for the browser (GLSL ES and its
/// reflection) and kept for the shader pack.
pub fn early(vm: &mut ScriptVm) {
    if std::env::args().any(|a| a == "--analyse") && !std::env::args().any(|a| a == "--check-glyphs") {
        vm.host.cx_mut().record_shader_pack(true);
        // Which script modules register and which the film uses.
        makepad_widgets::makepad_platform::makepad_script_census::census_record(vm);
    }
}

impl Analyser {
    /// The analyser `--analyse <dir>` asks for.
    pub fn from_args() -> Option<Analyser> {
        let args: Vec<String> = std::env::args().collect();
        let dir = args.windows(2).find(|w| w[0] == "--analyse").map(|w| PathBuf::from(&w[1]))?;
        let check_n = args.windows(2).find(|w| w[0] == "--check-glyphs").and_then(|w| w[1].parse().ok());
        Some(Analyser {
            texts: BTreeMap::new(),
            instances: BTreeMap::new(),
            coverage: BTreeSet::new(),
            check: None,
            check_n: check_n.unwrap_or(0),
            missing: 0,
            shaders: Vec::new(),
            text3d: false,
            dir,
            i: 0,
            fps: 60.0,
            duration: 0.0,
            started: false,
            waiting_since: 0.0,
            slowest: (0.0, 0.0),
            kernels: BTreeMap::new(),
            glyphs: BTreeMap::new(),
            diagnostics: String::new(),
            modules: None,
            finished: false,
        })
    }

    /// Turns the recorders on (before the film loads anything).
    pub fn start(&mut self, cx: &mut Cx) {
        kernel::record_compiled(true);
        makepad_script_audio_aot::wasm::record_compiled(true);
        makepad_text_mesh::letters::record_glyphs(true);
        makepad_doc_fonts::record_coverage(true);
        CxDraw::lazy_construct_fonts(cx);
        cx.get_global::<std::rc::Rc<std::cell::RefCell<text::fonts::Fonts>>>().borrow_mut().record_glyphs(true);
        self.started = true;
        self.waiting_since = Cx::monotonic_now();
    }

    /// The time to draw now.
    pub fn time(&self) -> f64 {
        match &self.check {
            Some(times) => times.get(self.i).copied().unwrap_or(self.duration),
            None => self.i as f64 / self.fps,
        }
    }

    /// Frames to make: every frame, or the check's random times.
    fn frames(&self) -> usize {
        match &self.check {
            Some(times) => times.len(),
            None => (self.duration * self.fps).floor() as usize + 1,
        }
    }

    fn note(&mut self, severity: &str, message: &str) {
        self.diagnostics.push_str(&format!("{severity}\t\t0\t0\t{}\n", message.replace('\n', "\\n").replace('\t', " ")));
    }

    /// After a draw at [`Self::time`]: the frame counts when it drew what
    /// the film shows there ([`Drawn`]). Moves on to the next frame, or finishes (writes
    /// the analysis; the caller quits).
    pub fn frame_done(&mut self, cx: &mut Cx, film: &FilmRuntime, d: Drawn) {
        // A hidden window does not present, so `painted` stays false: the
        // recorders see everything at record time (shaders at apply, glyphs at
        // layout, kernels at compile).
        let exact = d.drawn && d.exact;
        if self.finished || !self.started {
            return;
        }
        let now = Cx::monotonic_now();
        if let Some(m) = &film.manifest {
            self.duration = m.film.duration;
            if m.film.fps > 0.0 {
                self.fps = m.film.fps;
            }
        }
        self.collect(cx);
        if self.duration <= 0.0 {
            return;
        }
        if self.check_n > 0 && self.check.is_none() {
            // Random times over the film, the same on every run.
            let mut seed = 0x9e37_79b9_7f4a_7c15u64;
            let times = (0..self.check_n)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 11) as f64 / (1u64 << 53) as f64 * self.duration
                })
                .collect();
            self.check = Some(times);
            self.i = 0;
            return;
        }
        if !exact {
            let waited = now - self.waiting_since;
            if waited > 10.0 && (waited % 10.0) < 0.05 {
                let t = self.time();
                eprintln!("analyse: waiting {waited:.0} s at {t:.3} ({}): plate ready {}, drawn {}, exact {}, presented {}", film.plate_name(t).unwrap_or("-"), film.ready_at(t), d.drawn, d.exact, d.painted);
            }
            if waited > FRAME_WAIT {
                let t = self.time();
                self.note("error", &format!("{} at {t:.3} s never became ready", film.plate_name(t).unwrap_or("the film")));
                self.finish(cx, film);
            }
            return;
        }
        let took = now - self.waiting_since;
        if took > self.slowest.1 {
            self.slowest = (self.time(), took);
        }
        self.i += 1;
        self.waiting_since = now;
        if self.i % 30 == 0 {
            println!("analyse {:.3} {:.3}", self.i as f64 / self.frames().max(1) as f64 * self.duration, self.duration);
        }
        if self.i >= self.frames() {
            self.finish(cx, film);
        }
    }

    /// Takes what the recorders hold so far.
    fn collect(&mut self, cx: &mut Cx) {
        for r in kernel::take_recorded() {
            self.kernels.entry(r.kernel.program_key()).or_insert(r);
        }
        let fonts = cx.get_global::<std::rc::Rc<std::cell::RefCell<text::fonts::Fonts>>>().clone();
        self.missing = fonts.borrow().missing_glyphs();
        for r in fonts.borrow_mut().take_recorded_glyphs() {
            let hash = fnv_hex(r.font.data().as_slice());
            let coords: Vec<String> = r.variation_coords().iter().map(|(tag, v)| format!("{}={v}", String::from_utf8_lossy(&tag.to_be_bytes()))).collect();
            self.instances.entry(hash.clone()).or_default().insert(coords.join(","));
            self.glyphs.entry(hash.clone()).or_default().extend(r.glyphs);
            let texts = self.texts.entry(hash).or_default();
            for t in r.texts {
                *texts.entry(t).or_default() += 1;
            }
        }
        for d in makepad_doc_fonts::take_coverage() {
            self.coverage.insert((d.family, d.spec));
        }
        for (hash, ids) in makepad_text_mesh::letters::take_recorded_glyphs() {
            self.text3d = true;
            self.glyphs.entry(format!("{hash:016x}")).or_default().extend(ids);
        }
    }

    fn finish(&mut self, cx: &mut Cx, film: &FilmRuntime) {
        self.collect(cx);
        self.shaders = cx.take_shader_pack();
        self.modules = cx.module_census(&["makepad_story_player"]);
        // The widget kit's and theme's Splash the run never reached: a web
        // build blanks it (`makepad_platform::splash_blank`). Only the UI
        // kit's own blocks: the documents run in VMs of their own.
        if self.check.is_none() {
            let list = cx.with_vm(|vm| makepad_widgets::makepad_platform::splash_blank::compute_blank_list(vm));
            let list = makepad_widgets::makepad_platform::splash_blank::BlankList { blocks: list.blocks.into_iter().filter(|b| b.file.starts_with("widgets/")).collect() };
            if let Err(e) = std::fs::create_dir_all(&self.dir).and_then(|_| std::fs::write(self.dir.join(SPLASH_BLANK), list.write())) {
                error!("player: writing the splash blank list: {e}");
            }
        }
        self.finished = true;
        if let Err(e) = self.write(film) {
            error!("player: writing the analysis: {e}");
            std::process::exit(1);
        }
    }

    fn write(&mut self, film: &FilmRuntime) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        if self.check.is_some() {
            std::fs::write(self.dir.join("check.txt"), format!("missing={}\nframes={}\n", self.missing, self.i))?;
            std::fs::write(self.dir.join("done"), b"")?;
            return Ok(());
        }
        let mut m = makepad_web_manifest::Manifest::default();
        m.texts = self.texts.clone();
        m.coverage = self.coverage.iter().map(|(face, spec)| makepad_web_manifest::Coverage { face: face.clone(), spec: spec.clone(), by: String::new() }).collect();
        m.glyphs = self.glyphs.clone();

        // The kernels, as the browser runs them: the threaded film's shared
        // memory and no relaxed multiply-add (the same on every machine).
        let target = Target { shared: true, max_pages: None, relaxed_fma: false };
        let mut linkable: Vec<(u64, KernelRef)> = Vec::new();
        let mut refused = Vec::new();
        for (key, r) in &self.kernels {
            let k = &r.kernel;
            if kernel::wasm_module(&[k.as_ref()], target).is_some() {
                linkable.push((*key, k.clone()));
            } else {
                refused.push(format!("{k:?}"));
            }
        }
        for k in refused {
            self.note("error", &format!("{k} has no wasm form (it uses audio I/O or host calls): the browser cannot run it"));
        }
        let refs: Vec<&Kernel> = linkable.iter().map(|(_, k)| k.as_ref()).collect();
        let (module, simd) = if refs.is_empty() { (Vec::new(), Vec::new()) } else { kernel::wasm_module(&refs, target).unwrap_or_default() };
        // One entry per kernel, export k kernel k's: four wide or scalar.
        let entries: Vec<KernelEntry> = linkable
            .iter()
            .zip(&simd)
            .enumerate()
            .map(|(i, ((key, _), simd))| if *simd { KernelEntry { key: *key, scalar: u32::MAX, simd: Some(i as u32) } } else { KernelEntry { key: *key, scalar: i as u32, simd: None } })
            .collect();
        m.kernels_wasm = module.clone();
        m.kernel_keys = KernelEntry::write_all(&entries);
        // The audio programs, linked like the kernels on the thread that
        // runs them (the UI thread of the single-threaded film).
        let mut audio: BTreeMap<u64, makepad_script_audio_aot::AudioShaderRef> = BTreeMap::new();
        for shader in makepad_script_audio_aot::wasm::take_recorded() {
            audio.entry(shader.program_key()).or_insert(shader);
        }
        let audio: Vec<(u64, makepad_script_audio_aot::AudioShaderRef)> = audio
            .into_iter()
            .filter(|(key, s)| {
                let ok = makepad_script_audio_aot::wasm::module(&[s.as_ref()], target).is_some();
                if !ok {
                    self.note("warning", &format!("audio program {key:016x} has no wasm form: it runs on the interpreter in the browser"));
                }
                ok
            })
            .collect();
        if !audio.is_empty() {
            let shaders: Vec<&makepad_script_audio_aot::AudioShader> = audio.iter().map(|(_, s)| s.as_ref()).collect();
            if let Some(module) = makepad_script_audio_aot::wasm::module(&shaders, target) {
                let keys: Vec<u64> = audio.iter().map(|(key, _)| *key).collect();
                let blob = |bytes: Vec<u8>| makepad_web_manifest::KernelBlob { bytes, by: "audio programs".into() };
                m.kernel_blobs.insert(makepad_story_film_pack::AUDIO_BLOB.into(), blob(module));
                m.kernel_blobs.insert(makepad_story_film_pack::AUDIO_KEYS_BLOB.into(), blob(makepad_story_film_pack::write_audio_keys(&keys)));
                m.summary.insert("audio_programs".into(), keys.len().to_string());
            }
        }
        // The player's subsystem features this film needs (its cargo
        // features): Scene3D when a kernel or 3D type was used.
        if !self.kernels.is_empty() || self.text3d {
            m.features.insert("scene3d".into());
        }
        if makepad_motion_draw::vj_effects_used() {
            self.note("warning", "the film names VJ effects (a vjfx/ preset or a library effect), which need the VJ effect pack; the player has none, so it cannot draw them");
        }
        // Per kernel: the size of its scalar and four wide code (the scalar
        // form of the four wide kernels built here only to compare).
        let sizes = function_sizes(&module);
        let scalar_module = kernel::wasm_module_scalar(&refs, target).unwrap_or_default();
        let scalar_sizes = function_sizes(&scalar_module);
        let mut table = String::from("# key scalar_bytes simd_bytes simd/scalar (the film links simd_bytes where present)\n");
        for (i, e) in entries.iter().enumerate() {
            let scalar = scalar_sizes.get(i).copied().unwrap_or(0);
            match e.simd.and_then(|i| sizes.get(i as usize).copied()) {
                Some(v) => {
                    let ratio = v as f64 / scalar.max(1) as f64;
                    table.push_str(&format!("{:016x} {scalar} {v} {ratio:.2}\n", e.key));
                    if ratio > 3.0 {
                        self.note("warning", &format!("kernel {:016x}: its four wide code is {ratio:.1}x its scalar code ({v} vs {scalar} bytes)", e.key));
                    }
                }
                None => table.push_str(&format!("{:016x} {scalar} - -\n", e.key)),
            }
        }
        let shader_count = makepad_widgets::makepad_script::shader_pack::inspect_shader_pack(&self.shaders).len();
        m.summary.insert("shaders".into(), shader_count.to_string());
        if let Some(modules) = self.modules.take() {
            m.summary.insert("modules_used".into(), format!("{} of {}", modules.used.len(), modules.registered.len()));
            m.modules = modules.used;
            m.modules_registered = modules.registered;
        }
        m.shaders = std::mem::take(&mut self.shaders);
        for e in &film.errors {
            self.note("error", e);
        }
        m.summary.insert("frames".into(), self.i.to_string());
        m.summary.insert("duration".into(), format!("{:.3}", self.duration));
        m.summary.insert("kernels".into(), entries.len().to_string());
        m.summary.insert("kernel_bytes".into(), module.len().to_string());
        m.summary.insert("simd_kernels".into(), entries.iter().filter(|e| e.simd.is_some()).count().to_string());
        m.summary.insert("fonts".into(), self.glyphs.len().to_string());
        m.summary.insert("glyphs".into(), self.glyphs.values().map(|g| g.len()).sum::<usize>().to_string());
        m.summary.insert("slowest_frame".into(), format!("{:.3}s at {:.3}", self.slowest.1, self.slowest.0));
        m.diagnostics = self
            .diagnostics
            .lines()
            .filter_map(|l| {
                let mut f = l.splitn(5, '\t');
                let severity = makepad_web_manifest::Severity::parse(f.next()?);
                let (file, line, col, message) = (f.next()?.to_string(), f.next()?.parse().unwrap_or(0), f.next()?.parse().unwrap_or(0), makepad_web_manifest::unescape(f.next()?));
                Some(makepad_web_manifest::Diagnostic { severity, file, line, col, message })
            })
            .collect();
        m.write(&self.dir)?;
        std::fs::write(self.dir.join("kernel_sizes.txt"), table)?;
        // Per font: `<hash>\t<tag>=<value>,...`, one line per instance.
        let mut instances = String::new();
        for (font, set) in &self.instances {
            for coords in set {
                instances.push_str(&format!("{font}\t{coords}\n"));
            }
        }
        std::fs::write(self.dir.join("instances.txt"), instances)?;
        // Which font file each laid-out font is (by content hash): the app's
        // own fonts load by resource path, so the export must know the files.
        let mut files = String::new();
        for path in font_files() {
            if let Ok(bytes) = std::fs::read(&path) {
                let hash = fnv_hex(&bytes);
                if self.glyphs.contains_key(&hash) {
                    files.push_str(&format!("{hash}\t{}\n", path.display()));
                }
            }
        }
        std::fs::write(self.dir.join("fontfiles.txt"), files)?;
        // Per kernel: its sources, to compile it again elsewhere.
        let src_dir = self.dir.join("kernel_sources");
        std::fs::create_dir_all(&src_dir)?;
        for (key, r) in &self.kernels {
            // A kernel built from Splash values has no text of its own.
            if !r.source.is_empty() {
                std::fs::write(src_dir.join(format!("{key:016x}.splash")), &r.source)?;
            }
            std::fs::write(src_dir.join(format!("{key:016x}.layouts.json")), layouts_json(&r.layouts))?;
            for module in &r.modules {
                let name: String = module.path.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' { c } else { '_' }).collect();
                std::fs::write(src_dir.join(format!("{key:016x}.module.{name}.splash")), &module.text)?;
            }
        }
        Ok(())
    }
}

/// `[{name, stride, fields: [{name, ty, offset}]}]`.
fn layouts_json(layouts: &[kernel::Layout]) -> String {
    let fields = |l: &kernel::Layout| {
        l.fields.iter().map(|f| format!("{{\"name\":{:?},\"ty\":\"{:?}\",\"offset\":{}}}", f.name, f.ty, f.offset)).collect::<Vec<_>>().join(",")
    };
    let items: Vec<String> = layouts.iter().map(|l| format!("{{\"name\":{:?},\"stride\":{},\"fields\":[{}]}}", l.name, l.stride, fields(l))).collect();
    format!("[{}]\n", items.join(","))
}

/// The body size of every function of a wasm module (it imports no
/// functions, so export `k` is function `k`).
fn function_sizes(wasm: &[u8]) -> Vec<usize> {
    fn leb(b: &[u8], at: &mut usize) -> Option<usize> {
        let (mut v, mut shift) = (0usize, 0);
        loop {
            let byte = *b.get(*at)?;
            *at += 1;
            v |= ((byte & 0x7f) as usize) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
            shift += 7;
        }
    }
    let mut out = Vec::new();
    let mut at = 8;
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let Some(len) = leb(wasm, &mut at) else { break };
        let end = at + len;
        if id == 10 {
            let mut p = at;
            let Some(count) = leb(wasm, &mut p) else { break };
            for _ in 0..count {
                let Some(size) = leb(wasm, &mut p) else { break };
                out.push(size);
                p += size;
            }
        }
        at = end;
    }
    out
}

/// The font files a Makepad app can load by resource path: every face of
/// every family in Motion's table (`makepad_doc_fonts::FAMILIES`) that
/// is on this machine.
fn font_files() -> Vec<PathBuf> {
    makepad_doc_fonts::register_root("makepad_widgets", makepad_widgets::widgets_dir());
    let mut out: Vec<PathBuf> = makepad_doc_fonts::FAMILIES
        .iter()
        .flat_map(|family| family.files.iter())
        .filter(|file| matches!(file.role, makepad_doc_fonts::FontFileRole::Face { .. }))
        .filter_map(makepad_doc_fonts::local_path)
        .filter(|path| path.is_file())
        .collect();
    out.sort();
    out.dedup();
    out
}
