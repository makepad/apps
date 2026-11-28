//! The cloud volumes' GPU march, the engine's half (design §12.3, §12.11 item 4; task-13 study §3): what a cloud
//! volume hands the GPU to march, and what comes back.
//!
//! - **The ask** ([`CloudAsk`]). An export at least GPU_FROM_WIDTH wide whose stack holds a volume asks the app for its
//!   records (`Env::cloud_ask`), and so does the in-app preview (`Env::cloud_ask_in`, the app's choice of widths and
//!   mode). Each volume's [`VolumeUpload`] is plain data built on the asking job: its parts ([`GpuPart`]), each two
//!   linear float buffers BUFFER_WIDTH texels a row (scalars, and RGBA vectors) and the uniforms that point into them
//!   (the footprint grid with its max pyramid, the noise tiles, the tables with the blue noise, the near bits, tau_up,
//!   the sun-depth grid), and the volume's own numbers its march reads as named uniforms (each module writes its own:
//!   `gpu_layout`). The GPU crate (`makepad-cloud-gpu`) moves each part's buffers into textures and draws its shader,
//!   whose functions mirror the CPU march's.
//! - **Resident parts** (design §12.11 item 4.6, the preview on the GPU): a part's key names everything its data and
//!   its uniforms read, so the GPU keeps the parts it was given between asks, and an ask built while the GPU holds a
//!   part (the caller's `resident`) carries its key alone: a sun drag uploads the sun's grid, a Size drag the footprint
//!   grid and the light grids, a light row nothing.
//! - **The record** ([`GpuRecord`]). Per texel of the map's upper half and per segment of its march, eight halves as two
//!   RGBA16F targets give them: (transmittance, the sun's light low and high with the lobes mixed at the entry's Silver
//!   lining, the mean depth) and (ambient, open side, bounce, the shader's loop passes). Kept as the halves (16 bytes a
//!   texel and segment, 268 MB a segment at 8192) and decoded per read, so the compositor's `lit_record` shades it as it
//!   shades a march ([`GpuRecord::read`] rebuilds a `VolumeMarch` whose lobes are equal).
//! - **The hand-off** (`Env::attach_cloud_records`): all or nothing (design §12.11 item 4.5). Every volume's record must
//!   be there under its key, or none is kept and the export marches on the CPU, so no GPU/CPU seam shows in one map.

use makepad_draw::*;
use makepad_half::f16;
use makepad_render_material::ibl;
use crate::arr;
use crate::cloud_volume_light::VolumeMarch;
use std::sync::Arc;

/// Exports this wide and wider ask for the GPU: every export size (1K, 2K, 4K, 8K) draws the look the preview shows,
/// and the CPU marches an export's clouds only as the fallback (ruled after the landing, 2026-11-01; design §12.11
/// item 4.6 had started with the 4096 and 8192 exports).
pub const GPU_FROM_WIDTH: usize = 1024;
/// The linear buffers' row: a texel index i is at (i % BUFFER_WIDTH, i / BUFFER_WIDTH).
pub const BUFFER_WIDTH: usize = 4096;
// The shader's buffer reads take a texel's row and column with a shift and a mask (`& 4095u`, `>> 12u`).
const _: () = assert!(BUFFER_WIDTH == 4096, "the shader's buffer reads are literal: 4096 texels a row");
/// The most rows a part's buffer takes: a texture's height on D3D11. An ask with a part past it is not made, and the
/// map marches on the CPU (logged; the sun's grid grows toward a low sun, task-13 review M4).
pub const MAX_ROWS: usize = 16384;
/// Segments the shader marches a texel: two RGBA16F targets each, two segments a pass (a pass has at most four
/// targets), so a band of three or four segments is drawn twice. A volume cut into more is marched on the CPU
/// (task-13 study §3.2, logged).
pub const PASS_SEGMENTS: usize = 4;
/// The halves a texel and segment keep.
pub const HALVES: usize = 8;

