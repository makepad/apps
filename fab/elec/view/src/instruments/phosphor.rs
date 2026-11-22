//! Waveform dwell histograms, built on workers and accumulated on the GPU.
//!
//! A waveform (times relative to its trigger, values) is mapped to device pixels by a
//! [`PhosphorMap`], then reduced per pixel column to the minimum-to-maximum span of its polyline
//! inside that column: the LOD that makes cost follow pixels, not samples. Each column's span,
//! widened by half a pixel each way, shares one count among the visible rows it overlaps.
//! Each row gets its covered length divided by the clipped span's length. A flat line lands
//! on one row or splits across two; joints between segments never count twice.
//!
//! Counts are column-major (`col * h + row`): a column's span is contiguous in memory. The GPU
//! texture holds the image transposed (h texels wide, w tall) and the shader swaps coordinates.
//!
//! Cost per waveform: O(samples + covered pixels); no allocation once a [`Raster`]'s scratch has
//! grown to the map's width.

/// How waveforms land on the plot: device pixel columns and rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhosphorMap {
    /// Plot size in device pixels.
    pub w: u32,
    pub h: u32,
    /// Time at the left edge, seconds relative to the trigger.
    pub t_left: f64,
    /// Seconds per pixel column.
    pub t_px: f64,
    /// Value at the top edge.
    pub v_top: f64,
    /// Value units per pixel row (positive: values grow upwards).
    pub v_px: f64,
}

impl PhosphorMap {
    /// The map of a scope screen: `w` x `h` device pixels showing `t_span` seconds starting `pre`
    /// seconds before the trigger, and values `v_lo` to `v_hi` bottom to top.
    pub fn screen(w: u32, h: u32, pre: f64, t_span: f64, v_lo: f64, v_hi: f64) -> PhosphorMap {
        PhosphorMap {
            w,
            h,
            t_left: -pre,
            t_px: t_span / w.max(1) as f64,
            v_top: v_hi,
            v_px: (v_hi - v_lo) / h.max(1) as f64,
        }
    }
    fn x(&self, t: f64) -> f64 {
        (t - self.t_left) / self.t_px
    }
    fn y(&self, v: f64) -> f64 {
        (self.v_top - v) / self.v_px
    }
}

/// A half-open rectangle of changed columns and rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dirty {
    pub c0: u32,
    pub c1: u32,
    pub r0: u32,
    pub r1: u32,
}

impl Dirty {
    /// The smallest rectangle containing both nonempty rectangles.
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            c0: self.c0.min(other.c0),
            c1: self.c1.max(other.c1),
            r0: self.r0.min(other.r0),
            r1: self.r1.max(other.r1),
        }
    }

    /// Whether the rectangle covers no pixels.
    pub fn is_empty(self) -> bool {
        self.c0 >= self.c1 || self.r0 >= self.r1
    }
}

/// A frame's dwell counts: column-major, `w * h` floats.
#[derive(Clone, Debug, PartialEq)]
pub struct Hits {
    pub w: u32,
    pub h: u32,
    pub data: Vec<f32>,
    /// Waveforms completed since the last clear.
    pub waveforms: u64,
    /// Per column, the windows that swept it into this frame: a window that hands over its settled
    /// columns while it still draws (`Stroke::sweep`) counts here, its remaining columns too when it
    /// completes, instead of in `whole()`. Empty when none did. Column `c`'s waveform weight in this
    /// frame is `whole() + partial[c]`.
    pub partial: Vec<f32>,
    /// Of `waveforms`, those whose columns came through `partial`.
    pub partial_done: u64,
    /// Feed configuration epoch; zero for standalone rasters. Never merge different axes.
    pub generation: u64,
    /// Actual raster axes. Source generations alone cannot identify in-place value retunes.
    pub map: Option<PhosphorMap>,
    /// Overlapping windows can finish on different maps in one publication interval.
    pub mixed_maps: bool,
    /// The newest waveform holds a steady source's values from this trigger-relative time on
    /// (repeated, not observed; before the window's start: all of it). None when it was observed.
    pub held: Option<f64>,
    /// The newest waveform's stretch below the simulation's time resolution, trigger-relative
    /// (the first such step's start to the last one's end); None when all of it was resolved.
    pub below: Option<(f64, f64)>,
    pub dirty: Dirty,
    /// Saturation reported by the source for this frame, in lane units.
    pub clipped: fab_elec_bench::source::Clipped,
}

