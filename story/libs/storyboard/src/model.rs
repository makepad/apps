//! The board as typed data, read from its Splash (`Storyboard::load`) and
//! written back canonically (`Storyboard::to_source`, for new boards; a
//! board that exists is changed by patches, `crate::edit`).
//!
//! ```splash
//! use mod.storyboard.*
//!
//! let story = Row{kind: @story label: "Story"}
//! let voice = Row{kind: @voice label: "Voiceover"}
//! let refs = Row{kind: @refs label: "References"}
//!
//! let hook = Scene{row: story span: [0.0, 4.0] title: "Hook" intent: "One tap, the note is there"
//!     mood: "bright, confident" look: "white type slams in on the downbeat" kit: "@swiss"}
//! let vo1 = Note{row: voice span: [0.4, 3.2] text: "Your ideas move fast."}
//! let photo = Ref{row: refs span: [0.0, 4.0] media: @image src: asset("ast_…", "arev_…") label: "Desk photo"
//!     role: @generate prompt: "the desk at golden hour, slow push in" flow: "image_to_video" seed: 7}
//! let brand = Ref{row: refs span: [0.0, 20.0] media: @swatch colors: [#ff5c39, #14171c] label: "Brand"}
//! let l1 = Link{from: hook to: photo label: "style"}
//!
//! Storyboard{
//!     title: "Notes launch"
//!     duration: 20.0
//!     bpm: 120
//!     rows: [story, voice, refs]
//!     items: [hook, vo1, photo, brand]
//!     links: [l1]
//! }
//! ```
//!
//! Times are seconds; `span: [start, length]` as in a cut. `row` names a
//! `Row`; `from`/`to` name items. The music reference (`music: song`)
//! carries the tempo the ruler shows as bars and beats.

use crate::splash::{self, color_literal, number, parse, string_literal, Kind, Node, Stmt};

/// What a row holds (its colour and default height on the canvas, and what
/// the cut makes of its items).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowKind {
    /// Scene cards: the story beats.
    Story,
    /// Shots and visual ideas.
    Visuals,
    /// Voiceover and on-screen text: each note is a line.
    Voice,
    /// Music and sound.
    Music,
    /// References: images, clips, frames, links, colours, fonts.
    Refs,
    /// Free notes.
    Notes,
    /// A showreel's chapter list: one card per chapter on the beat grid,
    /// each named after one skill with its look, colour, hero element and
    /// the transition out (REF-Ac7SuDYTQDc §2).
    Chapters,
    /// Treatments to choose between (a music video's first decision): one
    /// is marked chosen.
    Treatments,
    /// The visual bible: pages of palette and colour arc, type, character
    /// model sheet, props and what is off-limits.
    Bible,
    /// The song's sections in words (intro, build, drop …), at their spans.
    Sections,
    /// A lyric film's lines: each line with its pun and how it is built
    /// (the set, the type voice, the device).
    Lyrics,
    /// Running devices: a value that keeps changing (tide height per section)
    /// or a recurring UI, one card per occurrence.
    Devices,
}

impl RowKind {
    pub const ALL: [RowKind; 12] = [RowKind::Treatments, RowKind::Bible, RowKind::Sections, RowKind::Lyrics, RowKind::Devices, RowKind::Story, RowKind::Chapters, RowKind::Visuals, RowKind::Voice, RowKind::Music, RowKind::Refs, RowKind::Notes];

    pub fn tag(self) -> &'static str {
        match self {
            RowKind::Story => "story",
            RowKind::Visuals => "visuals",
            RowKind::Voice => "voice",
            RowKind::Music => "music",
            RowKind::Refs => "refs",
            RowKind::Notes => "notes",
            RowKind::Chapters => "chapters",
            RowKind::Treatments => "treatments",
            RowKind::Bible => "bible",
            RowKind::Sections => "sections",
            RowKind::Lyrics => "lyrics",
            RowKind::Devices => "devices",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.tag() == tag)
    }

    pub fn label(self) -> &'static str {
        match self {
            RowKind::Story => "Story",
            RowKind::Visuals => "Visuals",
            RowKind::Voice => "Voiceover",
            RowKind::Music => "Music",
            RowKind::Refs => "References",
            RowKind::Notes => "Notes",
            RowKind::Chapters => "Chapters",
            RowKind::Treatments => "Treatments",
            RowKind::Bible => "Visual bible",
            RowKind::Sections => "Sections",
            RowKind::Lyrics => "Lyric lines",
            RowKind::Devices => "Running devices",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: String,
    pub kind: RowKind,
    pub label: String,
}

/// What a reference is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Media {
    Image,
    Video,
    Audio,
    /// A Motion document.
    Motion,
    /// Another cut.
    Cut,
    /// One frame of a video (`src` + `at`).
    Frame,
    Url,
    Swatch,
    Font,
    /// A local file not yet in the library.
    File,
}

impl Media {
    pub const ALL: [Media; 10] = [Media::Image, Media::Video, Media::Audio, Media::Motion, Media::Cut, Media::Frame, Media::Url, Media::Swatch, Media::Font, Media::File];

    pub fn tag(self) -> &'static str {
        match self {
            Media::Image => "image",
            Media::Video => "video",
            Media::Audio => "audio",
            Media::Motion => "motion",
            Media::Cut => "cut",
            Media::Frame => "frame",
            Media::Url => "url",
            Media::Swatch => "swatch",
            Media::Font => "font",
            Media::File => "file",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|media| media.tag() == tag)
    }

    /// Picture media: it has a thumbnail, and the AI can look at it.
    pub fn visual(self) -> bool {
        matches!(self, Media::Image | Media::Video | Media::Frame | Media::Motion | Media::Cut)
    }

    /// It names a library revision.
    pub fn in_library(self) -> bool {
        matches!(self, Media::Image | Media::Video | Media::Audio | Media::Motion | Media::Cut | Media::Frame)
    }
}

/// What a reference is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    /// Something to look at for style, mood or content; not in the video.
    #[default]
    Reference,
    /// Goes into the video as it is.
    Use,
    /// Source material for a generative segment.
    Generate,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Reference, Role::Use, Role::Generate];

    pub fn tag(self) -> &'static str {
        match self {
            Role::Reference => "reference",
            Role::Use => "use",
            Role::Generate => "generate",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.tag() == tag)
    }
}

