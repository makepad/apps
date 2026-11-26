//! Musical and spoken time (EDITS.md §10.3): `beat(n)`, `bar(n, beat)`,
//! `downbeat(n)`, `onset(n)` / `onset(@name)`, `section(@name)`,
//! `word(@voice, i)` and `line(@voice, i)`, each with an offset in seconds,
//! and `beats(x)` as a length. They are written anywhere a time goes (spans,
//! `in`, key and tween times, staggers) and resolved like `marker()`: against
//! a [`Timing`] the host supplies (the music's baked beat grid, onsets and
//! sections, the voiceover's word timings). Resolution is a pure function of
//! the reference and the timing, so a document re-syncs by resolving again
//! when the music or a take changes.
//!
//! The same types serve Motion documents and edits (`makepad-stage-video-edit`
//! depends on this crate without its drawing feature for exactly this).
//!
//! Counting is 0-based everywhere: `beat(0)` is the grid's first beat,
//! `bar(0, 0)` its first downbeat, `word(@voice, 0)` the voice's first word.
//! Beat and bar numbers may be fractional (`beat(2.5)` is halfway between
//! beats 2 and 3).

/// Something that happens at a musical or spoken moment.
#[derive(Clone, Debug, PartialEq)]
pub enum TimeRef {
    /// `beat(n)`: the n-th beat of the grid.
    Beat(f64),
    /// `bar(n, beat)`: beat `beat` of bar `n`, counted from the first downbeat.
    Bar { bar: f64, beat: f64 },
    /// `downbeat(n)`: the first beat of bar `n` (`bar(n, 0)`).
    Downbeat(f64),
    /// `onset(n)`: the n-th detected onset.
    Onset(u32),
    /// `onset(@name)`: a named onset (a hit someone marked).
    NamedOnset(String),
    /// `section(@name)`, `section(@name, offset, nth)`: the start of the
    /// nth section with that name (`@intro`, `@build`, `@drop`, `@break`,
    /// `@outro`, or whatever the analysis names).
    Section { name: String, nth: u32 },
    /// `word(@voice, i)`: when the i-th word of a voiceover node is spoken
    /// (its lines' words, in order).
    Word { voice: String, index: u32 },
    /// `word(@voice, "12.4 million", offset, nth, after)`: when the (nth)
    /// run of spoken words matching that text starts (compared lowercase,
    /// letters, digits and `.` only, so "$12.4M," matches "12.4m"); with
    /// `after`, the nth run starting at or after that second of the clock
    /// it is written on (`after: 0` is from the layer's, or the document's,
    /// own start; resolved as a host second here).
    WordText { voice: String, text: String, nth: u32, after: Option<f64> },
    /// `line(@voice, i)`: when the i-th line of a voiceover node starts.
    Line { voice: String, index: u32 },
    /// `edit(s)`: second `s` of the host edit (the same moment in every
    /// graphic that shows it; no timing data needed).
    Edit(f64),
}

impl TimeRef {
    /// How the reference is written, for messages.
    pub fn describe(&self) -> String {
        match self {
            Self::Beat(n) => format!("beat({})", fmt(*n)),
            Self::Bar { bar, beat } => format!("bar({}, {})", fmt(*bar), fmt(*beat)),
            Self::Downbeat(n) => format!("downbeat({})", fmt(*n)),
            Self::Onset(n) => format!("onset({n})"),
            Self::NamedOnset(name) => format!("onset(@{name})"),
            Self::Section { name, nth: 0 } => format!("section(@{name})"),
            Self::Section { name, nth } => format!("section(@{name}, offset, {nth})"),
            Self::Word { voice, index } => format!("word(@{voice}, {index})"),
            Self::WordText { voice, text, after: None, .. } => format!("word(@{voice}, \"{text}\")"),
            Self::WordText { voice, text, nth, after: Some(after) } => format!("word(@{voice}, \"{text}\", 0, {nth}, {})", fmt(*after)),
            Self::Line { voice, index } => format!("line(@{voice}, {index})"),
            Self::Edit(s) => format!("edit({})", fmt(*s)),
        }
    }

