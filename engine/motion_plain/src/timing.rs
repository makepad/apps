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
            Self::Beat(n) => format!("beat({})", format_seconds(*n)),
            Self::Bar { bar, beat } => format!("bar({}, {})", format_seconds(*bar), format_seconds(*beat)),
            Self::Downbeat(n) => format!("downbeat({})", format_seconds(*n)),
            Self::Onset(n) => format!("onset({n})"),
            Self::NamedOnset(name) => format!("onset(@{name})"),
            Self::Section { name, nth: 0 } => format!("section(@{name})"),
            Self::Section { name, nth } => format!("section(@{name}, offset, {nth})"),
            Self::Word { voice, index } => format!("word(@{voice}, {index})"),
            Self::WordText { voice, text, after: None, .. } => format!("word(@{voice}, \"{text}\")"),
            Self::WordText { voice, text, nth, after: Some(after) } => format!("word(@{voice}, \"{text}\", 0, {nth}, {})", format_seconds(*after)),
            Self::Line { voice, index } => format!("line(@{voice}, {index})"),
            Self::Edit(s) => format!("edit({})", format_seconds(*s)),
        }
    }

    /// Whether the reference needs the beat grid (and so resolves, without
    /// one, against the document's `bpm`).
    pub fn is_grid(&self) -> bool {
        matches!(self, Self::Beat(_) | Self::Bar { .. } | Self::Downbeat(_))
    }
}

pub fn format_seconds(value: f64) -> String {
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