/// What the shader's march does where it differs from the CPU on purpose (design §12.10 item 4, task-13 study §3.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuMode {
    /// The CPU's march: the hybrid sun (two live steps, the grid), no fine detail. The GPU-against-CPU tests' mode.
    Cpu = 0,
    /// The export's: the ported renderer (look lane b; `makepad-cloud-gpu`'s export shaders, one a cloud type, design
    /// §12.13): the type's own density on this layout, a live sun march, fine detail. What `Env::cloud_ask` asks for, and the preview's asks (design
    /// §12.12 items 1-2: the GPU look; the CPU path, the fallback, draws the CPU Cumulus and differs by design).
    Export = 1,
}

/// The mode an export's asks march in (`Env::cloud_ask`, `Env::attach_cloud_records`): the export mode (design §12.12
/// item 1).
pub const EXPORT_MODE: GpuMode = GpuMode::Export;

/// Who asks (task-14 review N5): an export's ask and the preview's never share a key, so neither waits for the other's
/// march nor takes its stall rule or its failure, even while both march in one mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskFor {
    Export = 1,
    Preview = 2,
}

/// Which records a lookup reads for the cloud volumes: those attached to the map, these (one a volume, in the stack's
/// order: an antialiasing bake's, marched at its own jittered directions, design §12.1 item 10), or none (every volume
/// marched on the CPU).
#[derive(Clone, Copy, Default)]
pub enum VolumeRecords<'a> {
    #[default]
    Attached,
    These(&'a [Arc<GpuRecord>]),
    Cpu,
}

/// A half's float (the records' and the readback's bits, little-endian u16).
pub fn half_to_f32(h: u16) -> f32 {
    f16::from_bits(h).to_f32()
}

/// An offset into a buffer as the shader reads it from a float: exact under 2^24.
pub(crate) fn index(offset: usize) -> f32 {
    assert!(offset < 1 << 24, "a GPU buffer offset of {offset} is past a float's exact integers");
    offset as f32
}

/// FNV-1a over `words`' bytes: the parts' and the asks' keys.
pub(crate) fn key_of(words: &[u64]) -> u64 {
    words.iter().flat_map(|w| w.to_le_bytes()).fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// The parts of a volume's upload (design §12.11 item 4.6): each one's buffers are their own textures on the GPU, kept
/// between asks under its key, which names everything its data and its uniforms read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PartKind {
    /// The footprint grid (`FootprintGrid::gpu_layout`): its levels' nodes, its max pyramid and their tables as vectors,
    /// the strengths as scalars; the uniforms `fgrid`, `fgrid2`. Its key is the grid's: Size, the seed, the wind and
    /// the shell's heights, not Cover (Cover moves the threshold, a volume uniform).
    Grid,
    /// The noise tiles (`Tile3::gpu_texels`), vectors; the uniform `tiles`, and in the export mode the shape tile and
    /// `tiles2` (look-b review M3). The map's seed and the mode.
    Tiles,
    /// The tables (the columns' tops, the base's rise, the billows' levels, the blue noise's ranks), scalars; the
    /// uniform `tables`. Keyed by their content.
    Tables,
    /// The near bits (`VolumeShape::inside_at_near_bits`), scalars; the CPU mode's only. The shell's heights and the
    /// width.
    Near,
    /// tau_up (`TauUpGrid::gpu_layout`), scalars; `tau_up_a`, `tau_up_b`; the CPU mode's only. The tau_up grid's key:
    /// the shape, Cover and the seed.
    TauUp,
    /// The sun-depth grid (`SunDepthGrid::gpu_layout`): its levels' nodes as scalars, their rows as vectors; the sun's
    /// frame and the levels' uniforms. Its levels' keys: the sun, the shape, Cover and the seed.
    Sun,
}

impl PartKind {
    /// Every part, in the order an upload holds them (an export-mode upload has no Near and no TauUp).
    pub const ALL: [PartKind; 6] = [PartKind::Grid, PartKind::Tiles, PartKind::Tables, PartKind::Near, PartKind::TauUp, PartKind::Sun];
}

/// The key of a part `kind` of a field keyed `key`.
pub(crate) fn part_key(kind: PartKind, key: u64) -> u64 {
    key_of(&[0x7061_7274 + kind as u64, key])
}