/// An exact library revision.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AssetRef {
    pub asset_id: String,
    pub revision: String,
}

/// How a generative segment is made from a picture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gen {
    /// The reference item it starts from (a scene card's; a reference with
    /// role `@generate` is its own source).
    pub from: Option<String>,
    pub prompt: String,
    pub negative: String,
    /// A FLOW preset name (`image_to_video`, `img2img`, `video_to_video`).
    pub flow: String,
    pub seed: Option<i64>,
}

impl Gen {
    pub fn is_empty(&self) -> bool {
        self.from.is_none() && self.prompt.is_empty() && self.negative.is_empty() && self.flow.is_empty() && self.seed.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub title: String,
    /// The one idea the scene carries.
    pub intent: String,
    pub mood: String,
    /// What is on screen and how it moves.
    pub look: String,
    /// A style kit (`@swiss`, `@neon_noir`).
    pub kit: String,
    /// Where it sits in the music.
    pub music: String,
    /// Media it needs generated or found.
    pub media: String,
    /// Its layer stack, back to front.
    pub stack: String,
    /// A shot list's columns: the bars it covers (`9–12`), the camera, why
    /// this shot, and the lyric over it.
    pub bars: String,
    pub camera: String,
    pub why: String,
    pub lyric: String,
    pub gen: Gen,
}

/// One chapter of a showreel-like video: its number is its place in time
/// among the chapters; its colour is the item's `color` (the chapter's
/// ground).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chapter {
    /// The skill (or topic) it is named after: `EASING`, `TYPOGRAPHY`.
    pub skill: String,
    /// What is built and how it moves.
    pub look: String,
    /// The one hero element: `the orange ball`, `the word MOVE.`.
    pub hero: String,
    /// How it hands over to the next chapter, motivated by its hero: `the
    /// ball grows into the next background`.
    pub out: String,
}

/// One treatment of the video to choose between.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Treatment {
    pub title: String,
    /// The idea in a few lines.
    pub idea: String,
    pub look: String,
    pub palette: Vec<u32>,
    /// The one the person picked.
    pub chosen: bool,
}

/// A page of the visual bible.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub title: String,
    /// Its text (markdown-ish).
    pub body: String,
    /// The references it points at (item ids).
    pub refs: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reference {
    pub media: Option<Media>,
    pub src: Option<AssetRef>,
    /// A frame's time in its video.
    pub at: Option<f64>,
    pub url: String,
    pub colors: Vec<u32>,
    pub font: String,
    pub file: String,
    pub label: String,
    pub note: String,
    pub role: Role,
    pub gen: Gen,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Note { text: String },
    Scene(Scene),
    Ref(Reference),
    Chapter(Chapter),
    Treatment(Treatment),
    Page(Page),
    Lyric(Lyric),
    Device(Device),
}

/// One sung line and how it is built: the line, the joke it makes, the set
/// that makes the joke, its type voice (a board voice id) and the device
/// the type performs (rides the loss curve, is stamped, tiles the frame).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lyric {
    pub text: String,
    pub pun: String,
    pub set: String,
    pub voice: Option<String>,
    pub device: String,
}

/// One occurrence of a running device: its name (`Tide`, `Harbor log`)
/// and its value here (`0.15`, `East quay`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub name: String,
    pub value: String,
    pub note: String,
}

/// A type voice of the board's legend: one of the few ways type speaks in
/// the video (slam, condensed wall, mono UI, serif italic, handwriting).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TypeVoice {
    pub id: String,
    pub label: String,
    /// The face: `wide heavy grotesk, caps`.
    pub face: String,
    /// What it is for: `stressed words`.
    pub role: String,
    pub color: Option<u32>,
}

/// The lyric film's five voices (REF-threejs-2104029135790072196 §3).
pub fn lyric_film_voices() -> Vec<TypeVoice> {
    let voice = |id: &str, label: &str, face: &str, role: &str, color: u32| TypeVoice { id: id.into(), label: label.into(), face: face.into(), role: role.into(), color: Some(color) };
    vec![
        voice("slam", "Slam", "wide heavy grotesk, caps", "stressed words: SKYLINE, SEAM, GOLD, NIGHT", 0xff5a1fff),
        voice("wall", "Condensed wall", "condensed caps, tiled edge to edge", "walls of words: WINDOWS WARM THE FOG, RIPPLES CROSS GLASS", 0xf4f0e6ff),
        voice("mono", "Mono UI", "monospace", "prompts, tables, footnotes, counters", 0x8fb4ffff),
        voice("serif", "Serif italic", "serif italic", "the quiet, philosophical lines: a lantern reflected in the tide", 0xd6c7a1ff),
        voice("hand", "Handwriting", "handwriting", "corrections in the harbor log", 0xf0c75eff),
    ]
}

impl Body {
    pub fn ty(&self) -> &'static str {
        match self {
            Body::Note { .. } => "Note",
            Body::Scene(_) => "Scene",
            Body::Ref(_) => "Ref",
            Body::Chapter(_) => "Chapter",
            Body::Treatment(_) => "Treatment",
            Body::Page(_) => "Page",
            Body::Lyric(_) => "Lyric",
            Body::Device(_) => "Device",
        }
    }

    /// The kind as the tools name it.
    pub fn kind(&self) -> &'static str {
        match self {
            Body::Note { .. } => "note",
            Body::Scene(_) => "scene",
            Body::Ref(_) => "ref",
            Body::Chapter(_) => "chapter",
            Body::Treatment(_) => "treatment",
            Body::Page(_) => "page",
            Body::Lyric(_) => "lyric",
            Body::Device(_) => "device",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub row: String,
    pub start: f64,
    pub length: f64,
    /// A note's colour (a sticky's), when not the row's; a chapter's ground.
    pub color: Option<u32>,
    /// Where in the frame it goes (a band of the board's `layout`).
    pub band: Option<String>,
    pub body: Body,
}

impl Item {
    pub fn end(&self) -> f64 {
        self.start + self.length
    }

