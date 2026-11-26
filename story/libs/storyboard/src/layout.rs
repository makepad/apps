//! The canvas's geometry, in logical pixels: time across (a ruler on top,
//! zoomed and panned), rows down (a header column on the left), items as
//! boxes. Items of one row that overlap in time stack in lanes, so nothing
//! hides another. Pure: the widget draws what this computes and asks it
//! what is under the pointer and where a drag snaps.

use crate::model::*;

pub const RULER: f64 = 30.0;
pub const HEADER: f64 = 132.0;
pub const ROW_GAP: f64 = 6.0;
pub const LANE_GAP: f64 = 4.0;
pub const PAD: f64 = 6.0;
/// Grab width of an item's ends (resize).
pub const EDGE: f64 = 7.0;
pub const MIN_PX_PER_SEC: f64 = 4.0;
pub const MAX_PX_PER_SEC: f64 = 800.0;
/// How near (px) a drag snaps.
pub const SNAP_PX: f64 = 8.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w && other.x < self.x + self.w && self.y < other.y + other.h && other.y < self.y + self.h
    }

    /// The rectangle spanned by two corners.
    pub fn from_corners(a: (f64, f64), b: (f64, f64)) -> Self {
        Self { x: a.0.min(b.0), y: a.1.min(b.1), w: (a.0 - b.0).abs(), h: (a.1 - b.1).abs() }
    }
}

/// Zoom and pan. `t0` is the time at the left edge of the tracks, `y0`
/// the vertical scroll of the rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub px_per_sec: f64,
    pub t0: f64,
    pub y0: f64,
    /// The canvas's size.
    pub width: f64,
    pub height: f64,
}

impl Default for View {
    fn default() -> Self {
        Self { px_per_sec: 60.0, t0: 0.0, y0: 0.0, width: 1200.0, height: 600.0 }
    }
}

impl View {
    pub fn x_of(&self, t: f64) -> f64 {
        HEADER + (t - self.t0) * self.px_per_sec
    }

    pub fn t_of(&self, x: f64) -> f64 {
        self.t0 + (x - HEADER) / self.px_per_sec
    }

    /// Show `[0, duration]` across the tracks, with a little room.
    pub fn fit(&mut self, duration: f64) {
        let tracks = (self.width - HEADER - 24.0).max(100.0);
        self.px_per_sec = (tracks / duration.max(1.0)).clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
        self.t0 = -8.0 / self.px_per_sec;
    }

    /// Zoom by `factor`, keeping the time under `x` where it is.
    pub fn zoom_at(&mut self, factor: f64, x: f64) {
        let t = self.t_of(x);
        self.px_per_sec = (self.px_per_sec * factor).clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
        self.t0 = t - (x - HEADER) / self.px_per_sec;
    }

    /// Pan by pixels, clamped so the board stays in reach.
    pub fn pan(&mut self, dx: f64, dy: f64, duration: f64, content_height: f64) {
        let visible = (self.width - HEADER) / self.px_per_sec;
        let slack = visible * 0.5;
        self.t0 = (self.t0 - dx / self.px_per_sec).clamp(-slack, (duration - visible + slack).max(-slack));
        let max_y = (content_height - (self.height - RULER) + 40.0).max(0.0);
        self.y0 = (self.y0 - dy).clamp(0.0, max_y);
    }
}

