//! Sketch slides: `Slide{name: @x sketch: "Title\n- point\n- point"}`. The
//! sketch is plain text a person typed on the slide, its first line the
//! title and every other line a bullet; it stays in the deck as the slide's
//! intent. Without a `graphic` the slide shows the default layout, a real
//! Motion document bundled here (`sketch_layout.splash`) given the text as
//! inputs, in the deck's look (`Deck{look: "shared/..."}`, else the neutral
//! `sketch_look.splash`). Once a graphic is attached (written by hand or by
//! "Fancy it up"), the graphic is the picture and the sketch stays.
//!
//! The layout reaches the look through one path of its own, `.stage/look`:
//! [`SketchTree`] serves the two bundled files at `.stage/...` (a hidden
//! folder no tree holds) and everything else from the deck's tree, so the
//! layout imports the deck's look module the way any slide does.

use makepad_asset_data::tree::{TreeContext, TreeFile, TreeLookup, TreeResolver};
use makepad_asset_data::AssetKind;
use makepad_flow::Loc;
use makepad_story_cut::graphic::{motion_inputs, GraphicNode, GraphicSrc, InputValue};
use makepad_story_cut::{Animated, CutDoc, CutNode, CutNodeKind, Fit, OutputFormat, Wire};
use std::sync::Arc;

/// The default layout of a sketch slide.
pub const LAYOUT: &str = include_str!("sketch_layout.splash");
/// The look of a sketch slide in a deck that names none.
pub const NEUTRAL_LOOK: &str = include_str!("sketch_look.splash");
/// Where the layout is served from (never a path of a real tree: hidden
/// folders are not part of one).
pub const LAYOUT_PATH: &str = ".stage/sketch_layout.splash";
/// What the layout imports as the look.
pub const LOOK_PATH: &str = ".stage/look.splash";

/// Markers a bullet line may start with; the layout draws its own.
const MARKERS: [&str; 4] = ["- ", "* ", "\u{2022} ", "\u{2013} "];

/// A sketch's words, as the layout shows them: the first line that is not
/// blank is the title, every later one that is not blank a bullet (a
/// leading `- `, `* ` or `• ` is the bullet's mark, not its words).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SketchText {
    pub title: String,
    pub lines: Vec<String>,
}

impl SketchText {
    pub fn parse(text: &str) -> SketchText {
        let mut rows = text.lines().map(str::trim).filter(|row| !row.is_empty());
        let title = rows.next().unwrap_or("").to_string();
        let lines = rows.map(|row| MARKERS.iter().find_map(|mark| row.strip_prefix(mark)).unwrap_or(row).trim().to_string())
            .filter(|row| !row.is_empty()).collect();
        SketchText { title, lines }
    }

    /// How the layout sets these words (`sketch_layout.splash`), in pixels
    /// of a 1080-line picture: what DECK's typing on the slide matches.
    pub fn metrics(&self) -> SketchMetrics {
        // More lines, smaller type: up to five bullets at full size, then
        // down to a floor; a long title a size down.
        let count = self.lines.len() as f64;
        let body_px = (50.0 - 3.0 * (count - 5.0).max(0.0)).max(34.0);
        let title_px = if self.title.chars().count() > 40 { 76.0 } else { 96.0 };
        SketchMetrics { title_px, body_px, left: 150.0, top: 170.0, mark: body_px * 1.2, rule_gap: 34.0, rule: (120.0, 4.0), body_gap: 70.0 }
    }

    /// The layout's inputs for an `output`-sized picture.
    pub fn params(&self, output: (u32, u32)) -> Vec<(String, InputValue)> {
        vec![
            ("title".into(), InputValue::Text(self.title.clone())),
            ("lines".into(), InputValue::List(self.lines.iter().map(|line| InputValue::Text(line.clone())).collect())),
            ("frame".into(), InputValue::Vec2([output.0 as f64, output.1 as f64])),
        ]
    }
}

/// The default layout's type sizes and margins, in pixels of a 1080-line
/// picture (the layout scales them to the deck's height). They are the
/// numbers of `sketch_layout.splash`; a test holds the two together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SketchMetrics {
    pub title_px: f64,
    pub body_px: f64,
    /// The left (and right) margin, and the title's top.
    pub left: f64,
    pub top: f64,
    /// The bullets' indent past the left margin (their mark sits in it).
    pub mark: f64,
    /// Below the title: the gap to the accent rule, its size, the gap to
    /// the bullets.
    pub rule_gap: f64,
    pub rule: (f64, f64),
    pub body_gap: f64,
}

/// The deck's tree with the layout and the look at `.stage/`.
pub struct SketchTree {
    inner: Option<Arc<dyn TreeResolver>>,
    /// The deck's look module, a path of `inner`.
    look: Option<String>,
}

impl SketchTree {
    pub fn context(tree: Option<&TreeContext>, look: Option<&str>) -> TreeContext {
        let resolver = SketchTree { inner: tree.map(|tree| tree.resolver.clone()), look: look.map(str::to_string) };
        TreeContext { resolver: Arc::new(resolver), document: LAYOUT_PATH.into() }
    }
}

fn bundled(path: &str, text: &'static str) -> TreeLookup {
    TreeLookup::File(TreeFile { path: path.into(), reference: None, base: None, kind: AssetKind::Motion, bytes: Arc::from(text.as_bytes()) })
}

/// `path` names `file` (`.stage/look` or `.stage/look.splash`).
fn names(path: &str, file: &str) -> bool {
    let path = path.strip_prefix("./").unwrap_or(path);
    path == file || file.strip_suffix(".splash") == Some(path)
}