    /// The text a card or note shows first.
    pub fn headline(&self) -> String {
        match &self.body {
            Body::Note { text } => text.lines().next().unwrap_or("").to_string(),
            Body::Scene(scene) => if scene.title.is_empty() { scene.intent.clone() } else { scene.title.clone() },
            Body::Chapter(chapter) => if chapter.skill.is_empty() { chapter.look.clone() } else { chapter.skill.clone() },
            Body::Treatment(treatment) => treatment.title.clone(),
            Body::Page(page) => page.title.clone(),
            Body::Lyric(lyric) => lyric.text.clone(),
            Body::Device(device) => format!("{} {}", device.name, device.value).trim().to_string(),
            Body::Ref(reference) => {
                if !reference.label.is_empty() {
                    reference.label.clone()
                } else if !reference.url.is_empty() {
                    reference.url.clone()
                } else if !reference.font.is_empty() {
                    reference.font.clone()
                } else {
                    reference.media.map_or("Reference", |media| match media {
                        Media::Image => "Image",
                        Media::Video => "Video",
                        Media::Audio => "Audio",
                        Media::Motion => "Motion",
                        Media::Cut => "Edit",
                        Media::Frame => "Frame",
                        Media::Url => "Link",
                        Media::Swatch => "Colours",
                        Media::Font => "Font",
                        Media::File => "File",
                    }).to_string()
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub id: String,
    pub from: String,
    pub to: String,
    pub label: String,
}

/// One card's (or note's) share of the cut built from the board.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub item: String,
    /// Edit node ids; a voiceover line is `node#index`.
    pub nodes: Vec<String>,
    /// The item's span when the cut was built.
    pub start: f64,
    pub length: f64,
    /// [`crate::diff::signature`] of the item's content then.
    pub sig: String,
}

/// The cut last built from this board (the provenance of its parts).
#[derive(Clone, Debug, PartialEq)]
pub struct Built {
    pub edit: AssetRef,
    /// [`Storyboard::revision`] of the board it was built from.
    pub board: String,
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Storyboard {
    pub title: String,
    pub duration: f64,
    pub bpm: Option<f64>,
    /// Where beat 0 is, in seconds.
    pub beat_offset: f64,
    pub beats_per_bar: u32,
    /// The music reference item the tempo comes from.
    pub music: Option<String>,
    /// The brief the board answers.
    pub brief: String,
    pub style: String,
    pub kit: String,
    /// The output size the cut is made at.
    pub format: Option<(u32, u32)>,
    /// A frame layout template (`crate::frame`, e.g. `shorts`).
    pub layout: Option<String>,
    /// The type-voice legend.
    pub voices: Vec<TypeVoice>,
    /// The cut made from this board (its asset id).
    pub edit: Option<String>,
    pub built: Option<Built>,
    pub rows: Vec<Row>,
    pub items: Vec<Item>,
    pub links: Vec<Link>,
}

impl Default for Storyboard {
    fn default() -> Self {
        Self {
            title: String::new(),
            duration: 30.0,
            bpm: None,
            beat_offset: 0.0,
            beats_per_bar: 4,
            music: None,
            brief: String::new(),
            style: String::new(),
            kit: String::new(),
            format: None,
            layout: None,
            voices: Vec::new(),
            edit: None,
            built: None,
            rows: Vec::new(),
            items: Vec::new(),
            links: Vec::new(),
        }
    }
}

/// A problem in a board's source, with where it is and how to fix it.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub line: usize,
    pub col: usize,
    pub message: String,
    /// A warning leaves the board usable.
    pub warning: bool,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}{}", self.line, self.col, if self.warning { "warning: " } else { "" }, self.message)
    }
}

pub const SCENE_FIELDS: &[&str] = &["row", "span", "color", "band", "title", "intent", "mood", "look", "kit", "music", "media", "stack", "bars", "camera", "why", "lyric", "from", "prompt", "negative", "flow", "seed"];
pub const NOTE_FIELDS: &[&str] = &["row", "span", "color", "band", "text"];
pub const CHAPTER_FIELDS: &[&str] = &["row", "span", "color", "band", "skill", "look", "hero", "out"];
pub const TREATMENT_FIELDS: &[&str] = &["row", "span", "color", "title", "idea", "look", "palette", "chosen"];
pub const PAGE_FIELDS: &[&str] = &["row", "span", "color", "title", "body", "refs"];
pub const LYRIC_FIELDS: &[&str] = &["row", "span", "color", "band", "text", "pun", "set", "voice", "device"];
pub const DEVICE_FIELDS: &[&str] = &["row", "span", "color", "band", "name", "value", "note"];
pub const VOICE_FIELDS: &[&str] = &["label", "face", "role", "color"];
pub const REF_FIELDS: &[&str] = &["row", "span", "color", "band", "media", "src", "at", "url", "colors", "font", "file", "label", "note", "role", "prompt", "negative", "flow", "seed"];
pub const BOARD_FIELDS: &[&str] = &["title", "duration", "bpm", "beat_offset", "beats_per_bar", "music", "brief", "style", "kit", "format", "layout", "voices", "cut", "built", "rows", "items", "links"];

impl Storyboard {
    pub fn row(&self, id: &str) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == id)
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }

    pub fn item_mut(&mut self, id: &str) -> Option<&mut Item> {
        self.items.iter_mut().find(|item| item.id == id)
    }

    /// The first row of a kind.
    pub fn row_of(&self, kind: RowKind) -> Option<&Row> {
        self.rows.iter().find(|row| row.kind == kind)
    }

    /// Scene cards in time order: "scene N" counts from 1 in this order.
    pub fn scenes(&self) -> Vec<&Item> {
        let mut scenes: Vec<&Item> = self.items.iter().filter(|item| matches!(item.body, Body::Scene(_))).collect();
        scenes.sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
        scenes
    }

    /// Chapters in time order: "01/08" counts in this order.
    pub fn chapters(&self) -> Vec<&Item> {
        let mut chapters: Vec<&Item> = self.items.iter().filter(|item| matches!(item.body, Body::Chapter(_))).collect();
        chapters.sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
        chapters
    }

    /// Lyric lines in time order.
    pub fn lyrics(&self) -> Vec<&Item> {
        let mut lines: Vec<&Item> = self.items.iter().filter(|item| matches!(item.body, Body::Lyric(_))).collect();
        lines.sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
        lines
    }

    pub fn line_number(&self, id: &str) -> Option<usize> {
        self.lyrics().iter().position(|item| item.id == id).map(|n| n + 1)
    }

    pub fn voice(&self, id: &str) -> Option<&TypeVoice> {
        self.voices.iter().find(|voice| voice.id == id)
    }

    /// The running devices: each name with its occurrences in time order
    /// (`(item id, start, value)`).
    pub fn devices(&self) -> Vec<(String, Vec<(String, f64, String)>)> {
        let mut out: Vec<(String, Vec<(String, f64, String)>)> = Vec::new();
        let mut items: Vec<&Item> = self.items.iter().filter(|item| matches!(item.body, Body::Device(_))).collect();
        items.sort_by(|a, b| a.start.total_cmp(&b.start));
        for item in items {
            let Body::Device(device) = &item.body else { continue };
            match out.iter_mut().find(|(name, _)| *name == device.name) {
                Some((_, list)) => list.push((item.id.clone(), item.start, device.value.clone())),
                None => out.push((device.name.clone(), vec![(item.id.clone(), item.start, device.value.clone())])),
            }
        }
        out
    }

    /// 1-based number of a chapter in time order.
    pub fn chapter_number(&self, id: &str) -> Option<usize> {
        self.chapters().iter().position(|item| item.id == id).map(|n| n + 1)
    }

    /// The frame layout template the board plans against.
    pub fn frame(&self) -> Option<&'static crate::frame::Template> {
        self.layout.as_deref().and_then(crate::frame::template)
    }

    /// Whether `t` is on the beat grid (within 10 ms); true without a tempo.
    pub fn on_beat(&self, t: f64) -> bool {
        match self.bpm {
            Some(bpm) => {
                let beat = 60.0 / bpm;
                let n = ((t - self.beat_offset) / beat).round();
                (self.beat_offset + n * beat - t).abs() < 0.01
            }
            None => true,
        }
    }

    /// 1-based number of a scene card in time order.
    pub fn scene_number(&self, id: &str) -> Option<usize> {
        self.scenes().iter().position(|item| item.id == id).map(|n| n + 1)
    }

    /// The content key of the board: every item, row, link and header
    /// field, not the build record. Two boards with the same revision say
    /// the same thing.
    pub fn revision(&self) -> String {
        let mut copy = self.clone();
        copy.built = None;
        format!("sb-{:016x}", splash::fnv64(copy.to_source().as_bytes()))
    }

    /// A name not used yet: `prefix1`, `prefix2`, …
    pub fn fresh_id(&self, prefix: &str) -> String {
        let taken = |name: &str| self.items.iter().any(|item| item.id == name) || self.rows.iter().any(|row| row.id == name) || self.links.iter().any(|link| link.id == name) || self.voices.iter().any(|voice| voice.id == name);
        (1..).map(|n| format!("{prefix}{n}")).find(|name| !taken(name)).unwrap()
    }

    /// The default rows of a new board.
    pub fn with_default_rows(mut self) -> Self {
        for kind in [RowKind::Story, RowKind::Visuals, RowKind::Voice, RowKind::Music, RowKind::Refs] {
            self.rows.push(Row { id: kind.tag().to_string(), kind, label: kind.label().to_string() });
        }
        self
    }

    /// Assets the board names (dependencies of its library revision, so
    /// GC keeps what a board refers to).
    pub fn dependencies(&self) -> Vec<AssetRef> {
        let mut out: Vec<AssetRef> = Vec::new();
        for item in &self.items {
            if let Body::Ref(reference) = &item.body {
                if let Some(src) = &reference.src {
                    if !out.contains(src) {
                        out.push(src.clone());
                    }
                }
            }
        }
        out
    }

    // ------------------------------------------------------------ load

    /// Read a board. Errors make it unusable (`Err`); warnings (an unknown
    /// field, a dangling link) come back with the board.
    pub fn load(src: &str) -> Result<(Storyboard, Vec<Diagnostic>), Vec<Diagnostic>> {
        let parsed = parse(src);
        let mut diags: Vec<Diagnostic> = parsed.errors.iter().map(|(at, message)| diag(src, *at, message.clone(), false)).collect();
        let lets: Vec<&Stmt> = parsed.stmts.iter().filter(|stmt| stmt.name.is_some()).collect();
        let Some(board_stmt) = parsed.stmts.iter().rev().find(|stmt| stmt.name.is_none() && stmt.value.ty() == Some("Storyboard")) else {
            diags.push(Diagnostic { line: 1, col: 1, message: "a board ends with `Storyboard{title: … duration: … rows: [...] items: [...]}`".into(), warning: false });
            return Err(diags);
        };
        let root = &board_stmt.value;
        let mut board = Storyboard::default();
        let w = |diags: &mut Vec<Diagnostic>, node: &Node, message: String| diags.push(diag(src, node.start, message, true));
        for field in root.fields() {
            if !BOARD_FIELDS.contains(&field.name.as_str()) {
                w(&mut diags, &field.value, format!("Storyboard has no field `{}` (it has {}); it is ignored", field.name, BOARD_FIELDS.join(", ")));
            }
        }
        let text = |node: &Node, key: &str| node.field(key).and_then(Node::str).unwrap_or("").to_string();
        board.title = text(root, "title");
        board.brief = text(root, "brief");
        board.style = text(root, "style");
        board.kit = text(root, "kit");
        match root.field("duration").map(|node| (node, node.num())) {
            Some((_, Some(duration))) if duration > 0.0 => board.duration = duration,
            Some((node, _)) => diags.push(diag(src, node.start, format!("`duration` is a positive number of seconds, not {}", node.what()), false)),
            None => {}
        }
        if let Some(node) = root.field("bpm") {
            match node.num() {
                Some(bpm) if bpm > 0.0 => board.bpm = Some(bpm),
                _ if matches!(node.kind, Kind::Nil) => {}
                _ => diags.push(diag(src, node.start, "`bpm` is a positive tempo".into(), false)),
            }
        }
        board.beat_offset = root.field("beat_offset").and_then(Node::num).unwrap_or(0.0);
        board.beats_per_bar = root.field("beats_per_bar").and_then(Node::num).filter(|n| *n >= 1.0 && *n <= 16.0).map_or(4, |n| n as u32);
        board.music = root.field("music").and_then(Node::name).map(str::to_string);
        board.edit = root.field("cut").and_then(|node| match &node.kind {
            Kind::Str(s) => Some(s.clone()),
            Kind::Call { name, args } if name == "asset" => args.first().and_then(Node::str).map(str::to_string),
            _ => None,
        });
        board.format = root.field("format").and_then(|node| {
            let width = node.field("width").and_then(Node::num)?;
            let height = node.field("height").and_then(Node::num)?;
            (width >= 1.0 && height >= 1.0).then_some((width as u32, height as u32))
        });
        if let Some(node) = root.field("layout") {
            match node.name().map(|id| (id, crate::frame::template(id))) {
                Some((id, Some(_))) => board.layout = Some(id.to_string()),
                _ => w(&mut diags, node, format!("`layout` is one of @{}; it is ignored", crate::frame::TEMPLATES.iter().map(|t| t.id).collect::<Vec<_>>().join(", @"))),
            }
        }
        // The voices before the items (lines name them).
        for node in root.field("voices").and_then(Node::arr).unwrap_or(&[]) {
            let Some(name) = node.name() else { continue };
            match lets.iter().find(|stmt| stmt.name.as_deref() == Some(name)).map(|stmt| &stmt.value) {
                Some(value) if value.ty() == Some("Voice") => {
                    let text = |key: &str| value.field(key).and_then(Node::str).unwrap_or("").to_string();
                    let color = value.field("color").and_then(|node| match node.kind {
                        Kind::Color(color) => Some(color),
                        _ => None,
                    });
                    board.voices.push(TypeVoice { id: name.to_string(), label: text("label"), face: text("face"), role: text("role"), color });
                }
                _ => diags.push(diag(src, node.start, format!("voice `{name}` is listed but there is no `let {name} = Voice{{label: … face: … role: …}}`"), false)),
            }
        }
        if let Some(node) = root.field("built") {
            match read_built(node) {
                Some(built) => board.built = Some(built),
                None => w(&mut diags, node, "`built` is `Built{edit: asset(…) board: \"sb-…\" parts: [Part{item: … nodes: [...] span: [s, l] sig: \"…\"}]}`; it is ignored".into()),
            }
        }
        let find = |name: &str| lets.iter().find(|stmt| stmt.name.as_deref() == Some(name)).copied();
        let names = |key: &str| -> Vec<(String, &Node)> {
            root.field(key).and_then(Node::arr).unwrap_or(&[]).iter().filter_map(|node| node.name().map(|name| (name.to_string(), node))).collect()
        };
        for (name, at) in names("rows") {
            let Some(stmt) = find(&name) else {
                diags.push(diag(src, at.start, format!("row `{name}` is listed but there is no `let {name} = Row{{…}}`"), false));
                continue;
            };
            let value = &stmt.value;
            if value.ty() != Some("Row") {
                diags.push(diag(src, value.start, format!("`{name}` is listed in `rows` but is {}", value.what()), false));
                continue;
            }
            let kind = match value.field("kind").and_then(Node::tag).map(|tag| (tag, RowKind::from_tag(tag))) {
                Some((_, Some(kind))) => kind,
                Some((tag, None)) => {
                    diags.push(diag(src, value.start, format!("row `{name}`: `kind: @{tag}` is not a row kind (@story, @visuals, @voice, @music, @refs, @notes)"), false));
                    continue;
                }
                None => RowKind::from_tag(&name).unwrap_or(RowKind::Notes),
            };
            let label = value.field("label").and_then(Node::str).map(str::to_string).unwrap_or_else(|| kind.label().to_string());
            board.rows.push(Row { id: name, kind, label });
        }
        for (name, at) in names("items") {
            let Some(stmt) = find(&name) else {
                diags.push(diag(src, at.start, format!("item `{name}` is listed but there is no `let {name} = …`"), false));
                continue;
            };
            match read_item(src, &name, &stmt.value, &board, &mut diags) {
                Some(item) => {
                    if board.items.iter().any(|other| other.id == item.id) {
                        diags.push(diag(src, at.start, format!("item `{name}` is listed twice"), false));
                    } else {
                        board.items.push(item);
                    }
                }
                None => continue,
            }
        }
        for (name, at) in names("links") {
            let Some(stmt) = find(&name) else {
                diags.push(diag(src, at.start, format!("link `{name}` is listed but there is no `let {name} = Link{{…}}`"), false));
                continue;
            };
            let value = &stmt.value;
            let from = value.field("from").and_then(Node::name).unwrap_or("").to_string();
            let to = value.field("to").and_then(Node::name).unwrap_or("").to_string();
            if board.item(&from).is_none() || board.item(&to).is_none() {
                w(&mut diags, value, format!("link `{name}` joins `{from}` and `{to}`, and one of them is not an item; it is ignored"));
                continue;
            }
            board.links.push(Link { id: name, from, to, label: text(value, "label") });
        }
        // A page's references must be items.
        for item in &board.items {
            if let Body::Page(page) = &item.body {
                for id in page.refs.iter().filter(|id| board.item(id).is_none()) {
                    diags.push(Diagnostic { line: 1, col: 1, message: format!("page `{}` names `{id}` in `refs`, which is not an item", item.id), warning: true });
                }
            }
        }
        if let Some(music) = &board.music {
            if board.item(music).is_none() {
                w(&mut diags, root.field("music").unwrap(), format!("`music: {music}` is not an item"));
                board.music = None;
            }
        }
        // Lets nobody lists are kept in the source but not on the board.
        for stmt in &lets {
            let name = stmt.name.as_deref().unwrap_or("");
            let listed = board.rows.iter().any(|row| row.id == name) || board.items.iter().any(|item| item.id == name) || board.links.iter().any(|link| link.id == name);
            if !listed && matches!(stmt.value.ty(), Some("Scene" | "Note" | "Ref" | "Row" | "Link")) {
                w(&mut diags, &stmt.value, format!("`{name}` is not listed in the board's `rows`, `items` or `links`, so it is not on the board"));
            }
        }
        if diags.iter().any(|d| !d.warning) {
            return Err(diags);
        }
        Ok((board, diags))
    }

    // ----------------------------------------------------------- write

    /// The whole board as Splash, in the canonical layout.
    pub fn to_source(&self) -> String {
        let mut out = String::from("use mod.storyboard.*\n\n");
        for row in &self.rows {
            out.push_str(&format!("let {} = {}\n", row.id, row_source(row)));
        }
        if !self.rows.is_empty() {
            out.push('\n');
        }
        for item in &self.items {
            out.push_str(&format!("let {} = {}\n", item.id, item_source(item)));
        }
        if !self.links.is_empty() {
            out.push('\n');
        }
        for link in &self.links {
            out.push_str(&format!("let {} = {}\n", link.id, link_source(link)));
        }
        if !self.voices.is_empty() {
            out.push('\n');
        }
        for voice in &self.voices {
            out.push_str(&format!("let {} = {}\n", voice.id, voice_source(voice)));
        }
        out.push_str("\nStoryboard{\n");
        for (key, value) in self.header_fields() {
            out.push_str(&format!("    {key}: {value}\n"));
        }
        let ids = |names: Vec<String>| splash::names_array(&names, "    ");
        out.push_str(&format!("    rows: {}\n", ids(self.rows.iter().map(|row| row.id.clone()).collect())));
        if !self.voices.is_empty() {
            out.push_str(&format!("    voices: {}\n", ids(self.voices.iter().map(|voice| voice.id.clone()).collect())));
        }
        out.push_str(&format!("    items: {}\n", ids(self.items.iter().map(|item| item.id.clone()).collect())));
        if !self.links.is_empty() {
            out.push_str(&format!("    links: {}\n", ids(self.links.iter().map(|link| link.id.clone()).collect())));
        }
        if let Some(built) = &self.built {
            out.push_str(&format!("    built: {}\n", built_source(built)));
        }
        out.push_str("}\n");
        out
    }

    /// The header's fields as they are written (not rows/items/links/built).
    pub fn header_fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = vec![("title", string_literal(&self.title)), ("duration", number(self.duration))];
        if let Some(bpm) = self.bpm {
            fields.push(("bpm", number(bpm).trim_end_matches(".0").to_string()));
        }
        if self.beat_offset != 0.0 {
            fields.push(("beat_offset", number(self.beat_offset)));
        }
        if self.beats_per_bar != 4 {
            fields.push(("beats_per_bar", self.beats_per_bar.to_string()));
        }
        if let Some(music) = &self.music {
            fields.push(("music", music.clone()));
        }
        if let Some((width, height)) = self.format {
            fields.push(("format", format!("{{width: {width} height: {height}}}")));
        }
        if let Some(layout) = &self.layout {
            fields.push(("layout", format!("@{layout}")));
        }
        if !self.brief.is_empty() {
            fields.push(("brief", string_literal(&self.brief)));
        }
        if !self.style.is_empty() {
            fields.push(("style", string_literal(&self.style)));
        }
        if !self.kit.is_empty() {
            fields.push(("kit", string_literal(&self.kit)));
        }
        if let Some(edit) = &self.edit {
            fields.push(("cut", string_literal(edit)));
        }
        fields
    }
}