/// A part's data: its two buffers padded to whole rows of BUFFER_WIDTH (`scalars` one float a texel, `vectors` four),
/// their rows (0 for a buffer it does not have), and the uniforms that point into them.
pub struct PartData {
    pub scalars: Vec<f32>,
    pub scalar_rows: usize,
    pub vectors: Vec<f32>,
    pub vector_rows: usize,
    pub uniforms: Vec<(&'static str, [f32; 4])>,
}

impl PartData {
    /// The bytes its buffers hold.
    pub fn bytes(&self) -> usize {
        (self.scalars.len() + self.vectors.len()) * 4
    }
}

/// A part of a volume's upload: its kind, its key and its data, or None where the ask was built counting on the GPU
/// holding it (the renderer's resident copy, its uniforms with it).
pub struct GpuPart {
    pub kind: PartKind,
    pub key: u64,
    pub data: Option<PartData>,
}

/// Builds a part's data, or a volume's own uniforms (each module's `gpu_layout` adds its own data and uniforms).
#[derive(Default)]
pub struct UploadBuilder {
    scalars: Vec<f32>,
    vectors: Vec<[f32; 4]>,
    uniforms: Vec<(&'static str, [f32; 4])>,
}

impl UploadBuilder {
    /// Appends `data` to the scalar buffer: its first texel's index.
    pub(crate) fn scalars(&mut self, data: impl IntoIterator<Item = f32>) -> usize {
        let at = self.scalars.len();
        self.scalars.extend(data);
        at
    }

    /// Appends `data` to the vector buffer: its first texel's index.
    pub(crate) fn vectors(&mut self, data: impl IntoIterator<Item = [f32; 4]>) -> usize {
        let at = self.vectors.len();
        self.vectors.extend(data);
        at
    }

    /// Sets the uniform `name` (the shader's `uniform(vec4)` of that name).
    pub(crate) fn uniform(&mut self, name: &'static str, value: [f32; 4]) {
        match self.uniforms.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = value,
            None => self.uniforms.push((name, value)),
        }
    }

    /// The part `kind` keyed `key` this built: the buffers padded to whole rows.
    pub(crate) fn part(mut self, kind: PartKind, key: u64) -> GpuPart {
        let scalar_rows = self.scalars.len().div_ceil(BUFFER_WIDTH);
        let vector_rows = self.vectors.len().div_ceil(BUFFER_WIDTH);
        self.scalars.resize(scalar_rows * BUFFER_WIDTH, 0.0);
        self.vectors.resize(vector_rows * BUFFER_WIDTH, [0.0; 4]);
        let vectors = self.vectors.into_iter().flatten().collect();
        GpuPart { kind, key, data: Some(PartData { scalars: self.scalars, scalar_rows, vectors, vector_rows, uniforms: self.uniforms }) }
    }

    /// The uniforms it set (a volume's own), or Err where a buffer was written into it: a volume's own uniforms go with
    /// every ask and hold no buffer (a buffer belongs to a part, whose textures hold it), so its data would be lost and
    /// its uniforms point into some part's texture (task-14 review N6: checked in every build, the ask then not made).
    pub(crate) fn into_uniforms(self) -> Result<Vec<(&'static str, [f32; 4])>, String> {
        if !self.scalars.is_empty() || !self.vectors.is_empty() {
            return Err(format!("a volume's own uniforms were given buffers ({} scalars, {} vectors): only a part holds a buffer", self.scalars.len(), self.vectors.len()));
        }
        Ok(self.uniforms)
    }
}

/// One volume's GPU march, as plain data (task-13 study §3.3): its cloud type (the export mode draws the type's own
/// shader, design §12.13; the CPU mode marches the Cumulus only), its record's key, width and segments, its parts (each
/// with its data, or its key alone where the GPU holds it), its own uniforms by name, and the record its march fills,
/// made by `CloudAsk::make_records` on the asking job (a record is 268 MB a segment at 8192: never on the UI thread,
/// task-13 review S1; fallibly, task-14 review S5).
pub struct VolumeUpload {
    pub genus: crate::CloudGenus,
    pub key: u64,
    pub width: usize,
    pub segments: usize,
    pub parts: Vec<GpuPart>,
    pub uniforms: Vec<(&'static str, [f32; 4])>,
    pub record: Option<GpuRecord>,
}

impl VolumeUpload {
    /// The bytes its parts' data hold (what the ask uploads for it; nothing for a part the GPU holds).
    pub fn bytes(&self) -> usize {
        self.parts.iter().filter_map(|p| p.data.as_ref()).map(PartData::bytes).sum()
    }

