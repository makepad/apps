//! What the library list decides apart from how it is drawn: which columns
//! Stage can fill, what each cell reads, which rows the FILTER ticks keep and
//! in what order a head puts them, and how wide each shown column is.
//!
//! Ported from the donor console's `track_list_rules` (the rules of its
//! explorer list), cut to the columns Stage's catalogue answers. Pure: no
//! `Cx`, no widget, so it is tested on its own.

use crate::columns::{Column, ColumnWidth};

/// One row of the library list. `key` is the stable identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryRow {
    pub key: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The saved analysis's tempo.
    pub bpm: Option<f64>,
    /// Running time.
    pub seconds: Option<f64>,
    /// The four stems are saved with the track.
    pub stems: bool,
    /// Timed lyrics are saved with the track (karaoke).
    pub lyrics: bool,
    /// When the catalogue last changed the track, epoch ms; 0 is unknown.
    pub added_ms: u64,
    /// The analysis's progress or verdict for this row, after the title.
    pub note: String,
}

/// The columns Stage's library can fill, in the order the column menu lists
/// them when the layout has no other order. Genre, year, bitrate, key and
/// tags are not in the catalogue Stage reads, so they are not offered.
pub const STAGE_COLUMNS: [Column; 8] = [
    Column::Title,
    Column::Artist,
    Column::Album,
    Column::Bpm,
    Column::Time,
    Column::Stem,
    Column::Krk,
    Column::Added,
];

pub fn offered(column: Column) -> bool {
    STAGE_COLUMNS.contains(&column)
}

/// The FILTER ticks, in the order the chip lists them. Several AND together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Has {
    Stems,
    Karaoke,
    Bpm,
}

pub const HAS: [Has; 3] = [Has::Stems, Has::Karaoke, Has::Bpm];

impl Has {
    pub fn label(self) -> &'static str {
        match self {
            Has::Stems => "STEMS",
            Has::Karaoke => "KARAOKE",
            Has::Bpm => "BPM",
        }
    }

    fn holds(self, row: &LibraryRow) -> bool {
        match self {
            Has::Stems => row.stems,
            Has::Karaoke => row.lyrics,
            Has::Bpm => row.bpm.is_some(),
        }
    }
}

/// Does `row` carry everything the lit ticks (`mask`, bit per [`HAS`]) ask
/// for? AND, not OR: "stems and a tempo" is the question a set asks; no tick
/// lit is no filter.
pub fn passes(mask: u32, row: &LibraryRow) -> bool {
    HAS.iter().enumerate().all(|(index, has)| mask & (1 << index) == 0 || has.holds(row))
}

/// `m:ss`, or empty when unknown.
pub fn time_text(seconds: Option<f64>) -> String {
    match seconds {
        Some(seconds) if seconds.is_finite() && seconds >= 0.0 => {
            let total = seconds.round() as u64;
            format!("{}:{:02}", total / 60, total % 60)
        }
        _ => String::new(),
    }
}

/// `YYYY-MM-DD` (UTC) of an epoch-ms stamp, or empty when not recorded.
pub fn date_text(ms: u64) -> String {
    if ms == 0 {
        return String::new();
    }
    let days = (ms / 86_400_000) as i64;
    // Civil date from days since 1970-01-01 (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}-{month:02}-{day:02}")
}

/// What one column reads for one row. The tick columns are a mark.
pub fn cell_text(column: Column, row: &LibraryRow) -> String {
    match column {
        Column::Title if row.note.is_empty() => row.title.clone(),
        Column::Title => format!("{} — {}", row.title, row.note),
        Column::Artist => row.artist.clone(),
        Column::Album => row.album.clone(),
        Column::Bpm => row.bpm.map(|bpm| format!("{bpm:.1}")).unwrap_or_default(),
        Column::Time => time_text(row.seconds),
        Column::Stem => if row.stems { "✓" } else { "" }.to_string(),
        Column::Krk => if row.lyrics { "✓" } else { "" }.to_string(),
        Column::Added => date_text(row.added_ms),
        Column::Genre | Column::Year | Column::Bitrate | Column::Key | Column::Tags => String::new(),
    }
}

