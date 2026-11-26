//! `--gpu-report <dir>` (native): what the film holds on the GPU, plate by
//! plate, learned by playing it.
//!
//! The player steps through the film on locked time (`--gpu-step <s>`
//! apart, default 1/30 s), drawing at `--gpu-size <w>x<h>` (default the
//! film's size), waiting at each time until the plate's frame is drawn
//! exact, and once that frame painted takes `Cx::gpu_inventory`. Every
//! texture is attributed to the plate whose draw created it (the texture
//! slots that appeared while that plate drew), else to the app. It writes
//! into `<dir>`:
//!
//! - `frames.tsv`: per frame the time, the plate, the device's own total,
//!   the textures by state, the buffers, and the bytes each plate holds
//! - `report.txt`: per plate the peak and median of the whole process and
//!   of its own textures while it shows, and the breakdown at its peak
//!   grouped by (owner, name, format, size, state) with counts
//!
//! then quits. Progress goes to stdout as `gpu-report <t> <duration>`.

use crate::film::FilmRuntime;
use makepad_widgets::makepad_platform::gpu_inventory::{GpuInventory, GpuTextureState};
use makepad_widgets::*;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// How long one time may wait for its plate's frame before the report
/// moves on without it (counted as not painted).
const FRAME_WAIT: f64 = 20.0;

const MB: f64 = 1048576.0;

/// One texture of a peak's breakdown.
#[derive(Clone)]
struct Item {
    owner: String,
    name: String,
    format: &'static str,
    size: (usize, usize),
    mips: usize,
    state: &'static str,
    bytes: u64,
}

/// What one frame held.
struct Frame {
    t: f64,
    plate: Option<usize>,
    device: Option<u64>,
    live: u64,
    free_held: u64,
    retired: u64,
    cpu_pixels: u64,
    instance_gpu: u64,
    geometry_gpu: u64,
    retained: u64,
    /// Bytes per owner: plates, then the app.
    by_owner: Vec<u64>,
    painted: bool,
}

struct Peak {
    total: u64,
    t: f64,
    items: Vec<Item>,
    held_by_owner: Vec<u64>,
}

pub struct GpuReport {
    dir: PathBuf,
    pub size: Option<(u32, u32)>,
    step: f64,
    i: usize,
    duration: f64,
    waiting_since: f64,
    /// Texture slot (index, generation) -> the plate whose draw made it.
    pub owners: HashMap<(usize, u64), usize>,
    frames: Vec<Frame>,
    /// Per plate: the process peak while it shows, and its own peak.
    peaks: Vec<Option<Peak>>,
    own_peaks: Vec<Option<Peak>>,
    names: Vec<String>,
    unpainted: usize,
    settling: bool,
    pub finished: bool,
}