    /// Its uniform `name`, its own or a part's whose data it carries.
    pub fn uniform(&self, name: &str) -> Option<[f32; 4]> {
        let own = self.uniforms.iter();
        let parts = self.parts.iter().filter_map(|p| p.data.as_ref()).flat_map(|d| d.uniforms.iter());
        own.chain(parts).find(|(n, _)| *n == name).map(|(_, v)| *v)
    }

    /// Its part `kind`.
    pub fn part(&self, kind: PartKind) -> Option<&GpuPart> {
        self.parts.iter().find(|p| p.kind == kind)
    }

    /// A part whose buffer is past MAX_ROWS: its kind and rows.
    pub fn too_tall(&self) -> Option<(PartKind, usize)> {
        self.parts.iter().filter_map(|p| p.data.as_ref().map(|d| (p.kind, d.scalar_rows.max(d.vector_rows)))).find(|&(_, rows)| rows > MAX_ROWS)
    }
}

/// What a map asks of the GPU (design §12.3): the records of every cloud volume of its stack at its width (an
/// antialiasing pass's: of each of its jittered directions, `Env::cloud_ask_jittered`), keyed by all of theirs, by who
/// asks and by the mode.
pub struct CloudAsk {
    pub width: usize,
    pub key: u64,
    pub volumes: Vec<VolumeUpload>,
}

impl CloudAsk {
    /// The ask `asker` makes of `volumes` at `width` in `mode`, keyed by their keys. Its records are made apart
    /// (`make_records`).
    pub fn new(width: usize, asker: AskFor, mode: GpuMode, volumes: Vec<VolumeUpload>) -> CloudAsk {
        let key = Self::key_of(width, asker, mode, volumes.iter().map(|v| v.key));
        CloudAsk { width, key, volumes }
    }

    /// The key of the ask `asker` makes at `width` in `mode` of volumes keyed `keys`, in their order (`Env::cloud_ask_key`
    /// finds it without building the ask).
    pub fn key_of(width: usize, asker: AskFor, mode: GpuMode, keys: impl Iterator<Item = u64>) -> u64 {
        let seed = key_of(&[width as u64, asker as u64, mode as u64]);
        keys.fold(seed, |h, k| (h ^ k).wrapping_mul(0x0000_0100_0000_01b3))
    }

    /// The bytes its uploads hold.
    pub fn bytes(&self) -> usize {
        self.volumes.iter().map(VolumeUpload::bytes).sum()
    }

    /// The keys of the parts it counts on the GPU holding (built without their data): the renderer keeps them while the
    /// ask waits to be taken (task-14 review N1).
    pub fn counted_on(&self) -> Vec<u64> {
        self.volumes.iter().flat_map(|v| v.parts.iter()).filter(|p| p.data.is_none()).map(|p| p.key).collect()
    }

    /// The bytes its records take (made or to be made).
    pub fn record_bytes(&self) -> usize {
        self.volumes.iter().map(|v| GpuRecord::bytes_for(v.width, v.segments)).sum()
    }

    /// Makes every volume's record, each clear until its bands land (task-13 review S1: on the asking job, never the UI
    /// thread), fallibly (task-14 review S5): Err where one does not fit in memory, and the ask is then not made (a
    /// refinement pass says it did not fit, as its map would; a bake or an export marches on the CPU).
    pub fn make_records(&mut self) -> Result<(), String> {
        for volume in &mut self.volumes {
            if volume.record.is_none() {
                volume.record = Some(GpuRecord::try_new(volume.key, volume.width, volume.segments)?);
            }
        }
        Ok(())
    }
}

/// Every volume's record, as the GPU delivered them (one per volume of the ask).
pub type CloudRecords = Vec<Arc<GpuRecord>>;

/// A volume's march at one export width, from the GPU (design §12.3; task-13 study §3.2): for each texel of the upper
/// half of a map `width` x `width / 2`, one entry of HALVES halves per segment, as the shader's two targets a segment
/// give them. Rows from the zenith down, as `LightRecord`'s.
pub struct GpuRecord {
    pub key: u64,
    width: usize,
    height: usize,
    rows: usize,
    segments: usize,
    halves: Vec<[u16; HALVES]>,
}

/// A clear entry: transmittance 1, nothing gathered.
const CLEAR_HALVES: [u16; HALVES] = [0x3c00, 0, 0, 0, 0, 0, 0, 0];

impl GpuRecord {
    /// A record keyed `key` of `segments` segments for bakes `width` wide, every entry clear until its tile lands.
    pub fn new(key: u64, width: usize, segments: usize) -> GpuRecord {
        let width = width.max(2);
        let height = (width / 2).max(1);
        let rows = height.div_ceil(2);
        let segments = segments.max(1);
        GpuRecord { key, width, height, rows, segments, halves: vec![CLEAR_HALVES; width * rows * segments] }
    }