/// A row's height per lane by kind.
pub fn lane_height(kind: RowKind) -> f64 {
    match kind {
        RowKind::Story => 104.0,
        RowKind::Visuals => 50.0,
        RowKind::Voice => 44.0,
        RowKind::Music => 40.0,
        RowKind::Refs => 88.0,
        RowKind::Notes => 48.0,
        RowKind::Chapters => 96.0,
        RowKind::Treatments => 112.0,
        RowKind::Bible => 96.0,
        RowKind::Sections => 40.0,
        RowKind::Lyrics => 92.0,
        RowKind::Devices => 52.0,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RowBox {
    pub id: String,
    pub kind: RowKind,
    /// The row's band, across the whole width (header included).
    pub rect: Rect,
    pub lanes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ItemBox {
    pub id: String,
    pub row: usize,
    pub lane: usize,
    pub rect: Rect,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub rows: Vec<RowBox>,
    /// In board order (the draw order: later on top).
    pub items: Vec<ItemBox>,
    /// The rows' total height (for scrolling).
    pub content_height: f64,
}

impl Layout {
    pub fn item(&self, id: &str) -> Option<&ItemBox> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// Lanes for intervals `(index, start, end)`: each goes into the first
/// lane whose last item ends before it starts (in start order).
pub fn pack(intervals: &[(usize, f64, f64)]) -> (Vec<usize>, usize) {
    let mut order: Vec<usize> = (0..intervals.len()).collect();
    order.sort_by(|a, b| intervals[*a].1.total_cmp(&intervals[*b].1).then(intervals[*a].0.cmp(&intervals[*b].0)));
    let mut lane_ends: Vec<f64> = Vec::new();
    let mut lanes = vec![0; intervals.len()];
    for index in order {
        let (_, start, end) = intervals[index];
        match lane_ends.iter().position(|lane_end| *lane_end <= start + 1e-9) {
            Some(lane) => {
                lane_ends[lane] = end;
                lanes[index] = lane;
            }
            None => {
                lanes[index] = lane_ends.len();
                lane_ends.push(end);
            }
        }
    }
    let count = lane_ends.len().max(1);
    (lanes, count)
}

pub fn layout(board: &Storyboard, view: &View) -> Layout {
    let mut out = Layout::default();
    let mut y = RULER + ROW_GAP - view.y0;
    let top = y;
    for (row_index, row) in board.rows.iter().enumerate() {
        let items: Vec<(usize, &Item)> = board.items.iter().enumerate().filter(|(_, item)| item.row == row.id).collect();
        let intervals: Vec<(usize, f64, f64)> = items.iter().map(|(index, item)| (*index, item.start, item.end())).collect();
        let (lanes, count) = pack(&intervals);
        let lane_h = lane_height(row.kind);
        let height = PAD * 2.0 + count as f64 * lane_h + (count - 1) as f64 * LANE_GAP;
        out.rows.push(RowBox { id: row.id.clone(), kind: row.kind, rect: Rect::new(0.0, y, view.width, height), lanes: count });
        for ((index, item), lane) in items.iter().zip(lanes) {
            let x0 = view.x_of(item.start);
            let x1 = view.x_of(item.end());
            let rect = Rect::new(x0 + 1.0, y + PAD + lane as f64 * (lane_h + LANE_GAP), (x1 - x0 - 2.0).max(6.0), lane_h);
            out.items.push(ItemBox { id: item.id.clone(), row: row_index, lane, rect });
            let _ = index;
        }
        y += height + ROW_GAP;
    }
    // Board order, so later items draw on top.
    out.items.sort_by_key(|item_box| board.items.iter().position(|item| item.id == item_box.id).unwrap_or(usize::MAX));
    out.content_height = y - top;
    out
}

/// What is under a point.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Ruler { t: f64 },
    RowHeader { row: String },
    Item { id: String, part: Part },
    /// Empty space in a row (or below the rows: `row` is `None`).
    Empty { row: Option<String>, t: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Body,
    Start,
    End,
}

pub fn hit(layout: &Layout, view: &View, x: f64, y: f64) -> Hit {
    if y < RULER {
        return Hit::Ruler { t: view.t_of(x) };
    }
    if x < HEADER {
        if let Some(row) = layout.rows.iter().find(|row| row.rect.contains(x, y)) {
            return Hit::RowHeader { row: row.id.clone() };
        }
        return Hit::Empty { row: None, t: view.t_of(HEADER) };
    }
    // Topmost first.
    for item in layout.items.iter().rev() {
        let r = item.rect;
        if r.contains(x, y) {
            let edge = EDGE.min(r.w / 3.0);
            let part = if x < r.x + edge { Part::Start } else if x >= r.x + r.w - edge { Part::End } else { Part::Body };
            return Hit::Item { id: item.id.clone(), part };
        }
    }
    let row = layout.rows.iter().find(|row| row.rect.contains(x, y)).map(|row| row.id.clone());
    Hit::Empty { row, t: view.t_of(x) }
}

/// The row a y falls in (nearest row when between or beyond them).
pub fn row_at(layout: &Layout, y: f64) -> Option<&RowBox> {
    layout.rows.iter().min_by(|a, b| {
        let distance = |row: &RowBox| if y < row.rect.y { row.rect.y - y } else if y > row.rect.y + row.rect.h { y - row.rect.y - row.rect.h } else { 0.0 };
        distance(a).total_cmp(&distance(b))
    })
}

/// Items whose boxes meet a marquee.
pub fn marquee(layout: &Layout, rect: &Rect) -> Vec<String> {
    layout.items.iter().filter(|item| item.rect.intersects(rect)).map(|item| item.id.clone()).collect()
}

// ------------------------------------------------------------- ruler

/// One tick of the ruler.
#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    pub t: f64,
    pub major: bool,
    /// Set on labelled ticks.
    pub label: Option<String>,
}

/// `m:ss` (and `.f` when the step is under a second).
pub fn time_label(t: f64, fine: bool) -> String {
    let t = if t.abs() < 1e-9 { 0.0 } else { t };
    let sign = if t < 0.0 { "-" } else { "" };
    let t = t.abs();
    let minutes = (t / 60.0).floor() as i64;
    let seconds = t - minutes as f64 * 60.0;
    if fine {
        format!("{sign}{minutes}:{seconds:04.1}")
    } else {
        format!("{sign}{minutes}:{:02}", seconds.round() as i64)
    }
}

const SECOND_STEPS: [f64; 13] = [0.1, 0.2, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0];

/// Seconds ticks for `[t0, t1]`: labels at least `min_label_px` apart,
/// minor ticks between.
pub fn second_ticks(px_per_sec: f64, t0: f64, t1: f64, min_label_px: f64) -> Vec<Tick> {
    let major = SECOND_STEPS.iter().copied().find(|step| step * px_per_sec >= min_label_px).unwrap_or(600.0);
    let minor = SECOND_STEPS.iter().copied().rev().find(|step| *step < major && step * px_per_sec >= 8.0 && ((major / step) - (major / step).round()).abs() < 1e-6).unwrap_or(major);
    let fine = major < 1.0;
    let mut out = Vec::new();
    let first = (t0.max(0.0) / minor).floor() as i64;
    let last = (t1 / minor).ceil() as i64;
    for n in first..=last.min(first + 4000) {
        let t = n as f64 * minor;
        if t < t0 - 1e-9 || t > t1 + 1e-9 {
            continue;
        }
        let is_major = ((t / major) - (t / major).round()).abs() < 1e-6;
        out.push(Tick { t, major: is_major, label: is_major.then(|| time_label(t, fine)) });
    }
    out
}

/// The beat grid: bars labelled `bar.beat` (1-based, as musicians count)
/// when there is room, beats as minor ticks.
pub fn beat_ticks(bpm: f64, offset: f64, beats_per_bar: u32, px_per_sec: f64, t0: f64, t1: f64) -> Vec<Tick> {
    let beat = 60.0 / bpm;
    let per_bar = beats_per_bar.max(1) as i64;
    let beat_px = beat * px_per_sec;
    // Label every bar, every 2, 4, 8… bars so labels stay apart.
    let mut bars_per_label = 1;
    while (bars_per_label * per_bar) as f64 * beat_px < 44.0 && bars_per_label < 1024 {
        bars_per_label *= 2;
    }
    let show_beats = beat_px >= 6.0;
    let first = ((t0 - offset) / beat).floor().max(0.0) as i64;
    let last = ((t1 - offset) / beat).ceil() as i64;
    let mut out = Vec::new();
    for n in first..=last.min(first + 8000) {
        let t = offset + n as f64 * beat;
        if t < t0 - 1e-9 || t > t1 + 1e-9 {
            continue;
        }
        let bar = n / per_bar;
        let is_bar = n % per_bar == 0;
        if !is_bar && !show_beats {
            continue;
        }
        let labelled = is_bar && bar % bars_per_label == 0;
        out.push(Tick { t, major: is_bar, label: labelled.then(|| format!("{}", bar + 1)) });
    }
    out
}

// ------------------------------------------------------------- snapping

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapTo {
    Off,
    /// Whole seconds (tenths when zoomed far in).
    Seconds,
    /// Beats of the board's tempo.
    Beats,
}

/// What a time snapped to, for the snap line's label.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapped {
    pub t: f64,
    pub what: Option<String>,
}

/// Snap `t` to the nearest item edge in `edges`, else the grid, within
/// `SNAP_PX` on screen.
pub fn snap(t: f64, mode: SnapTo, board: &Storyboard, px_per_sec: f64, edges: &[(f64, String)]) -> Snapped {
    if mode == SnapTo::Off {
        return Snapped { t, what: None };
    }
    let reach = SNAP_PX / px_per_sec;
    if let Some((edge, name)) = edges.iter().filter(|(edge, _)| (edge - t).abs() <= reach).min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs())) {
        return Snapped { t: *edge, what: Some(name.clone()) };
    }
    match (mode, board.bpm) {
        (SnapTo::Beats, Some(bpm)) => {
            let beat = 60.0 / bpm;
            let n = ((t - board.beat_offset) / beat).round();
            let snapped = board.beat_offset + n * beat;
            if (snapped - t).abs() <= reach.max(beat * 0.5) {
                let per_bar = board.beats_per_bar.max(1) as i64;
                let n = n as i64;
                return Snapped { t: snapped.max(0.0), what: Some(format!("bar {} beat {}", n.div_euclid(per_bar) + 1, n.rem_euclid(per_bar) + 1)) };
            }
            Snapped { t, what: None }
        }
        _ => {
            // Whole seconds, or tenths when a second is wide.
            let step = if px_per_sec >= 200.0 { 0.1 } else if px_per_sec >= 40.0 { 0.5 } else { 1.0 };
            let snapped = (t / step).round() * step;
            Snapped { t: snapped.max(0.0), what: None }
        }
    }
}

