//! Independent validation and decoding of a printer archive.

use crate::format;
use makepad_strict_json::Value;
use std::collections::HashMap;
use std::fmt;

mod docs;
mod runs;
mod scene;
mod vector;
mod zip;
#[cfg(test)]
mod tests;

pub use runs::decode_runs;
pub use scene::read_scene;
pub use vector::read_vector;

/// The check that rejected an archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    Zip, Deflated, Crc, Hash, Entries, LayerEntries, Json, Resin, ScreenSize,
    Preview, SceneSize, SceneMarkers, SceneCount, SceneHeights, SceneBounds,
    TableCount, TableIndex, TableHeights, TableExposure, TableLift, Runs, LitArea,
    VectorSize, VectorMarkers, VectorCapacity, SegmentOrder, Closed, Rounding,
    Winding, VectorArea, VectorContours, VectorBounds, Volume, Cost,
}

/// A failed check and its observed and expected values.
#[derive(Clone, Debug)]
pub struct Fault {
    pub rule: Rule,
    pub detail: String,
}
impl Fault {
    fn new(rule: Rule, detail: impl Into<String>) -> Self {
        Self { rule, detail: detail.into() }
    }
    fn at(mut self, place: &str) -> Self {
        self.detail = format!("{place}: {}", self.detail);
        self
    }
}
impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.rule, self.detail)
    }
}
impl std::error::Error for Fault {}

/// Screen dimensions and pitches in millimetres.
#[derive(Clone, Debug)]
pub struct Screen {
    pub pixels: [u32; 2],
    pub pitch: [f64; 2],
    pub size: [f64; 3],
}
impl Screen {
    fn pixels_len(&self) -> Result<usize, Fault> {
        (self.pixels[0] as usize).checked_mul(self.pixels[1] as usize)
            .ok_or_else(|| Fault::new(Rule::ScreenSize, "pixel count exceeds address space"))
    }
    fn half_extents(&self) -> [f64; 2] {
        [self.pixels[0] as f64 * self.pitch[0] / 2.0,
         self.pixels[1] as f64 * self.pitch[1] / 2.0]
    }
}

/// Job settings in millimetres, seconds and millimetres per second.
#[derive(Clone, Debug)]
pub struct Job {
    pub layer_height: f64,
    pub exposure: f64,
    pub bottom_exposure: f64,
    pub bottom_layers: u32,
    pub light_off: f64,
    pub lift_height: f64,
    pub lift_speed: f64,
    pub retract_speed: f64,
    pub anti_alias: u32,
}

/// The representation used by every layer in an archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding { Raster, Vector }
impl Encoding {
    fn extension(self) -> &'static str {
        match self {
            Self::Raster => format::RASTER_EXTENSION,
            Self::Vector => format::VECTOR_EXTENSION,
        }
    }
}

/// The scene's writer, parts' box and slicing planes.
#[derive(Clone, Debug)]
pub struct Scene {
    pub writer: String,
    pub bounds: [f32; 6],
    pub records: Vec<SceneRecord>,
}

/// One slicing plane, with its area, centred box and contour count.
#[derive(Clone, Debug)]
pub struct SceneRecord {
    pub z: f32,
    pub area: f32,
    pub bounds: [f32; 4],
    pub contours: u32,
}

/// One layer's exposure and motion settings.
#[derive(Clone, Debug)]
pub struct TableRow {
    pub exposure: f64,
    pub min_height: f64,
    pub thickness: f64,
    pub lift_height: f64,
    pub lift_speed: f64,
}

/// The estimated cost, duration and resin volume.
#[derive(Clone, Debug)]
pub struct Totals {
    pub cost: f64,
    pub currency: String,
    pub print_seconds: u64,
    pub volume_ml: f64,
}

/// The software identification recorded by the writer.
#[derive(Clone, Debug)]
pub struct Software {
    pub mark: String,
    pub opengl: String,
    pub os: String,
    pub version: String,
}

/// An edge as stored; a swapped edge runs from `to` to `from` on its contour.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub from: [f32; 2],
    pub to: [f32; 2],
    pub swapped: bool,
}