    /// Whether the reference needs the beat grid (and so resolves, without
    /// one, against the document's `bpm`).
    pub fn is_grid(&self) -> bool {
        matches!(self, Self::Beat(_) | Self::Bar { .. } | Self::Downbeat(_))
    }
}

fn fmt(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// A point in time as written: seconds, or a reference plus an offset in
/// seconds.
#[derive(Clone, Debug, PartialEq)]
pub enum TimeExpr {
    Seconds(f64),
    At { at: TimeRef, offset: f64 },
}

impl TimeExpr {
    pub fn is_musical(&self) -> bool {
        matches!(self, Self::At { .. })
    }
}

/// A length as written: seconds, or `beats(x)` (x beats at the tempo where
/// the length is measured).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Seconds(f64),
    Beats(f64),
}

/// The beat grid: beat times in seconds, strictly increasing, and where
/// the bars start. With no `beats`, a constant tempo from `origin`.
#[derive(Clone, Debug, PartialEq)]
pub struct BeatGrid {
    /// Seconds of each beat. Before the first and after the last, the grid
    /// continues at the tempo of its nearest end.
    pub beats: Vec<f64>,
    /// The tempo of a grid without beats, and of the continuation of one
    /// with a single beat.
    pub bpm: f64,
    /// Where beat 0 is when `beats` is empty.
    pub origin: f64,
    pub beats_per_bar: u32,
    /// The index (in beats) of the first downbeat: bar 0 starts there.
    pub downbeat: u32,
}

impl BeatGrid {
    /// A steady grid: beat 0 at `origin`, `bpm` beats a minute, 4/4, bar 0
    /// on beat 0.
    pub fn steady(bpm: f64, origin: f64) -> Self {
        Self { beats: Vec::new(), bpm, origin, beats_per_bar: 4, downbeat: 0 }
    }

    /// A grid from detected beats (a music bake's beat times).
    pub fn from_beats(beats: Vec<f64>, beats_per_bar: u32, downbeat: u32, bpm: f64) -> Self {
        Self { beats, bpm, origin: 0.0, beats_per_bar: beats_per_bar.max(1), downbeat, }
    }

    fn period_at_end(&self, first: bool) -> f64 {
        let n = self.beats.len();
        let steady = 60.0 / self.bpm.max(1e-6);
        if n < 2 {
            return steady;
        }
        let d = if first { self.beats[1] - self.beats[0] } else { self.beats[n - 1] - self.beats[n - 2] };
        if d > 0.0 { d } else { steady }
    }

    /// Seconds of fractional beat index `x`.
    pub fn time_of_beat(&self, x: f64) -> f64 {
        let n = self.beats.len();
        if n == 0 {
            return self.origin + x * 60.0 / self.bpm.max(1e-6);
        }
        if x <= 0.0 {
            return self.beats[0] + x * self.period_at_end(true);
        }
        let last = (n - 1) as f64;
        if x >= last {
            return self.beats[n - 1] + (x - last) * self.period_at_end(false);
        }
        let i = x.floor() as usize;
        let u = x - i as f64;
        self.beats[i] + (self.beats[i + 1] - self.beats[i]) * u
    }

    /// The fractional beat index at `t` seconds (the inverse of
    /// [`Self::time_of_beat`]).
    pub fn beat_at(&self, t: f64) -> f64 {
        let n = self.beats.len();
        if n == 0 {
            return (t - self.origin) * self.bpm.max(1e-6) / 60.0;
        }
        if t <= self.beats[0] {
            return (t - self.beats[0]) / self.period_at_end(true);
        }
        if t >= self.beats[n - 1] {
            return (n - 1) as f64 + (t - self.beats[n - 1]) / self.period_at_end(false);
        }
        let i = self.beats.partition_point(|&b| b <= t) - 1;
        i as f64 + (t - self.beats[i]) / (self.beats[i + 1] - self.beats[i])
    }

