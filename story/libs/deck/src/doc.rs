//! The typed deck and its slide clocks.

use makepad_story_app_run::call::Call;
use makepad_story_cut::{AssetRef, CutDoc, Dep};

/// A problem in a deck's source, with the place it was written.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    /// 1-based, in the deck's own source (also for problems inside
    /// a slide's cut).
    pub line: u32,
    pub col: u32,
    /// The slide it belongs to, when it belongs to one.
    pub slide: Option<String>,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

/// `transition(seconds, effect, dials)`: how one slide gives way to another.
#[derive(Clone, Debug, PartialEq)]
pub struct Transition {
    pub seconds: f64,
    /// A transition preset by name; empty is a dissolve.
    pub effect: String,
    /// The preset's dials by name (`p1`..`p3`), sorted.
    pub dials: Vec<(String, f64)>,
}

impl Transition {
    /// The transition between slides when a deck names none.
    pub fn default_dissolve() -> Transition {
        Transition { seconds: 0.6, effect: String::new(), dials: Vec::new() }
    }

    /// The dial `name`, when the change sets it.
    pub fn dial(&self, name: &str) -> Option<f64> {
        self.dials.iter().find(|(key, _)| key == name).map(|(_, value)| *value)
    }
}

/// What a slide plays.
#[derive(Clone, Debug)]
pub enum SlideCut {
    /// A cut written in the deck file.
    Inline(Box<CutDoc>),
    /// A library cut at an exact revision (`cut(id, rev)`); the player
    /// resolves it through the library.
    Library { id: String, rev: String },
    /// A tree graphic not evaluated yet ([`DeckDoc::load_tree_first`]):
    /// the slide's `graphic` names it, and the full load replaces the deck.
    Pending,
}

impl SlideCut {
    /// The cut's length in seconds, when the cut is in the file.
    pub fn duration(&self) -> Option<f64> {
        match self {
            SlideCut::Inline(doc) => Some(doc.duration),
            SlideCut::Library { .. } | SlideCut::Pending => None,
        }
    }
}

/// How a slide's clock runs after it is entered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Loop {
    /// The whole cut, again and again.
    Whole,
    /// `play: [a, b]`: from the start of the slide, play from `a`, and wrap
    /// from `b` back to `a`.
    Range(f64, f64),
    /// `play: @hold`: play to the end of the cut, then hold its last frame.
    Hold,
}

/// A cut time from the time since a slide was entered. `cut_duration` is
/// the cut's length; the result is in `[0, cut_duration]`.
pub fn loop_wrap(looping: &Loop, cut_duration: f64, t: f64) -> f64 {
    let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
    if cut_duration <= 0.0 {
        return 0.0;
    }
    match *looping {
        Loop::Whole => t % cut_duration,
        Loop::Range(a, b) => {
            let a = a.clamp(0.0, cut_duration);
            let b = b.clamp(a, cut_duration);
            if b - a <= 0.0 {
                a
            } else {
                a + t % (b - a)
            }
        }
        Loop::Hold => t.min(cut_duration),
    }
}

/// A live app a slide hosts. `App{app: "calculator"}` is enough: the window
/// defaults to 900 × 600 points, muted, with no extra arguments or environment.
#[derive(Clone, Debug, PartialEq)]
pub struct AppDecl {
    /// The name the slide's cut refers to it by.
    pub name: String,
    /// The shared app registry id.
    pub app: String,
    /// Window size in points (optional in source; defaults to `DEFAULT_SIZE`).
    pub size: (f64, f64),
    /// Whether it may make sound; defaults to false.
    pub audio: bool,
    /// Optional extra command line arguments, after the registry's own.
    pub args: Vec<String>,
    /// Optional extra environment variables (DECK's own settings win).
    pub env: Vec<(String, String)>,
    /// A fixed points-to-pixels factor (a deliberately zoomed app); None:
    /// automatic (see [`AppDecl::render_dpi`]).
    pub dpi: Option<f64>,
    /// A fixed pixel size the app renders at (a capture of a known size);
    /// None: automatic. Not together with `dpi`.
    pub resolution: Option<(u32, u32)>,
    /// `next: true`: while the slide's demo has begun and not finished, Next
    /// (Right, Space, the play bar) is the app's, not the deck's: the app is
    /// sent `deck.next` (`Event::Custom`) and the slide stays. A live-coding
    /// app uses it to resume or finish what it writes.
    pub next: bool,
    /// Optional automation owned by this app. Setup runs on every launch;
    /// D plays the demo of the active AppView. Slide-level steps remain the
    /// fallback for the first app of older decks.
    pub setup: Vec<DemoStep>,
    pub demo: Vec<DemoStep>,
}