/// A validated archive retaining compressed entries for decoding on demand.
#[derive(Debug)]
pub struct Archive {
    file: Vec<u8>,
    places: Vec<zip::Place>,
    lookup: HashMap<String, usize>,
    layer_places: Vec<usize>,
    capacity: u32,
    pub names: Vec<String>,
    pub screen: Screen,
    pub job: Job,
    pub resin: String,
    pub encoding: Encoding,
    pub layers: u32,
    pub scene: Scene,
    pub table: Vec<TableRow>,
    pub totals: Totals,
    pub software: Software,
    pub settings: Value,
}
impl Archive {
    /// Inflate an entry by name.
    pub fn entry(&self, name: &str) -> Option<Vec<u8>> {
        self.places.get(*self.lookup.get(name)?)?.inflate(&self.file).ok()
    }

    /// Decode a raster layer to levels 0 through 15, with row zero first.
    pub fn raster(&self, k: u32) -> Result<Vec<u8>, Fault> {
        let place = self.layer(k, Encoding::Raster)?;
        decode_runs(&place.inflate(&self.file)?, self.screen.pixels_len()?)
            .map_err(|e| Fault::new(Rule::Runs, e).at(&place.name))
    }

    /// Decode a vector layer to its stored edges.
    pub fn segments(&self, k: u32) -> Result<Vec<Segment>, Fault> {
        let place = self.layer(k, Encoding::Vector)?;
        read_vector(&place.inflate(&self.file)?, self.capacity, &self.scene.records[k as usize])
            .map_err(|e| e.at(&place.name))
    }

    fn layer(&self, k: u32, encoding: Encoding) -> Result<&zip::Place, Fault> {
        if self.encoding != encoding || k >= self.layers {
            return Err(Fault::new(Rule::LayerEntries, format!(
                "layer {k}, requested {encoding:?}; archive has {} {:?} layers", self.layers, self.encoding)));
        }
        Ok(&self.places[self.layer_places[k as usize]])
    }
}

/// Validate an entire archive in rule order and retain its compressed contents.
pub fn read(file: Vec<u8>) -> Result<Archive, Fault> {
    let places = zip::parse(&file)?;
    parallel(places.len(), |i| places[i].verify(&file))?;
    let lookup: HashMap<_, _> = places.iter().enumerate().map(|(i, p)| (p.name.clone(), i)).collect();
    for name in format::LEADING.into_iter().chain([format::SCENE]) {
        if !lookup.contains_key(name) {
            return Err(Fault::new(Rule::Entries, format!("missing {name}")));
        }
    }
    let (encoding, layer_places) = layer_entries(&places, &lookup)?;
    let layers = u32::try_from(layer_places.len())
        .map_err(|_| Fault::new(Rule::LayerEntries, "layer count exceeds u32"))?;
    let entry = |name: &str| places[lookup[name]].inflate(&file);
    let settings = docs::parse(&entry(format::SETTINGS)?, format::SETTINGS)?;
    let table_doc = docs::parse(&entry(format::LAYER_TABLE)?, format::LAYER_TABLE)?;
    let totals_doc = docs::parse(&entry(format::PRINT_INFO)?, format::PRINT_INFO)?;
    let software_doc = docs::parse(&entry(format::SOFTWARE)?, format::SOFTWARE)?;
    let config = docs::settings(&settings).map_err(|e| e.at(format::SETTINGS))?;
    let totals = docs::totals(&totals_doc).map_err(|e| e.at(format::PRINT_INFO))?;
    let software = docs::software(&software_doc).map_err(|e| e.at(format::SOFTWARE))?;
    let table = docs::table_rows(&table_doc).map_err(|e| e.at(format::LAYER_TABLE))?;
    docs::screen_size(&config.screen).map_err(|e| e.at(format::SETTINGS))?;
    for (i, name) in [format::PREVIEW_SMALL, format::PREVIEW_LARGE].into_iter().enumerate() {
        docs::preview(&entry(name)?, config.previews[i]).map_err(|e| e.at(name))?;
    }
    let scene = read_scene(&entry(format::SCENE)?, layers, config.job.layer_height, config.screen.half_extents())
        .map_err(|e| e.at(format::SCENE))?;
    docs::check_table(&table_doc, &table, layers, &config.job).map_err(|e| e.at(format::LAYER_TABLE))?;
    let pixels = config.screen.pixels_len()?;
    parallel(layer_places.len(), |k| {
        let place = &places[layer_places[k]];
        let stream = place.inflate(&file)?;
        let result = match encoding {
            Encoding::Raster => runs::check_area(&stream, pixels, config.screen.pitch, scene.records[k].area),
            Encoding::Vector => read_vector(&stream, config.capacity, &scene.records[k]).map(|_| ()),
        };
        result.map_err(|e| e.at(&place.name))
    })?;
    let volume = scene.records.iter().map(|r| r.area as f64).sum::<f64>() * config.job.layer_height / 1000.0;
    if !near(totals.volume_ml, volume, volume.abs() * 0.01) {
        return Err(Fault::new(Rule::Volume, format!("{}: volume {}, expected {volume} ml", format::PRINT_INFO, totals.volume_ml)));
    }
    let cost = totals.volume_ml * config.price / config.bottle;
    if !near(totals.cost, cost, cost.abs() * 0.01 + 0.01) {
        return Err(Fault::new(Rule::Cost, format!("{}: cost {}, expected {cost}", format::PRINT_INFO, totals.cost)));
    }
    Ok(Archive {
        names: places.iter().map(|p| p.name.clone()).collect(), file, places, lookup, layer_places,
        capacity: config.capacity, screen: config.screen, job: config.job, resin: config.resin,
        encoding, layers, scene, table, totals, software, settings,
    })
}

