//! The cloud layers drawn together (design §2.1, §2.2 and §3.3): the gates, the built entries in the per-Env order,
//! their light (cloud_light.rs's pass), and the compositor.
//!
//! - **Order.** Built entries are kept in ascending height (where each stands, `stand_height_m`), ties by layer index:
//!   index 0 is the front for upward rays. The compositor, the light pass and t_above walk it. A Nimbostratus' scud
//!   is composited at its own height (its own step of the walk), so a layer between the scud and its slab shows
//!   between them.
//! - **Compositing** (design §2.2): sky x T + discs x Tu + L, front to back. T falls by each entry's faded alpha, Tu by
//!   its unfaded cover (a classic volume's faded alpha, as today). The walk stops once T is under EARLY_OUT_T, except
//!   on a texel of a disc (D1), which goes on for Tu alone, and on a stack of classic entries alone, which keeps
//!   today's couplings.
//! - **The marches** (design §2.2's splitting, §4.2): every marched entry (the first classic layer with Volume on, and
//!   every cloud volume: a modern Cumulus) is cut at every flat entry inside its shell, so such a flat entry is drawn
//!   in its place; the classic one on its own step grid and jitter (`VolumetricClouds::march_split`), a volume by its
//!   own march (`CloudVolume::march`). Each is marched once per ray, at its first segment of the walk, and every step
//!   of the walk is placed at the near distance where it begins (its height: an upward ray meets heights in order).
//!   Overlapping shells do not march jointly yet: their segments composite one behind the other by where they begin.
//! - **A volume's light** (design §12.1 item 9): its march keeps its light apart from the colours (`VolumeMarch`), and
//!   at the held and released widths (RECORD_WIDTHS) the march of each texel is kept in a light record shared across
//!   maps, so a drag of Silver lining, Brightness, Sky light, the sun's colour or the haze recombines it
//!   (`cloud_volume_light::shade`) without a march. Refinement and export widths march every texel.
//! - Nothing here allocates per texel: the segments live in arrays of EVENTS, then in reused per-thread spills.

use makepad_draw::*;
use crate::cloud_classic::{downwind, stand_height_m, ClassicEntry, ClassicSpec};
use crate::cloud_sheets::{horizon_fade, Sheet};
use crate::cloud_slabs::{Slab, SlabSeen};
use crate::cloud_light::{self, EntryLight, LightInputs, LightRows, LitEntry};
pub use crate::cloud_light::{CoverSample, Shape};
use crate::cloud_volume_cumulus::{CloudVolume, LightRecord, VolumeSpec};
use crate::cloud_placed::{PlacedSpec, PlacedVolume};
use crate::cloud_volume_gpu::{AskFor, CloudAsk, GpuMode, GpuRecord, VolumeRecords, VolumeUpload, MAX_ROWS, PASS_SEGMENTS};
use crate::cloud_volume_light::{shade, ShadeInputs, VolumeMarch};
use crate::clouds::CloudSample;
use crate::caster::RowRunner;
use crate::volumetric::{self, height_at, CloudCache, CloudMarch, VolumetricClouds};
use crate::{cloud_display_name, seeded_wind_deg, CloudGenus, CloudLayerParams, SkyParams};
use std::sync::{Arc, Mutex, OnceLock};

/// Below this the light still to come counts for nothing and the walk stops (design §2.2).
pub const EARLY_OUT_T: f32 = 0.002;
/// Segments of the march the compositor keeps on its stack; more go to the per-thread spill.
const EVENTS: usize = 16;
/// The bake widths a cloud volume's light record is kept at (design §12.1 item 9): the held and the released preview.
/// Any bake of such a width reads it (an export or AA pass too); none is kept at a wider one (a record is 36 bytes a
/// texel and segment: 670 MB at 8192).
const RECORD_WIDTHS: [usize; 2] = [512, 1024];
/// A lookup along one exact direction (no texel) marches a volume as a bake this wide does: the released preview.
const EXACT_WIDTH: usize = 1024;
/// The stack's visibility (design §2.3's haze on a volume's light): this over the atmosphere's Haze.
const VISIBILITY_AT_HAZE_1_M: f32 = 60_000.0;
/// One bake's stack cache stays under this many MB; past it the cache marches at FALLBACK_CACHE_WIDTH (design §7).
const CACHE_BUDGET_MB: f64 = 256.0;
const FALLBACK_CACHE_WIDTH: usize = 1024;

/// The marched entry's segments, bakes wider than MARCH_MAX_WIDTH read (one record per segment a texel).
pub type StackCache = CloudCache;

/// What one lookup hands the compositor: whether it is a held bake's (the shadows between layers then take fewer
/// octaves), the bake's cache, and which GPU records its cloud volumes read (an antialiasing bake's own, marched at its
/// jittered directions: design §12.1 item 10).
pub struct StackLook<'a> {
    pub held: bool,
    pub cache: Option<&'a StackCache>,
    pub gpu: VolumeRecords<'a>,
}

/// How an entry is drawn (design §2.1's classes).
pub enum EntryClass {
    /// A lifted layer, drawn by today's code from its parts (cloud_classic.rs).
    Classic(ClassicEntry),
    /// A modern Cirrus, Stratocumulus or Cumulus (cloud_sheets.rs).
    Sheet(Sheet),
    /// A modern Stratus (cloud_slabs.rs).
    Slab(Slab),
    /// A modern Cumulus, Stratus, Altocumulus, Stratocumulus or Nimbostratus: a marched cloud volume
    /// (cloud_volume_cumulus.rs and its types' arms, cloud_volume_nimbostratus.rs, design §2 and §4.1).
    Volume(CloudVolume),
    /// A Cumulonimbus or a Lenticular: a placed volume (cloud_placed.rs, design §2.1's placed and lens classes),
    /// marched where a ray meets its bounds; its march's segments walk as a volume's.
    Placed(PlacedVolume),
}

/// One built layer.
pub struct CloudEntry {
    layer: usize,
    genus: CloudGenus,
    class: EntryClass,
    classic: bool,
    height_m: f32,
    base_m: f32,
    light_height_m: f32,
    /// A slab's scud, drawn at this height in its own step of the walk.
    scud_m: Option<f32>,
    brightness: f32,
    sky_light: f32,
    lining: f32,
    shade_below: f32,
    presence: f32,
    classic_amount: Option<f32>,
    marched: bool,
    /// A higher entry can shade it (t_above): any modern flat one or modern cloud volume, or a classic flat one above a
    /// modern one (design §2.3: two classic entries never pair, as today's layers never shade each other).
    shaded_from_above: bool,
}

/// One step of the front-to-back walk of an upward ray.
#[derive(Clone, Copy, Debug)]
enum Step {
    Flat(usize),
    /// Segment k of the classic march.
    Segment(usize),
    /// The scud of the slab at this index, at its own height.
    Scud(usize),
    /// Segment k (the second field) of the cloud volume `volumes[v]` (the first).
    Volume(usize, usize),
    /// Segment k of the placed volume `volumes[v]`.
    Placed(usize, usize),
}

/// A cloud volume of the stack: its entry, the heights its march is cut at (the flat entries inside its shell,
/// ascending), its first record among the walk's volume records, and its light records at RECORD_WIDTHS, looked up
/// when a bake of that width first reads them.
struct VolumeSlot {
    entry: usize,
    cuts: Vec<f32>,
    first: usize,
    records: [OnceLock<Arc<LightRecord>>; 2],
    /// Its march at an export's width from the GPU (design §12.3), when the app attached one.
    gpu: Option<Arc<GpuRecord>>,
}

/// What a flat entry saw along a ray: its shape, and for a classic entry today's sample (its self-shadow and the sun's
/// cosine), which its own shading reads.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    shape: Shape,
    classic: CloudSample,
    slab: SlabSeen,
}

/// The built cloud layers of one Env.
pub struct CloudStack {
    entries: Vec<CloudEntry>,
    lights: Vec<EntryLight>,
    plan: Vec<Step>,
    /// The classic marched entry, and the heights its march is cut at (the flat entries inside its shell, ascending).
    marched: Option<usize>,
    cuts: Vec<f32>,
    /// The cloud volumes, and how many records their segments take in all.
    volumes: Vec<VolumeSlot>,
    records: usize,
    all_classic: bool,
    sun_dir: Vec3f,
    /// The distance over which the air turns a volume's light to the sky behind (design §2.3), metres.
    visibility_m: f32,
    /// What the whole stack passes along each of the light pass's dome looks (`crate::dome_look`, DOME_ROWS x
    /// DOME_COLUMNS), through its entries' stand-ins: the sky a cloud volume's distance haze turns toward is the sky seen
    /// through the stack there (`seen_through`). Empty without a Nimbostratus volume, the one type whose haze reads it.
    seen: Vec<f32>,
}

/// What a layer draws (design §3.2, the master Cover): a classic layer's amount presence x classic.coverage, a modern
/// one's master x shown_cover; 0 for a hidden layer and a type this build does not draw.
fn effective_cover(layer: &CloudLayerParams, master: f32) -> f32 {
    if !layer.enabled || !layer.genus().is_some_and(|genus| genus.is_built()) {
        return 0.0;
    }
    match &layer.classic {
        Some(look) => master * look.coverage,
        None => master * layer.shown_cover(),
    }
}

/// The layer's own type block asks for a volume (the Volume row of a Stratocumulus, Nimbostratus or Cumulus).
fn volume_on(layer: &CloudLayerParams) -> bool {
    match layer.genus() {
        Some(CloudGenus::Stratocumulus) => layer.stratocumulus_params().volume,
        Some(CloudGenus::Nimbostratus) => layer.nimbostratus_params().volume,
        Some(CloudGenus::Cumulus) => layer.cumulus_params().volume,
        _ => false,
    }
}

static KEPT: Mutex<Vec<String>> = Mutex::new(Vec::new());
static FLAT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Logs `line` the first time this process meets `key` in `said`; a poisoned lock logs every time.
fn say_once(said: &Mutex<Vec<String>>, key: &str, line: impl FnOnce() -> String) {
    let first = match said.lock() {
        Ok(mut keys) => {
            let first = !keys.iter().any(|k| k == key);
            if first {
                keys.push(key.to_string());
            }
            first
        }
        Err(_) => true,
    };
    if first {
        log!("{}", line());
    }
}

/// The walk of an upward ray (design §2.2, §4.2), each step placed at the near distance where it begins, which on an
/// upward ray is its height: the flat steps (the flat entries and the scuds, each at its height, a tie keeping the
/// entries first) and the segments of every marched shell, each cut at the flat steps inside it, a segment at the
/// height it begins (its base, or its cut). A flat step and a segment that begin at one height take the flat step
/// first, as the classic march's cuts always have. Returns the plan, the classic march's cuts and the volumes.
fn plan_of(entries: &[CloudEntry], marched: Option<usize>) -> (Vec<Step>, Vec<f32>, Vec<VolumeSlot>) {
    let shell = |i: usize| match &entries[i].class {
        EntryClass::Volume(volume) => Some(volume.shape().shell_m()),
        EntryClass::Placed(placed) => Some(placed.shell_m()),
        _ if Some(i) == marched => entries[i].volume().map(|volume| volume.shell_m()),
        _ => None,
    };
    let mut flats: Vec<(f32, Step)> = (0..entries.len()).filter(|&i| shell(i).is_none()).map(|i| (entries[i].height_m, Step::Flat(i))).collect();
    flats.extend((0..entries.len()).filter_map(|i| entries[i].scud_m.map(|h| (h, Step::Scud(i)))));
    flats.sort_by(|a, b| a.0.total_cmp(&b.0));
    // (where it begins, a segment, the step)
    let mut steps: Vec<(f32, bool, Step)> = flats.iter().map(|&(h, step)| (h, false, step)).collect();
    let (mut classic_cuts, mut volumes, mut records) = (Vec::new(), Vec::new(), 0usize);
    for m in 0..entries.len() {
        let Some((base, top)) = shell(m) else {
            continue;
        };
        let cuts: Vec<f32> = flats.iter().map(|&(h, _)| h).filter(|&h| h > base && h < top).collect();
        let placed = matches!(entries[m].class, EntryClass::Placed(_));
        let volume = placed || matches!(entries[m].class, EntryClass::Volume(_));
        for k in 0..=cuts.len() {
            let begins = if k == 0 { base } else { cuts[k - 1] };
            steps.push((begins, true, if placed { Step::Placed(volumes.len(), k) } else if volume { Step::Volume(volumes.len(), k) } else { Step::Segment(k) }));
        }
        if volume {
            let first = records;
            records += cuts.len() + 1;
            volumes.push(VolumeSlot { entry: m, cuts, first, records: [OnceLock::new(), OnceLock::new()], gpu: None });
        } else {
            classic_cuts = cuts;
        }
    }
    // Stable: equal heights keep the flat steps first, in their order, then the segments in the entries' order.
    steps.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    (steps.into_iter().map(|(_, _, step)| step).collect(), classic_cuts, volumes)
}

/// Runs `f` with room for `n` records of type `T` (each `clear`): EVENTS on the stack, more in the per-thread `spill`,
/// which is reused and grows (logged), so the compositor allocates nothing per texel and no count of segments fails
/// (design §2.2).
fn with_room<T: Copy, R>(n: usize, clear: T, spill: &'static std::thread::LocalKey<std::cell::RefCell<Vec<T>>>, f: impl FnOnce(&mut [T]) -> R) -> R {
    if n <= EVENTS {
        let mut local = [clear; EVENTS];
        return f(&mut local[..n]);
    }
    spill.with(|spill| {
        let mut spill = spill.borrow_mut();
        if spill.len() < n {
            log!("hdri clouds: the compositor's spill grows to {n} segments on this thread");
            spill.resize(n, clear);
        }
        f(&mut spill[..n])
    })
}