impl Hits {
    pub fn new(w: u32, h: u32) -> Hits {
        Hits {
            w,
            h,
            data: vec![0.0; w as usize * h as usize],
            waveforms: 0,
            partial: Vec::new(),
            partial_done: 0,
            generation: 0,
            map: None,
            mixed_maps: false,
            held: None,
            below: None,
            dirty: Dirty::default(),
            clipped: Default::default(),
        }
    }
    /// Clear the dirty rectangle, or the whole buffer on resize, reusing its allocation.
    pub fn reset(&mut self, w: u32, h: u32) {
        if (self.w, self.h) == (w, h) {
            if !self.dirty.is_empty() {
                for c in self.dirty.c0..self.dirty.c1 {
                    let start = c as usize * h as usize;
                    self.data[start + self.dirty.r0 as usize..start + self.dirty.r1 as usize].fill(0.0);
                }
            }
        } else {
            self.data.resize(w as usize * h as usize, 0.0);
            self.data.fill(0.0);
            self.w = w;
            self.h = h;
        }
        self.clipped = Default::default();
        self.waveforms = 0;
        self.partial.clear();
        self.partial_done = 0;
        self.generation = 0;
        self.map = None;
        self.mixed_maps = false;
        self.held = None;
        self.below = None;
        self.dirty = Dirty::default();
    }
    /// Mark direct writes to `data`, clipped to this frame's dimensions.
    pub fn mark(&mut self, c0: u32, c1: u32, r0: u32, r1: u32) {
        let dirty = Dirty {
            c0: c0.min(self.w),
            c1: c1.min(self.w),
            r0: r0.min(self.h),
            r1: r1.min(self.h),
        };
        if !dirty.is_empty() {
            self.dirty = self.dirty.union(dirty);
        }
    }
    fn record_map(&mut self, map: Option<PhosphorMap>) {
        if self.map.is_none() { self.map = map; }
        else if self.map != map { self.mixed_maps = true; }
    }

    /// The waveforms that came whole: completed without a sweep before.
    pub fn whole(&self) -> u64 {
        self.waveforms.saturating_sub(self.partial_done)
    }

    /// Whether the frame brings waveform weight to any column (a whole waveform, or a sweep's).
    pub fn swept(&self) -> bool {
        self.whole() > 0 || self.partial.iter().any(|&n| n > 0.0)
    }

    /// Columns `range` of one window arrive in this frame while it sweeps or as it completes.
    fn add_partial(&mut self, range: std::ops::Range<usize>) {
        if self.partial.len() != self.w as usize { self.partial.resize(self.w as usize, 0.0); }
        for n in &mut self.partial[range] { *n += 1.0; }
    }

    /// Add another worker's dirty counts and waveform count at the same size.
    pub fn add(&mut self, other: &Hits) {
        debug_assert_eq!((self.w, self.h), (other.w, other.h));
        self.clipped.include(other.clipped);
        // The newer frame's newest waveform decides whether the picture ends held.
        if other.swept() { self.record_map(other.map); self.mixed_maps |= other.mixed_maps; }
        if other.waveforms > 0 { self.held = other.held; self.below = other.below; }
        if !other.partial.is_empty() {
            if self.partial.len() != self.w as usize { self.partial.resize(self.w as usize, 0.0); }
            for (n, &m) in self.partial.iter_mut().zip(&other.partial) { *n += m; }
        }
        self.partial_done += other.partial_done;
        if !other.dirty.is_empty() {
            for c in other.dirty.c0..other.dirty.c1 {
                let start = c as usize * self.h as usize;
                let range = start + other.dirty.r0 as usize..start + other.dirty.r1 as usize;
                for (a, b) in self.data[range.clone()].iter_mut().zip(&other.data[range]) {
                    *a += *b;
                }
            }
            self.dirty = self.dirty.union(other.dirty);
        }
        self.waveforms += other.waveforms;
    }
    /// The count at a pixel.
    pub fn at(&self, col: u32, row: u32) -> f32 {
        self.data[col as usize * self.h as usize + row as usize]
    }
}