fn diag(src: &str, at: usize, message: String, warning: bool) -> Diagnostic {
    let (line, col) = splash::line_col(src, at);
    Diagnostic { line, col, message, warning }
}

fn asset_of(node: &Node) -> Option<AssetRef> {
    match &node.kind {
        Kind::Call { name, args } if name == "asset" && args.len() == 2 => Some(AssetRef { asset_id: args[0].str()?.to_string(), revision: args[1].str()?.to_string() }),
        _ => None,
    }
}

fn colors_of(node: Option<&Node>) -> Vec<u32> {
    node.and_then(Node::arr).unwrap_or(&[]).iter().filter_map(|node| match node.kind {
        Kind::Color(color) => Some(color),
        _ => None,
    }).collect()
}

fn read_gen(node: &Node) -> Gen {
    let text = |key: &str| node.field(key).and_then(Node::str).unwrap_or("").to_string();
    Gen {
        from: node.field("from").and_then(Node::name).map(str::to_string),
        prompt: text("prompt"),
        negative: text("negative"),
        flow: text("flow"),
        seed: node.field("seed").and_then(Node::num).map(|n| n as i64),
    }
}

fn read_item(src: &str, name: &str, value: &Node, board: &Storyboard, diags: &mut Vec<Diagnostic>) -> Option<Item> {
    let ty = value.ty().unwrap_or("");
    let known: &[&str] = match ty {
        "Scene" => SCENE_FIELDS,
        "Note" => NOTE_FIELDS,
        "Ref" => REF_FIELDS,
        "Chapter" => CHAPTER_FIELDS,
        "Treatment" => TREATMENT_FIELDS,
        "Page" => PAGE_FIELDS,
        "Lyric" => LYRIC_FIELDS,
        "Device" => DEVICE_FIELDS,
        _ => {
            diags.push(diag(src, value.start, format!("item `{name}` is {}; an item is a Scene{{…}}, a Chapter{{…}}, a Lyric{{…}}, a Device{{…}}, a Note{{…}}, a Ref{{…}}, a Treatment{{…}} or a Page{{…}}", value.what()), false));
            return None;
        }
    };
    for field in value.fields() {
        if !known.contains(&field.name.as_str()) {
            diags.push(diag(src, field.start, format!("{ty} `{name}` has no field `{}` (it has {}); it is ignored", field.name, known.join(", ")), true));
        }
    }
    let row = match value.field("row").and_then(Node::name) {
        Some(row) if board.row(row).is_some() => row.to_string(),
        Some(row) => {
            diags.push(diag(src, value.start, format!("item `{name}`: `row: {row}` is not one of the board's rows ({})", board.rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>().join(", ")), false));
            return None;
        }
        None => {
            diags.push(diag(src, value.start, format!("item `{name}` needs `row: <row name>`"), false));
            return None;
        }
    };
    let span = value.field("span").and_then(Node::arr).and_then(|span| match span {
        [start, length] => Some((start.num()?, length.num()?)),
        _ => None,
    });
    let Some((start, length)) = span.filter(|(start, length)| *start >= 0.0 && *length > 0.0) else {
        diags.push(diag(src, value.start, format!("item `{name}` needs `span: [start, length]` in seconds, start ≥ 0 and length > 0"), false));
        return None;
    };
    let color = value.field("color").and_then(|node| match node.kind {
        Kind::Color(color) => Some(color),
        _ => None,
    });
    let band = value.field("band").and_then(Node::name).map(str::to_string);
    if let Some(band) = &band {
        match board.frame() {
            Some(frame) if frame.band(band).is_none() => diags.push(diag(src, value.start, format!("item `{name}`: `band: @{band}` is not a band of the `{}` layout (@{})", frame.id, frame.band_ids().join(", @")), true)),
            None => diags.push(diag(src, value.start, format!("item `{name}` names `band: @{band}` but the board has no `layout`"), true)),
            _ => {}
        }
    }
    let text = |key: &str| value.field(key).and_then(Node::str).unwrap_or("").to_string();
    let body = match ty {
        "Note" => Body::Note { text: text("text") },
        "Chapter" => Body::Chapter(Chapter { skill: text("skill"), look: text("look"), hero: text("hero"), out: text("out") }),
        "Treatment" => Body::Treatment(Treatment {
            title: text("title"),
            idea: text("idea"),
            look: text("look"),
            palette: colors_of(value.field("palette")),
            chosen: matches!(value.field("chosen").map(|node| &node.kind), Some(Kind::Bool(true))),
        }),
        "Lyric" => {
            let voice = value.field("voice").and_then(Node::name).map(str::to_string);
            if let Some(voice) = &voice {
                if !board.voices.iter().any(|v| v.id == *voice) {
                    diags.push(diag(src, value.start, format!("line `{name}`: `voice: @{voice}` is not in the board's `voices` ({})", board.voices.iter().map(|v| v.id.as_str()).collect::<Vec<_>>().join(", ")), true));
                }
            }
            Body::Lyric(Lyric { text: text("text"), pun: text("pun"), set: text("set"), voice, device: text("device") })
        }
        "Device" => Body::Device(Device { name: text("name"), value: text("value"), note: text("note") }),
        "Page" => {
            let refs: Vec<String> = value.field("refs").and_then(Node::arr).unwrap_or(&[]).iter().filter_map(|node| node.name().map(str::to_string)).collect();
            Body::Page(Page { title: text("title"), body: text("body"), refs })
        }
        "Scene" => Body::Scene(Scene {
            title: text("title"),
            intent: text("intent"),
            mood: text("mood"),
            look: text("look"),
            kit: text("kit"),
            music: text("music"),
            media: text("media"),
            stack: text("stack"),
            bars: text("bars"),
            camera: text("camera"),
            why: text("why"),
            lyric: text("lyric"),
            gen: read_gen(value),
        }),
        _ => {
            let media = match value.field("media").and_then(Node::tag) {
                Some(tag) => match Media::from_tag(tag) {
                    Some(media) => Some(media),
                    None => {
                        diags.push(diag(src, value.start, format!("Ref `{name}`: `media: @{tag}` is not one of @{}", Media::ALL.map(Media::tag).join(", @")), true));
                        None
                    }
                },
                None => None,
            };
            let role = match value.field("role").and_then(Node::tag) {
                Some(tag) => Role::from_tag(tag).unwrap_or_else(|| {
                    diags.push(diag(src, value.start, format!("Ref `{name}`: `role: @{tag}` is @reference, @use or @generate"), true));
                    Role::Reference
                }),
                None => Role::Reference,
            };
            let src_ref = value.field("src").and_then(asset_of);
            if value.field("src").is_some() && src_ref.is_none() {
                diags.push(diag(src, value.start, format!("Ref `{name}`: `src` is `asset(\"<asset id>\", \"<revision>\")`"), true));
            }
            let colors = value.field("colors").and_then(Node::arr).unwrap_or(&[]).iter().filter_map(|node| match node.kind {
                Kind::Color(color) => Some(color),
                _ => None,
            }).collect();
            Body::Ref(Reference {
                media,
                src: src_ref,
                at: value.field("at").and_then(Node::num),
                url: text("url"),
                colors,
                font: text("font"),
                file: text("file"),
                label: text("label"),
                note: text("note"),
                role,
                gen: read_gen(value),
            })
        }
    };
    Some(Item { id: name.to_string(), row, start, length, color, band, body })
}

