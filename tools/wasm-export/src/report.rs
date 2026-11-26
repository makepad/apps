//! The size report: the files written, the wasm (raw as shipped, brotli 11 as the
//! benchmark), and the wasm by part: code, the crates' Splash, the document's Splash, the
//! shader pack, glyphs, media and data, and what goes beside the page.
//!
//! Parts are measured on their own bytes (brotli 9 for the parts, within a few percent of 11).
//! Code is the rest of the wasm: everything but the embed, the pack and the crates' kept Splash.

use crate::coverage::Facts;
use crate::embed::{Built, Kind};
use crate::packs::MergedPack;
use crate::{Driver, Options};
use makepad_strict_json::{self as json, Value};
use makepad_web_pack::output::SizedFile;

#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub name: String,
    pub raw: usize,
    /// None: not measured on its own (it is spread through the module).
    pub br: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub training_drivers: Vec<Driver>,
    pub out_dir: String,
    pub files: Vec<SizedFile>,
    pub wasm_raw: usize,
    pub wasm_br: usize,
    pub parts: Vec<Part>,
    pub external: Vec<(String, u64)>,
    pub gate: String,
    pub pack: String,
    pub embed: String,
    pub functions: usize,
}

impl Report {
    #[allow(clippy::too_many_arguments)]
    pub fn new(opts: &Options, wasm: &[u8], wasm_br: usize, built: &Built, pack: &MergedPack, shipped: &Facts, gate: &str, files: &[SizedFile]) -> Report {
        let est = |bytes: &[u8]| (!bytes.is_empty()).then(|| makepad_web_pack::brotli_estimate(bytes));
        let embed_raw: usize = built.embed.write().len();
        let crate_splash = shipped.crate_splash.1 as usize;
        let mut parts = Vec::new();
        let code = wasm.len().saturating_sub(embed_raw + pack.bytes.len() + crate_splash);
        parts.push(Part { name: "code".into(), raw: code, br: None });
        parts.push(Part { name: "built-in Splash".into(), raw: crate_splash, br: None });
        for kind in [Kind::Splash, Kind::Font, Kind::Texture, Kind::Data, Kind::Listing] {
            let bodies = built.bodies_of(kind);
            parts.push(Part { name: kind.name().into(), raw: bodies.len(), br: est(&bodies) });
        }
        parts.push(Part { name: "shader pack".into(), raw: pack.bytes.len(), br: est(&pack.bytes) });
        // code's share of the whole: the module's brotli less its measured parts
        let measured: usize = parts.iter().filter_map(|p| p.br).sum();
        parts[0].br = Some(wasm_br.saturating_sub(measured));
        Report {
            training_drivers: opts.drivers.clone(),
            out_dir: opts.out_dir.display().to_string(),
            files: files.to_vec(),
            wasm_raw: wasm.len(),
            wasm_br,
            parts,
            external: built.external.iter().map(|x| (x.name.clone(), x.size)).collect(),
            gate: gate.to_string(),
            pack: pack.summary(),
            embed: built.summary(),
            functions: shipped.functions,
        }
    }

    pub fn text(&self) -> String {
        let kb = |n: usize| format!("{:.1} KB", n as f64 / 1024.0);
        let mut out = format!("wasm: {} raw (shipped; the host compresses it), {} brotli 11\n", kb(self.wasm_raw), kb(self.wasm_br));
        for driver in &self.training_drivers {
            out.push_str(&format!("training driver {}: {} query={}\n", driver.name, driver.window, driver.query));
        }
        for p in &self.parts {
            let br = p.br.map(kb).unwrap_or_else(|| "-".into());
            out.push_str(&format!("  {:<20} {:>12} raw {:>12} br\n", p.name, kb(p.raw), br));
        }
        for (name, size) in &self.external {
            out.push_str(&format!("beside the page: {name} {}\n", kb(*size as usize)));
        }
        for f in &self.files {
            out.push_str(&format!("wrote {} ({})\n", f.name, kb(f.bytes)));
        }
        out.push_str(&format!("pack: {}\nembed: {}\ngate: {}\n{} functions traced in the shipped run\n", self.pack, self.embed, self.gate, self.functions));
        out
    }

    pub fn to_json(&self) -> String {
        let n = |v: usize| Value::Int(v as i64);
        let parts = self.parts.iter().map(|p| json::obj(vec![("name", json::s(&p.name)), ("raw", n(p.raw)), ("br", p.br.map(n).unwrap_or(Value::Null))])).collect();
        let files = self.files.iter().map(|f| json::obj(vec![("name", json::s(&f.name)), ("bytes", n(f.bytes))])).collect();
        let external = self.external.iter().map(|(name, size)| json::obj(vec![("name", json::s(name)), ("bytes", n(*size as usize))])).collect();
        json::obj(vec![
            ("training_drivers", drivers_json(&self.training_drivers)),
            ("out_dir", json::s(&self.out_dir)),
            ("wasm", json::obj(vec![("raw", n(self.wasm_raw)), ("br", n(self.wasm_br))])),
            ("parts", Value::Arr(parts)),
            ("files", Value::Arr(files)),
            ("external", Value::Arr(external)),
            ("gate", json::s(&self.gate)),
            ("pack", json::s(&self.pack)),
            ("embed", json::s(&self.embed)),
            ("functions", n(self.functions)),
        ])
        .to_json()
    }
}

pub(crate) fn drivers_json(drivers: &[Driver]) -> Value {
    Value::Arr(drivers.iter().map(|d| json::obj(vec![
        ("name", json::s(&d.name)), ("query", json::s(&d.query)), ("window", json::s(&d.window)),
        ("environment", Value::Arr(d.environment.args().iter().map(|s| json::s(s)).collect())),
        ("script", d.script.as_ref().map(|p| json::s(&p.display().to_string())).unwrap_or(Value::Null)),
        ("until", d.until.as_ref().map(|s| json::s(s)).unwrap_or(Value::Null)),
        ("ms", d.ms.map(|ms| json::s(&ms.to_string())).unwrap_or(Value::Null)),
        ("stress", Value::Bool(d.stress)),
    ])).collect())
}