/// Reusable column spans for one acquisition window.
#[derive(Clone, Debug, Default)]
pub struct Stroke {
    lo: Vec<f32>,
    hi: Vec<f32>,
    touched: Vec<u32>,
    map: Option<PhosphorMap>,
    t_trigger: f64,
    /// The polyline's last end (time, value, x, row): the next segment from it reuses its x and
    /// row, so a sample costs one x and one row however many windows draw it.
    last: Option<(f64, f32, f64, f64)>,
    /// Columns before this one were handed to a frame while the window drew (`sweep`).
    committed: usize,
}

impl Stroke {
    /// Start a window, discarding any unfinished coverage.
    pub fn begin(&mut self, map: &PhosphorMap, t_trigger: f64) {
        for c in self.touched.drain(..) {
            self.lo[c as usize] = f32::INFINITY;
            self.hi[c as usize] = f32::NEG_INFINITY;
        }
        self.lo.resize(map.w as usize, f32::INFINITY);
        self.hi.resize(map.w as usize, f32::NEG_INFINITY);
        self.map = Some(*map);
        self.t_trigger = t_trigger;
        self.last = None;
        self.committed = 0;
    }

    /// Extend the window with a segment at absolute times; equal times make a vertical span.
    pub fn segment(&mut self, t0: f64, y0: f32, t1: f64, y1: f32) {
        let Some(map) = self.map else { return };
        if map.w == 0 || map.h == 0 {
            return;
        }
        let wf = map.w as f64;
        let (x0, r0) = match self.last {
            Some((t, v, x, r)) if t == t0 && v == y0 => (x, r),
            _ => (map.x(t0 - self.t_trigger), map.y(y0 as f64)),
        };
        let (x1, r1) = (map.x(t1 - self.t_trigger), map.y(y1 as f64));
        self.last = Some((t1, y1, x1, r1));
        let (y0, y1) = (r0, r1);
        // Most samples of a dense waveform stay in their predecessor's column: the segment is that
        // column's span between its two ends, as the walk below finds it.
        if x1 > x0 && x0 >= 0.0 && x1 < wf - 1e-9 && x0 as usize == x1 as usize {
            self.span(x0 as usize, y0.min(y1) as f32, y0.max(y1) as f32);
            return;
        }
        if !(x1 >= x0) || x1 < 0.0 || x0 >= wf {
            return;
        }
        if x1 == x0 {
            self.span(x0 as usize, y0.min(y1) as f32, y0.max(y1) as f32);
            return;
        }
        let slope = (y1 - y0) / (x1 - x0);
        let (xa, xb) = (x0.max(0.0), x1.min(wf - 1e-9));
        let (ca, cb) = (xa as usize, xb as usize);
        for c in ca..=cb {
            // The segment's part inside column c.
            let (sa, sb) = ((c as f64).max(xa), ((c + 1) as f64).min(xb));
            let (ya, yb) = (y0 + slope * (sa - x0), y0 + slope * (sb - x0));
            self.span(c, ya.min(yb) as f32, ya.max(yb) as f32);
        }
    }

    /// Extend the window with a run of samples over `t0..=t1` (the segment into its first sample
    /// drawn already) whose values span `lo..=hi` and end at `last`, without visiting them: a run
    /// inside one column adds exactly the span its own segments add there, `lo..=hi`, and a run
    /// wholly outside the window adds nothing, as they would. Both return true. A run across a
    /// column edge or into the last column's guard draws nothing and returns false: the caller
    /// draws its segments. The next segment starts from `last`.
    pub fn fold(&mut self, t0: f64, t1: f64, lo: f32, hi: f32, last: f32) -> bool {
        let Some(map) = self.map else { return true };
        if map.w == 0 || map.h == 0 {
            return true;
        }
        let wf = map.w as f64;
        let x0 = match self.last {
            Some((t, _, x, _)) if t == t0 => x,
            _ => map.x(t0 - self.t_trigger),
        };
        let x1 = map.x(t1 - self.t_trigger);
        if x1 < 0.0 || x0 >= wf {
            self.last = None;
            return true;
        }
        if !(x0 >= 0.0 && x1 < wf - 1e-9 && x0 as usize == x1 as usize) {
            return false;
        }
        let (ra, rb) = (map.y(lo as f64), map.y(hi as f64));
        self.span(x0 as usize, ra.min(rb) as f32, ra.max(rb) as f32);
        self.last = Some((t1, last, x1, map.y(last as f64)));
        true
    }