    /// [`GpuRecord::new`], fallibly: Err where its memory cannot be had (268 MB a segment at 8192, task-14 review S5).
    pub fn try_new(key: u64, width: usize, segments: usize) -> Result<GpuRecord, String> {
        let width = width.max(2);
        let height = (width / 2).max(1);
        let rows = height.div_ceil(2);
        let segments = segments.max(1);
        let n = width * rows * segments;
        let mut halves = Vec::new();
        if halves.try_reserve_exact(n).is_err() {
            return Err(format!("the clouds' {width}-wide record ({:.0} MB) does not fit in memory", Self::bytes_for(width, segments) as f64 / 1.0e6));
        }
        halves.resize(n, CLEAR_HALVES);
        Ok(GpuRecord { key, width, height, rows, segments, halves })
    }

    /// The bytes a record of `segments` segments for bakes `width` wide takes.
    pub fn bytes_for(width: usize, segments: usize) -> usize {
        let width = width.max(2);
        width * (width / 2).max(1).div_ceil(2) * segments.max(1) * HALVES * 2
    }

    pub fn width(&self) -> usize {
        self.width
    }

    /// Rows over the horizon (the ones it records).
    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn segments(&self) -> usize {
        self.segments
    }

    /// The bytes it holds.
    pub fn bytes(&self) -> usize {
        self.halves.len() * HALVES * 2
    }

    /// Copies a tile's reads in: rows `y0..y0 + rows` of the map, segments `seg0..seg0 + reads.len() / 2`, each segment's
    /// two targets (`width` x `rows` texels of four little-endian halves) one after the other in `reads`. False where a
    /// read's size is not the tile's.
    pub fn put_tile(&mut self, y0: usize, rows: usize, seg0: usize, reads: &[&[u8]]) -> bool {
        self.put_rect(0, self.width, y0, rows, seg0, reads)
    }

    /// Copies tightly packed target pairs into a rectangle of the record. Checks every read before writing.
    pub fn put_rect(&mut self, x0: usize, cols: usize, y0: usize, rows: usize, seg0: usize, reads: &[&[u8]]) -> bool {
        let bytes = cols.checked_mul(rows).and_then(|n| n.checked_mul(8));
        if reads.len() % 2 != 0
            || x0.checked_add(cols).is_none_or(|end| end > self.width)
            || y0.checked_add(rows).is_none_or(|end| end > self.rows)
            || seg0.checked_add(reads.len() / 2).is_none_or(|end| end > self.segments)
            || reads.iter().any(|r| Some(r.len()) != bytes)
        {
            return false;
        }
        let half = |b: &[u8], i: usize| u16::from_le_bytes([b[2 * i], b[2 * i + 1]]);
        for (k, pair) in reads.chunks_exact(2).enumerate() {
            let (a, b) = (pair[0], pair[1]);
            for dy in 0..rows {
                for dx in 0..cols {
                    let t = dy * cols + dx;
                    let entry = &mut self.halves[((y0 + dy) * self.width + x0 + dx) * self.segments + seg0 + k];
                    for c in 0..4 {
                        entry[c] = half(a, 4 * t + c);
                        entry[4 + c] = half(b, 4 * t + c);
                    }
                }
            }
        }
        true
    }