/// The rows to show, as indexes into `rows`: those passing the ticks, in
/// the order of the sorted column (`(column, ascending)`), else the parent's
/// own order. The sort is stable, so the parent's order breaks ties; a value
/// nothing has judged yet sorts below every real one.
pub fn filter_sort(rows: &[LibraryRow], mask: u32, sort: Option<(Column, bool)>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).filter(|index| passes(mask, &rows[*index])).collect();
    let Some((column, ascending)) = sort else { return order };
    let number = |value: Option<f64>| value.unwrap_or(-1.0);
    let cmp_num = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
    let text = |a: &str, b: &str| a.to_lowercase().cmp(&b.to_lowercase());
    order.sort_by(|a, b| {
        let (a, b) = (&rows[*a], &rows[*b]);
        match column {
            Column::Title => text(&a.title, &b.title),
            Column::Artist => text(&a.artist, &b.artist),
            Column::Album => text(&a.album, &b.album),
            Column::Bpm => cmp_num(number(a.bpm), number(b.bpm)),
            Column::Time => cmp_num(number(a.seconds), number(b.seconds)),
            Column::Stem => a.stems.cmp(&b.stems),
            Column::Krk => a.lyrics.cmp(&b.lyrics),
            Column::Added => a.added_ms.cmp(&b.added_ms),
            Column::Genre | Column::Year | Column::Bitrate | Column::Key | Column::Tags => std::cmp::Ordering::Equal,
        }
    });
    if !ascending {
        order.reverse();
    }
    order
}

/// The sort after a head is clicked: up, then down, then the parent's order.
pub fn sort_after_head(sort: Option<(Column, bool)>, column: Column) -> Option<(Column, bool)> {
    match sort {
        Some((current, true)) if current == column => Some((column, false)),
        Some((current, false)) if current == column => None,
        _ => Some((column, true)),
    }
}