    /// Add a lone sample at an absolute time.
    pub fn point(&mut self, t: f64, y: f32) {
        let Some(map) = self.map else { return };
        if map.w == 0 || map.h == 0 {
            return;
        }
        let (x, r) = (map.x(t - self.t_trigger), map.y(y as f64));
        self.last = Some((t, y, x, r));
        if x >= 0.0 && x < map.w as f64 {
            self.span(x as usize, r as f32, r as f32);
        }
    }

    /// Share one count over each visible column span and reuse the scratch. A window that swept
    /// (`sweep`) hands over only its columns from the last sweep on.
    pub fn finish(&mut self, hits: &mut Hits) {
        self.last = None;
        let Some(map) = self.map.take() else { return };
        if map.w == 0 || map.h == 0 {
            return;
        }
        debug_assert_eq!((hits.w, hits.h), (map.w, map.h), "stroke and frame dimensions must match");
        hits.record_map(Some(map));
        self.hand_over(hits, self.touched.len());
        if self.committed > 0 {
            hits.add_partial(self.committed..map.w as usize);
            hits.partial_done += 1;
            self.committed = 0;
        }
        hits.waveforms += 1;
    }

    /// While the window still draws, hands its settled columns to `hits`: those left of its pen,
    /// which no later sample reaches. Each column is handed over once, counted in `hits.partial`,
    /// with the same shares `finish` gives it. O(columns settled since the last sweep).
    pub fn sweep(&mut self, hits: &mut Hits) {
        let (Some(map), Some((_, _, x, _))) = (self.map, self.last) else { return };
        if map.w == 0 || map.h == 0 {
            return;
        }
        let upto = if x > 0.0 { (x as usize).min(map.w as usize) } else { 0 };
        if upto <= self.committed {
            return;
        }
        debug_assert_eq!((hits.w, hits.h), (map.w, map.h), "stroke and frame dimensions must match");
        hits.record_map(Some(map));
        // The pen only moves right, so the touched columns are in ascending order.
        debug_assert!(self.touched.windows(2).all(|p| p[0] < p[1]), "a stroke touches its columns left to right");
        let settled = self.touched.partition_point(|&c| (c as usize) < upto);
        self.hand_over(hits, settled);
        hits.add_partial(self.committed..upto);
        self.committed = upto;
    }

    /// Adds the first `n` touched columns' spans to `hits` and forgets them.
    fn hand_over(&mut self, hits: &mut Hits, n: usize) {
        let h = hits.h as usize;
        let hf = hits.h as f32;
        for &c in &self.touched[..n] {
            let c = c as usize;
            // Normalize the visible span, including clipped edge rows.
            let (lo, hi) = ((self.lo[c] - 0.5).max(0.0), (self.hi[c] + 0.5).min(hf));
            self.lo[c] = f32::INFINITY;
            self.hi[c] = f32::NEG_INFINITY;
            if !(hi > lo) {
                continue;
            }
            let col = &mut hits.data[c * h..c * h + h];
            let (ra, rb) = (lo as usize, ((hi.ceil() as usize).max(1) - 1).min(h - 1));
            if ra == rb {
                col[ra] += 1.0;
            } else {
                let scale = 1.0 / (hi - lo);
                col[ra] += ((ra + 1) as f32 - lo) * scale;
                for v in &mut col[ra + 1..rb] {
                    *v += scale;
                }
                col[rb] += (hi - rb as f32) * scale;
            }
            hits.mark(c as u32, c as u32 + 1, ra as u32, rb as u32 + 1);
        }
        self.touched.drain(..n);
    }

    fn span(&mut self, c: usize, lo: f32, hi: f32) {
        if self.lo[c] == f32::INFINITY {
            self.touched.push(c as u32);
        }
        self.lo[c] = self.lo[c].min(lo);
        self.hi[c] = self.hi[c].max(hi);
    }
}

/// Reusable scratch for adding sampled waveforms in one call.
#[derive(Clone, Debug, Default)]
pub struct Raster {
    stroke: Stroke,
}

impl Raster {
    pub fn new() -> Raster {
        Raster::default()
    }