    /// The halves of texel (x, y) and segment `seg` (test support: the GPU crate compares them with the CPU's march).
    #[doc(hidden)]
    pub fn halves_at(&self, x: usize, y: usize, seg: usize) -> [f32; HALVES] {
        self.halves[(y * self.width + x) * self.segments + seg].map(|h| f16::from_bits(h).to_f32())
    }

    /// The records of the texel holding the unit direction `d` (map frame) into `out` (its first segments, up to its
    /// length), as `VolumeMarch`es `shade` lights as the march's (both lobes the mixed light, the mean depth at weight
    /// 1): false, with `out` untouched, where `d` lies in no recorded texel. The texel is `LightRecord::read`'s.
    pub fn read(&self, d: Vec3f, out: &mut [VolumeMarch]) -> bool {
        if !(d.y > 0.0) || !d.is_finite() {
            return false;
        }
        let uv = ibl::dir_to_equirect_uv(arr(d));
        let x = ((uv[0] * self.width as f32) as i64).rem_euclid(self.width as i64) as usize;
        let y = (uv[1] * self.height as f32).max(0.0) as usize;
        if y >= self.rows {
            return false;
        }
        let n = out.len().min(self.segments);
        let at = (y * self.width + x) * self.segments;
        for (m, h) in out[..n].iter_mut().zip(&self.halves[at..at + n]) {
            *m = record_of(h);
        }
        true
    }
}

/// A `VolumeMarch` from an entry's halves: both lobes the mixed light (so any Silver lining mixes them back to it) and
/// the mean depth at weight 1 (0 where nothing was met).
fn record_of(h: &[u16; HALVES]) -> VolumeMarch {
    let f = |i: usize| f16::from_bits(h[i]).to_f32();
    let (low, high, depth) = (f(1), f(2), f(3));
    VolumeMarch {
        transmittance: f(0),
        forward_low: low,
        forward_high: high,
        back_low: low,
        back_high: high,
        ambient: f(4),
        open: f(5),
        bounce: f(6),
        depth_weighted: depth,
        weight: if depth > 0.0 { 1.0 } else { 0.0 },
    }
}

/// The CPU march's entry as the shader writes it, before the halves (test support): (transmittance, the lobes mixed at
/// `lining` low and high, the mean depth, ambient, open, bounce, 0).
#[doc(hidden)]
pub fn entry_of(m: &VolumeMarch, lining: f32) -> [f32; HALVES] {
    [m.transmittance, lining * m.forward_low + (1.0 - lining) * m.back_low, lining * m.forward_high + (1.0 - lining) * m.back_high, m.mean_depth_m(), m.ambient, m.open, m.bounce, 0.0]
}

/// A record of `segments` segments for bakes `width` wide keyed `key`, every entry `entry(x, y)`'s (one array of
/// entries a segment) put through the halves as the GPU's targets hold them (test support: the half record test, and
/// the GPU crate's).
#[doc(hidden)]
pub fn record_from(key: u64, width: usize, segments: usize, entry: &(dyn Fn(usize, usize) -> Vec<[f32; HALVES]> + Sync)) -> GpuRecord {
    let mut record = GpuRecord::new(key, width, segments);
    let (w, s) = (record.width, record.segments);
    for (y, row) in record.halves.chunks_mut(w * s).enumerate() {
        for x in 0..w {
            for (k, e) in entry(x, y).iter().enumerate().take(s) {
                row[x * s + k] = e.map(|v| f16::from_f32(v).to_bits());
            }
        }
    }
    record
}

/// One Cumulus at its defaults under a 30 degree sun (test support: this module's test and the GPU crate's).
#[doc(hidden)]
pub fn one_cumulus_params() -> crate::HdriParams {
    let mut p = crate::cloud_stack::busy_stack_params();
    if let Some(stack) = p.sky.cloud_stack.as_mut() {
        stack.layers.retain(|layer| layer.genus() == Some(crate::CloudGenus::Cumulus));
    }
    p
}

/// The CPU's entries of every texel of the upper half of a map `width` wide for `volume`'s march cut at `cuts`, lobes
/// mixed at `lining` (test support): row by row, one array of entries a segment per texel, spread over `threads`.
#[doc(hidden)]
pub fn cpu_entries(volume: &crate::cloud_volume_cumulus::CloudVolume, width: usize, cuts: &[f32], lining: f32, threads: usize) -> Vec<Vec<[f32; HALVES]>> {
    let rows = (width / 2).max(1).div_ceil(2);
    let texels: Vec<(usize, usize)> = (0..width * rows).map(|i| (i % width, i / width)).collect();
    cpu_entries_at(volume, width, &texels, cuts, lining, threads)
}

/// [`cpu_entries`] at the texels (x, y) of the upper half alone, in their order (test support: rows sampled at an export
/// width, task-13a review S4).
#[doc(hidden)]
pub fn cpu_entries_at(volume: &crate::cloud_volume_cumulus::CloudVolume, width: usize, at: &[(usize, usize)], cuts: &[f32], lining: f32, threads: usize) -> Vec<Vec<[f32; HALVES]>> {
    let height = (width / 2).max(1);
    let texels = at.len();
    let out: Vec<std::sync::OnceLock<Vec<[f32; HALVES]>>> = (0..texels).map(|_| std::sync::OnceLock::new()).collect();
    std::thread::scope(|scope| {
        for t in 0..threads.max(1) {
            let out = &out;
            scope.spawn(move || {
                let mut marches = vec![VolumeMarch::CLEAR; cuts.len() + 1];
                let mut i = t;
                while i < texels {
                    let (x, y) = at[i];
                    let d = crate::vec(ibl::equirect_uv_to_dir([(x as f32 + 0.5) / width as f32, (y as f32 + 0.5) / height as f32]));
                    volume.march(d, width, cuts, &mut marches);
                    let _ = out[i].set(marches.iter().map(|m| entry_of(m, lining)).collect());
                    i += threads.max(1);
                }
            });
        }
    });
    out.into_iter().map(|e| e.into_inner().unwrap_or_default()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Env, ImageSet};

    #[test]
    fn put_rect_writes_sub_row_slices_identically_to_put_tile() {
        const WIDTH: usize = 32;
        let targets = |x0: usize, cols: usize, y0: usize, rows: usize| {
            let mut reads = [const { Vec::new() }; 4];
            for (target, bytes) in reads.iter_mut().enumerate() {
                for y in y0..y0 + rows {
                    for x in x0..x0 + cols {
                        for channel in 0..4 {
                            let value = ((target * 256 + y * WIDTH + x) * 4 + channel) as u16;
                            bytes.extend_from_slice(&value.to_le_bytes());
                        }
                    }
                }
            }
            reads
        };
        let mut full = GpuRecord::new(1, WIDTH, 3);
        let mut tiled = GpuRecord::new(1, WIDTH, 3);
        let reads = targets(0, WIDTH, 1, 2);
        assert!(full.put_tile(1, 2, 1, &reads.each_ref().map(Vec::as_slice)));
        for (x, y, rows) in [(16, 1, 2), (0, 2, 1), (0, 1, 1)] {
            let reads = targets(x, 16, y, rows);
            assert!(tiled.put_rect(x, 16, y, rows, 1, &reads.each_ref().map(Vec::as_slice)));
        }
        assert_eq!(tiled.halves, full.halves, "sub-row copies preserve both segments and their neighbours");

        let bytes = [0xff; 8];
        let pair = [&bytes[..], &bytes[..]];
        for (x, y, seg) in [(WIDTH, 0, 0), (0, tiled.rows(), 0), (0, 0, 3), (usize::MAX, 0, 0), (0, usize::MAX, 0), (0, 0, usize::MAX)] {
            assert!(!tiled.put_rect(x, 1, y, 1, seg, &pair));
        }
        assert!(!tiled.put_rect(0, 1, 0, 1, 0, &[&bytes, &bytes, &bytes]));
        assert!(!tiled.put_rect(0, 1, 0, 1, 0, &[&bytes, &bytes, &bytes, &bytes[..7]]));
        assert_eq!(tiled.halves, full.halves, "an invalid rectangle or target leaves the entire record untouched");
    }

    /// Task-13 study §5 step 2: a record filled from the CPU's own marches and put through the halves the GPU's targets
    /// hold bakes a 1024 map within the halves' rounding of the marched bake: each texel's opacity within 1e-3 and its
    /// radiance within 0.3 % (plus 1e-4 of the map's mean); and a map without records is the marched map (the guard,
    /// `legacy_maps_bake_as_before`, holds every other map bit for bit).
    #[test]
    fn a_half_record_bakes_as_the_march() {
        const WIDTH: usize = 1024;
        let params = one_cumulus_params();
        let mut env = Env::new_for_width(&params, &ImageSet::default(), WIDTH, &|| false).expect("an Env");
        let marched = env.bake(WIDTH);
        let ask = env.cloud_ask_at(WIDTH, GpuMode::Cpu).expect("a Cumulus asks for its record");
        assert!(env.cloud_ask(GPU_FROM_WIDTH / 2, &|_| false).is_none(), "an export narrower than GPU_FROM_WIDTH marches on the CPU");
        let stack = env.cloud_stack().expect("a stack");
        let volumes = stack.volume_marches();
        assert_eq!((ask.volumes.len(), volumes.len()), (1, 1), "one volume, one upload");
        let (volume, cuts, lining) = volumes[0];
        let upload = &ask.volumes[0];
        for part in &upload.parts {
            let data = part.data.as_ref().expect("nothing resident: every part's data");
            println!("{:?}: {:.1} MB ({} x {} scalars, {} x {} vectors, {} uniforms)", part.kind, data.bytes() as f64 / 1.0e6, BUFFER_WIDTH, data.scalar_rows, BUFFER_WIDTH, data.vector_rows, data.uniforms.len());
        }
        println!("the upload: {:.1} MB, {} uniforms of its own", upload.bytes() as f64 / 1.0e6, upload.uniforms.len());
        let entries = cpu_entries(volume, WIDTH, cuts, lining, 8);
        let record = Arc::new(record_from(upload.key, WIDTH, upload.segments, &|x, y| entries[y * WIDTH + x].clone()));
        // Opacity: each entry's, through the halves.
        let mut alpha_off = 0.0f32;
        for (i, e) in entries.iter().enumerate() {
            let got = record.halves_at(i % WIDTH, i / WIDTH, 0);
            alpha_off = alpha_off.max((got[0] - e[0][0]).abs());
        }
        println!("opacity through the halves: off by up to {alpha_off:.2e}");
        assert!(alpha_off <= 1.0e-3, "opacity off by {alpha_off}");
        env.attach_cloud_records_in(WIDTH, GpuMode::Cpu, &[record.clone()]).expect("the record is the volume's");
        assert!(env.has_cloud_records());
        let read = env.bake(WIDTH);
        let lum = |c: &[f32; 4]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let mean = marched.data.iter().map(lum).sum::<f32>() / marched.data.len() as f32;
        let (mut worst, mut at, mut differ) = (0.0f32, 0usize, 0usize);
        for (i, (a, b)) in marched.data.iter().zip(&read.data).enumerate() {
            let (a, b) = (lum(a), lum(b));
            let off = (a - b).abs() / (3.0e-3 * a.abs() + 1.0e-4 * mean);
            differ += (a != b) as usize;
            if off > worst {
                (worst, at) = (off, i);
            }
        }
        let read_mean = read.data.iter().map(lum).sum::<f32>() / read.data.len() as f32;
        println!("{differ} texels differ; the worst at ({}, {}) is {worst:.3} of the bound; mean luminance {mean:.5} against {read_mean:.5}", at % WIDTH, at / WIDTH);
        assert!(differ > 0, "the record was read");
        assert!(worst <= 1.0, "texel ({}, {}): {} against the march's {}", at % WIDTH, at / WIDTH, lum(&read.data[at]), lum(&marched.data[at]));
        // A record under another key is refused, and nothing is attached.
        let mut other = Env::new_for_width(&params, &ImageSet::default(), WIDTH, &|| false).expect("an Env");
        let stale = Arc::new(GpuRecord::new(record.key ^ 1, WIDTH, upload.segments));
        assert!(other.attach_cloud_records_in(WIDTH, GpuMode::Cpu, &[stale]).is_err());
        assert!(!other.has_cloud_records());
        assert_eq!(other.bake(WIDTH).data, marched.data, "a map without records marches as before");
    }
}