    /// Seconds one beat lasts at `t`.
    pub fn beat_length_at(&self, t: f64) -> f64 {
        let x = self.beat_at(t).floor();
        self.time_of_beat(x + 1.0) - self.time_of_beat(x)
    }
}

/// A detected onset: when, how strong (0..1), and a name when someone
/// named it.
#[derive(Clone, Debug, PartialEq)]
pub struct Onset {
    pub t: f64,
    pub strength: f64,
    pub name: Option<String>,
}

/// A section of the music: `intro`, `build`, `drop`, `break`, `outro`, …
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub name: String,
    pub start: f64,
    pub end: f64,
}

/// A voiceover's spoken lines: each line's start and its words' starts.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct VoiceTiming {
    pub lines: Vec<SpokenTiming>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SpokenTiming {
    pub start: f64,
    /// Start of each word, in order.
    pub words: Vec<f64>,
    /// Each word's text, when known (for `word(@vo, "text")`).
    pub texts: Vec<String>,
}

/// A word as `word(@vo, "text")` compares it: lowercase, letters, digits
/// and `.` only.
pub fn normalize_word(word: &str) -> String {
    word.chars().filter(|c| c.is_alphanumeric() || *c == '.').flat_map(char::to_lowercase).collect::<String>().trim_matches('.').to_string()
}

/// Everything musical and spoken time resolves against. All times are
/// seconds on the clock of the document being resolved (an edit's, or a
/// graphic's own: see [`Timing::shifted`]).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Timing {
    /// `None`: grid references resolve against the document's own `bpm`
    /// (a steady grid from 0), and a warning says so.
    pub grid: Option<BeatGrid>,
    pub onsets: Vec<Onset>,
    pub sections: Vec<Section>,
    /// Voiceover timings by node id.
    pub voices: Vec<(String, VoiceTiming)>,
}