    /// Add one waveform with times relative to its trigger, including equal-time min/max pairs.
    pub fn add(&mut self, map: &PhosphorMap, t: &[f64], y: &[f32], hits: &mut Hits) {
        let n = t.len().min(y.len());
        if n == 0 || map.w == 0 || map.h == 0 {
            return;
        }
        self.stroke.begin(map, 0.0);
        if n == 1 {
            self.stroke.point(t[0], y[0]);
        }
        for k in 1..n {
            self.stroke.segment(t[k - 1], y[k - 1], t[k], y[k]);
        }
        self.stroke.finish(hits);
    }
}

/// The intensity a count maps to on the grading ramp: logarithmic, 0 for none, 1 at `full`
/// counts (and above). The shader's grading uses the same curve.
pub fn grade_x(count: f32, full: f32) -> f32 {
    if !(count > 0.0) {
        return 0.0;
    }
    ((1.0 + count).ln() / (1.0 + full.max(1.0)).ln()).min(1.0)
}

/// The persistence factor kept per frame for a decay time `tau` (seconds; infinite keeps all,
/// zero keeps nothing) at a frame interval `dt`.
pub fn decay_keep(tau: f64, dt: f64) -> f64 {
    if tau == f64::INFINITY {
        1.0
    } else if !(tau > 0.0) {
        0.0
    } else {
        (-dt / tau).exp()
    }
}

/// One accumulation pass: `p' = keep * p + scale * c`, with pixel hits `c`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blend {
    pub keep: f32,
    pub scale: f32,
}

/// The waveform weight behind each column of a picture: `N' = exp(-dt / tau) * N + n`, `n` being
/// the waveforms that reached the column in a frame. A column's decay spans the time since a
/// frame last brought it waveforms, so a column a sweep has not reached yet keeps its picture as
/// it was, as a scope's does. Whole waveforms reach every column alike: then every column has the
/// one weight of the picture's waveform-weighted mean.
#[derive(Debug, Default)]
pub struct Persistence {
    weight: Vec<f64>,
    elapsed: Vec<f64>,
}

impl Clone for Persistence {
    fn clone(&self) -> Self {
        Self { weight: self.weight.clone(), elapsed: self.elapsed.clone() }
    }
    /// Reuses the allocations: a view tries each frame's blend on a copy.
    fn clone_from(&mut self, source: &Self) {
        self.weight.clone_from(&source.weight);
        self.elapsed.clone_from(&source.elapsed);
    }
}

impl Persistence {
    /// Forget the picture: the next frame replaces it.
    pub fn clear(&mut self) {
        self.weight.clear();
        self.elapsed.clear();
    }

    /// `dt` seconds pass with nothing blended: every column's decay waits for its next waveforms.
    pub fn idle(&mut self, dt: f64) {
        for elapsed in &mut self.elapsed {
            *elapsed += dt.max(0.0);
        }
    }

    /// Blends a frame of `columns` columns after `dt` seconds: `whole` waveforms reach every
    /// column and `partial[c]` more reach column `c` (`Hits::whole`, `Hits::partial`; empty for
    /// none). Writes one blend per column into `out`, a column no waveform reached keeping its
    /// picture (keep 1, scale 0). False, writing nothing, for a frame that brings none: it retains
    /// the elapsed time. Infinite `tau` gives a running mean; zero replaces what is reached.
    /// O(columns).
    pub fn frame(&mut self, columns: usize, whole: u64, partial: &[f32], dt: f64, tau: f64, out: &mut Vec<Blend>) -> bool {
        if self.weight.len() != columns {
            self.weight.resize(columns, 0.0);
            self.elapsed.resize(columns, 0.0);
        }
        self.idle(dt);
        let n = |c: usize| whole as f64 + partial.get(c).map_or(0.0, |&p| f64::from(p));
        if !(0..columns).any(|c| n(c) > 0.0) {
            return false;
        }
        out.clear();
        for c in 0..columns {
            let n = n(c);
            if n > 0.0 {
                let kept = decay_keep(tau, self.elapsed[c]) * self.weight[c];
                self.weight[c] = kept + n;
                self.elapsed[c] = 0.0;
                out.push(Blend {
                    keep: (kept / self.weight[c]) as f32,
                    scale: (1.0 / self.weight[c]) as f32,
                });
            } else {
                out.push(Blend { keep: 1.0, scale: 0.0 });
            }
        }
        true
    }

    /// A column's decayed waveform weight, excluding pending idle time.
    pub fn weight(&self, column: usize) -> f64 {
        self.weight.get(column).copied().unwrap_or(0.0)
    }
}