fn read_built(node: &Node) -> Option<Built> {
    let edit = node.field("cut").and_then(asset_of)?;
    let board = node.field("board").and_then(Node::str)?.to_string();
    let mut parts = Vec::new();
    for part in node.field("parts").and_then(Node::arr).unwrap_or(&[]) {
        let item = part.field("item").and_then(Node::name)?.to_string();
        let nodes = part.field("nodes").and_then(Node::arr).unwrap_or(&[]).iter().filter_map(|n| n.str().map(str::to_string)).collect();
        let (start, length) = match part.field("span").and_then(Node::arr)? {
            [start, length] => (start.num()?, length.num()?),
            _ => return None,
        };
        let sig = part.field("sig").and_then(Node::str).unwrap_or("").to_string();
        parts.push(Part { item, nodes, start, length, sig });
    }
    Some(Built { edit, board, parts })
}

// ------------------------------------------------------------ writers

pub fn asset_literal(asset: &AssetRef) -> String {
    format!("asset({}, {})", string_literal(&asset.asset_id), string_literal(&asset.revision))
}

pub fn row_source(row: &Row) -> String {
    format!("Row{{kind: @{} label: {}}}", row.kind.tag(), string_literal(&row.label))
}

pub fn voice_source(voice: &TypeVoice) -> String {
    let mut out = format!("Voice{{label: {}", string_literal(&voice.label));
    for (key, value) in [("face", &voice.face), ("role", &voice.role)] {
        if !value.is_empty() {
            out.push_str(&format!(" {key}: {}", string_literal(value)));
        }
    }
    if let Some(color) = voice.color {
        out.push_str(&format!(" color: {}", color_literal(color)));
    }
    out.push('}');
    out
}