impl AppDecl {
    pub const DEFAULT_SIZE: (f64, f64) = (900.0, 600.0);

    /// The points-to-pixels factor the app renders with when its frame
    /// covers `frame_px` pixels of the output ([`frame_pixels`]; None: not
    /// placed yet). It keeps its declared point layout and renders at least
    /// at the frame's pixels on both axes (never one fixed size scaled up),
    /// in steps of [`DPI_STEP`] (upwards) so a frame that moves by a pixel
    /// does not resize the app, and never under 1. `dpi` and `resolution`
    /// fix it.
    pub fn render_dpi(&self, frame_px: Option<(u32, u32)>) -> f64 {
        if let Some(dpi) = self.dpi {
            return dpi;
        }
        let Some((w, h)) = self.resolution.or(frame_px) else { return 1.0 };
        let need = (w as f64 / self.size.0.max(1.0)).max(h as f64 / self.size.1.max(1.0));
        ((need / DPI_STEP - 1e-9).ceil() * DPI_STEP).max(1.0)
    }
}

/// [`AppDecl::render_dpi`] rounds up to multiples of this.
pub const DPI_STEP: f64 = 0.125;

/// The pixels a frame covers on a picture shown `picture_px` big, `frac`
/// its width and height as fractions of the picture: rounded up, so
/// whatever renders into it is never squeezed at a fractional size. The
/// one sizing rule for what a slide frame shows -- the hosted apps' render
/// size and the slide's own native-resolution rendering.
pub fn frame_pixels(frac: [f64; 2], picture_px: (f64, f64)) -> (u32, u32) {
    let px = |frac: f64, total: f64| ((frac.max(0.0) * total.max(0.0)) - 1e-6).ceil().max(1.0) as u32;
    (px(frac[0], picture_px.0), px(frac[1], picture_px.1))
}

/// What a demo step does.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// An app run call (`click`, `wait_text`, ...), checked at load by the
    /// same parser the app run script uses.
    Call(Call),
    /// Stops the demo until the presenter resumes it.
    Pause,
}

/// One demo step, with where it is written (1-based line).
#[derive(Clone, Debug, PartialEq)]
pub struct DemoStep {
    pub step: Step,
    pub line: u32,
}

impl DemoStep {
    /// The step as the presenter's list reads it: `click @increment_button`.
    pub fn describe(&self) -> String {
        match &self.step {
            Step::Pause => "pause".to_string(),
            Step::Call(call) => match call.record_target() {
                Some(target) => format!("{} {}", call.name(), target.describe()),
                None => match call {
                    Call::Hold { seconds } => format!("hold {seconds}"),
                    Call::Mark { name } => format!("mark {name}"),
                    other => other.name().to_string(),
                },
            },
        }
    }
}

/// Where a link slide jumps: a deck file of the same tree, at its first
/// slide or at the slide named `slide` (`link_slide: @name`).
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// The deck file's canonical tree path.
    pub path: String,
    pub slide: Option<String>,
}