/// Each shown column's width across `width` points: a column the operator
/// sized keeps it, a fixed one takes its own, and the fill columns share
/// what is left within their min/max. Nothing collapses below `floor`.
pub fn fit_widths(columns: &[Column], width: f64, sized: &dyn Fn(Column) -> Option<f64>, floor: f64) -> Vec<f64> {
    let mut out = vec![0.0; columns.len()];
    let mut left = width;
    let mut fills = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        if let Some(w) = sized(*column) {
            out[index] = w.max(floor);
            left -= out[index];
            continue;
        }
        match column.width() {
            ColumnWidth::Fixed(w) => {
                out[index] = w.max(floor);
                left -= out[index];
            }
            ColumnWidth::Fill { .. } => fills.push(index),
        }
    }
    // Share the rest; a fill column held at its max hands its slack back to
    // the others, so walk until nothing more is clamped.
    let mut open = fills;
    loop {
        if open.is_empty() {
            break;
        }
        let share = (left / open.len() as f64).max(0.0);
        let mut clamped = None;
        for (at, index) in open.iter().enumerate() {
            if let ColumnWidth::Fill { min, max } = columns[*index].width() {
                if let Some(max) = max.filter(|max| share > *max) {
                    clamped = Some((at, max));
                    break;
                }
                if let Some(min) = min.filter(|min| share < *min) {
                    clamped = Some((at, min));
                    break;
                }
            }
        }
        match clamped {
            Some((at, w)) => {
                let index = open.remove(at);
                out[index] = w.max(floor);
                left -= out[index];
            }
            None => {
                for index in &open {
                    out[*index] = share.max(floor);
                }
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(title: &str) -> LibraryRow {
        LibraryRow { key: title.into(), title: title.into(), ..Default::default() }
    }

    fn titles(rows: &[LibraryRow], order: &[usize]) -> Vec<String> {
        order.iter().map(|index| rows[*index].title.clone()).collect()
    }

    #[test]
    fn no_tick_and_no_sort_is_the_parents_order() {
        let rows = vec![row("c"), row("a"), row("b")];
        assert_eq!(titles(&rows, &filter_sort(&rows, 0, None)), ["c", "a", "b"]);
    }

    #[test]
    fn the_filter_ticks_keep_rows_carrying_all_of_them() {
        let mut rows = vec![row("a"), row("b"), row("c")];
        rows[0].stems = true;
        rows[0].bpm = Some(120.0);
        rows[1].stems = true;
        rows[2].lyrics = true;
        assert_eq!(titles(&rows, &filter_sort(&rows, 0b001, None)), ["a", "b"]);
        assert_eq!(titles(&rows, &filter_sort(&rows, 0b101, None)), ["a"]);
        assert_eq!(titles(&rows, &filter_sort(&rows, 0b010, None)), ["c"]);
        assert!(filter_sort(&rows, 0b111, None).is_empty());
    }

    #[test]
    fn a_head_sorts_by_its_own_kind_of_value_and_unknowns_go_together() {
        let mut rows = vec![row("x"), row("y"), row("z")];
        rows[0].bpm = Some(128.0);
        rows[2].bpm = Some(90.5);
        assert_eq!(titles(&rows, &filter_sort(&rows, 0, Some((Column::Bpm, true)))), ["y", "z", "x"]);
        assert_eq!(titles(&rows, &filter_sort(&rows, 0, Some((Column::Bpm, false)))), ["x", "z", "y"]);
        rows[0].seconds = Some(61.0);
        rows[1].seconds = Some(600.0);
        assert_eq!(titles(&rows, &filter_sort(&rows, 0, Some((Column::Time, true)))), ["z", "x", "y"]);
        assert_eq!(titles(&rows, &filter_sort(&rows, 0, Some((Column::Title, false)))), ["z", "y", "x"]);
    }

    #[test]
    fn a_head_runs_up_down_then_off() {
        let up = sort_after_head(None, Column::Bpm);
        assert_eq!(up, Some((Column::Bpm, true)));
        let down = sort_after_head(up, Column::Bpm);
        assert_eq!(down, Some((Column::Bpm, false)));
        assert_eq!(sort_after_head(down, Column::Bpm), None);
        assert_eq!(sort_after_head(down, Column::Title), Some((Column::Title, true)));
    }

    #[test]
    fn cells_read_as_the_console_wrote_them() {
        let mut r = row("Song");
        r.bpm = Some(123.456);
        r.seconds = Some(201.6);
        r.stems = true;
        r.added_ms = 1_759_449_600_000; // 2025-10-03
        assert_eq!(cell_text(Column::Bpm, &r), "123.5");
        assert_eq!(cell_text(Column::Time, &r), "3:22");
        assert_eq!(cell_text(Column::Stem, &r), "✓");
        assert_eq!(cell_text(Column::Krk, &r), "");
        assert_eq!(cell_text(Column::Added, &r), "2025-10-03");
        r.note = "analysing 40%".into();
        assert_eq!(cell_text(Column::Title, &r), "Song — analysing 40%");
        assert_eq!(date_text(0), "");
        assert_eq!(date_text(951_782_400_000), "2000-02-29");
    }

    #[test]
    fn widths_fill_the_list_and_keep_what_the_operator_sized() {
        let columns = [Column::Title, Column::Artist, Column::Bpm];
        let none = |_: Column| None;
        let widths = fit_widths(&columns, 600.0, &none, 24.0);
        assert_eq!(widths[2], 54.0);
        // Artist stops at its max and hands the rest to the title.
        assert_eq!(widths[1], 150.0);
        assert_eq!(widths[0], 600.0 - 54.0 - 150.0);
        let sized = |column: Column| (column == Column::Bpm).then_some(80.0);
        let widths = fit_widths(&columns, 600.0, &sized, 24.0);
        assert_eq!(widths[2], 80.0);
        // A narrow list keeps the title's minimum.
        let widths = fit_widths(&columns, 200.0, &none, 24.0);
        assert_eq!(widths[0], 180.0);
    }

    #[test]
    fn only_columns_stage_fills_are_offered() {
        for column in crate::columns::ALL {
            let filled = !cell_text(column, &LibraryRow {
                title: "t".into(),
                artist: "a".into(),
                album: "b".into(),
                bpm: Some(1.0),
                seconds: Some(1.0),
                stems: true,
                lyrics: true,
                added_ms: 1,
                ..Default::default()
            })
            .is_empty();
            assert_eq!(offered(column), filled, "{column:?}");
        }
    }
}