pub fn link_source(link: &Link) -> String {
    let mut out = format!("Link{{from: {} to: {}", link.from, link.to);
    if !link.label.is_empty() {
        out.push_str(&format!(" label: {}", string_literal(&link.label)));
    }
    out.push('}');
    out
}

/// An item's fields in their written order: `(name, literal)`.
pub fn item_fields(item: &Item) -> Vec<(&'static str, String)> {
    let mut fields = vec![("row", item.row.clone()), ("span", format!("[{}, {}]", number(item.start), number(item.length)))];
    if let Some(color) = item.color {
        fields.push(("color", color_literal(color)));
    }
    if let Some(band) = &item.band {
        fields.push(("band", format!("@{band}")));
    }
    let text = |fields: &mut Vec<(&'static str, String)>, key: &'static str, value: &str| {
        if !value.is_empty() {
            fields.push((key, string_literal(value)));
        }
    };
    let gen = |fields: &mut Vec<(&'static str, String)>, gen: &Gen, with_from: bool| {
        if with_from {
            if let Some(from) = &gen.from {
                // A tag, not a bare name: the reference may be written
                // after the card, and Splash reads names in order.
                fields.push(("from", format!("@{from}")));
            }
        }
        for (key, value) in [("prompt", &gen.prompt), ("negative", &gen.negative), ("flow", &gen.flow)] {
            if !value.is_empty() {
                fields.push((key, string_literal(value)));
            }
        }
        if let Some(seed) = gen.seed {
            fields.push(("seed", seed.to_string()));
        }
    };
    match &item.body {
        Body::Note { text: body } => fields.push(("text", string_literal(body))),
        Body::Treatment(treatment) => {
            fields.push(("title", string_literal(&treatment.title)));
            text(&mut fields, "idea", &treatment.idea);
            text(&mut fields, "look", &treatment.look);
            if !treatment.palette.is_empty() {
                fields.push(("palette", format!("[{}]", treatment.palette.iter().map(|c| color_literal(*c)).collect::<Vec<_>>().join(", "))));
            }
            if treatment.chosen {
                fields.push(("chosen", "true".into()));
            }
        }
        Body::Lyric(lyric) => {
            fields.push(("text", string_literal(&lyric.text)));
            text(&mut fields, "pun", &lyric.pun);
            text(&mut fields, "set", &lyric.set);
            if let Some(voice) = &lyric.voice {
                fields.push(("voice", format!("@{voice}")));
            }
            text(&mut fields, "device", &lyric.device);
        }
        Body::Device(device) => {
            fields.push(("name", string_literal(&device.name)));
            fields.push(("value", string_literal(&device.value)));
            text(&mut fields, "note", &device.note);
        }
        Body::Page(page) => {
            fields.push(("title", string_literal(&page.title)));
            text(&mut fields, "body", &page.body);
            if !page.refs.is_empty() {
                // Tags: a page may name references written after it.
                fields.push(("refs", format!("[{}]", page.refs.iter().map(|id| format!("@{id}")).collect::<Vec<_>>().join(", "))));
            }
        }
        Body::Chapter(chapter) => {
            fields.push(("skill", string_literal(&chapter.skill)));
            for (key, value) in [("look", &chapter.look), ("hero", &chapter.hero), ("out", &chapter.out)] {
                text(&mut fields, key, value);
            }
        }
        Body::Scene(scene) => {
            fields.push(("title", string_literal(&scene.title)));
            for (key, value) in [("intent", &scene.intent), ("mood", &scene.mood), ("look", &scene.look), ("kit", &scene.kit), ("music", &scene.music), ("media", &scene.media), ("stack", &scene.stack), ("bars", &scene.bars), ("camera", &scene.camera), ("why", &scene.why), ("lyric", &scene.lyric)] {
                text(&mut fields, key, value);
            }
            gen(&mut fields, &scene.gen, true);
        }
        Body::Ref(reference) => {
            if let Some(media) = reference.media {
                fields.push(("media", format!("@{}", media.tag())));
            }
            if let Some(src) = &reference.src {
                fields.push(("src", asset_literal(src)));
            }
            if let Some(at) = reference.at {
                fields.push(("at", number(at)));
            }
            text(&mut fields, "url", &reference.url);
            if !reference.colors.is_empty() {
                fields.push(("colors", format!("[{}]", reference.colors.iter().map(|c| color_literal(*c)).collect::<Vec<_>>().join(", "))));
            }
            text(&mut fields, "font", &reference.font);
            text(&mut fields, "file", &reference.file);
            text(&mut fields, "label", &reference.label);
            text(&mut fields, "note", &reference.note);
            if reference.role != Role::Reference {
                fields.push(("role", format!("@{}", reference.role.tag())));
            }
            gen(&mut fields, &reference.gen, false);
        }
    }
    fields
}

/// An item as a constructor, on one line (wrapped past 100 columns).
pub fn item_source(item: &Item) -> String {
    let fields = item_fields(item);
    let mut out = format!("{}{{", item.body.ty());
    let mut line_len = out.len() + 12;
    for (index, (key, value)) in fields.iter().enumerate() {
        let entry = format!("{key}: {value}");
        if index > 0 {
            if line_len + entry.len() > 110 {
                out.push_str("\n    ");
                line_len = 4;
            } else {
                out.push(' ');
                line_len += 1;
            }
        }
        line_len += entry.len();
        out.push_str(&entry);
    }
    out.push('}');
    out
}

pub fn built_source(built: &Built) -> String {
    let mut out = format!("Built{{edit: {} board: {} parts: [", asset_literal(&built.edit), string_literal(&built.board));
    for part in &built.parts {
        out.push_str(&format!(
            "\n        Part{{item: {} nodes: [{}] span: [{}, {}] sig: {}}}",
            part.item,
            part.nodes.iter().map(|n| string_literal(n)).collect::<Vec<_>>().join(", "),
            number(part.start),
            number(part.length),
            string_literal(&part.sig)
        ));
    }
    if !built.parts.is_empty() {
        out.push_str("\n    ");
    }
    out.push_str("]}");
    out
}