impl Link {
    /// The slide the jump enters in `target`: `link_slide`'s index, else 0.
    pub fn entry(&self, target: &DeckDoc) -> Result<usize, String> {
        match &self.slide {
            None => Ok(0),
            Some(name) => target.slides.iter().position(|slide| slide.name == *name)
                .ok_or_else(|| format!("`link_slide: @{name}`: {} has no slide `{name}`", self.path)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Slide {
    pub name: String,
    /// The canonical tree file selected by graphic or graphic(), otherwise an inline cut.
    pub graphic: Option<String>,
    /// The text the person typed for the slide (`sketch: "Title\n- point"`):
    /// its intent, kept when a graphic is attached. A sketch slide without a
    /// graphic shows the default layout ([`crate::sketch`]).
    pub sketch: Option<String>,
    pub cut: SlideCut,
    pub looping: Loop,
    /// Story times held until Next releases them, strictly increasing and
    /// inside the cut. The first pass starts at zero, before `play` loops.
    pub beats: Vec<f64>,
    /// The transition into this slide; the deck's default when None.
    pub enter: Option<Transition>,
    /// The transition used when leaving backwards from this slide.
    pub exit: Option<Transition>,
    pub notes: String,
    pub apps: Vec<AppDecl>,
    /// Steps run on the first app by themselves when the slide's apps are prepared,
    /// before the first beat (a framing, a zoom); re-run with every launch of the app.
    pub setup: Vec<DemoStep>,
    pub demo: Vec<DemoStep>,
    /// What the presenter timer compares the slide against.
    pub target_seconds: Option<f64>,
    /// `link: "deck.splash"`: Next after the slide's last beat enters that
    /// deck of the same tree (a jump into another talk), and the presenter
    /// can return to the slide after this one.
    pub link: Option<Link>,
    /// Where the slide is written, 1-based.
    pub line: u32,
    /// The `Slide{...}` record's extent in the source: its `{` to just past
    /// its `}`.
    pub span: (usize, usize),
    /// The slide's `Flow{...}` extent, when the cut is written in the file
    /// (inline in the slide, or the `let` it names).
    pub cut_span: Option<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct DeckDoc {
    pub source: String,
    /// Pinned file context, retained when a document is inspected or edited.
    pub tree: Option<makepad_story_cut::tree::TreeContext>,
    pub title: String,
    /// The deck's look module (`look: "shared/makepad_deck_look"`), a tree
    /// path: what a sketch slide's default layout is styled with.
    pub look: Option<String>,
    /// Output picture size in pixels.
    pub output: (u32, u32),
    /// The transition between slides unless a slide names its own.
    pub transition: Transition,
    /// The talk's length the speaker set (`talk: seconds`); None: the
    /// slides' speaking targets add up to it ([`DeckDoc::talk_target`]).
    pub talk_seconds: Option<f64>,
    pub slides: Vec<Slide>,
    /// Each tree graphic as its file loaded, by tree path: the output it
    /// was loaded for and the document, before a slide set its duration
    /// and fit. [`DeckDoc::reload_tree`] takes one again instead of
    /// evaluating its file when neither the file nor anything it reads
    /// changed.
    pub graphics: std::collections::BTreeMap<String, ((u32, u32), std::sync::Arc<CutDoc>)>,
}

impl Slide {
    /// How the slide's clock runs after its first pass: what its `play` says,
    /// the whole cut again and again by default, whether or not the slide
    /// hosts apps (a looping backdrop keeps moving behind a live app).
    pub fn play(&self) -> Loop {
        self.looping
    }

    /// A sketch slide shown with the default layout (no graphic of its own yet).
    pub fn sketch_only(&self) -> bool {
        self.sketch.is_some() && self.graphic.is_none() && self.cut_span.is_none() && self.apps.is_empty()
    }

    pub fn setup_for<'a>(&'a self, app: &'a AppDecl) -> &'a [DemoStep] {
        if !app.setup.is_empty() {
            &app.setup
        } else if self.apps.first().is_some_and(|first| first.name == app.name) {
            &self.setup
        } else {
            &[]
        }
    }

    pub fn demo_for<'a>(&'a self, app: &'a AppDecl) -> &'a [DemoStep] {
        if !app.demo.is_empty() {
            &app.demo
        } else if self.apps.first().is_some_and(|first| first.name == app.name) {
            &self.demo
        } else {
            &[]
        }
    }
}

/// The links of `doc` (a deck file of a tree) checked against the decks
/// they name: each target loads (`decks` gives it, None: not a loaded deck)
/// and its `link_slide` exists.
pub fn check_links<'a>(doc: &DeckDoc, decks: impl Fn(&str) -> Option<&'a DeckDoc>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for slide in &doc.slides {
        let Some(link) = &slide.link else { continue };
        let problem = match decks(&link.path) {
            None => Some(format!("`link`: {} did not load as a deck", link.path)),
            Some(target) if target.slides.is_empty() => Some(format!("`link`: {} has no slides", link.path)),
            Some(target) => link.entry(target).err(),
        };
        if let Some(message) = problem {
            out.push(Diagnostic { line: slide.line, col: 1, slide: Some(slide.name.clone()), message });
        }
    }
    out
}

impl DeckDoc {
    /// The talk's length the presenter clock counts down from: the length
    /// set on the deck, else the sum of the slides' speaking targets.
    pub fn talk_target(&self) -> f64 {
        self.talk_seconds.unwrap_or_else(|| self.slides.iter().filter_map(|slide| slide.target_seconds).sum())
    }

    /// Every library revision the deck names, in slide order, each once: the
    /// `cut(id, rev)` a slide plays and whatever an inline cut uses. These
    /// are the dependencies of the deck's library revision.
    pub fn dependencies(&self) -> Vec<Dep> {
        let mut deps = Vec::new();
        for slide in &self.slides {
            let named = match &slide.cut {
                SlideCut::Inline(doc) => makepad_story_cut::dependencies(doc),
                SlideCut::Library { id, rev } => vec![Dep::Cut(AssetRef { id: id.clone(), rev: rev.clone() })],
                SlideCut::Pending => Vec::new(),
            };
            for dep in named {
                if !deps.contains(&dep) {
                    deps.push(dep);
                }
            }
        }
        deps
    }

    /// Evaluate and check a deck file.
    pub fn load(source: &str) -> Result<DeckDoc, Vec<Diagnostic>> {
        crate::eval::load(source, None, false)
    }

    /// All paths, including inline cut sources, resolve in this exact snapshot.
    pub fn load_tree(source: &str, tree: makepad_story_cut::tree::TreeContext) -> Result<DeckDoc, Vec<Diagnostic>> {
        crate::eval::load(source, Some(tree), false)
    }

    /// The deck file and the first slide's graphic only: every later tree
    /// graphic is [`SlideCut::Pending`] and unchecked. Each graphic is a
    /// script of its own (a plate can compute for seconds), so opening a
    /// deck shows its first slide from this and takes the full
    /// [`DeckDoc::load_tree`] when it is ready.
    pub fn load_tree_first(source: &str, tree: makepad_story_cut::tree::TreeContext) -> Result<DeckDoc, Vec<Diagnostic>> {
        crate::eval::load(source, Some(tree), true)
    }

    /// The deck again after the tree files `changed` (tree paths) were
    /// written: as [`DeckDoc::load_tree`], but a tree graphic whose file is
    /// unchanged and reads none of them is `prior`'s, not evaluated again.
    /// A code edit of one slide's file evaluates that slide, not every
    /// slide of the deck.
    pub fn reload_tree(source: &str, tree: makepad_story_cut::tree::TreeContext, prior: &DeckDoc, changed: &[String]) -> Result<DeckDoc, Vec<Diagnostic>> {
        let reuse = crate::eval::Reuse::new(prior, changed, &tree);
        crate::eval::load_reusing(source, Some(tree), false, reuse)
    }

    /// Whether a slide's graphic is still to be loaded.
    pub fn pending(&self) -> bool {
        self.slides.iter().any(|slide| matches!(slide.cut, SlideCut::Pending))
    }

    /// The transition played when entering slide `to` from `from`: going
    /// forward the slide's `enter`, going back the slide left behind's
    /// `exit`, else the deck's default.
    pub fn transition_for(&self, from: usize, to: usize) -> &Transition {
        let named = if to >= from {
            self.slides.get(to).and_then(|slide| slide.enter.as_ref())
        } else {
            self.slides.get(from).and_then(|slide| slide.exit.as_ref())
        };
        named.unwrap_or(&self.transition)
    }
}