fn layer_entries(places: &[zip::Place], lookup: &HashMap<String, usize>) -> Result<(Encoding, Vec<usize>), Fault> {
    let mut encoding = None;
    let mut count = 0;
    for p in places {
        let name = p.name.as_str();
        if format::LEADING.contains(&name) || name == format::SCENE || format::OPTIONAL.contains(&name) {
            continue;
        }
        let Some(suffix) = name.strip_prefix(format::LAYER_PREFIX) else {
            return Err(Fault::new(Rule::Entries, format!("unexpected entry {name}")));
        };
        let (digits, kind) = if let Some(d) = suffix.strip_suffix(format::RASTER_EXTENSION) {
            (d, Encoding::Raster)
        } else if let Some(d) = suffix.strip_suffix(format::VECTOR_EXTENSION) {
            (d, Encoding::Vector)
        } else {
            return Err(Fault::new(Rule::LayerEntries, format!("{name}: unknown layer extension")));
        };
        let k = digits.parse::<u32>().map_err(|_| Fault::new(Rule::LayerEntries, format!("{name}: invalid layer index")))?;
        if name != format::layer_entry(k, kind.extension()) || encoding.is_some_and(|e| e != kind) {
            return Err(Fault::new(Rule::LayerEntries, format!("{name}: noncanonical index or mixed encoding")));
        }
        encoding = Some(kind);
        count += 1;
    }
    let encoding = encoding.ok_or_else(|| Fault::new(Rule::LayerEntries, "no layer entries or encoding"))?;
    let indices = (0..count).map(|k| {
        let name = format::layer_entry(k, encoding.extension());
        lookup.get(&name).copied().ok_or_else(|| Fault::new(Rule::LayerEntries, format!("missing {name}")))
    }).collect::<Result<_, _>>()?;
    Ok((encoding, indices))
}

fn parallel<F>(len: usize, check: F) -> Result<(), Fault>
where F: Fn(usize) -> Result<(), Fault> + Sync {
    if len == 0 { return Ok(()); }
    let workers = std::thread::available_parallelism().map_or(1, usize::from).min(len);
    let chunk = len.div_ceil(workers);
    std::thread::scope(|scope| {
        let check = &check;
        let handles: Vec<_> = (0..len).step_by(chunk).map(|start| {
            scope.spawn(move || {
                for i in start..(start + chunk).min(len) { check(i)?; }
                Ok(())
            })
        }).collect();
        let mut first = Ok(());
        for handle in handles {
            let result = handle.join().expect("archive validation worker panicked");
            if first.is_ok() { first = result; }
        }
        first
    })
}

fn near(found: f64, expected: f64, tolerance: f64) -> bool {
    found.is_finite() && expected.is_finite() && (found - expected).abs() <= tolerance
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn f32_at(bytes: &[u8], at: usize) -> f32 { f32::from_bits(u32_at(bytes, at)) }