impl GpuReport {
    pub fn from_args() -> Option<GpuReport> {
        let args: Vec<String> = std::env::args().collect();
        let value = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
        let dir = PathBuf::from(value("--gpu-report")?);
        let size = value("--gpu-size").and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        });
        let step = value("--gpu-step").and_then(|s| s.parse().ok()).filter(|s: &f64| *s > 0.0).unwrap_or(1.0 / 30.0);
        Some(GpuReport {
            dir,
            size,
            step,
            i: 0,
            duration: 0.0,
            waiting_since: Cx::monotonic_now(),
            owners: HashMap::new(),
            frames: Vec::new(),
            peaks: Vec::new(),
            own_peaks: Vec::new(),
            names: Vec::new(),
            unpainted: 0,
            settling: false,
            finished: false,
        })
    }

    pub fn time(&self) -> f64 {
        (self.i as f64 * self.step).min(self.duration.max(0.0))
    }

    fn frames_total(&self) -> usize {
        (self.duration / self.step).floor() as usize + 1
    }

    /// After a draw at [`Self::time`]: once the frame is drawn exact and
    /// painted (or waited for too long), take the inventory and move on.
    pub fn frame_done(&mut self, cx: &mut Cx, film: &FilmRuntime, drawn: bool, exact: bool, painted: bool) {
        if self.finished {
            return;
        }
        let Some(m) = &film.manifest else { return };
        self.duration = m.film.duration;
        if self.names.is_empty() {
            self.names = m.plates.iter().map(|p| p.name.clone()).collect();
            self.peaks = (0..self.names.len()).map(|_| None).collect();
            self.own_peaks = (0..self.names.len()).map(|_| None).collect();
        }
        let now = Cx::monotonic_now();
        let waited = now - self.waiting_since;
        let ready = drawn && exact;
        if !ready && waited < FRAME_WAIT {
            return;
        }
        // The exact frame paints after this draw (its targets allocate as
        // it is encoded): measure on the next one, at the same time.
        if !self.settling {
            self.settling = true;
            return;
        }
        self.settling = false;
        if !ready {
            self.unpainted += 1;
            eprintln!("gpu-report: {:.3} ({}) not ready after {FRAME_WAIT} s: drawn {drawn} exact {exact} painted {painted}", self.time(), film.plate_name(self.time()).unwrap_or("-"));
        }
        let t = self.time();
        let plate = m.plate_at(t);
        cx.frame_completion_serial();
        let inv = cx.gpu_inventory();
        self.record(t, plate, &inv, ready && painted);
        self.i += 1;
        self.waiting_since = now;
        if self.i % 30 == 0 {
            println!("gpu-report {:.3} {:.3}", t, self.duration);
        }
        if self.i >= self.frames_total() {
            self.finished = true;
            if let Err(e) = self.write(film) {
                error!("player: writing the gpu report: {e}");
            }
        }
    }

    fn owner_of(&self, slot: usize, generation: u64) -> usize {
        self.owners.get(&(slot, generation)).copied().unwrap_or(self.names.len())
    }

    fn owner_name(&self, owner: usize) -> String {
        self.names.get(owner).cloned().unwrap_or_else(|| "app".into())
    }

    fn record(&mut self, t: f64, plate: Option<usize>, inv: &GpuInventory, painted: bool) {
        let n = self.names.len();
        let mut f = Frame {
            t,
            plate,
            device: inv.device_allocated_bytes,
            live: 0,
            free_held: 0,
            retired: inv.retired_texture_bytes,
            cpu_pixels: 0,
            instance_gpu: inv.instance_gpu_bytes(),
            geometry_gpu: inv.geometry_gpu_bytes.unwrap_or(0),
            retained: inv.retained_allocation_bytes,
            by_owner: vec![0; n + 1],
            painted,
        };
        let mut items = Vec::with_capacity(inv.textures.len());
        for tex in &inv.textures {
            let owner = self.owner_of(tex.slot, tex.generation);
            let current = tex.gpu_bytes.unwrap_or(tex.texel_bytes);
            match tex.state {
                GpuTextureState::Live => f.live += current,
                GpuTextureState::FreeSlotHeld => f.free_held += current,
            }
            f.cpu_pixels += tex.cpu_bytes;
            f.by_owner[owner] += tex.bytes();
            if tex.bytes() == 0 {
                continue;
            }
            let state = match tex.state {
                GpuTextureState::Live => "live",
                GpuTextureState::FreeSlotHeld => "dropped-held",
            };
            if current > 0 {
                items.push(Item { owner: self.owner_name(owner), name: tex.name(), format: tex.format, size: (tex.width, tex.height), mips: tex.mip_levels, state, bytes: current });
            }
        }
        let total = f.device.unwrap_or(f.live + f.free_held + f.retired + f.instance_gpu + f.geometry_gpu);
        if let Some(p) = plate {
            if self.peaks[p].as_ref().is_none_or(|k| total > k.total) {
                self.peaks[p] = Some(Peak { total, t, items: items.clone(), held_by_owner: f.by_owner.clone() });
            }
            let own = f.by_owner[p];
            if self.own_peaks[p].as_ref().is_none_or(|k| own > k.total) {
                let own_name = self.names[p].clone();
                let own_items = items.iter().filter(|i| i.owner == own_name).cloned().collect();
                self.own_peaks[p] = Some(Peak { total: own, t, items: own_items, held_by_owner: Vec::new() });
            }
        }
        self.frames.push(f);
    }

    fn write(&self, film: &FilmRuntime) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let n = self.names.len();
        let mut tsv = String::from("t\tplate\tdevice\tlive\tdropped_held\tretired\tcpu_pixels\tinstance_gpu\tgeometry_gpu\tretained\tpainted");
        for name in &self.names {
            tsv.push_str(&format!("\t{name}"));
        }
        tsv.push_str("\tapp\n");
        for f in &self.frames {
            tsv.push_str(&format!(
                "{:.4}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                f.t,
                f.plate.map_or("-".to_string(), |p| self.names[p].clone()),
                f.device.map_or("-".to_string(), |d| d.to_string()),
                f.live, f.free_held, f.retired, f.cpu_pixels, f.instance_gpu, f.geometry_gpu, f.retained, f.painted as u8
            ));
            for b in &f.by_owner {
                tsv.push_str(&format!("\t{b}"));
            }
            tsv.push('\n');
        }
        std::fs::write(self.dir.join("frames.tsv"), tsv)?;

        let total = |f: &Frame| f.device.unwrap_or(f.live + f.free_held + f.retired + f.instance_gpu + f.geometry_gpu);
        let median = |mut v: Vec<u64>| -> u64 {
            if v.is_empty() {
                return 0;
            }
            v.sort_unstable();
            v[v.len() / 2]
        };
        let mb = |b: u64| b as f64 / MB;
        let mut r = String::new();
        let size = self.size.map_or("film size".to_string(), |(w, h)| format!("{w}x{h}"));
        r.push_str(&format!("gpu report: {size}, {} frames every {:.4} s, {} not exact in time\n", self.frames.len(), self.step, self.unpainted));
        if let Some(f) = self.frames.iter().max_by_key(|f| total(f)) {
            r.push_str(&format!("process peak {:.1} MB at {:.2} s ({}); device total is the HAL backend's own (Metal currentAllocatedSize), else the textures and buffers measured\n", mb(total(f)), f.t, f.plate.map_or("-", |p| self.names[p].as_str())));
        }
        // The whole process at its peak, by what the textures are (owners
        // merged), by format, and by state.
        if let Some(k) = self.peaks.iter().flatten().max_by_key(|k| k.total) {
            let merged: Vec<Item> = k.items.iter().map(|i| Item { owner: String::new(), ..i.clone() }).collect();
            r.push_str(&format!("\nat the process peak ({:.1} MB at {:.2} s), every plate merged:\n", mb(k.total), k.t));
            r.push_str(&group_lines(&merged, 25));
            let mut by: BTreeMap<(&'static str, &'static str), (usize, u64)> = BTreeMap::new();
            for i in &k.items {
                let e = by.entry((i.format, i.state)).or_default();
                e.0 += 1;
                e.1 += i.bytes;
            }
            let mut by: Vec<_> = by.into_iter().collect();
            by.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
            r.push_str("  by format and state:\n");
            for ((format, state), (count, bytes)) in by {
                r.push_str(&format!("  {:8.1} MB  {count:>4} x {format:<16} {state}\n", mb(bytes)));
            }
        }
        r.push_str("\nplate        start   dur | process peak / median MB | own peak / median MB | at process peak: other plates' MB, app MB, dropped-held MB\n");
        for p in 0..n {
            let frames: Vec<&Frame> = self.frames.iter().filter(|f| f.plate == Some(p)).collect();
            let info = film.manifest.as_ref().map(|m| (m.plates[p].start, m.plates[p].dur)).unwrap_or_default();
            let proc_med = median(frames.iter().map(|f| total(f)).collect());
            let own_med = median(frames.iter().map(|f| f.by_owner[p]).collect());
            let (peak, peak_t, others, app) = match &self.peaks[p] {
                Some(k) => (k.total, k.t, k.held_by_owner.iter().enumerate().filter(|(o, _)| *o != p && *o < n).map(|(_, b)| *b).sum::<u64>(), k.held_by_owner[n]),
                None => (0, 0.0, 0, 0),
            };
            let at = frames.iter().find(|f| f.t == peak_t);
            let own_peak = self.own_peaks[p].as_ref().map_or(0, |k| k.total);
            r.push_str(&format!(
                "{:<12} {:6.2} {:5.2} | {:8.1} / {:8.1} | {:7.1} / {:7.1} | {:7.1} {:6.1} {:6.1}\n",
                self.names[p], info.0, info.1, mb(peak), mb(proc_med), mb(own_peak), mb(own_med), mb(others), mb(app),
                mb(at.map_or(0, |f| f.free_held))
            ));
        }
        for p in 0..n {
            let Some(k) = &self.peaks[p] else { continue };
            r.push_str(&format!("\n== {} (process peak {:.1} MB at {:.2} s; held by owner:", self.names[p], mb(k.total), k.t));
            for (o, b) in k.held_by_owner.iter().enumerate() {
                if *b > 0 {
                    r.push_str(&format!(" {}={:.1}", self.owner_name(o), mb(*b)));
                }
            }
            r.push_str(")\n");
            r.push_str(&group_lines(&k.items, 14));
            if let Some(own) = &self.own_peaks[p] {
                r.push_str(&format!("  -- its own textures at their peak ({:.1} MB at {:.2} s):\n", mb(own.total), own.t));
                r.push_str(&group_lines(&own.items, 8));
            }
        }
        // The whole process over time, one line per second.
        r.push_str("\ntime  plate        device_MB live_MB dropped_held_MB retired_MB instance_MB plates_holding\n");
        let mut next = 0.0;
        for f in &self.frames {
            if f.t + 1e-9 < next {
                continue;
            }
            next = f.t.floor() + 1.0;
            let holding = f.by_owner[..n].iter().filter(|b| **b > 0).count();
            r.push_str(&format!(
                "{:5.1} {:<12} {:8.1} {:7.1} {:7.1} {:7.1} {:7.1} {}\n",
                f.t, f.plate.map_or("-", |p| self.names[p].as_str()), f.device.map_or(-1.0, |d| mb(d)), mb(f.live), mb(f.free_held), mb(f.retired), mb(f.instance_gpu), holding
            ));
        }
        std::fs::write(self.dir.join("report.txt"), r)?;
        std::fs::write(self.dir.join("done"), b"")?;
        Ok(())
    }
}

/// The items grouped by (owner, name, format, size, state), largest first.
fn group_lines(items: &[Item], top: usize) -> String {
    let mut groups: BTreeMap<(String, String, &'static str, (usize, usize), usize, &'static str), (usize, u64)> = BTreeMap::new();
    for i in items {
        let g = groups.entry((i.owner.clone(), i.name.clone(), i.format, i.size, i.mips, i.state)).or_default();
        g.0 += 1;
        g.1 += i.bytes;
    }
    let mut v: Vec<_> = groups.into_iter().collect();
    v.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    let mut out = String::new();
    let rest: u64 = v.iter().skip(top).map(|(_, (_, b))| *b).sum();
    for ((owner, name, format, (w, h), mips, state), (count, bytes)) in v.iter().take(top) {
        out.push_str(&format!(
            "  {:8.1} MB  {count:>3} x {format:<16} {w}x{h}{}  {state:<13} {owner:<10} {name}\n",
            *bytes as f64 / MB,
            if *mips > 1 { format!(" mips{mips}") } else { String::new() }
        ));
    }
    if rest > 0 {
        out.push_str(&format!("  {:8.1} MB  in {} smaller groups\n", rest as f64 / MB, v.len().saturating_sub(top)));
    }
    out
}