impl Timing {
    /// The same moments `dt` seconds later on the clock (a graphic that
    /// starts at edit time `s` sees the edit's timing shifted by `-s`).
    pub fn shifted(&self, dt: f64) -> Timing {
        let mut out = self.clone();
        if let Some(grid) = &mut out.grid {
            grid.origin += dt;
            for beat in &mut grid.beats {
                *beat += dt;
            }
        }
        for onset in &mut out.onsets {
            onset.t += dt;
        }
        for section in &mut out.sections {
            section.start += dt;
            section.end += dt;
        }
        for (_, voice) in &mut out.voices {
            for line in &mut voice.lines {
                line.start += dt;
                for word in &mut line.words {
                    *word += dt;
                }
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.grid.is_none() && self.onsets.is_empty() && self.sections.is_empty() && self.voices.is_empty()
    }

    /// The grid references resolve against: the timing's own, else a steady
    /// one at `bpm` from 0.
    fn grid_or(&self, bpm: f64) -> std::borrow::Cow<'_, BeatGrid> {
        match &self.grid {
            Some(grid) => std::borrow::Cow::Borrowed(grid),
            None => std::borrow::Cow::Owned(BeatGrid::steady(bpm, 0.0)),
        }
    }

    /// Seconds of `at` (without its offset). `bpm` is the document's tempo,
    /// used when the timing has no grid. The error says what is missing and
    /// what there is, so an author (or an agent) can fix the reference.
    pub fn resolve_ref(&self, at: &TimeRef, bpm: f64) -> Result<f64, String> {
        match at {
            TimeRef::Beat(n) => Ok(self.grid_or(bpm).time_of_beat(*n)),
            TimeRef::Edit(s) => Ok(*s),
            TimeRef::Bar { bar, beat } => {
                let grid = self.grid_or(bpm);
                Ok(grid.time_of_beat(grid.downbeat as f64 + bar * grid.beats_per_bar as f64 + beat))
            }
            TimeRef::Downbeat(n) => {
                let grid = self.grid_or(bpm);
                Ok(grid.time_of_beat(grid.downbeat as f64 + n * grid.beats_per_bar as f64))
            }
            TimeRef::Onset(n) => match self.onsets.get(*n as usize) {
                Some(onset) => Ok(onset.t),
                None if self.onsets.is_empty() => Err(format!("{} needs the music's onsets, and none are known yet (is the music analysed?)", at.describe())),
                None => Err(format!("{} is past the last onset: there are {} (onset(0) to onset({}))", at.describe(), self.onsets.len(), self.onsets.len() - 1)),
            },
            TimeRef::NamedOnset(name) => match self.onsets.iter().find(|onset| onset.name.as_deref() == Some(name)) {
                Some(onset) => Ok(onset.t),
                None => {
                    let names: Vec<String> = self.onsets.iter().filter_map(|onset| onset.name.as_ref().map(|name| format!("@{name}"))).collect();
                    Err(if names.is_empty() {
                        format!("{} names an onset, but no onset has a name; use onset(n) by index", at.describe())
                    } else {
                        format!("{} is not a named onset; named onsets: {}", at.describe(), names.join(", "))
                    })
                }
            },
            TimeRef::Section { name, nth } => {
                match self.sections.iter().filter(|section| &section.name == name).nth(*nth as usize) {
                    Some(section) => Ok(section.start),
                    None => {
                        let mut names: Vec<String> = Vec::new();
                        for section in &self.sections {
                            let tag = format!("@{}", section.name);
                            if !names.contains(&tag) {
                                names.push(tag);
                            }
                        }
                        let count = self.sections.iter().filter(|section| &section.name == name).count();
                        Err(if self.sections.is_empty() {
                            format!("{} needs the music's sections, and none are known yet (is the music analysed?)", at.describe())
                        } else if count > 0 {
                            format!("{} asks for section @{name} number {nth} (0-based), but the music has {count}", at.describe())
                        } else {
                            format!("{} is not a section of the music; its sections are {}", at.describe(), names.join(", "))
                        })
                    }
                }
            }
            TimeRef::WordText { voice, text, nth, after } => {
                let Some((_, timing)) = self.voices.iter().find(|(id, _)| id == voice) else {
                    return Err(format!("{} needs the timings of voiceover `{voice}`, and it is not timed here", at.describe()));
                };
                let starts: Vec<f64> = timing.lines.iter().flat_map(|line| line.words.iter().copied()).collect();
                let texts: Vec<String> = timing.lines.iter().flat_map(|line| line.texts.iter().map(|t| normalize_word(t))).collect();
                if texts.is_empty() || texts.len() != starts.len() {
                    return Err(format!("{} needs the voiceover's word texts, and `{voice}` has only its word times", at.describe()));
                }
                let wanted: Vec<String> = text.split_whitespace().map(normalize_word).filter(|w| !w.is_empty()).collect();
                if wanted.is_empty() {
                    return Err(format!("{} has no words to look for", at.describe()));
                }
                let hits: Vec<usize> = (0..texts.len().saturating_sub(wanted.len() - 1))
                    .filter(|&i| texts[i..i + wanted.len()] == wanted[..] && after.is_none_or(|after| starts[i] >= after - 1e-9))
                    .collect();
                match hits.get(*nth as usize) {
                    Some(&i) => Ok(starts[i]),
                    None if hits.is_empty() && after.is_some() => Err(format!("{} is not spoken by `{voice}` at or after {} s", at.describe(), fmt(after.unwrap_or(0.0)))),
                    None if hits.is_empty() => {
                        let first = &wanted[0];
                        let mut near: Vec<(usize, &String)> = texts.iter().map(|t| (makepad_script::suggest::levenshtein(t, first), t)).collect();
                        near.sort();
                        near.dedup_by(|a, b| a.1 == b.1);
                        let near: Vec<String> = near.iter().take(5).map(|(_, t)| format!("\"{t}\"")).collect();
                        Err(format!("{} is not spoken by `{voice}`; its closest words are {}", at.describe(), near.join(", ")))
                    }
                    None => Err(format!("{} is spoken {} times by `{voice}`; nth {nth} (0-based) is past the end", at.describe(), hits.len())),
                }
            }
            TimeRef::Word { voice, index } | TimeRef::Line { voice, index } => {
                let Some((_, timing)) = self.voices.iter().find(|(id, _)| id == voice) else {
                    let known: Vec<String> = self.voices.iter().map(|(id, _)| format!("@{id}")).collect();
                    return Err(if known.is_empty() {
                        format!("{} needs the timings of voiceover `{voice}`, and no voiceover is timed yet", at.describe())
                    } else {
                        format!("{} names voiceover `{voice}`, which is not timed here; timed voiceovers: {}", at.describe(), known.join(", "))
                    });
                };
                let words: Vec<f64> = timing.lines.iter().flat_map(|line| line.words.iter().copied()).collect();
                let (list, what) = if matches!(at, TimeRef::Word { .. }) {
                    (words, "words")
                } else {
                    (timing.lines.iter().map(|line| line.start).collect(), "lines")
                };
                list.get(*index as usize).copied().ok_or_else(|| {
                    format!("{} is past the end: `{voice}` has {} {what} (0-based)", at.describe(), list.len())
                })
            }
        }
    }

    /// Seconds of `expr`.
    pub fn resolve(&self, expr: &TimeExpr, bpm: f64) -> Result<f64, String> {
        match expr {
            TimeExpr::Seconds(seconds) => Ok(*seconds),
            TimeExpr::At { at, offset } => Ok(self.resolve_ref(at, bpm)? + offset),
        }
    }

    /// Seconds `length` lasts when it starts at `at` seconds.
    pub fn length(&self, length: Length, at: f64, bpm: f64) -> f64 {
        match length {
            Length::Seconds(seconds) => seconds,
            Length::Beats(beats) => {
                let grid = self.grid_or(bpm);
                let start = grid.beat_at(at);
                grid.time_of_beat(start + beats) - grid.time_of_beat(start)
            }
        }
    }
}

/// One argument of a time call, as the value readers of both documents
/// present it.
#[derive(Clone, Debug, PartialEq)]
pub enum TimeArg {
    Num(f64),
    Id(String),
    /// A string (`word(@vo, "text")`).
    Text(String),
    Nil,
    Other,
}

/// The fields the time helpers of `mod.motion` and `mod.edit` write:
/// `{time: @beat n: 4 offset: 0.0}`, `{time: @section name: @drop nth: 0
/// offset: 0.0}`, `{time: @word voice: @vo index: 3 offset: 0.0}`,
/// `{time: @beats n: 0.125}` (a length). Both loaders read their values
/// into a lookup and call [`parse_time_fields`].
pub const TIME_KINDS: &[&str] = &["beat", "bar", "downbeat", "onset", "section", "word", "line", "edit"];

/// What a `{time: …}` object means: a point, a length, or why neither.
#[derive(Clone, Debug, PartialEq)]
pub enum TimeValue {
    Point(TimeExpr),
    Length(Length),
}

/// Read a `{time: @kind …}` object through `field`. `None` when the object
/// has no `time` field (it is not a time helper's).
pub fn parse_time_fields(field: &dyn Fn(&str) -> TimeArg) -> Option<Result<TimeValue, String>> {
    let kind = match field("time") {
        TimeArg::Id(kind) => kind,
        _ => return None,
    };
    let num = |name: &str| match field(name) {
        TimeArg::Num(value) if value.is_finite() => Some(value),
        _ => None,
    };
    let offset = match field("offset") {
        TimeArg::Nil => 0.0,
        TimeArg::Num(value) if value.is_finite() => value,
        _ => return Some(Err(format!("`{kind}(…)`'s offset is seconds, e.g. {kind}(…, -0.1)"))),
    };
    let whole = |name: &str, what: &str| -> Result<u32, String> {
        match num(name) {
            Some(value) if value >= 0.0 && value.fract() == 0.0 && value <= u32::MAX as f64 => Ok(value as u32),
            _ => Err(format!("`{kind}(…)` takes {what} as a whole number from 0")),
        }
    };
    let id = |name: &str| match field(name) {
        TimeArg::Id(id) => Some(id),
        _ => None,
    };
    let at = match kind.as_str() {
        "beats" => {
            return Some(match num("n") {
                Some(beats) => Ok(TimeValue::Length(Length::Beats(beats))),
                None => Err("`beats(x)` is a length in beats, e.g. beats(1/8) or beats(2)".into()),
            })
        }
        "beat" => match num("n") {
            Some(n) => TimeRef::Beat(n),
            None => return Some(Err("`beat(n)` takes the beat number (0 is the first beat; fractions are between beats)".into())),
        },
        "edit" => match num("s") {
            Some(s) => TimeRef::Edit(s),
            None => return Some(Err("`edit(s)` takes a second of the host edit, e.g. edit(23.0)".into())),
        },
        "downbeat" => match num("n") {
            Some(n) => TimeRef::Downbeat(n),
            None => return Some(Err("`downbeat(n)` takes the bar number (0 is the first bar)".into())),
        },
        "bar" => match (num("n"), field("beat")) {
            (Some(bar), TimeArg::Nil) => TimeRef::Bar { bar, beat: 0.0 },
            (Some(bar), TimeArg::Num(beat)) if beat.is_finite() => TimeRef::Bar { bar, beat },
            _ => return Some(Err("`bar(n, beat)` takes the bar number and the beat in it (both from 0), e.g. bar(8, 2)".into())),
        },
        "onset" => match field("n") {
            TimeArg::Id(name) => TimeRef::NamedOnset(name),
            TimeArg::Num(_) => match whole("n", "the onset's index") {
                Ok(n) => TimeRef::Onset(n),
                Err(error) => return Some(Err(error)),
            },
            _ => return Some(Err("`onset(n)` takes an onset index from 0, or `onset(@name)` a named onset".into())),
        },
        "section" => match id("name") {
            Some(name) => {
                let nth = match field("nth") {
                    TimeArg::Nil => 0,
                    _ => match whole("nth", "which of the sections with that name (0 is the first)") {
                        Ok(nth) => nth,
                        Err(error) => return Some(Err(error)),
                    },
                };
                TimeRef::Section { name, nth }
            }
            None => return Some(Err("`section(@name)` names a section with an @id, e.g. section(@drop)".into())),
        },
        "word" | "line" => {
            let Some(voice) = id("voice") else {
                return Some(Err(format!("`{kind}(@voice, i)` names the voiceover node with an @id, e.g. {kind}(@vo, 2)")));
            };
            if let (true, TimeArg::Text(text)) = (kind == "word", field("index")) {
                let nth = match field("nth") {
                    TimeArg::Nil => 0,
                    _ => match whole("nth", "which of the matching runs (0 is the first)") {
                        Ok(nth) => nth,
                        Err(error) => return Some(Err(error)),
                    },
                };
                let after = match field("after") {
                    TimeArg::Nil => None,
                    TimeArg::Num(after) if after.is_finite() => Some(after),
                    _ => return Some(Err("`word(@voice, \"text\", offset, nth, after)`'s after is a second of the document, e.g. 0 for its own start".into())),
                };
                TimeRef::WordText { voice, text, nth, after }
            } else {
                let index = match whole("index", "the index (0 is the first)") {
                    Ok(index) => index,
                    Err(error) => return Some(Err(error)),
                };
                if kind == "word" { TimeRef::Word { voice, index } } else { TimeRef::Line { voice, index } }
            }
        }
        other => {
            return Some(Err(format!(
                "`{other}` is not a time reference; use beat(n), bar(n, beat), downbeat(n), onset(n), onset(@name), section(@name), word(@voice, i), line(@voice, i) or beats(x)"
            )))
        }
    };
    Some(Ok(TimeValue::Point(TimeExpr::At { at, offset })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields<'a>(pairs: &'a [(&'a str, TimeArg)]) -> impl Fn(&str) -> TimeArg + 'a {
        move |name| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| value.clone()).unwrap_or(TimeArg::Nil)
    }

    #[test]
    fn a_steady_grid_counts_beats_and_bars_from_zero() {
        let timing = Timing::default();
        assert_eq!(timing.resolve_ref(&TimeRef::Beat(4.0), 120.0).unwrap(), 2.0);
        assert_eq!(timing.resolve_ref(&TimeRef::Bar { bar: 1.0, beat: 2.0 }, 120.0).unwrap(), 3.0);
        assert_eq!(timing.resolve_ref(&TimeRef::Downbeat(2.0), 60.0).unwrap(), 8.0);
        assert_eq!(timing.length(Length::Beats(0.5), 1.0, 120.0), 0.25);
    }

    #[test]
    fn a_detected_grid_interpolates_and_continues_past_its_ends() {
        let grid = BeatGrid::from_beats(vec![0.5, 1.0, 1.6, 2.0], 4, 1, 120.0);
        let timing = Timing { grid: Some(grid.clone()), ..Default::default() };
        assert!((timing.resolve_ref(&TimeRef::Beat(1.5), 0.0).unwrap() - 1.3).abs() < 1e-12);
        // Past the end at the last spacing (0.4 s), before the start at the first (0.5 s).
        assert!((grid.time_of_beat(5.0) - 2.8).abs() < 1e-12);
        assert!((grid.time_of_beat(-1.0) - 0.0).abs() < 1e-12);
        // Bar 0 starts at the downbeat, beat 1.
        assert_eq!(timing.resolve_ref(&TimeRef::Downbeat(0.0), 0.0).unwrap(), 1.0);
        for x in [-1.5, 0.0, 0.7, 2.2, 6.0] {
            assert!((grid.beat_at(grid.time_of_beat(x)) - x).abs() < 1e-9, "{x}");
        }
        assert!((timing.length(Length::Beats(1.0), 1.0, 0.0) - 0.6).abs() < 1e-12);
    }

    #[test]
    fn onsets_sections_and_words_resolve_and_explain_what_is_missing() {
        let timing = Timing {
            grid: None,
            onsets: vec![Onset { t: 0.4, strength: 1.0, name: None }, Onset { t: 2.2, strength: 0.5, name: Some("snare".into()) }],
            sections: vec![
                Section { name: "intro".into(), start: 0.0, end: 8.0 },
                Section { name: "drop".into(), start: 8.0, end: 16.0 },
                Section { name: "drop".into(), start: 24.0, end: 32.0 },
            ],
            voices: vec![("vo".into(), VoiceTiming { lines: vec![
                SpokenTiming { start: 1.0, words: vec![1.0, 1.3], texts: vec!["Revenue".into(), "hit".into()] },
                SpokenTiming { start: 3.0, words: vec![3.0, 3.5, 3.9], texts: vec!["$12.4M,".into(), "this".into(), "year.".into()] },
            ] })],
        };
        assert_eq!(timing.resolve(&TimeExpr::At { at: TimeRef::Onset(1), offset: -0.1 }, 120.0).unwrap(), 2.1);
        assert_eq!(timing.resolve_ref(&TimeRef::NamedOnset("snare".into()), 120.0).unwrap(), 2.2);
        assert_eq!(timing.resolve_ref(&TimeRef::Section { name: "drop".into(), nth: 1 }, 120.0).unwrap(), 24.0);
        assert_eq!(timing.resolve_ref(&TimeRef::Word { voice: "vo".into(), index: 3 }, 120.0).unwrap(), 3.5);
        let text = |t: &str, nth: u32| timing.resolve_ref(&TimeRef::WordText { voice: "vo".into(), text: t.into(), nth, after: None }, 120.0);
        assert_eq!(text("12.4m this", 0).unwrap(), 3.0, "\"$12.4M,\" normalises to 12.4m");
        assert_eq!(text("HIT", 0).unwrap(), 1.3);
        assert!(text("hit", 1).unwrap_err().contains("past the end"));
        assert!(text("revenues", 0).unwrap_err().contains("\"revenue\""), "closest words are named");
        // `after`: the nth run at or after a second (a plate placed later in
        // a whole song finds its own chorus, not the first one).
        let after = |t: &str, nth: u32, after: f64| timing.resolve_ref(&TimeRef::WordText { voice: "vo".into(), text: t.into(), nth, after: Some(after) }, 120.0);
        assert_eq!(after("this", 0, 2.0).unwrap(), 3.5);
        assert_eq!(after("revenue", 0, 0.0).unwrap(), 1.0);
        assert!(after("revenue", 0, 2.0).unwrap_err().contains("at or after 2"));
        assert_eq!(timing.resolve_ref(&TimeRef::Line { voice: "vo".into(), index: 1 }, 120.0).unwrap(), 3.0);
        let error = timing.resolve_ref(&TimeRef::Section { name: "chorus".into(), nth: 0 }, 120.0).unwrap_err();
        assert!(error.contains("@intro, @drop"), "{error}");
        let error = timing.resolve_ref(&TimeRef::Word { voice: "vo".into(), index: 5 }, 120.0).unwrap_err();
        assert!(error.contains("has 5 words"), "{error}");
        let error = Timing::default().resolve_ref(&TimeRef::Onset(0), 120.0).unwrap_err();
        assert!(error.contains("none are known yet"), "{error}");
        // Shifting moves every moment.
        let shifted = timing.shifted(-8.0);
        assert_eq!(shifted.resolve_ref(&TimeRef::Section { name: "drop".into(), nth: 0 }, 120.0).unwrap(), 0.0);
        assert_eq!(shifted.resolve_ref(&TimeRef::Word { voice: "vo".into(), index: 0 }, 120.0).unwrap(), -7.0);
    }

    #[test]
    fn time_objects_parse_with_teaching_errors() {
        let parse = |pairs: &[(&str, TimeArg)]| parse_time_fields(&fields(pairs));
        assert_eq!(parse(&[("n", TimeArg::Num(3.0))]), None);
        assert_eq!(
            parse(&[("time", TimeArg::Id("bar".into())), ("n", TimeArg::Num(8.0)), ("beat", TimeArg::Num(2.0)), ("offset", TimeArg::Num(-0.05))]),
            Some(Ok(TimeValue::Point(TimeExpr::At { at: TimeRef::Bar { bar: 8.0, beat: 2.0 }, offset: -0.05 })))
        );
        assert_eq!(
            parse(&[("time", TimeArg::Id("onset".into())), ("n", TimeArg::Id("snare".into()))]),
            Some(Ok(TimeValue::Point(TimeExpr::At { at: TimeRef::NamedOnset("snare".into()), offset: 0.0 })))
        );
        assert_eq!(parse(&[("time", TimeArg::Id("beats".into())), ("n", TimeArg::Num(0.125))]), Some(Ok(TimeValue::Length(Length::Beats(0.125)))));
        let error = parse(&[("time", TimeArg::Id("section".into())), ("name", TimeArg::Num(1.0))]).unwrap().unwrap_err();
        assert!(error.contains("section(@drop)"), "{error}");
        let error = parse(&[("time", TimeArg::Id("word".into())), ("voice", TimeArg::Id("vo".into())), ("index", TimeArg::Num(1.5))]).unwrap().unwrap_err();
        assert!(error.contains("whole number"), "{error}");
    }
}