thread_local! {
    static SEGMENT_SPILL: std::cell::RefCell<Vec<CloudMarch>> = const { std::cell::RefCell::new(Vec::new()) };
    static RECORD_SPILL: std::cell::RefCell<Vec<VolumeMarch>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Runs `f` with room for `n` segments of the classic march ([`with_room`]).
fn with_segments<R>(n: usize, f: impl FnOnce(&mut [CloudMarch]) -> R) -> R {
    with_room(n, CloudMarch::default(), &SEGMENT_SPILL, f)
}

/// Runs `f` with room for `n` records of the cloud volumes' segments ([`with_room`]).
fn with_records<R>(n: usize, f: impl FnOnce(&mut [VolumeMarch]) -> R) -> R {
    with_room(n, VolumeMarch::CLEAR, &RECORD_SPILL, f)
}

impl CloudStack {
    /// The stack of `sky.cloud_stack`, built after the night (lib.rs) with its light. Ok(None) for gate 1 (design
    /// §3.3): no stack, a master Cover <= 0, or no layer with an effective cover, decided before any noise, table or
    /// grid exists; also when every entry gate 2 lets through drew nothing. Gate 2 skips a hidden layer, a type this
    /// build does not draw (logged once a genus, kept in the file), and an amount of 0. Err(Cancelled) when `cancelled`
    /// stopped a build whose rows `run` spreads (D15): a classic volume's first noise, a cloud volume's footprint grid,
    /// noise or light grids.
    pub fn build(sky: &SkyParams, map_seed: u32, light: &LightInputs, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<CloudStack>, volumetric::Cancelled> {
        let Some(params) = sky.cloud_stack.as_ref() else {
            return Ok(None);
        };
        let master = params.cover;
        if !(master > 0.0) || !params.layers.iter().any(|layer| effective_cover(layer, master) > 0.0) {
            return Ok(None);
        }
        let sun = light.sun_dir.normalize();
        let sun_dir = if sun.is_finite() && sun.length() > 0.5 { sun } else { vec3f(0.0, 1.0, 0.0) };
        // The first classic layer with Volume on, in the list's order, holds the one march: cloud_lift.rs's
        // marched_cloud_layer, the rule the app's rows and set_clouds read too.
        let march_slot = crate::marched_cloud_layer(params);
        let mut entries: Vec<CloudEntry> = Vec::new();
        for (i, layer) in params.layers.iter().enumerate() {
            if !layer.enabled {
                continue;
            }
            let Some(genus) = layer.genus().filter(|genus| genus.is_built()) else {
                say_once(&KEPT, &layer.genus, || format!("hdri clouds: {} kept, not drawn by this build ({})", layer.genus, cloud_display_name(&params.layers, i)));
                continue;
            };
            let amount = effective_cover(layer, master);
            if !(amount > 0.0) {
                continue;
            }
            let (class, light_height_m) = match &layer.classic {
                Some(look) => {
                    let marches = march_slot == Some(i);
                    if volume_on(layer) && !marches {
                        let name = cloud_display_name(&params.layers, i);
                        say_once(&FLAT, &name, || format!("hdri clouds: {name} drawn flat: one marched layer at a time until joint marching"));
                    }
                    let spec = ClassicSpec { layer, genus, look, map_seed, presence: master, marches, march_steps: params.march_steps, stack_wind_deg: params.wind_deg };
                    let Some(entry) = ClassicEntry::build(&spec, sun_dir, cancelled, run)? else {
                        continue;
                    };
                    // Lit where today lit it (design §1.2): a deck at its base, a volume at its middle, a cirrus at
                    // classic.light_m (the older deck's altitude).
                    let light_height_m = match &entry {
                        ClassicEntry::Cirrus(_) => look.light_m,
                        ClassicEntry::Volume { volume, .. } => {
                            let (base, top) = volume.shell_m();
                            0.5 * (base + top)
                        }
                        ClassicEntry::Deck(_) => layer.base_m(),
                    };
                    (EntryClass::Classic(entry), light_height_m)
                }
                // A modern layer, lit at the height it stands; its wind is its own, else the Clouds wind, else the map
                // seed's angle (seeded_wind_deg, which the Clouds entry shows).
                None => {
                    let height_m = stand_height_m(layer, genus);
                    let deg = layer.wind_deg.or(params.wind_deg).unwrap_or_else(|| seeded_wind_deg(map_seed));
                    let class = match genus {
                        // A Cumulus, a Stratus, an Altocumulus (its deck of cloudlets), a Stratocumulus and a Nimbostratus
                        // are marched volumes (design §2; cloud_volume_nimbostratus.rs). Their light grids are built whole
                        // whatever the bake: a held bake reads the grids a released one reads (design §12.1, held shows the
                        // same clouds).
                        CloudGenus::Cumulus | CloudGenus::Stratus | CloudGenus::Altocumulus | CloudGenus::Stratocumulus | CloudGenus::Nimbostratus => {
                            let spec = VolumeSpec { layer, genus, cover: amount, map_seed, downwind: downwind(deg), sun_dir, march_steps: params.march_steps };
                            CloudVolume::build(&spec, false, cancelled, run)?.map(EntryClass::Volume)
                        }
                        // A Cumulonimbus or a Lenticular stands where its rows place it (design §2.1's placed and lens
                        // classes).
                        CloudGenus::Cumulonimbus | CloudGenus::Lenticular => {
                            let spec = PlacedSpec { layer, genus, cover: amount, map_seed, downwind: downwind(deg), sun_dir };
                            PlacedVolume::build(&spec, cancelled, run)?.map(EntryClass::Placed)
                        }
                        _ => Sheet::build(layer, genus, amount, map_seed, downwind(deg), sun_dir, height_m).map(EntryClass::Sheet),
                    };
                    let Some(class) = class else {
                        continue;
                    };
                    (class, height_m)
                }
            };
            let marched = matches!(&class, EntryClass::Classic(ClassicEntry::Volume { .. }) | EntryClass::Volume(_) | EntryClass::Placed(_));
            let scud_m = match &class {
                EntryClass::Slab(slab) => slab.scud_height_m(),
                _ => None,
            };
            entries.push(CloudEntry {
                layer: i,
                genus,
                classic: layer.classic.is_some(),
                height_m: stand_height_m(layer, genus),
                base_m: layer.base_m(),
                light_height_m,
                scud_m,
                brightness: layer.brightness.unwrap_or(1.0),
                sky_light: layer.sky_light.unwrap_or(1.0),
                lining: layer.lining.unwrap_or(genus.default_lining()),
                shade_below: layer.shade_below.unwrap_or(genus.default_shade_below()),
                presence: master,
                classic_amount: match &class {
                    EntryClass::Classic(ClassicEntry::Cirrus(_)) => None,
                    EntryClass::Classic(_) => Some(amount),
                    EntryClass::Sheet(_) | EntryClass::Slab(_) | EntryClass::Volume(_) | EntryClass::Placed(_) => None,
                },
                marched,
                shaded_from_above: false,
                class,
            });
        }
        if entries.is_empty() {
            return Ok(None);
        }
        entries.sort_by(|a, b| a.height_m.total_cmp(&b.height_m).then(a.layer.cmp(&b.layer)));
        for i in 0..entries.len() {
            let shaded = match &entries[i].class {
                // A volume is shaded by every flat entry standing over its base (`cloud_light::t_above_volume`).
                EntryClass::Volume(volume) => {
                    let base_m = volume.shape().shell_m().0;
                    entries.iter().any(|other| !other.marched && other.height_m > base_m)
                }
                EntryClass::Placed(placed) => {
                    let base_m = placed.light_shell_m().0;
                    entries.iter().any(|other| !other.marched && other.height_m > base_m)
                }
                // A flat entry by every entry over it that casts (`cloud_light::t_above`): a flat one, or a modern cloud
                // volume (not yet a placed one); never the classic volume, and two classic entries never pair.
                _ => {
                    !entries[i].marched
                        && entries[i + 1..].iter().any(|above| !matches!(above.class, EntryClass::Placed(_)) && !(above.classic && (above.marched || entries[i].classic)))
                }
            };
            entries[i].shaded_from_above = shaded;
        }
        let marched = entries.iter().position(|entry| matches!(&entry.class, EntryClass::Classic(ClassicEntry::Volume { .. })));
        let (plan, cuts, volumes) = plan_of(&entries, marched);
        let records = volumes.iter().map(|v| v.cuts.len() + 1).sum();
        let all_classic = entries.iter().all(|entry| entry.classic);
        let lights = cloud_light::light_pass(&entries, light);
        let haze = sky.atmosphere.haze;
        let visibility_m = if haze > 0.0 { VISIBILITY_AT_HAZE_1_M / haze } else { f32::INFINITY };
        // (read only by a Nimbostratus' haze: a stack without one keeps no table)
        let seen = if !entries.iter().any(|entry| entry.genus == CloudGenus::Nimbostratus && entry.marched) {
            Vec::new()
        } else {
            (0..crate::DOME_ROWS * crate::DOME_COLUMNS)
                .map(|i| {
                    let d = crate::dome_look(i / crate::DOME_COLUMNS, i % crate::DOME_COLUMNS).0;
                    entries.iter().fold(1.0f32, |t, entry| t * (1.0 - entry.stand_in(d).cover.clamp(0.0, 1.0)))
                })
                .collect()
        };
        #[cfg(test)]
        count_entries(entries.len());
        Ok(Some(CloudStack { entries, lights, plan, marched, cuts, volumes, records, all_classic, sun_dir, visibility_m, seen }))
    }

    /// sky x T + discs x Tu + L along the unit direction `d` (design §2.2): T falls by each entry's faded alpha, Tu by
    /// its unfaded cover. d.y <= 0 gives sky + discs; so does a texel where no entry drew, with its bits (gate 3). It
    /// never allocates. A lookup along one exact direction: a cloud volume marches as a released bake does.
    pub fn composite(&self, d: Vec3f, sky: Vec3f, discs: Vec3f, look: &StackLook) -> Vec3f {
        self.composite_at(d, sky, discs, look, 0.0)
    }

    /// [`Self::composite`] for a lookup of a bake whose texels are `texel` radians wide (0: one exact direction). A
    /// cloud volume marches at that bake's width (its jitter and its detail follow the texel), and at RECORD_WIDTHS
    /// reads the light record of the texel holding `d`.
    pub fn composite_at(&self, d: Vec3f, sky: Vec3f, discs: Vec3f, look: &StackLook, texel: f32) -> Vec3f {
        if !(d.y > 0.0) {
            return sky + discs;
        }
        let disc_texel = discs.max_elem() > 0.0;
        let walk = |segments: &mut [CloudMarch]| match self.records {
            0 => self.walk(d, sky, look, texel, disc_texel, segments, &mut []),
            n => with_records(n, |records| self.walk(d, sky, look, texel, disc_texel, segments, records)),
        };
        let (l, t, tu) = match self.marched {
            Some(_) => with_segments(self.cuts.len() + 1, walk),
            None => walk(&mut []),
        };
        if t == 1.0 && tu == 1.0 && l == Vec3f::default() {
            return sky + discs;
        }
        sky * t + discs * tu + l
    }

    /// The width a lookup of a bake whose texels are `texel` radians wide marches a cloud volume at (EXACT_WIDTH for an
    /// exact lookup).
    fn march_width(texel: f32) -> usize {
        if texel > 0.0 {
            (std::f32::consts::TAU / texel).round() as usize
        } else {
            EXACT_WIDTH
        }
    }

    /// The march of the cloud volume `slot` along `d` into `out` (its records, one a segment) for a lookup of a bake
    /// whose texels are `texel` radians wide: the GPU's record `gpu` at that width (the lookup's choice, `StackLook::gpu`),
    /// else the light record of the texel holding `d` at RECORD_WIDTHS, else its own march at that width (at EXACT_WIDTH
    /// for an exact lookup). A lookup that draws a disc takes the same record, so its cloud is the map's; what its disc
    /// passes is its own (`walk`).
    fn march_volume(&self, slot: &VolumeSlot, gpu: Option<&Arc<GpuRecord>>, volume: &CloudVolume, d: Vec3f, texel: f32, out: &mut [VolumeMarch]) {
        // A Stratus' far band (design §6.3, for a layer): a look under its lowest marched one reads the march of the look
        // mirrored over it, at the layer's true distance along the look (the haze takes it there), so the layer reaches
        // the horizon.
        let far = if self.entries[slot.entry].genus == CloudGenus::Stratus { volume.shape().far_band(d) } else { None };
        let Some((raised, depth_m)) = far else {
            return self.march_volume_at(slot, gpu, volume, d, texel, out);
        };
        self.march_volume_at(slot, gpu, volume, raised, texel, out);
        for m in out.iter_mut().filter(|m| m.weight > 0.0) {
            m.depth_weighted = depth_m * m.weight;
        }
    }

    /// `march_volume` along `d` as it is.
    fn march_volume_at(&self, slot: &VolumeSlot, gpu: Option<&Arc<GpuRecord>>, volume: &CloudVolume, d: Vec3f, texel: f32, out: &mut [VolumeMarch]) {
        let width = Self::march_width(texel);
        // The GPU's record at an export's width (design §12.3): its texel's entry, read as a march.
        if let Some(record) = gpu.filter(|r| texel > 0.0 && r.width() == width) {
            if record.read(d, out) {
                return;
            }
        }
        if let Some(k) = RECORD_WIDTHS.iter().position(|&w| w == width).filter(|_| texel > 0.0) {
            let record = slot.records[k].get_or_init(|| volume.light_record(width, &slot.cuts));
            if record.read(volume, d, out) {
                return;
            }
        }
        volume.march(d, width, &slot.cuts, out);
    }

    /// The march of the placed volume `slot` along `d` into `out`: the GPU's record `gpu` at that width (the lookup's
    /// choice, `StackLook::gpu`), else its own march at the lookup's width (no light record: its march is quick and only
    /// where a ray meets its bounds).
    fn march_placed(&self, slot: &VolumeSlot, gpu: Option<&Arc<GpuRecord>>, placed: &PlacedVolume, d: Vec3f, texel: f32, out: &mut [VolumeMarch]) {
        let width = Self::march_width(texel);
        if let Some(record) = gpu.filter(|r| texel > 0.0 && r.width() == width) {
            if record.read(d, out) {
                return;
            }
        }
        placed.march(d, width, &slot.cuts, out);
    }

    /// What the GPU marches for a map `width` texels wide (design §12.3): one upload per cloud volume, in `mode`, its
    /// lobes mixed at its entry's Silver lining, its texels' directions moved by `jitter` texels (an antialiasing bake's,
    /// design §12.1 item 10; 0 for the centres), the parts `resident` says the GPU holds going as their keys alone
    /// (design §12.11 item 4.6). Empty without a volume, when a volume is cut into more than PASS_SEGMENTS segments, when
    /// a part's buffer is past MAX_ROWS, and when a volume's upload could not be built (each logged): the map then marches
    /// on the CPU, all of it (design §12.11 item 4.5).
    pub fn gpu_asks(&self, width: usize, mode: GpuMode, jitter: [f32; 2], resident: &dyn Fn(u64) -> bool) -> Vec<VolumeUpload> {
        if !self.gpu_marches() {
            return Vec::new();
        }
        let mut uploads = Vec::with_capacity(self.volumes.len());
        for slot in &self.volumes {
            let entry = &self.entries[slot.entry];
            // a placed volume reads the seed's Tiles part as the Cumulus does, so the GPU holds the noise tiles once for
            // every volume of a seed
            let upload = match &entry.class {
                EntryClass::Volume(volume) => volume.gpu_upload(width, &slot.cuts, entry.lining, mode, jitter, resident),
                EntryClass::Placed(placed) => placed.gpu_upload(width, &slot.cuts, entry.lining, mode, jitter, resident),
                _ => continue,
            };
            match upload {
                Ok(upload) => uploads.push(upload),
                Err(why) => {
                    log!("hdri clouds: {why}; the clouds march on the CPU");
                    return Vec::new();
                }
            }
        }
        if let Some((kind, rows)) = uploads.iter().find_map(VolumeUpload::too_tall) {
            log!("hdri clouds: a volume's {kind:?} takes {rows} rows, past the GPU's {MAX_ROWS}: the clouds march on the CPU");
            return Vec::new();
        }
        uploads
    }

    /// The key of the ask `asker` makes of [`Self::gpu_asks`]'s uploads at `width` in `mode` (texel centres), without
    /// building it (a host that kept the records of an ask finds them by it): None where it would be empty for its
    /// volumes or their cuts.
    pub fn gpu_ask_key(&self, width: usize, mode: GpuMode, asker: AskFor) -> Option<u64> {
        if !self.gpu_marches() {
            return None;
        }
        let keys = self.volumes.iter().filter_map(|slot| match &self.entries[slot.entry].class {
            EntryClass::Volume(volume) => Some(volume.gpu_key(width, &slot.cuts, self.entries[slot.entry].lining, mode, [0.0; 2])),
            EntryClass::Placed(placed) => Some(placed.gpu_key(width, &slot.cuts, self.entries[slot.entry].lining, mode, [0.0; 2])),
            _ => None,
        });
        Some(CloudAsk::key_of(width, asker, mode, keys))
    }

    /// The records among `records` that this stack's volumes read at `width` in `mode` with their texels' directions
    /// moved by `jitter` texels, one a volume in its order: all or nothing (design §12.11 item 4.5), Err naming a volume
    /// that has none.
    pub fn gpu_records_for(&self, width: usize, mode: GpuMode, jitter: [f32; 2], records: &[Arc<GpuRecord>]) -> Result<Vec<Arc<GpuRecord>>, String> {
        let mut found = Vec::with_capacity(self.volumes.len());
        for slot in &self.volumes {
            let key = match &self.entries[slot.entry].class {
                EntryClass::Volume(volume) => volume.gpu_key(width, &slot.cuts, self.entries[slot.entry].lining, mode, jitter),
                EntryClass::Placed(placed) => placed.gpu_key(width, &slot.cuts, self.entries[slot.entry].lining, mode, jitter),
                _ => return Err(format!("the volume of layer {} is not one", self.entries[slot.entry].layer + 1)),
            };
            match records.iter().find(|r| r.key == key && r.width() == width && r.segments() == slot.cuts.len() + 1) {
                Some(record) => found.push(record.clone()),
                None => return Err(format!("no GPU record for the volume of layer {} at {width}", self.entries[slot.entry].layer + 1)),
            }
        }
        Ok(found)
    }

    /// Whether its volumes can march on the GPU: it has one, and none is cut into more than PASS_SEGMENTS segments
    /// (logged).
    fn gpu_marches(&self) -> bool {
        if let Some(slot) = self.volumes.iter().find(|slot| slot.cuts.len() + 1 > PASS_SEGMENTS) {
            log!("hdri clouds: a volume cut into {} segments marches on the CPU (the GPU takes {PASS_SEGMENTS})", slot.cuts.len() + 1);
            return false;
        }
        !self.volumes.is_empty()
    }

    /// Attaches the GPU's records of the texel centres (design §12.3), all or nothing (design §12.11 item 4.5): each
    /// volume takes the record under its key, and when one has none, none is kept and Err says which. Records already
    /// attached are replaced.
    pub fn attach_gpu_records(&mut self, width: usize, mode: GpuMode, records: &[Arc<GpuRecord>]) -> Result<(), String> {
        let found = self.gpu_records_for(width, mode, [0.0; 2], records)?;
        for (slot, record) in self.volumes.iter_mut().zip(found) {
            slot.gpu = Some(record);
        }
        Ok(())
    }

    /// Every cloud volume with the heights its march is cut at and its entry's Silver lining (test support: the GPU
    /// crate marches them on the CPU to compare).
    #[doc(hidden)]
    pub fn volume_marches(&self) -> Vec<(&CloudVolume, &[f32], f32)> {
        self.volumes
            .iter()
            .filter_map(|slot| match &self.entries[slot.entry].class {
                EntryClass::Volume(volume) => Some((volume, slot.cuts.as_slice(), self.entries[slot.entry].lining)),
                _ => None,
            })
            .collect()
    }

    /// Whether every cloud volume marches from a GPU record (and there is one).
    pub fn has_gpu_records(&self) -> bool {
        !self.volumes.is_empty() && self.volumes.iter().all(|slot| slot.gpu.is_some())
    }

    /// A cloud volume's record of one segment along `d`, lit (design §2.3, §12.1 item 9): its light with the light
    /// pass's suns at its base and top (dimmed by the flat entries over the point its light came from), its dome and
    /// ground, the entry's rows, hazed toward `sky` at the record's mean depth. Returns its radiance and its alpha,
    /// both faded at the horizon as a flat entry's, and its unfaded opacity.
    fn lit_record(&self, i: usize, r: &VolumeMarch, d: Vec3f, sky: Vec3f, held: bool) -> (Vec3f, f32, f32) {
        let entry = &self.entries[i];
        let light = &self.lights[i];
        let opacity = r.alpha();
        let shaded = if entry.shaded_from_above {
            let t = r.mean_depth_m();
            cloud_light::t_above_volume(&self.entries, i, vec2f(d.x * t, d.z * t), height_at(t, d.y), self.sun_dir, held)
        } else {
            1.0
        };
        let inputs = ShadeInputs {
            lining: entry.lining,
            sun_base: light.sun * shaded,
            sun_top: light.sun_top * shaded,
            brightness: entry.brightness,
            sky_light: entry.sky_light,
            ambient: light.ambient,
            side: light.side_seen_along(d),
            ground: light.ground,
            ground_sun: light.ground_sun,
            sky,
            visibility_m: self.visibility_m,
        };
        // A placed volume stands at its own distance down to the horizon, its rain on the ground there: no fade. A Nimbostratus,
        // a Stratus and an Altocumulus' far deck reach the horizon (their own fades); the others fade as the flat decks do.
        let fade = match entry.genus {
            _ if matches!(entry.class, EntryClass::Placed(_)) => 1.0,
            CloudGenus::Nimbostratus => crate::cloud_volume_nimbostratus::horizon_fade(d.y),
            CloudGenus::Stratus => crate::cloud_volume_cumulus::stratus_horizon_fade(d.y),
            CloudGenus::Altocumulus => crate::cloud_volume_altocumulus::horizon_fade(d.y),
            _ => horizon_fade(d.y),
        };
        // The air in front of a Nimbostratus turns its light toward the sky as seen through the stack along `d`, not the
        // clear sky: what the stack passes there of the sky behind, and its own light (this record's, a unit of opacity,
        // unhazed) for the rest, so a far deck under a closed sky stays its own grey rather than the clear horizon's.
        // (lane ns; the other types keep the clear sky's haze their export looks were recorded with)
        let passes = if entry.genus == CloudGenus::Nimbostratus { self.seen_through(d) } else { 1.0 };
        let inputs = if passes < 1.0 && opacity > 1.0e-4 {
            let own = shade(r, &ShadeInputs { visibility_m: f32::INFINITY, ..inputs }) * (1.0 / opacity);
            ShadeInputs { sky: sky * passes + own * (1.0 - passes), ..inputs }
        } else {
            inputs
        };
        (shade(r, &inputs) * fade, opacity * fade, opacity)
    }

    /// What the whole stack passes along `d` (`seen`, the dome looks' table): the nearest look row by sine, linear round
    /// the turn; 1 without a table and at and below the horizon.
    fn seen_through(&self, d: Vec3f) -> f32 {
        if self.seen.is_empty() || !(d.y > 0.0) {
            return 1.0;
        }
        let (rows, cols) = (crate::DOME_ROWS, crate::DOME_COLUMNS);
        let j = ((d.y.min(1.0) * rows as f32 - 0.5).round().max(0.0) as usize).min(rows - 1);
        // the row's looks stand at (i + turn) / cols of the turn, as `crate::dome_look`'s, azimuth clockwise from north
        let turn = if j % 2 == 0 { 0.5 } else { 0.0 };
        let az = d.x.atan2(-d.z).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * cols as f32 - turn;
        let i0 = az.floor();
        let t = az - i0;
        let at = |i: f32| self.seen[j * cols + (i.rem_euclid(cols as f32) as usize).min(cols - 1)];
        at(i0) + (at(i0 + 1.0) - at(i0)) * t
    }

    /// The front-to-back walk: the light gathered, T and Tu. `sky` is what lies behind every cloud along `d` (a
    /// volume's haze turns its light toward it), `texel` the lookup's (`composite_at`).
    fn walk(&self, d: Vec3f, sky: Vec3f, look: &StackLook, texel: f32, disc_texel: bool, segments: &mut [CloudMarch], records: &mut [VolumeMarch]) -> (Vec3f, f32, f32) {
        // D1: a disc texel walks every entry; a classic-only stack walks every entry, as today. Past EARLY_OUT_T a disc
        // texel of a modern stack goes on for Tu alone (`dark`): its light and T stop where the lookup without the disc
        // stops, so the two differ by the disc alone (the preview's disc visibility, `Env::split_sky_discs`).
        let early = !self.all_classic;
        let mut dark = false;
        let volume = self.marched.and_then(|m| self.entries[m].volume());
        let (mut l, mut t, mut tu) = (Vec3f::default(), 1.0f32, 1.0f32);
        // A classic-only stack's flat covers toward a disc, each over what is in front of it, as today's layer
        // combines its deck and veil (classic_transmittance_toward).
        let mut covered = 0.0f32;
        let mut marched = false;
        // A slab's lookup and its shade from above, kept from its scud's step for its own (None: not looked up yet).
        let mut kept: Option<(usize, Seen, Option<f32>)> = None;
        for step in &self.plan {
            match *step {
                Step::Flat(i) | Step::Scud(i) => {
                    let entry = &self.entries[i];
                    // (another entry's steps between a scud and its slab leave it kept)
                    let (seen, mut shade) = match kept {
                        Some((j, seen, shade)) if j == i => {
                            kept = None;
                            (seen, shade)
                        }
                        _ => (entry.flat_at(d, look.held, texel), None),
                    };
                    let scud = matches!(step, Step::Scud(_));
                    // A slab with a scud step draws only its own shape at its own step; the scud at its own.
                    let shape = match (scud, entry.scud_m) {
                        (true, _) => &seen.slab.scud,
                        (false, Some(_)) => &seen.slab.own,
                        (false, None) => &seen.shape,
                    };
                    if shape.alpha > 0.0 && !dark {
                        let t_above = *shade.get_or_insert_with(|| if entry.shaded_from_above { cloud_light::t_above(&self.entries, i, &seen.shape, self.sun_dir, look.held) } else { 1.0 });
                        l += entry.radiance(&self.lights[i], &seen, d, self.sun_dir, t_above, scud) * t;
                        t *= 1.0 - shape.alpha;
                    }
                    let shape = *shape;
                    if scud {
                        kept = Some((i, seen, shade));
                    }
                    if disc_texel {
                        if self.all_classic {
                            covered += shape.cover * (1.0 - covered);
                        } else {
                            tu *= 1.0 - entry.disc_cover(d, &shape, texel);
                        }
                    }
                }
                Step::Segment(k) => {
                    let (Some(m), Some(volume)) = (self.marched, volume) else {
                        continue;
                    };
                    if !marched {
                        match look.cache {
                            Some(cache) => cache.sample_segments(volume, d, segments),
                            None if segments.len() == 1 => segments[0] = volume.march(d),
                            None => volume.march_split(d, &self.cuts, segments),
                        }
                        marched = true;
                    }
                    let s = segments[k];
                    if s.alpha > 0.0 {
                        if !dark {
                            let light = &self.lights[m];
                            let lit = light.sun * s.sun + light.ambient * s.ambient + light.ground * s.bounce;
                            l += lit * (self.entries[m].brightness * t);
                            t *= 1.0 - s.alpha;
                        }
                        tu *= 1.0 - s.alpha;
                    }
                }
                Step::Volume(v, k) => {
                    let slot = &self.volumes[v];
                    let EntryClass::Volume(volume) = &self.entries[slot.entry].class else {
                        continue;
                    };
                    let mine = &mut records[slot.first..slot.first + slot.cuts.len() + 1];
                    // Marched once a ray, at its first segment: every later one comes after it in the walk. A lookup that
                    // draws a disc passes what the clouds pass along `d` itself, the whole ray's opacity at the march's
                    // width (the lit march's, bit for bit): the disc is a texel or less wide and the map draws it from
                    // sub-samples over its texels, as the key's cone mean takes it (design §2.4, §12.8 item 3; the
                    // texel's centre alone put a carved Cumulus' drawn disc 0.8 of its key). Its cloud stays the
                    // record's, the one the lookup without the disc reads, so the two differ by the disc alone: lit
                    // from its own march, the light that march differs by from the record's showed in the preview's
                    // disc visibility (`Env::split_sky_discs`) as the sun's disc through an opaque body.
                    if k == 0 {
                        if disc_texel {
                            tu *= 1.0 - volume.shape().opacity(d, Self::march_width(texel), true);
                        }
                        if dark {
                            continue;
                        }
                        let gpu = match look.gpu {
                            VolumeRecords::Attached => slot.gpu.as_ref(),
                            VolumeRecords::These(records) => records.get(v),
                            VolumeRecords::Cpu => None,
                        };
                        self.march_volume(slot, gpu, volume, d, texel, mine);
                    }
                    if dark {
                        continue;
                    }
                    let r = mine[k];
                    if r.alpha() > 0.0 {
                        let (lit, alpha, opacity) = self.lit_record(slot.entry, &r, d, sky, look.held);
                        l += lit * t;
                        t *= 1.0 - alpha;
                        if !disc_texel {
                            tu *= 1.0 - opacity;
                        }
                    }
                }
                // A placed volume's segments, as a volume's: marched at its first, a disc texel's own pass its opacity.
                Step::Placed(v, k) => {
                    let slot = &self.volumes[v];
                    let EntryClass::Placed(placed) = &self.entries[slot.entry].class else {
                        continue;
                    };
                    let mine = &mut records[slot.first..slot.first + slot.cuts.len() + 1];
                    if k == 0 {
                        if disc_texel {
                            tu *= 1.0 - placed.opacity(d);
                        }
                        if dark {
                            continue;
                        }
                        let gpu = match look.gpu {
                            VolumeRecords::Attached => slot.gpu.as_ref(),
                            VolumeRecords::These(records) => records.get(v),
                            VolumeRecords::Cpu => None,
                        };
                        self.march_placed(slot, gpu, placed, d, texel, mine);
                    }
                    if dark {
                        continue;
                    }
                    let r = mine[k];
                    if r.alpha() > 0.0 {
                        let (lit, alpha, opacity) = self.lit_record(slot.entry, &r, d, sky, look.held);
                        l += lit * t;
                        t *= 1.0 - alpha;
                        if !disc_texel {
                            tu *= 1.0 - opacity;
                        }
                    }
                }
            }
            if early && t < EARLY_OUT_T {
                if !disc_texel {
                    break;
                }
                dark = true;
            }
        }
        if covered > 0.0 {
            tu *= 1.0 - covered.clamp(0.0, 1.0);
        }
        (l, t, tu)
    }

    /// A classic-only stack's cover with the older map's arithmetic, which the map's discs and key read in place of
    /// the one cover function (lib.rs `cover_toward` and its disc covers; the compositor's Tu combines the same way):
    /// its flat entries' covers combined, each over what is in front of it, as today's layer combines its deck and
    /// veil, then one minus that times the volume's transmittance. It is the product of the entries' 1 - cover, but
    /// a near-closed sky's transmittance is a small difference of covers near 1, so the product's rounding differs
    /// from the older map's by a few parts in 1e5 there (an overcast weather's key).
    pub(crate) fn classic_transmittance_toward(&self, d: Vec3f) -> f32 {
        let (mut covered, mut volume) = (0.0f32, 1.0f32);
        for entry in &self.entries {
            match entry.volume() {
                Some(v) => volume *= v.transmittance_toward(d),
                None => covered += entry.cover_toward(d) * (1.0 - covered),
            }
        }
        (1.0 - covered.clamp(0.0, 1.0)) * volume
    }

    /// The built entries, in the per-Env order (ascending height): index 0 is the front for upward rays.
    pub fn entries(&self) -> &[CloudEntry] {
        &self.entries
    }

    /// The light pass's result, by the entries' index.
    pub fn lights(&self) -> &[EntryLight] {
        &self.lights
    }

    /// Every entry is a lifted layer: the stack keeps today's couplings (design §2.2, classic layers alone).
    pub fn all_classic(&self) -> bool {
        self.all_classic
    }

    /// A cache `width` x `width / 2` of the marched entry's segments, for a bake wider than the march (Env::cloud_cache
    /// hands it MARCH_MAX_WIDTH); None when nothing marches. Past CACHE_BUDGET_MB for one bake it is FALLBACK_CACHE_WIDTH
    /// wide, and the log says so (design §7).
    pub fn cache_for(&self, width: usize) -> Option<StackCache> {
        self.marched?;
        let segments = self.cuts.len() + 1;
        let mb = |w: usize| w as f64 * (w / 2) as f64 * segments as f64 * 16.0 / (1024.0 * 1024.0);
        let width = if mb(width) > CACHE_BUDGET_MB && width > FALLBACK_CACHE_WIDTH {
            log!("hdri clouds: the cloud cache drops to 1024 wide ({:.0} MB over the 256 MB bake budget)", mb(width));
            FALLBACK_CACHE_WIDTH
        } else {
            width
        };
        Some(CloudCache::with_cuts(width, self.cuts.clone()))
    }
}

impl CloudEntry {
    /// What a flat entry saw along the unit direction `d` (d.y > 0), looked up exactly (the tests').
    #[cfg(test)]
    fn flat(&self, d: Vec3f, held: bool) -> Seen {
        self.flat_at(d, held, 0.0)
    }

    /// What a flat entry saw along the unit direction `d` (d.y > 0); `held` is a held bake's (its lookups take 2
    /// self-shadow steps), `texel` the lookup's (0: one exact direction), which a sheet's finest detail follows.
    fn flat_at(&self, d: Vec3f, held: bool, texel: f32) -> Seen {
        match &self.class {
            EntryClass::Classic(entry) => match entry.flat_sample(d) {
                Some((classic, cover, hit_m, height_m)) => Seen { shape: Shape { cover, alpha: classic.alpha, tau: 0.0, tau_sun: 0.0, hit_m, height_m }, classic, ..Seen::default() },
                None => Seen::default(),
            },
            EntryClass::Sheet(sheet) => Seen { shape: sheet.shape_at(d, held, true, texel), ..Seen::default() },
            EntryClass::Slab(slab) => {
                let (shape, slab) = slab.shape(d);
                Seen { shape, slab, ..Seen::default() }
            }
            // A volume is never a flat step: the walk marches it.
            EntryClass::Volume(_) | EntryClass::Placed(_) => Seen::default(),
        }
    }

    /// What it hides of a disc along `d` at a lookup of a bake whose texels are `texel` radians wide (`shape` is what
    /// the lookup saw): its cover as the key's cone mean reads it, exactly (design §2.4). A sheet's finest detail
    /// follows the texel for its light alone, so a disc texel and the lookup without the disc differ by the disc alone.
    fn disc_cover(&self, d: Vec3f, shape: &Shape, texel: f32) -> f32 {
        match &self.class {
            EntryClass::Sheet(sheet) if texel > 0.0 => sheet.cover_toward(d),
            _ => shape.cover,
        }
    }

    /// Its light along the ray, premultiplied by its alpha: a classic entry by today's shade (cloud_classic.rs), a
    /// modern one by its class (cloud_sheets.rs, cloud_slabs.rs) through its family's law (cloud_light.rs `radiance`),
    /// the sun shaded by `t_above`.
    /// `scud`: a slab's scud's light alone, at its own step.
    fn radiance(&self, light: &EntryLight, seen: &Seen, d: Vec3f, sun_dir: Vec3f, t_above: f32, scud: bool) -> Vec3f {
        match &self.class {
            EntryClass::Classic(entry) => entry.flat_radiance(&seen.classic, light.sun, light.ambient, t_above, self.brightness),
            EntryClass::Sheet(sheet) => sheet.radiance(light, &seen.shape, d, sun_dir, t_above),
            EntryClass::Slab(slab) => slab.radiance(light, &seen.slab, d, sun_dir, t_above, scud),
            EntryClass::Volume(_) | EntryClass::Placed(_) => Vec3f::default(),
        }
    }

    /// The box of sky directions a placed entry's bounds hold (`PlacedVolume::sky_box`); None for any other entry. The
    /// tests' search for a placed type's edge.
    #[cfg(test)]
    pub(crate) fn sky_box(&self) -> Option<[f32; 4]> {
        match &self.class {
            EntryClass::Placed(placed) => Some(placed.sky_box()),
            _ => None,
        }
    }

    /// The classic marched volume, when this entry is it.
    fn volume(&self) -> Option<&VolumetricClouds> {
        match &self.class {
            EntryClass::Classic(entry) => entry.volume(),
            EntryClass::Sheet(_) | EntryClass::Slab(_) | EntryClass::Volume(_) | EntryClass::Placed(_) => None,
        }
    }
}

impl LitEntry for CloudEntry {
    fn layer(&self) -> usize {
        self.layer
    }
    fn genus(&self) -> CloudGenus {
        self.genus
    }
    fn is_classic(&self) -> bool {
        self.classic
    }
    fn marched(&self) -> bool {
        self.marched
    }
    fn height_m(&self) -> f32 {
        self.height_m
    }
    fn base_m(&self) -> f32 {
        self.base_m
    }
    fn light_height_m(&self) -> f32 {
        self.light_height_m
    }
    fn rows(&self) -> LightRows {
        LightRows { brightness: self.brightness, sky_light: self.sky_light, lining: self.lining, shade_below: self.shade_below }
    }
    fn presence(&self) -> f32 {
        self.presence
    }
    fn classic_amount(&self) -> Option<f32> {
        self.classic_amount
    }
    fn cover_toward(&self, d: Vec3f) -> f32 {
        match &self.class {
            EntryClass::Classic(entry) => entry.cover_toward(d),
            EntryClass::Sheet(sheet) => sheet.cover_toward(d),
            EntryClass::Slab(slab) => slab.cover_toward(d),
            EntryClass::Volume(volume) => volume.cover_toward(d),
            EntryClass::Placed(placed) => placed.cover_toward(d),
        }
    }
    fn stand_in(&self, d: Vec3f) -> CoverSample {
        match &self.class {
            EntryClass::Classic(entry) => CoverSample { cover: entry.stand_in(d), tau: 0.0 },
            EntryClass::Sheet(sheet) => sheet.stand_in(d),
            EntryClass::Slab(slab) => slab.stand_in(d),
            EntryClass::Volume(volume) => volume.stand_in(d),
            EntryClass::Placed(placed) => placed.stand_in(d),
        }
    }
    fn plane_cover(&self, xz_m: Vec2f, octaves: u32) -> f32 {
        match &self.class {
            EntryClass::Classic(entry) => entry.plane_cover(xz_m, octaves),
            EntryClass::Sheet(sheet) => sheet.plane_cover(xz_m, octaves),
            EntryClass::Slab(slab) => slab.plane_cover(xz_m, octaves),
            // A cloud volume's shadow on a lower flat layer, at a point of its base.
            EntryClass::Volume(volume) => volume.shadow_cover(xz_m),
            EntryClass::Placed(_) => 0.0,
        }
    }
    fn volume_shell_m(&self) -> Option<(f32, f32)> {
        match &self.class {
            EntryClass::Volume(volume) => Some(volume.shape().shell_m()),
            EntryClass::Placed(placed) => Some(placed.light_shell_m()),
            _ => None,
        }
    }
    fn sun_share(&self) -> Option<f32> {
        match &self.class {
            // (lane ns: a Nimbostratus passes the sun to the ground by its own sunlit share; the other volumes by their
            // dome's pass, as their export looks were recorded)
            EntryClass::Volume(volume) if self.genus == CloudGenus::Nimbostratus => Some(volume.sun_share()),
            _ => None,
        }
    }
}

/// The busy stack (design §12.5's budgets): a Cirrus, a Stratocumulus, a Cumulus, a Stratus at Cover 0.4 and a
/// Nimbostratus at 0.3, the sun 30 degrees up at azimuth 200. Test support (this crate's tests and the GPU crate's).
#[doc(hidden)]
pub fn busy_stack_params() -> crate::HdriParams {
    let mut p = crate::HdriParams::default();
    p.sky.sun.mode = "manual".to_string();
    p.sky.sun.elevation_deg = 30.0;
    p.sky.sun.azimuth_deg = 200.0;
    let mut layers: Vec<CloudLayerParams> = Vec::new();
    for (genus, cover) in [(CloudGenus::Cirrus, None), (CloudGenus::Stratocumulus, None), (CloudGenus::Cumulus, None), (CloudGenus::Stratus, Some(0.4)), (CloudGenus::Nimbostratus, Some(0.3))] {
        let mut layer = CloudLayerParams::new(genus, &layers, p.seed);
        layer.cover = cover.or(layer.cover);
        layers.push(layer);
    }
    p.sky.cloud_stack = Some(crate::CloudStackParams { layers, ..crate::CloudStackParams::default() });
    p
}

#[cfg(test)]
thread_local! {
    static ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_entries(n: usize) {
    ENTRIES.with(|e| e.set(e.get() + n));
}

/// Entries and quantile tables built on this thread so far (the pattern of lib.rs's `probe` counters): the Cover-0
/// tests' "nothing was built".
#[cfg(test)]
pub(crate) fn built_on_this_thread() -> (usize, usize) {
    (ENTRIES.with(|e| e.get()), crate::cloud_cover::tables_measured_on_this_thread())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::presets::preset;
    use crate::{dir_from_az_el, legacy_cloud_layers, CloudStackParams, Env, HdriParams, Sampling, VolumetricParams};
    use crate::noise::hash01;
    use crate::{vec, CirrusParams, CumulusParams, StratocumulusParams, StratusParams, CLOUD_GENUS_BUILT};
    use crate::sky::luminance;
    use makepad_render_material::ibl::{self, EnvMap};

    fn inputs_sun_at(_: f32) -> Vec3f {
        vec3f(1.0, 0.9, 0.8)
    }

    /// Light for a stack built on its own (the Env builds its own from the atmosphere).
    fn inputs() -> LightInputs<'static> {
        LightInputs { sun_dir: dir_from_az_el(200.0, 30.0), a_top: vec3f(0.1, 0.15, 0.3), ground_albedo: Vec3f::all(0.2), sun_at: &inputs_sun_at, sky_at: None }
    }

    fn look() -> StackLook<'static> {
        StackLook { held: false, cache: None, gpu: VolumeRecords::Attached }
    }

    /// The phase-1 busy stack (D7; the slab and stack tests read it): cloudy_sky's sun (manual, 30 degrees up at 200),
    /// map seed 1, no older clouds, and CloudLayerParams::new for Cirrus, Stratocumulus and Cumulus at their defaults,
    /// Stratus at Cover 0.4 and Nimbostratus at 0.3.
    pub(crate) fn busy_stack() -> HdriParams {
        super::busy_stack_params()
    }

    /// The busy stack builds its five entries, in ascending height, one of each class's types: the Cumulus and the
    /// Nimbostratus marched volumes, walked at the heights their segments begin. The Nimbostratus' facts (design §12.14
    /// item 3): its class, its scud its own march's (no scud step), its far deck past the march's range, and closed at
    /// Cover 1.
    #[test]
    fn the_busy_stack_builds_every_type() {
        let env = Env::new(&busy_stack());
        let stack = env.cloud_stack.as_ref().expect("the busy stack draws");
        assert_eq!(stack.entries().len(), 5);
        assert!(stack.entries().windows(2).all(|w| w[0].height_m() <= w[1].height_m()), "ascending height");
        let ns = stack.entries().iter().position(|e| e.genus() == CloudGenus::Nimbostratus).expect("the Nimbostratus is built");
        let EntryClass::Volume(ns_volume) = &stack.entries()[ns].class else { panic!("a modern Nimbostratus is a volume") };
        assert!(stack.entries()[ns].marched(), "the Nimbostratus is marched");
        assert!(!stack.plan.iter().any(|step| matches!(step, Step::Scud(i) if *i == ns)), "its scud is its own march's: {:?}", stack.plan);
        // Its far deck: a look whose deck base lies past the march's range meets a mass as often as a line of sight that
        // long through a deck of its Cover does (its GPU arm's fill): half a degree up, some sixty cells of Size through
        // its heights, all but closed at the busy stack's Cover 0.3; under it, the march's own stand-in.
        let cover = ns_volume.shape().light_key().1;
        assert!(cover > 0.0 && cover < 0.5, "the premise: a broken Nimbostratus, Cover {cover}");
        for az in [0.0f32, 90.0, 200.0, 300.0] {
            let far = stack.entries()[ns].stand_in(dir_from_az_el(az, 0.5)).cover;
            assert!(far > 0.99, "the far deck at azimuth {az}, 0.5 degrees up: {far}");
        }
        // Closed at Cover 1: its far deck is whole and it hides the dome.
        let mut closed = busy_stack();
        for layer in closed.sky.cloud_stack.as_mut().expect("a stack").layers.iter_mut() {
            if layer.genus() == Some(CloudGenus::Nimbostratus) {
                layer.cover = Some(1.0);
            }
        }
        let closed = Env::new(&closed);
        let closed = closed.cloud_stack.as_ref().expect("the busy stack draws");
        let i = closed.entries().iter().position(|e| e.genus() == CloudGenus::Nimbostratus).expect("the Nimbostratus is built");
        assert_eq!(closed.entries()[i].stand_in(dir_from_az_el(45.0, 0.5)).cover, 1.0, "a closed far deck");
        assert!(closed.lights()[i].dome_share > 0.95, "a closed Nimbostratus hides {} of the dome", closed.lights()[i].dome_share);
        let step_m = |step: &Step| match *step {
            Step::Flat(i) => stack.entries()[i].height_m,
            Step::Scud(i) => stack.entries()[i].scud_m.expect("a scud step is a scud's"),
            Step::Segment(_) => unreachable!("no classic layer marches"),
            Step::Volume(v, k) => match k {
                0 => stack.entries()[stack.volumes[v].entry].volume_shell_m().expect("a volume's shell").0,
                _ => stack.volumes[v].cuts[k - 1],
            },
            Step::Placed(..) => unreachable!("the busy stack places no storm"),
        };
        assert!(stack.plan.windows(2).all(|w| step_m(&w[0]) <= step_m(&w[1])), "the walk: {:?}", stack.plan);
        let cumulus = stack.entries().iter().position(|e| e.genus() == CloudGenus::Cumulus).expect("the Cumulus is built");
        assert!(matches!(stack.entries()[cumulus].class, EntryClass::Volume(_)) && stack.entries()[cumulus].marched(), "the Cumulus is a marched volume");
        assert!(stack.plan.iter().any(|step| matches!(step, Step::Volume(_, 0))), "the walk marches it: {:?}", stack.plan);
        assert!(!stack.all_classic());
        let map = env.bake(64);
        assert!(map.data.iter().all(|t| t.iter().all(|v| v.is_finite() && *v >= 0.0)));

        // The light record (design §12.1 item 9). A lookup of a 512 or 1024 bake reads the record of the texel holding
        // its direction, which is bit for bit the volume's own march at that texel's centre at that width; an exact
        // lookup marches at 1024 and any other width marches at its own, and neither keeps a record.
        // (the Cumulus' slot: the Stratus and the Nimbostratus under it and the Stratocumulus are volumes too, the
        // volumes design §2.2)
        let slot_of = |stack: &CloudStack| stack.volumes.iter().position(|slot| stack.entries()[slot.entry].genus() == CloudGenus::Cumulus).expect("the Cumulus marches");
        let volume_of = |stack: &CloudStack| -> (usize, Vec<f32>) {
            let slot = &stack.volumes[slot_of(stack)];
            (slot.entry, slot.cuts.clone())
        };
        let (cumulus, cuts) = volume_of(stack);
        let EntryClass::Volume(volume) = &stack.entries()[cumulus].class else { panic!("the Cumulus is a volume") };
        let slot = &stack.volumes[slot_of(stack)];
        let segments = cuts.len() + 1;
        let bits = |r: &[VolumeMarch]| -> Vec<[u32; 10]> { r.iter().map(|m| [m.transmittance, m.forward_low, m.forward_high, m.back_low, m.back_high, m.ambient, m.open, m.bounce, m.depth_weighted, m.weight].map(f32::to_bits)).collect() };
        let (mut got, mut own) = (vec![VolumeMarch::CLEAR; segments], vec![VolumeMarch::CLEAR; segments]);
        let tau = std::f32::consts::TAU;
        let centre = |x: usize, y: usize, width: usize| vec(ibl::equirect_uv_to_dir([(x as f32 + 0.5) / width as f32, (y as f32 + 0.5) / (width / 2) as f32]));
        for (texel, width) in [(0.0f32, EXACT_WIDTH), (tau / 256.0, 256), (tau / 2048.0, 2048)] {
            for d in sky_dirs().into_iter().step_by(7) {
                stack.march_volume(slot, slot.gpu.as_ref(), volume, d, texel, &mut got);
                volume.march(d, width, &cuts, &mut own);
                assert_eq!(bits(&got), bits(&own), "a lookup {texel} wide at {d:?} marches at {width}");
            }
        }
        assert!(slot.records.iter().all(|r| r.get().is_none()), "only a 512 or 1024 bake keeps a record");
        for (k, width) in RECORD_WIDTHS.into_iter().enumerate() {
            let rows = width / 4;
            let mut cloudy = 0usize;
            for j in 0..24usize {
                for i in 0..12usize {
                    let (x, y) = ((i * width / 12 + j * 7) % width, j * (rows - 1) / 23);
                    // A direction inside the texel, off its centre, reads the same record.
                    let (jx, jy) = (hash01(i as i32, j as i32, k as i32, 7) * 0.6 - 0.3, hash01(i as i32, j as i32, k as i32, 8) * 0.6 - 0.3);
                    let inside = vec(ibl::equirect_uv_to_dir([(x as f32 + 0.5 + jx) / width as f32, (y as f32 + 0.5 + jy) / (width / 2) as f32]));
                    volume.march(centre(x, y, width), width, &cuts, &mut own);
                    for d in [centre(x, y, width), inside] {
                        stack.march_volume(slot, slot.gpu.as_ref(), volume, d, tau / width as f32, &mut got);
                        assert_eq!(bits(&got), bits(&own), "the {width} record at texel ({x}, {y}), read along {d:?}");
                    }
                    // A lookup that draws a disc differs from the one without it by the disc alone, passed as the volume
                    // passes along its own direction (design §12.8 item 3): grey (no light of the volume's own march in
                    // it), never more than the volume's own pass (the flat entries in front only lower it), and nothing
                    // where its own march is opaque.
                    let (sky, disc) = (vec3f(0.31, 0.42, 0.53), Vec3f::all(1.0));
                    let lookup = |discs: Vec3f| stack.composite_at(inside, sky, discs, &look(), tau / width as f32);
                    let (with, without) = (lookup(disc), lookup(Vec3f::default()));
                    let passed = with - without;
                    let own_pass = 1.0 - volume.shape().opacity(inside, width, true);
                    assert!((passed.x - passed.y).abs() < 1.0e-5 && (passed.x - passed.z).abs() < 1.0e-5, "a disc's lookup at {width} along {inside:?} adds light: {passed:?}");
                    assert!(passed.x >= -1.0e-5 && passed.x <= own_pass + 1.0e-5, "a disc's lookup at {width} along {inside:?} passes {} where its volume passes {own_pass}", passed.x);
                    if own_pass == 0.0 {
                        assert_eq!([with.x, with.y, with.z].map(f32::to_bits), [without.x, without.y, without.z].map(f32::to_bits), "an opaque volume along {inside:?} at {width} hides the disc");
                    }
                    cloudy += own.iter().any(|m| m.alpha() > 0.0) as usize;
                }
            }
            println!("the {width} record: {cloudy} of 288 texels read hold cloud ({segments} segments)");
            assert!(cloudy >= 20, "the {width} record was compared on {cloudy} cloudy texels");
            assert!(slot.records[k].get().is_some(), "a {width} lookup keeps the {width} record");
        }
        // Its key holds everything the march reads and nothing else: a map that differs in Silver lining, Brightness,
        // Sky light, the haze or the sun's colour (the ozone) shares the record, and its own march equals the record;
        // one that differs in Puffiness, the sun's way, Cover, the step budget, the cuts or the width gets its own.
        let record = volume.light_record(512, &cuts);
        let changed = |change: &dyn Fn(&mut HdriParams)| -> Env {
            let mut p = busy_stack();
            change(&mut p);
            Env::new(&p)
        };
        fn the_cumulus(p: &mut HdriParams) -> &mut CloudLayerParams {
            let layer = &mut p.sky.cloud_stack.as_mut().expect("the busy stack").layers[2];
            assert_eq!(layer.genus(), Some(CloudGenus::Cumulus));
            layer
        }
        let shares: [(&str, &dyn Fn(&mut HdriParams)); 5] = [
            ("Silver lining", &|p| the_cumulus(p).lining = Some(0.1)),
            ("Brightness", &|p| the_cumulus(p).brightness = Some(1.7)),
            ("Sky light", &|p| the_cumulus(p).sky_light = Some(0.3)),
            ("the haze", &|p| p.sky.atmosphere.haze = 4.0),
            ("the sun's colour", &|p| p.sky.atmosphere.ozone = 4.0),
        ];
        for (row, change) in shares {
            let env = changed(change);
            let other = env.cloud_stack.as_ref().expect("the busy stack draws");
            let (i, other_cuts) = volume_of(other);
            let EntryClass::Volume(other_volume) = &other.entries()[i].class else { panic!("the Cumulus is a volume") };
            let other_record = other_volume.light_record(512, &other_cuts);
            assert!(Arc::ptr_eq(&record, &other_record), "a change of {row} keeps the light record");
            for (x, y) in [(3usize, 5usize), (100, 40), (260, 90), (400, 127), (511, 64)] {
                // (this map's march fills the row; the other map reads it)
                assert!(record.read(volume, centre(x, y, 512), &mut got));
                assert!(other_record.read(other_volume, centre(x, y, 512), &mut got));
                other_volume.march(centre(x, y, 512), 512, &other_cuts, &mut own);
                assert_eq!(bits(&got), bits(&own), "after a change of {row}, the record at texel ({x}, {y})");
            }
        }
        let differs: [(&str, &dyn Fn(&mut HdriParams)); 4] = [
            ("Puffiness", &|p| {
                let cumulus = the_cumulus(p).cumulus.get_or_insert_with(CumulusParams::default);
                cumulus.puffiness = if cumulus.puffiness > 0.5 { 0.2 } else { 0.8 };
            }),
            ("the sun's way", &|p| p.sky.sun.azimuth_deg += 5.0),
            ("Cover", &|p| the_cumulus(p).cover = Some(0.6)),
            ("the step budget", &|p| p.sky.cloud_stack.as_mut().expect("the busy stack").march_steps = 40),
        ];
        let mut kept = vec![record.clone()];
        for (row, change) in differs {
            let env = changed(change);
            let other = env.cloud_stack.as_ref().expect("the busy stack draws");
            let (i, other_cuts) = volume_of(other);
            let EntryClass::Volume(other_volume) = &other.entries()[i].class else { panic!("the Cumulus is a volume") };
            let other_record = other_volume.light_record(512, &other_cuts);
            assert!(kept.iter().all(|r| !Arc::ptr_eq(r, &other_record)), "a change of {row} gives a new light record");
            kept.push(other_record);
        }
        let (base, top) = volume.shape().shell_m();
        // (low in the shell: a ray through a cloud's base crosses it before the cloud hides the rest)
        let cut = [base + 0.05 * (top - base)];
        for (what, other_record) in [("the cuts", volume.light_record(512, &cut)), ("the width", volume.light_record(1024, &cuts))] {
            assert!(kept.iter().all(|r| !Arc::ptr_eq(r, &other_record)), "a change of {what} gives a new light record");
            kept.push(other_record);
        }
        // A record of a march cut inside the shell keeps each segment's own march.
        let (mut got, mut own) = ([VolumeMarch::CLEAR; 2], [VolumeMarch::CLEAR; 2]);
        let mut both = 0usize;
        for (x, y) in (0..64usize).map(|i| ((i * 37) % 512, 8 + i)) {
            assert!(kept[kept.len() - 2].read(volume, centre(x, y, 512), &mut got));
            volume.march(centre(x, y, 512), 512, &cut, &mut own);
            assert_eq!(bits(&got), bits(&own), "the record cut at {cut:?}, texel ({x}, {y})");
            both += (own[0].alpha() > 0.0 && own[1].alpha() > 0.0) as usize;
        }
        println!("the record cut at {cut:?} m: {both} of 64 texels read hold cloud on both sides");
        assert!(both >= 4, "{both} texels read held cloud on both sides of the cut");
    }

    /// cloudy_sky's sun (tests/hdri_perf.rs): manual, 30 degrees up at azimuth 200, map seed 1, no older clouds.
    fn sunny() -> HdriParams {
        let mut p = HdriParams::default();
        p.sky.sun.mode = "manual".to_string();
        p.sky.sun.elevation_deg = 30.0;
        p.sky.sun.azimuth_deg = 200.0;
        p
    }

    /// One layer of `genus` (CloudLayerParams::new at its defaults; `cover` and `seed` when given) on the sunny sky: the
    /// map, and the stack its Env built.
    fn one_layer(genus: CloudGenus, cover: Option<f32>, seed: Option<u32>) -> (HdriParams, CloudStack) {
        let mut p = sunny();
        let mut layer = CloudLayerParams::new(genus, &[], p.seed);
        if cover.is_some() {
            layer.cover = cover;
        }
        if let Some(seed) = seed {
            layer.seed = seed;
        }
        p.sky.cloud_stack = Some(CloudStackParams { layers: vec![layer], ..CloudStackParams::default() });
        let stack = Env::new(&p).cloud_stack.expect("one layer draws");
        (p, stack)
    }

    /// 36 azimuths by 12 elevations, 1 to 88 degrees.
    fn sky_dirs() -> Vec<Vec3f> {
        (0..36).flat_map(|a| [1.0f32, 3.0, 6.0, 9.0, 14.0, 20.0, 28.0, 37.0, 47.0, 58.0, 72.0, 88.0].map(move |el| dir_from_az_el(a as f32 * 10.0 + 3.0, el))).collect()
    }

    /// Design §3.1's shares on a modern entry's plane over side x side points 0.731 cells apart, jittered: the share
    /// with any cloud and the share with full cloud (density 1 within 1e-3). A volume's plane is its footprint after
    /// the cover remap (a column there holds cloud), its cell its Size.
    fn plane_shares(entry: &CloudEntry, side: i32) -> (f32, f32) {
        let density = |p: Vec2f| match &entry.class {
            EntryClass::Sheet(sheet) => sheet.density_at(p),
            EntryClass::Slab(slab) => slab.density_at(p),
            EntryClass::Volume(volume) => {
                let cell_m = volume.shape().cell_m();
                volume.shape().footprint_at(p.x * cell_m, p.y * cell_m)
            }
            EntryClass::Classic(_) => panic!("a modern entry"),
            EntryClass::Placed(_) => panic!("a placed volume has no plane"),
        };
        let (mut any, mut full) = (0usize, 0usize);
        for j in 0..side {
            for i in 0..side {
                let p = vec2f(i as f32 * 0.731 + hash01(i, j, 3, 11) * 0.5, j as f32 * 0.731 + hash01(i, j, 4, 11) * 0.5);
                let d = density(p);
                any += (d > 0.0) as usize;
                full += (d >= 0.999) as usize;
            }
        }
        let n = (side * side) as f32;
        (any as f32 / n, full as f32 / n)
    }

    /// What an entry shows along the unit direction `d` in a bake `width` texels wide: a flat entry's shape (the width
    /// does not change it); a cloud volume's unfaded opacity there as its cover, faded at the horizon as its alpha (the
    /// compositor's own fade).
    fn shape_at(entry: &CloudEntry, d: Vec3f, width: usize) -> Shape {
        match &entry.class {
            EntryClass::Volume(volume) => {
                // (normalised as `CloudVolume::cover_toward` normalises: at 1024 its cover is the cover function's)
                let length = d.length();
                let cover = if length > 0.0 { volume.shape().opacity(d / length, width, true) } else { 0.0 };
                let alpha = if cover > 0.0 { cover * horizon_fade(d.y / length) } else { 0.0 };
                Shape { cover, alpha, ..Shape::default() }
            }
            // a placed volume: its opacity, unfaded (its rain and base stand on the horizon: `lit_record` skips the fade)
            EntryClass::Placed(placed) => {
                let cover = placed.opacity(d);
                Shape { cover, alpha: cover, ..Shape::default() }
            }
            _ => entry.flat(d, false).shape,
        }
    }

    /// The types placed by hand (cloud_placed.rs): they stand where their rows put them and have no plane.
    fn placed(genus: CloudGenus) -> bool {
        matches!(genus, CloudGenus::Cumulonimbus | CloudGenus::Lenticular)
    }

    /// A placed volume's extent (the rule for placed volumes, controller 2026-11-01): on an n x n grid of directions over
    /// the sky box `bx` (degrees, `PlacedVolume::sky_box`), the solid angle its opacity covers, and each direction's
    /// opacity.
    fn placed_extent(placed: &PlacedVolume, bx: [f32; 4], n: usize) -> (f32, Vec<f32>) {
        let (daz, del) = ((bx[1] - bx[0]) / n as f32, (bx[3] - bx[2]) / n as f32);
        let (mut covered, mut opacities) = (0.0f32, Vec::with_capacity(n * n));
        for j in 0..n {
            let el = bx[2] + (j as f32 + 0.5) * del;
            for i in 0..n {
                let opacity = placed.opacity(dir_from_az_el(bx[0] + (i as f32 + 0.5) * daz, el));
                if opacity > 0.0 {
                    covered += el.to_radians().cos() * daz.to_radians() * del.to_radians();
                }
                opacities.push(opacity);
            }
        }
        (covered, opacities)
    }

    /// The placed volume of a one-layer stack.
    fn placed_of(stack: &CloudStack) -> &PlacedVolume {
        match &stack.entries()[0].class {
            EntryClass::Placed(placed) => placed,
            _ => panic!("a placed type is a placed volume"),
        }
    }

    /// The lag (cells) at which `field`'s autocorrelation along `step` first falls under one half, over 400 lines.
    fn correlation_length(field: &dyn Fn(Vec2f) -> f32, step: Vec2f) -> f32 {
        let lags: Vec<f32> = (0..400).map(|k| k as f32 * 0.05).collect();
        let lines: Vec<Vec<f32>> = (0..400)
            .map(|i| {
                let start = vec2f(hash01(i, 0, 5, 3) * 400.0, hash01(i, 1, 5, 3) * 400.0);
                lags.iter().map(|&k| field(start + step * k)).collect()
            })
            .collect();
        let all: Vec<f32> = lines.iter().flatten().copied().collect();
        let mean = all.iter().sum::<f32>() / all.len() as f32;
        let variance = all.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / all.len() as f32;
        for (k, &lag) in lags.iter().enumerate() {
            let c = lines.iter().map(|line| (line[0] - mean) * (line[k] - mean)).sum::<f32>() / lines.len() as f32;
            if c < 0.5 * variance {
                return lag;
            }
        }
        lags[lags.len() - 1]
    }

    /// Design §8, for the five types of phase 1, each alone on cloudy_sky's sun: nothing at or below the horizon;
    /// deterministic per seed, and a new seed differs; monotone in Cover, direction by direction; no NaN, degenerate
    /// directions included; Send + Sync; the cover function at least the alpha. A cloud volume (a Cumulus, a
    /// Stratocumulus) by its march at the released width, faded at the horizon as the compositor fades it. And the
    /// signatures: a Cirrus' streaks three times longer along the wind than across, a closed Nimbostratus passing under
    /// 2 % of the key, a Stratus' patches at least twice as long along its wind as across at Streaks 1 and a closed
    /// Stratus of Density 0.2 passing over 30 % of the key, a Cumulus' correlation length within 30 % of its Size and
    /// its bases flatter than its tops, a Stratocumulus' cells spaced as its Size sets them (within 30 %) and its gaps
    /// leaving 0.2 to 0.4 of the upper sky clear at Cover 0.7.
    #[test]
    fn each_type_holds_the_rules() {
        fn shareable<T: Send + Sync>() {}
        shareable::<CloudStack>();
        shareable::<CloudEntry>();
        shareable::<crate::cloud_sheets::Sheet>();
        shareable::<crate::cloud_slabs::Slab>();
        shareable::<CloudVolume>();
        let (sky, discs) = (vec3f(0.31, 0.42, 0.53), Vec3f::all(0.2));
        for genus in CLOUD_GENUS_BUILT {
            let (_, stack) = one_layer(genus, Some(0.5), None);
            let entry = &stack.entries()[0];
            for el in [0.0f32, -0.5, -10.0, -90.0] {
                for az in (0..360).step_by(30) {
                    let d = dir_from_az_el(az as f32, el);
                    let seen = shape_at(entry, d, 1024);
                    assert_eq!((seen.alpha, seen.cover), (0.0, 0.0), "{genus:?} at ({az}, {el})");
                    assert_eq!(entry.cover_toward(d), 0.0, "{genus:?} at ({az}, {el})");
                    assert_eq!(stack.composite(d, sky, discs, &look()), sky + discs, "{genus:?} at ({az}, {el})");
                }
            }
            let (_, again) = one_layer(genus, Some(0.5), None);
            let (_, other) = one_layer(genus, Some(0.5), Some(CloudLayerParams::new(genus, &[], 1).seed + 1));
            if placed(genus) {
                // A placed volume stands in a few degrees of sky: its seed and its Cover are seen on its own extent (the
                // directions of its sky box), not over the whole sky. The same seed draws the same bits, a new seed
                // changes its shape, and a higher Cover makes it bigger (a storm or lens scaled by Cover's root does not
                // hold the smaller one in every direction, so the extent is compared, not each direction).
                let (a, b, c) = (placed_of(&stack), placed_of(&again), placed_of(&other));
                let (ba, bc) = (a.sky_box(), c.sky_box());
                let bx = [ba[0].min(bc[0]), ba[1].max(bc[1]), ba[2].min(bc[2]), ba[3].max(bc[3])];
                let ((_, oa), (_, ob), (_, oc)) = (placed_extent(a, bx, 96), placed_extent(b, bx, 96), placed_extent(c, bx, 96));
                assert!(oa.iter().zip(&ob).all(|(x, y)| x.to_bits() == y.to_bits()), "{genus:?}: the same seed draws the same bits");
                let covered = oa.iter().filter(|o| **o > 0.0).count();
                let changed = oa.iter().zip(&oc).filter(|(x, y)| (*x - *y).abs() > 0.05).count();
                println!("{genus:?}: a new seed changed {changed} of its {covered} covered directions");
                assert!(covered > 0 && changed as f32 > 0.02 * covered as f32, "{genus:?}: a new seed changed {changed} of its {covered} covered directions");
                let extents: Vec<f32> = [0.1f32, 0.3, 0.5, 0.7, 0.9].iter().map(|&c| { let s = one_layer(genus, Some(c), None).1; let v = placed_of(&s); placed_extent(v, v.sky_box(), 96).0 }).collect();
                assert!(extents.windows(2).all(|w| w[1] > w[0]), "{genus:?}: its extent by Cover {extents:?}");
            } else {
                let mut difference = 0.0f32;
                for d in sky_dirs() {
                    let (a, b, c) = (shape_at(entry, d, 1024), shape_at(&again.entries()[0], d, 1024), shape_at(&other.entries()[0], d, 1024));
                    assert_eq!((a.alpha.to_bits(), a.cover.to_bits(), a.tau.to_bits(), a.tau_sun.to_bits()), (b.alpha.to_bits(), b.cover.to_bits(), b.tau.to_bits(), b.tau_sun.to_bits()), "{genus:?}: the same seed draws the same bits at {d:?}");
                    difference += (a.alpha - c.alpha).abs();
                }
                assert!(difference > 1.0, "{genus:?}: a new seed changed only {difference}");
                let stacks: Vec<CloudStack> = [0.1f32, 0.3, 0.5, 0.7, 0.9].iter().map(|&c| one_layer(genus, Some(c), None).1).collect();
                let mut means = vec![0.0f32; stacks.len()];
                for d in sky_dirs() {
                    let alphas: Vec<f32> = stacks.iter().map(|s| shape_at(&s.entries()[0], d, 1024).alpha).collect();
                    assert!(alphas.windows(2).all(|w| w[1] >= w[0] - 1.0e-7), "{genus:?}: the alpha fell as Cover rose at {d:?}: {alphas:?}");
                    means.iter_mut().zip(&alphas).for_each(|(m, a)| *m += a);
                }
                assert!(means.windows(2).all(|w| w[1] > w[0]), "{genus:?}: the mean alpha by Cover {means:?}");
            }
            let mut dirs = sky_dirs();
            dirs.extend([vec3f(0.0, 1.0, 0.0), Vec3f::default(), vec3f(f32::NAN, 1.0, 0.0), vec3f(1.0, f32::INFINITY, 0.0), vec3f(1.0e-30, 1.0e-30, 0.0)]);
            for d in dirs {
                let unit = if d.is_finite() && d.length() > 0.0 { d.normalize() } else { d };
                let shape = shape_at(entry, unit, 1024);
                let cover = entry.cover_toward(d);
                assert!(shape.alpha.is_finite() && shape.cover.is_finite() && shape.tau.is_finite() && shape.tau_sun.is_finite() && cover.is_finite(), "{genus:?} at {d:?}: {shape:?}, {cover}");
                // A volume (a placed one too) is marched, and its march moves with the last bit of a direction: its cover
                // is compared along the very direction its alpha was taken along.
                let cover = if matches!(entry.class, EntryClass::Volume(_) | EntryClass::Placed(_)) { entry.cover_toward(unit) } else { cover };
                assert!(cover >= shape.alpha, "{genus:?} at {d:?}: the cover {cover} under the alpha {}", shape.alpha);
                let c = stack.composite(unit, sky, discs, &look());
                assert!(c.is_finite() && c.x >= 0.0 && c.y >= 0.0 && c.z >= 0.0, "{genus:?} at {d:?}: {c:?}");
                let held = stack.composite(unit, sky, discs, &StackLook { held: true, cache: None, gpu: VolumeRecords::Attached });
                assert!(held.is_finite(), "{genus:?} held at {d:?}: {held:?}");
            }
        }
        // The Cirrus' streaks run along its wind (from the north: along is +z, across x): three times longer.
        let mut p = sunny();
        let mut layer = CloudLayerParams::new(CloudGenus::Cirrus, &[], p.seed);
        layer.wind_deg = Some(0.0);
        p.sky.cloud_stack = Some(CloudStackParams { layers: vec![layer], ..CloudStackParams::default() });
        let stack = Env::new(&p).cloud_stack.expect("a cirrus");
        let EntryClass::Sheet(sheet) = &stack.entries()[0].class else { panic!("a modern cirrus is a sheet") };
        let field = |q: Vec2f| sheet.density_at(q);
        let (along, across) = (correlation_length(&field, vec2f(0.0, 1.0)), correlation_length(&field, vec2f(1.0, 0.0)));
        assert!(along >= 3.0 * across, "the streaks run {along} cells along the wind and {across} across");
        // A closed Nimbostratus passes under 2 % of the key.
        let clear = luminance(Env::new(&sunny()).sun().expect("the sun is the key").radiance);
        let (closed, _) = one_layer(CloudGenus::Nimbostratus, Some(1.0), None);
        let key = luminance(Env::new(&closed).sun().expect("still the key, only dimmed").radiance);
        assert!(key < 0.02 * clear, "a closed Nimbostratus passes {} of the key", key / clear);
        // A Stratus (the volumes design §2.2, its signature): its patches run along its wind (from the north: along is +z)
        // at least twice as far as across at Streaks 1, and a closed veil of Density 0.2 passes the sun, the key over 0.3
        // of the clear one. Its footprint is read in its mask's wavelengths (an eighth of Size), the lags' unit here. The
        // streaked layer is at the Cover a Stratus with none draws (a new one is made closed, nebulosus: no patches).
        let mut streaked = CloudLayerParams::new(CloudGenus::Stratus, &[], 1);
        streaked.cover = Some(CloudGenus::Stratus.default_cover());
        streaked.wind_deg = Some(0.0);
        streaked.stratus.get_or_insert_with(StratusParams::default).streaks = 1.0;
        let mut p = sunny();
        p.sky.cloud_stack = Some(CloudStackParams { layers: vec![streaked], ..CloudStackParams::default() });
        let stack = Env::new(&p).cloud_stack.expect("a Stratus");
        let EntryClass::Volume(volume) = &stack.entries()[0].class else { panic!("a modern Stratus is a volume") };
        let wave_m = volume.shape().cell_m() / 8.0;
        let field = |q: Vec2f| volume.shape().footprint_at(q.x * wave_m, q.y * wave_m);
        let (along, across) = (correlation_length(&field, vec2f(0.0, 1.0)), correlation_length(&field, vec2f(1.0, 0.0)));
        println!("a Stratus at Streaks 1: its patches run {along} along the wind and {across} across (in wavelengths)");
        assert!(along >= 2.0 * across, "the Stratus' patches run {along} along the wind and {across} across");
        let mut veil = CloudLayerParams::new(CloudGenus::Stratus, &[], 1);
        veil.cover = Some(1.0);
        veil.stratus.get_or_insert_with(StratusParams::default).density = 0.2;
        let mut p = sunny();
        p.sky.cloud_stack = Some(CloudStackParams { layers: vec![veil], ..CloudStackParams::default() });
        let key = luminance(Env::new(&p).sun().expect("the key").radiance);
        println!("a closed Stratus of Density 0.2 passes {:.3} of the key", key / clear);
        assert!(key > 0.3 * clear, "a closed Stratus of Density 0.2 passes {} of the key", key / clear);
        // A Cumulus (design §2.4, §8): one cloud a cell of Size, so its field's correlation length is Size, within
        // 30 %: past the lag where its autocorrelation first falls to zero (a cloud and the gap beside it) the next
        // peak is the next cloud, a cell away. And each cloud stands on a flat base under its own top: the bases spread
        // less than a quarter of the tops' spread.
        for size_km in [1.5f32, 3.0] {
            let mut layer = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
            layer.cover = Some(0.5);
            layer.cumulus.get_or_insert_with(CumulusParams::default).size_km = size_km;
            let mut p = sunny();
            p.sky.cloud_stack = Some(CloudStackParams { layers: vec![layer], ..CloudStackParams::default() });
            let stack = Env::new(&p).cloud_stack.expect("a Cumulus");
            let EntryClass::Volume(volume) = &stack.entries()[0].class else { panic!("a modern Cumulus is a volume") };
            let shape = volume.shape();
            // Columns (opacity, lowest and highest height with cloud) 100 m apart over a 48 km square round the viewer.
            let (side, spacing_km) = (480usize, 0.1f32);
            let columns: Vec<(f32, f32, f32)> = (0..side * side).map(|i| shape.column((i % side) as f32 * spacing_km * 1000.0 - 24_000.0, (i / side) as f32 * spacing_km * 1000.0 - 24_000.0, 48)).collect();
            let field: Vec<f32> = columns.iter().map(|c| c.0).collect();
            let mean = field.iter().sum::<f32>() / field.len() as f32;
            let variance = field.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / field.len() as f32;
            // The autocorrelation along x and z together, at lags 0 to 8 km.
            let rho: Vec<f32> = (0..80usize)
                .map(|lag| {
                    let (mut sum, mut n) = (0.0f64, 0usize);
                    for z in 0..side {
                        for x in 0..side - lag {
                            sum += ((field[z * side + x] - mean) * (field[z * side + x + lag] - mean)) as f64;
                            sum += ((field[x * side + z] - mean) * (field[(x + lag) * side + z] - mean)) as f64;
                            n += 2;
                        }
                    }
                    (sum / n as f64) as f32 / variance
                })
                .collect();
            let zero = rho.iter().position(|&r| r <= 0.0).expect("the field decorrelates within 8 km");
            let next = (zero..(5 * zero / 2).min(rho.len())).max_by(|&a, &b| rho[a].total_cmp(&rho[b])).expect("a window past the zero");
            let length_km = next as f32 * spacing_km;
            let spread = |v: Vec<f32>| {
                let mean = v.iter().sum::<f32>() / v.len().max(1) as f32;
                (v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / v.len().max(1) as f32).sqrt()
            };
            let (bases, tops): (Vec<f32>, Vec<f32>) = columns.iter().filter(|c| c.0 > 0.5).map(|c| (c.1, c.2)).unzip();
            let (base, top) = (spread(bases), spread(tops));
            println!("Cumulus at Size {size_km} km: decorrelated at {:.1} km, correlation length {length_km:.1} km (rho {:.3}); the bases spread {base:.0} m, the tops {top:.0} m", zero as f32 * spacing_km, rho[next]);
            assert!(next + 1 < rho.len() && rho[next] > rho[next - 1] && rho[next] >= rho[next + 1] && rho[next] > 0.03, "Cumulus at Size {size_km} km: no next cloud past the zero at {zero} (rho {:?})", &rho[zero..]);
            assert!((length_km / size_km - 1.0).abs() <= 0.3, "Cumulus at Size {size_km} km: a correlation length of {length_km} km");
            assert!(base < 0.25 * top, "Cumulus at Size {size_km} km: the bases spread {base} m, the tops {top} m");
        }
        // A Stratocumulus (design §2.2, §2.4: a lumpy deck of cells): its cells stand 0.07 of its Size apart (175 m at
        // the default 2.5 km; 0.14 until lane sc's round 4, whose references hold hundreds of cells a view overhead), so
        // its columns' optical depth, less its mean over a box round each column (the deck's groups and gaps, kilometres
        // wide, outweigh its cells), decorrelates within a cell and peaks again a cell away; that spacing is held within
        // 30 % of 0.07 Size, at two Sizes (it follows Size). Columns 1/100 of Size apart over a square of 4 Sizes, each
        // integrated straight up, at Cover 0.85 (a deck, few gaps).
        for size_km in [2.5f32, 5.0] {
            let mut layer = CloudLayerParams::new(CloudGenus::Stratocumulus, &[], 1);
            layer.cover = Some(0.85);
            layer.stratocumulus.get_or_insert_with(StratocumulusParams::default).size_km = size_km;
            let mut p = sunny();
            p.sky.cloud_stack = Some(CloudStackParams { layers: vec![layer], ..CloudStackParams::default() });
            let stack = Env::new(&p).cloud_stack.expect("a Stratocumulus");
            let EntryClass::Volume(volume) = &stack.entries()[0].class else { panic!("a modern Stratocumulus is a volume") };
            let shape = volume.shape();
            let (side, spacing_m) = (400usize, 10.0 * size_km);
            let half = 0.5 * side as f32 * spacing_m;
            let tau: Vec<f32> = (0..side * side)
                .map(|i| {
                    let opacity = shape.column((i % side) as f32 * spacing_m - half, (i / side) as f32 * spacing_m - half, 32).0;
                    -(1.0 - opacity.min(0.999_99)).ln()
                })
                .collect();
            // Less its mean over the square a cell wide round each column (a box, along x and then z, held inside).
            let r = 7usize;
            let boxed = |v: &[f32], along_x: bool| -> Vec<f32> {
                (0..side * side)
                    .map(|i| {
                        let (x, z) = (i % side, i / side);
                        let (a, b) = if along_x { (x, z) } else { (z, x) };
                        let (lo, hi) = (a.saturating_sub(r), (a + r).min(side - 1));
                        let at = |k: usize| if along_x { v[b * side + k] } else { v[k * side + b] };
                        (lo..=hi).map(at).sum::<f32>() / (hi - lo + 1) as f32
                    })
                    .collect()
            };
            let mean_box = boxed(&boxed(&tau, true), false);
            let field: Vec<f32> = tau.iter().zip(&mean_box).map(|(t, m)| t - m).collect();
            let mean = field.iter().sum::<f32>() / field.len() as f32;
            let variance = field.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / field.len() as f32;
            let rho: Vec<f32> = (0..40usize)
                .map(|lag| {
                    let (mut sum, mut n) = (0.0f64, 0usize);
                    for z in 0..side {
                        for x in 0..side - lag {
                            sum += ((field[z * side + x] - mean) * (field[z * side + x + lag] - mean)) as f64;
                            sum += ((field[x * side + z] - mean) * (field[(x + lag) * side + z] - mean)) as f64;
                            n += 2;
                        }
                    }
                    (sum / n as f64) as f32 / variance
                })
                .collect();
            let zero = rho.iter().position(|&r| r <= 0.0).expect("the deck's cells decorrelate within 0.4 Size");
            let next = (zero..(3 * zero).min(rho.len())).max_by(|&a, &b| rho[a].total_cmp(&rho[b])).expect("a window past the zero");
            let cells_m = next as f32 * spacing_m;
            println!("Stratocumulus at Size {size_km} km: decorrelated at {:.0} m, its cells {cells_m:.0} m apart (rho {:.3}); 0.07 Size is {:.0} m", zero as f32 * spacing_m, rho[next], 70.0 * size_km);
            assert!(next + 1 < rho.len() && rho[next] > rho[next - 1] && rho[next] >= rho[next + 1], "Stratocumulus at Size {size_km} km: no next cell past the zero at {zero} (rho {:?})", &rho[zero..]);
            assert!((cells_m / (70.0 * size_km) - 1.0).abs() <= 0.3, "Stratocumulus at Size {size_km} km: cells {cells_m} m apart");
        }
        // Its gaps (plan: Stratocumulus' signature): at Cover 0.7 the share of the sky above 20 degrees under half opaque
        // (released width) lies within 0.2 to 0.4, the mean over four seeds.
        let low = 20.0f32.to_radians().sin();
        let dirs: Vec<Vec3f> = (0..2048)
            .map(|k| {
                let y = low + (1.0 - low) * hash01(k, 1, 0, 0x6a95);
                let a = std::f32::consts::TAU * hash01(k, 2, 0, 0x6a95);
                let r = (1.0 - y * y).max(0.0).sqrt();
                vec3f(r * a.sin(), y, -r * a.cos())
            })
            .collect();
        let mut clear = 0.0f32;
        for seed in 41u32..45 {
            let (_, stack) = one_layer(CloudGenus::Stratocumulus, Some(0.7), Some(seed));
            let EntryClass::Volume(volume) = &stack.entries()[0].class else { panic!("a modern Stratocumulus is a volume") };
            clear += dirs.iter().filter(|&&d| volume.shape().opacity(d, 1024, true) <= 0.5).count() as f32 / dirs.len() as f32;
        }
        let clear = clear / 4.0;
        println!("Stratocumulus at Cover 0.7: {clear:.3} of the sky above 20 degrees clear");
        assert!((0.2..=0.4).contains(&clear), "a Stratocumulus at Cover 0.7 leaves {clear} of the upper sky clear");
    }

    /// Design §3.4, D3: per type, the share of its plane with any cloud at Cover 1e-4, 1e-3, 1e-2 and 0.1 lies in [0.5 c,
    /// 2 c], and rises with c (4M points for the two smallest covers, 1M for the others).
    /// A placed type has no plane (the rule for placed volumes, controller 2026-11-01): Cover 0 builds nothing, and a small
    /// Cover a small storm or lens that grows with it, its extent (the solid angle its opacity covers) rising with Cover
    /// from 1e-4 to 1.
    #[test]
    fn cover_rises_from_zero() {
        for genus in CLOUD_GENUS_BUILT {
            if placed(genus) {
                let mut p = sunny();
                let mut layer = CloudLayerParams::new(genus, &[], p.seed);
                layer.cover = Some(0.0);
                p.sky.cloud_stack = Some(CloudStackParams { layers: vec![layer], ..CloudStackParams::default() });
                assert!(Env::new(&p).cloud_stack.is_none(), "{genus:?} at Cover 0 builds nothing");
                let mut last = 0.0f32;
                for c in [1.0e-4f32, 1.0e-3, 1.0e-2, 0.1, 0.5, 1.0] {
                    let (_, stack) = one_layer(genus, Some(c), None);
                    let volume = placed_of(&stack);
                    let (extent, _) = placed_extent(volume, volume.sky_box(), 96);
                    println!("{genus:?} at Cover {c}: {extent:.3e} sr");
                    assert!(extent > last, "{genus:?} at Cover {c}: an extent of {extent} sr after {last}");
                    last = extent;
                }
                continue;
            }
            let mut last = 0.0f32;
            for c in [1.0e-4f32, 1.0e-3, 1.0e-2, 0.1] {
                let (_, stack) = one_layer(genus, Some(c), None);
                let (any, _) = plane_shares(&stack.entries()[0], if c <= 1.0e-3 { 2000 } else { 1000 });
                assert!(any >= 0.5 * c && any <= 2.0 * c, "{genus:?} at Cover {c}: a share of {any} with cloud");
                assert!(any > last, "{genus:?}: the share fell to {any} at {c}");
                last = any;
            }
        }
    }

    /// Design §3.4: per type at Cover 0.4, the share of the upper sky with any cloud and the share it hides (by solid
    /// angle over the texel centres) agree within 0.02 at 512, 1024 and 4096 wide. A cloud volume (a Cumulus) is
    /// marched at each width, as a bake of that width marches it; its rows are spread over the machine's threads.
    #[test]
    fn cover_is_the_same_share_at_every_width() {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        for genus in CLOUD_GENUS_BUILT {
            let (_, stack) = one_layer(genus, Some(0.4), None);
            let entry = &stack.entries()[0];
            let shares: Vec<(f32, f32)> = [512usize, 1024, 4096]
                .iter()
                .map(|&width| {
                    let height = width / 2;
                    // (any, hidden, total) of each row, weighted by its solid angle
                    let row = |y: usize| {
                        let v = (y as f32 + 0.5) / height as f32;
                        let weight = (std::f32::consts::PI * v).sin() as f64;
                        let (mut any, mut hidden, mut total) = (0.0f64, 0.0f64, 0.0f64);
                        for x in 0..width {
                            let d = vec(ibl::equirect_uv_to_dir([(x as f32 + 0.5) / width as f32, v]));
                            let shape = shape_at(entry, d, width);
                            any += weight * (shape.cover > 0.0) as u32 as f64;
                            hidden += weight * shape.alpha as f64;
                            total += weight;
                        }
                        (any, hidden, total)
                    };
                    let rows: Vec<(f64, f64, f64)> = std::thread::scope(|scope| {
                        let spawned: Vec<_> = (0..threads).map(|t| scope.spawn(move || (t..height / 2).step_by(threads).map(row).collect::<Vec<_>>())).collect();
                        spawned.into_iter().flat_map(|h| h.join().expect("a row thread")).collect()
                    });
                    let (any, hidden, total) = rows.iter().fold((0.0f64, 0.0f64, 0.0f64), |a, r| (a.0 + r.0, a.1 + r.1, a.2 + r.2));
                    ((any / total) as f32, (hidden / total) as f32)
                })
                .collect();
            println!("{genus:?}: any cloud and hidden at 512, 1024, 4096: {shares:?}");
            for (width, (any, hidden)) in [1024usize, 4096].iter().zip(&shares[1..]) {
                assert!((any - shares[0].0).abs() < 0.02 && (hidden - shares[0].1).abs() < 0.02, "{genus:?} at {width}: ({any}, {hidden}) against 512's {:?}", shares[0]);
            }
        }
    }

    /// Design §2.2: an opaque low sheet hides a high one (a closed Nimbostratus under a Cirrus that shades nothing:
    /// the Cirrus is never reached, bit for bit); a sheet inside a marched shell cuts it, so an opaque Stratus under the
    /// shell hides the whole volume and one inside it hides only the part above it.
    #[test]
    fn layers_composite_by_distance() {
        let (sky, none) = (vec3f(0.31, 0.42, 0.53), Vec3f::default());
        let closed = {
            let mut l = CloudLayerParams::new(CloudGenus::Nimbostratus, &[], 1);
            l.cover = Some(1.0);
            l
        };
        let mut cirrus = CloudLayerParams::new(CloudGenus::Cirrus, &[closed.clone()], 1);
        cirrus.shade_below = Some(0.0);
        let stack_of = |layers: Vec<CloudLayerParams>| {
            let mut p = sunny();
            p.sky.cloud_stack = Some(CloudStackParams { layers, ..CloudStackParams::default() });
            Env::new(&p).cloud_stack.expect("a stack")
        };
        let (both, mut alone) = (stack_of(vec![closed.clone(), cirrus]), stack_of(vec![closed]));
        // The Cirrus above dims the dome that lights the Nimbostratus whatever it shades below (design §2.3's ambient
        // cascade), so the Nimbostratus alone is given the light it gets under the Cirrus: what is compared is the walk.
        assert!(both.entries[0].genus == CloudGenus::Nimbostratus && alone.entries[0].genus == CloudGenus::Nimbostratus);
        alone.lights[0] = both.lights[0];
        let bits = |v: Vec3f| (v.x.to_bits(), v.y.to_bits(), v.z.to_bits());
        for d in sky_dirs().into_iter().filter(|d| d.y > 0.13) {
            assert_eq!(bits(both.composite(d, sky, none, &look())), bits(alone.composite(d, sky, none, &look())), "the Cirrus shows through a closed Nimbostratus at {d:?}");
        }
        // A volume lifted from Clear noon (1500..4000 m), an opaque flat deck under it or inside it: a classic Stratus
        // deck at full coverage (the Stratus and the Nimbostratus this case used are marched volumes now, the volumes
        // design §2.2, and no modern type is a flat opaque slab any more; overlapping volumes are the joint march's),
        // and a thin modern Cirrus at 10 km, so the stack is not all classic and its walk ends early as a modern one.
        let mut volume_sky = preset("Clear noon").unwrap();
        volume_sky.sky.clouds.coverage = 0.5;
        volume_sky.sky.volumetric = Some(VolumetricParams::default());
        assert!(volume_sky.lift_clouds());
        let with_slab = |base_m: f32| {
            let mut p = volume_sky.clone();
            let layers = &mut p.sky.cloud_stack.as_mut().unwrap().layers;
            let mut slab = CloudLayerParams::new(CloudGenus::Stratus, layers, p.seed);
            slab.classic = Some(crate::ClassicLook { coverage: 1.0, sharpness: 1.0, light_m: base_m });
            slab.stratus.get_or_insert_with(crate::StratusParams::default).base_m = base_m;
            layers.push(slab);
            let mut high = CloudLayerParams::new(CloudGenus::Cirrus, layers, p.seed);
            high.cover = Some(0.2);
            high.cirrus.get_or_insert_with(crate::CirrusParams::default).base_m = 10_000.0;
            layers.push(high);
            (Env::new(&p).cloud_stack.expect("a stack"), p)
        };
        let (under, _) = with_slab(1400.0);
        let (inside, inside_p) = with_slab(2500.0);
        assert_eq!(inside.cuts, vec![2500.0], "the slab cuts the march");
        assert!(matches!(inside.plan.as_slice(), [Step::Segment(0), Step::Flat(_), Step::Segment(1), Step::Flat(_), Step::Flat(_)]), "{:?}", inside.plan);
        assert!(matches!(under.plan.as_slice(), [Step::Flat(_), Step::Segment(0), Step::Flat(_), Step::Flat(_)]) && under.cuts.is_empty(), "{:?}", under.plan);
        // Under the shell the opaque slab ends the walk before the march: through a cache, not one row is marched.
        let cache = under.cache_for(64).expect("the volume marches");
        for d in sky_dirs().into_iter().filter(|d| d.y > 0.26) {
            let c = under.composite(d, sky, none, &StackLook { held: false, cache: Some(&cache), gpu: VolumeRecords::Attached });
            assert!(c.is_finite(), "{c:?}");
        }
        assert_eq!(cache.rows_marched(), 0, "an opaque sheet under the shell hides the whole volume");
        // Inside it, the volume's lower part stands in front of the slab, and the rest is hidden behind it. The slab's
        // and the Cirrus' light do not depend on the volume below them (the light pass reads what is above).
        let mut without = inside_p.clone();
        without.sky.cloud_stack.as_mut().unwrap().layers.retain(|l| l.genus != "cumulus");
        let no_volume = Env::new(&without).cloud_stack.expect("the slab and the Cirrus");
        let m = inside.marched.expect("the lifted volume marches");
        let volume = inside.entries[m].volume().expect("the marched entry's volume");
        let mut shown = 0;
        for d in sky_dirs().into_iter().filter(|d| d.y > 0.26) {
            let mut segments = [CloudMarch::default(); 2];
            volume.march_split(d, &[2500.0], &mut segments);
            let s = segments[0];
            let light = &inside.lights[m];
            let below = (light.sun * s.sun + light.ambient * s.ambient + light.ground * s.bounce) * inside.entries[m].brightness;
            let want = below + no_volume.composite(d, sky, none, &look()) * (1.0 - s.alpha);
            let got = inside.composite(d, sky, none, &look());
            assert!((got - want).length() <= 1.0e-4 * want.length().max(1.0e-6), "inside the shell at {d:?}: {got:?} against {want:?}");
            shown += (s.alpha > 0.05) as usize;
        }
        assert!(shown > 20, "the volume's lower part shows in front of the slab along {shown} directions");
    }

    /// Design §8: every recorded older case but X2's three (the seventeen presets among them), its None form
    /// against its lift: the 256 x 128 bake, the keyless copy, the key and ground_gain within 1e-5 relative (against 1e-6
    /// for the darkest values), weather included. Beside them, Clear noon with a lightning layer: its lift keeps the
    /// whole flash under the thin cirrus.
    #[test]
    fn the_lift_keeps_the_look() {
        let serial = |n: usize, f: &(dyn Fn(usize) + Sync)| (0..n).for_each(f);
        let near = |a: f32, b: f32| (a - b).abs() <= 1.0e-5 * a.abs().max(b.abs()).max(1.0e-6);
        let near3 = |a: Vec3f, b: Vec3f| near(a.x, b.x) && near(a.y, b.y) && near(a.z, b.z);
        let first_off = |a: &EnvMap, b: &EnvMap| (0..a.data.len()).find(|&i| !(0..3).all(|k| near(a.data[i][k], b.data[i][k])));
        let (mut off, mut compared) = (Vec::new(), 0usize);
        let mut lightning = crate::presets::preset("Clear noon").unwrap();
        lightning.lights.push(crate::LightParams { kind: Some("lightning".to_string()), ..crate::LightParams::default() });
        let beside = ("lightning_over_clear_noon".to_string(), lightning, crate::image_light::ImageSet::default());
        for (name, older, images) in crate::cloud_baseline::clouds_baseline_cases().into_iter().chain([beside]) {
            if name.starts_with("x2_") {
                continue;
            }
            compared += 1;
            let mut lifted = older.clone();
            lifted.lift_clouds();
            let (a, b) = (Env::new_with(&older, &images), Env::new_with(&lifted, &images));
            let (ma, mb) = (a.bake(256), b.bake(256));
            if let Some(i) = first_off(&ma, &mb) {
                off.push(format!("{name}: texel {i}: {:?} against {:?}", ma.data[i], mb.data[i]));
            }
            match (a.sun(), b.sun()) {
                (Some(ka), Some(kb)) => {
                    if !near3(ka.radiance, kb.radiance) || (ka.dir - kb.dir).length() > 1.0e-6 {
                        off.push(format!("{name}: key {ka:?} against {kb:?}"));
                    }
                    match (a.bake_keyless_par(&ma, serial), b.bake_keyless_par(&mb, serial)) {
                        (Some(la), Some(lb)) => {
                            if let Some(i) = first_off(&la, &lb) {
                                off.push(format!("{name}: keyless texel {i}: {:?} against {:?}", la.data[i], lb.data[i]));
                            }
                        }
                        (None, None) => {}
                        _ => off.push(format!("{name}: a keyless copy on one side only")),
                    }
                }
                (None, None) => {}
                (ka, kb) => off.push(format!("{name}: key {ka:?} against {kb:?}")),
            }
            if !near3(a.ground_gain, b.ground_gain) {
                off.push(format!("{name}: ground_gain {:?} against {:?}", a.ground_gain, b.ground_gain));
            }
        }
        assert_eq!(compared, 42, "every recorded older case but X2's three, and the lightning");
        assert!(off.is_empty(), "{off:#?}");
    }

    /// Design §1.2: a lifted layer stays classic, drawn by today's code, through the rows today's code draws (a new name
    /// keeps every bit; Base keeps it classic and moves it), and a row of the modern look drops it: the layer then draws
    /// as a modern layer of its type at the Cover it showed, any cloud on at least 0.8 of that share of its plane and full
    /// cloud on at most 1.3 of it.
    #[test]
    fn a_lifted_layer_keeps_its_look_until_its_own_look_changes() {
        let mut lifted = preset("Golden hour").unwrap();
        assert!(lifted.lift_clouds());
        let looked = Env::new(&lifted).bake(128);
        let cirrus = lifted.sky.cloud_stack.as_ref().unwrap().layers.iter().position(|l| l.genus == "cirrus").expect("Golden hour's cirrus");
        let edit = |key: &str, change: &dyn Fn(&mut CloudLayerParams)| {
            let mut p = lifted.clone();
            let layer = &mut p.sky.cloud_stack.as_mut().unwrap().layers[cirrus];
            change(layer);
            layer.edited(key);
            p
        };
        let classic_in = |env: &Env| env.cloud_stack.as_ref().unwrap().entries().iter().find(|e| e.layer() == cirrus).expect("the cirrus is built").is_classic();
        let renamed = Env::new(&edit("name", &|l| l.name = "Wisps".to_string()));
        assert!(classic_in(&renamed), "Name keeps the classic look");
        assert_eq!(renamed.bake(128), looked, "a new name keeps every bit");
        let raised = Env::new(&edit("base_m", &|l| l.cirrus.get_or_insert_with(CirrusParams::default).base_m = 11_000.0));
        assert!(classic_in(&raised), "Base keeps the classic look");
        assert_ne!(raised.bake(128), looked, "and moves it");
        let shown = lifted.sky.cloud_stack.as_ref().unwrap().layers[cirrus].shown_cover();
        let finer = edit("fineness", &|l| l.cirrus.get_or_insert_with(CirrusParams::default).fineness = 0.9);
        let layer = &finer.sky.cloud_stack.as_ref().unwrap().layers[cirrus];
        assert!(layer.classic.is_none(), "Fineness drops the classic look");
        assert!((layer.shown_cover() - shown).abs() < 1.0e-6, "at the Cover it showed: {} against {shown}", layer.shown_cover());
        let env = Env::new(&finer);
        assert!(!classic_in(&env), "drawn as a modern Cirrus");
        assert_ne!(env.bake(128), looked);
        let entry = env.cloud_stack.as_ref().unwrap().entries().iter().find(|e| e.layer() == cirrus).unwrap();
        let (any, full) = plane_shares(entry, 1000);
        assert!(any >= 0.8 * shown && full <= 1.3 * shown, "Cover {shown}: any cloud {any}, full cloud {full}");
    }

    /// Design §3.3's gates, through what was built on this thread (`built_on_this_thread`): gate 1 builds nothing at
    /// all (no stack, a master Cover of 0, every layer hidden, every amount 0); gate 2 skips a hidden layer, a type this
    /// build does not draw (kept), a name it does not know and an amount of 0, and builds the rest; gate 3 gives the
    /// sky its bits wherever no entry draws, and below the horizon.
    #[test]
    fn the_gates_build_nothing() {
        let golden = preset("Golden hour").unwrap();
        let lifted = legacy_cloud_layers(&golden.sky);
        assert_eq!(lifted.layers.len(), 2, "a deck and a cirrus: {:?}", lifted.layers);
        let build = |stack: Option<CloudStackParams>| {
            let mut sky = golden.sky.clone();
            sky.cloud_stack = stack;
            CloudStack::build(&sky, golden.seed, &inputs(), &|| false, &crate::caster::serial_rows).expect("nothing cancels")
        };
        let before = built_on_this_thread();
        assert!(build(None).is_none(), "no stack");
        assert!(build(Some(CloudStackParams { cover: 0.0, ..lifted.clone() })).is_none(), "master Cover 0");
        let mut hidden = lifted.clone();
        hidden.layers.iter_mut().for_each(|layer| layer.enabled = false);
        assert!(build(Some(hidden)).is_none(), "every layer hidden");
        let mut empty = lifted.clone();
        empty.layers.iter_mut().for_each(|layer| layer.classic.as_mut().unwrap().coverage = 0.0);
        assert!(build(Some(empty)).is_none(), "every amount 0");
        assert!(build(Some(CloudStackParams { layers: Vec::new(), ..lifted.clone() })).is_none(), "no layers");
        assert_eq!(built_on_this_thread(), before, "gate 1 built and measured nothing");
        // Gate 2: the deck is built; the hidden cirrus, an unbuilt type, an unknown one and a deck of amount 0 are not.
        let mut mixed = lifted.clone();
        let deck = mixed.layers.iter().position(|l| l.genus != "cirrus").unwrap();
        let cirrus = mixed.layers.iter().position(|l| l.genus == "cirrus").unwrap();
        mixed.layers[cirrus].enabled = false;
        let mut nothing = mixed.layers[deck].clone();
        nothing.classic.as_mut().unwrap().coverage = 0.0;
        mixed.layers.push(nothing);
        mixed.layers.push(CloudLayerParams { genus: "cirrocumulus".to_string(), ..CloudLayerParams::default() });
        mixed.layers.push(CloudLayerParams { genus: "nebulous".to_string(), ..CloudLayerParams::default() });
        let stack = build(Some(mixed)).expect("the deck draws");
        assert_eq!(stack.entries().len(), 1);
        assert_eq!(stack.entries()[0].layer(), deck);
        assert_eq!(built_on_this_thread(), (before.0 + 1, before.1), "one entry, and a classic deck measures no table");
        // Gate 3: below the horizon, and where the deck has a gap, the sky and the discs come back with their bits.
        let (sky, discs) = (vec3f(0.31, 0.42, 0.53), Vec3f::all(0.7));
        let sum = sky + discs;
        let bits = |v: Vec3f| (v.x.to_bits(), v.y.to_bits(), v.z.to_bits());
        for el in [0.0f32, -0.5, -30.0] {
            assert_eq!(bits(stack.composite(dir_from_az_el(40.0, el), sky, discs, &look())), bits(sum));
        }
        let gap = (0..2000)
            .map(|i| dir_from_az_el(i as f32 * 7.3, 10.0 + (i % 70) as f32))
            .find(|&d| stack.entries()[0].flat(d, false).shape.cover == 0.0)
            .expect("the 0.15 deck has gaps");
        assert_eq!(bits(stack.composite(gap, sky, discs, &look())), bits(sum), "no arithmetic where nothing draws");
    }

    /// Relative closeness for radiance, against 1e-6 for the darkest values.
    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1.0e-5 * a.abs().max(b.abs()).max(1.0e-6)
    }

    /// A lifted sky draws as its older form (design §1.2). Clear noon, Golden hour, Overcast and a volume (Clear noon at
    /// coverage 0.5 under the default volume), no weather: the composite (`sky_layers`) along 2400 upper directions and
    /// 64 round the sun's disc, and the upper half of the 128-wide bake (the disc's refinement included), within 1e-5
    /// relative. The lower half and the keyless copy also read ground_gain and the key, which a stack does not feed yet,
    /// so they are left out here.
    #[test]
    fn a_lifted_sky_draws_as_its_older_form() {
        let mut volume = preset("Clear noon").unwrap();
        volume.sky.clouds.coverage = 0.5;
        volume.sky.volumetric = Some(VolumetricParams::default());
        let cases: [(&str, HdriParams); 4] = [
            ("Clear noon", preset("Clear noon").unwrap()),
            ("Golden hour", preset("Golden hour").unwrap()),
            ("Overcast", preset("Overcast").unwrap()),
            ("volume", volume),
        ];
        for (name, older) in cases {
            let mut lifted = older.clone();
            assert!(lifted.lift_clouds(), "{name} lifts");
            let (a, b) = (Env::new(&older), Env::new(&lifted));
            assert!(b.cloud_stack.is_some() && b.clouds.is_none() && b.volumetric.is_none(), "{name}: the stack draws");
            assert!(b.cloud_stack.as_ref().unwrap().all_classic(), "{name}: a lift is classic");
            let (atmo_a, atmo_b) = (a.atmo.as_ref().unwrap(), b.atmo.as_ref().unwrap());
            let sun = atmo_a.sun_dir();
            let mut dirs: Vec<Vec3f> = (0..2400).map(|i| dir_from_az_el((i as f32 * 2.47) % 360.0, 0.5 + (i % 80) as f32 * 1.12)).collect();
            for k in 0..64 {
                let off = vec3f(((k * 37) % 17) as f32 - 8.0, ((k * 53) % 13) as f32 - 6.0, ((k * 29) % 11) as f32 - 5.0) * 4.0e-4;
                dirs.push((sun + off).normalize());
            }
            for d in dirs {
                let (x, y) = (a.sky_layers(d, atmo_a, None, Sampling::default()), b.sky_layers(d, atmo_b, None, Sampling::default()));
                assert!(close(x.x, y.x) && close(x.y, y.y) && close(x.z, y.z), "{name} along {d:?}: {x:?} against {y:?}");
            }
            let (ma, mb) = (a.bake(128), b.bake(128));
            for row in 0..ma.height / 2 {
                for col in 0..ma.width {
                    let (p, q) = (ma.data[row * ma.width + col], mb.data[row * mb.width + col]);
                    assert!(close(p[0], q[0]) && close(p[1], q[1]) && close(p[2], q[2]), "{name} texel ({col}, {row}): {p:?} against {q:?}");
                }
            }
        }
    }
}