impl TreeResolver for SketchTree {
    fn lookup(&self, from: &str, path: &str) -> Result<TreeLookup, String> {
        if names(path, LAYOUT_PATH) {
            return Ok(bundled(LAYOUT_PATH, LAYOUT));
        }
        if names(path, LOOK_PATH) {
            return match (&self.look, &self.inner) {
                (Some(look), Some(inner)) => inner.lookup("", look),
                _ => Ok(bundled(LOOK_PATH, NEUTRAL_LOOK)),
            };
        }
        match &self.inner {
            Some(inner) => inner.lookup(from, path),
            None => Err(format!("{path}: a deck without a tree has no files")),
        }
    }

    fn missing_optional(&self) -> Vec<String> {
        self.inner.as_ref().map(|inner| inner.missing_optional()).unwrap_or_default()
    }
}

/// A sketch slide's picture without a graphic: the default layout given
/// `text`, as a cut of one graphic over the deck's `output`. The layout is
/// loaded here, so a look that does not load is the slide's error.
pub fn layout_cut(text: &str, tree: Option<&TreeContext>, look: Option<&str>, output: (u32, u32), fps: f64) -> Result<CutDoc, String> {
    let context = SketchTree::context(tree, look);
    let params = SketchText::parse(text).params(output);
    let motion_context = makepad_motion::MotionContext { modules: makepad_motion::modules::Modules::from_tree(context.clone()), ..Default::default() };
    let motion = makepad_motion::MotionDoc::load_with(LAYOUT, &motion_inputs(&params), motion_context)
        .map_err(|diags| format!("the sketch layout does not load: {}", diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")))?;
    let loc = Loc { line: 1, col: 1 };
    Ok(CutDoc {
        missing_media: context.resolver.missing_optional(),
        source: LAYOUT.into(),
        label: motion.label.clone(),
        output: OutputFormat { width: output.0, height: output.1, fps, ..OutputFormat::default() },
        duration: motion.duration,
        bpm: motion.bpm,
        nodes: vec![CutNode { id: "output".into(), span: None, track: 0, loc: loc.clone(), kind: CutNodeKind::Output { frame: Some(Wire { node: "sketch".into(), port: "frame".into() }), audio: Vec::new() } }],
        output_node: "output".into(),
        graph: makepad_flow::Graph { revision: 0, label: motion.label, brief: String::new(), trigger: String::new(), concurrency: 1, autostart: false, nodes: Vec::new(), edges: Vec::new(), tools: Vec::new(), flow_ui_src: None, warnings: Vec::new(), components: Vec::new() },
        synthetic_layers: Vec::new(),
        generates: Vec::new(),
        time_bindings: Vec::new(),
        graphics: vec![GraphicNode { id: "sketch".into(), span: None, track: 0, loc, src: GraphicSrc::Tree(context), fit: Fit::Fill, params, opacity: Animated::scalar(1.0), bind: None }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_line_is_the_title_and_marks_are_not_words() {
        let text = SketchText::parse("  Why sketch\n\n- Type it\n* Then fancy it\n\u{2022} keep  the words \nplain line\n-not a mark\n");
        assert_eq!(text.title, "Why sketch");
        assert_eq!(text.lines, ["Type it", "Then fancy it", "keep  the words", "plain line", "-not a mark"]);
        assert_eq!(SketchText::parse(""), SketchText::default());
        assert_eq!(SketchText::parse("\n\nOnly a title").lines, Vec::<String>::new());
    }

    #[test]
    fn the_metrics_are_the_layouts_numbers() {
        // The typing in DECK's view sizes itself by `metrics`; the layout
        // draws the slide. A change to one is a change to the other.
        for line in [
            "let BODY_PX = U * max(34.0, 50.0 - 3.0 * max(0, COUNT - 5))",
            "let TITLE_PX = U * (if TITLE.len() > 40 { 76.0 } else { 96.0 })",
            "let MARK = BODY_PX * 1.2",
            "let LEFT = 150.0 * U",
            "let TOP = 170.0 * U",
            "y = y + 34.0 * U",
            "c.rect(LEFT, y, 120.0 * U, 4.0 * U, ACCENT, 0.9)",
            "y = y + 70.0 * U",
        ] {
            assert!(LAYOUT.contains(line), "sketch_layout.splash no longer has `{line}`: update SketchText::metrics");
        }
        let short = SketchText::parse("A title\n- one\n- two");
        assert_eq!(short.metrics(), SketchMetrics { title_px: 96.0, body_px: 50.0, left: 150.0, top: 170.0, mark: 60.0, rule_gap: 34.0, rule: (120.0, 4.0), body_gap: 70.0 });
        let long = SketchText::parse(&format!("{}\n{}", "t".repeat(41), "x\n".repeat(12)));
        assert_eq!((long.metrics().title_px, long.metrics().body_px), (76.0, 34.0));
    }

    #[test]
    fn the_bundled_files_are_served_and_the_rest_is_the_trees() {
        let context = SketchTree::context(None, None);
        let file = |path: &str| match context.resolver.lookup("", path) { Ok(TreeLookup::File(file)) => file.path, other => panic!("{path}: {}", other.is_ok()) };
        assert_eq!(file(".stage/look"), LOOK_PATH);
        assert_eq!(file(".stage/look.splash"), LOOK_PATH);
        assert_eq!(file(".stage/sketch_layout"), LAYOUT_PATH);
        assert!(context.resolver.lookup("", "shared/look").is_err());
    }
}