/// Every item's start and end but those being dragged, named for the snap
/// label.
pub fn edges(board: &Storyboard, except: &[String]) -> Vec<(f64, String)> {
    let mut out = vec![(0.0, "start".to_string()), (board.duration, "end".to_string())];
    for item in &board.items {
        if except.contains(&item.id) {
            continue;
        }
        let name = item.headline();
        out.push((item.start, format!("{name} start")));
        out.push((item.end(), format!("{name} end")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Storyboard {
        let mut board = Storyboard { duration: 20.0, bpm: Some(120.0), ..Default::default() }.with_default_rows();
        let note = |id: &str, row: &str, start: f64, length: f64| Item { id: id.into(), row: row.into(), start, length, color: None, band: None, body: Body::Note { text: id.into() } };
        board.items = vec![note("a", "voice", 0.0, 4.0), note("b", "voice", 2.0, 4.0), note("c", "voice", 4.0, 2.0), note("d", "story", 1.0, 3.0)];
        board
    }

    #[test]
    fn overlapping_items_stack_in_lanes() {
        let (lanes, count) = pack(&[(0, 0.0, 4.0), (1, 2.0, 6.0), (2, 4.0, 6.0), (3, 6.0, 7.0)]);
        assert_eq!((lanes, count), (vec![0, 1, 0, 0], 2));
        let board = board();
        let view = View { px_per_sec: 50.0, t0: 0.0, y0: 0.0, width: 1400.0, height: 800.0 };
        let layout = layout(&board, &view);
        let voice = layout.rows.iter().find(|row| row.id == "voice").unwrap();
        assert_eq!(voice.lanes, 2);
        assert_eq!(voice.rect.h, PAD * 2.0 + 2.0 * lane_height(RowKind::Voice) + LANE_GAP);
        let a = layout.item("a").unwrap();
        let b = layout.item("b").unwrap();
        assert_eq!((a.lane, b.lane, layout.item("c").unwrap().lane), (0, 1, 0));
        assert!((a.rect.x - (HEADER + 1.0)).abs() < 1e-9 && (a.rect.w - (200.0 - 2.0)).abs() < 1e-9);
        assert!(b.rect.y > a.rect.y);
        // Rows are stacked in order below the ruler.
        assert!(layout.rows[0].rect.y >= RULER);
        assert!(layout.rows.windows(2).all(|w| w[1].rect.y >= w[0].rect.y + w[0].rect.h));
    }

    #[test]
    fn hits_and_marquee() {
        let board = board();
        let view = View { px_per_sec: 50.0, t0: 0.0, y0: 0.0, width: 1400.0, height: 800.0 };
        let layout = layout(&board, &view);
        let a = layout.item("a").unwrap().rect;
        assert_eq!(hit(&layout, &view, a.x + a.w / 2.0, a.y + 5.0), Hit::Item { id: "a".into(), part: Part::Body });
        assert_eq!(hit(&layout, &view, a.x + 2.0, a.y + 5.0), Hit::Item { id: "a".into(), part: Part::Start });
        assert_eq!(hit(&layout, &view, a.x + a.w - 2.0, a.y + 5.0), Hit::Item { id: "a".into(), part: Part::End });
        assert!(matches!(hit(&layout, &view, 500.0, 10.0), Hit::Ruler { t } if (t - (500.0 - HEADER) / 50.0).abs() < 1e-9));
        assert!(matches!(hit(&layout, &view, 20.0, a.y + 5.0), Hit::RowHeader { row } if row == "voice"));
        let voice = layout.rows.iter().find(|row| row.id == "voice").unwrap();
        assert!(matches!(hit(&layout, &view, view.x_of(15.0), voice.rect.y + 10.0), Hit::Empty { row: Some(row), t } if row == "voice" && (t - 15.0).abs() < 1e-9));
        let all = marquee(&layout, &Rect::from_corners((HEADER, voice.rect.y), (view.x_of(3.0), voice.rect.y + voice.rect.h)));
        assert_eq!(all, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn zoom_keeps_the_pointer_time_and_fit_shows_all() {
        let mut view = View::default();
        let before = view.t_of(700.0);
        view.zoom_at(2.0, 700.0);
        assert!((view.t_of(700.0) - before).abs() < 1e-9);
        assert_eq!(view.px_per_sec, 120.0);
        view.fit(20.0);
        assert!(view.x_of(20.0) <= view.width && view.x_of(0.0) >= HEADER);
    }

    #[test]
    fn ruler_ticks_and_snapping() {
        let ticks = second_ticks(60.0, 0.0, 10.0, 56.0);
        let labels: Vec<&str> = ticks.iter().filter_map(|tick| tick.label.as_deref()).collect();
        assert_eq!(labels, ["0:00", "0:01", "0:02", "0:03", "0:04", "0:05", "0:06", "0:07", "0:08", "0:09", "0:10"]);
        assert!(ticks.iter().any(|tick| !tick.major));
        let fine = second_ticks(400.0, 0.0, 1.0, 56.0);
        assert!(fine.iter().any(|tick| tick.label.as_deref() == Some("0:00.2")));
        let beats = beat_ticks(120.0, 0.0, 4, 60.0, 0.0, 8.0);
        assert_eq!(beats.iter().filter_map(|tick| tick.label.clone()).collect::<Vec<_>>(), ["1", "2", "3", "4", "5"]);
        assert_eq!(beats.len(), 17);
        let board = board();
        let snapped = snap(3.9, SnapTo::Seconds, &board, 60.0, &edges(&board, &["c".into()]));
        assert_eq!(snapped, Snapped { t: 4.0, what: Some("a end".into()) });
        let beat = snap(1.3, SnapTo::Beats, &board, 60.0, &[]);
        assert_eq!(beat, Snapped { t: 1.5, what: Some("bar 1 beat 4".into()) });
        assert_eq!(snap(7.3, SnapTo::Seconds, &board, 20.0, &[]).t, 7.0);
        assert_eq!(snap(7.3, SnapTo::Off, &board, 20.0, &[]).t, 7.3);
        assert_eq!(time_label(75.0, false), "1:15");
    }
}
