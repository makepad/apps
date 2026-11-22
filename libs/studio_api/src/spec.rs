//! What a tool is: its name, what it tells the model, its argument and
//! result schemas, and whether it changes the cut.

use makepad_strict_json::Value;

/// The families of CUTS.md §10.4 (plus the session and effect-authoring
/// tools), in the order the tool list shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// What is open, the change feed, undo.
    Session,
    /// Project / edit: the open cut's source, its timeline and its ops.
    Cut,
    /// Motion documents and vector graphics (§10.2).
    Motion,
    /// VJ effects and transitions as documents: list, author, apply.
    Effects,
    /// The library and generation on the AI Hub through FLOW presets.
    Assets,
    /// App runs (§6): registry apps, dumps, scripts, marks.
    AppRuns,
    /// Music analysis, beat sync, voiceovers (§8, §10.3).
    Audio,
    /// Looking at the result: inspect, frames, strips (§10.2).
    Feedback,
    /// The storyboard the cut is made from: its cards, notes and
    /// references, and updating the cut when it changes.
    Storyboard,
    /// Scores: live-coded music the SCORE page plays, the AI composes and
    /// edits, and CUT and games use.
    Music,
}

impl Family {
    pub const ALL: [Family; 10] = [Family::Session, Family::Storyboard, Family::Cut, Family::Motion, Family::Effects, Family::Assets, Family::AppRuns, Family::Audio, Family::Music, Family::Feedback];

    pub fn label(self) -> &'static str {
        match self {
            Family::Session => "Session",
            Family::Cut => "Project / cut",
            Family::Motion => "Motion / vector",
            Family::Effects => "Effects and transitions",
            Family::Assets => "Assets / generation",
            Family::AppRuns => "App runs",
            Family::Audio => "Audio",
            Family::Feedback => "Feedback",
            Family::Storyboard => "Storyboard",
            Family::Music => "Music / scores",
        }
    }
}

/// One tool as every front end describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSpec {
    /// MCP-legal: lowercase letters, digits and `_`.
    pub name: &'static str,
    pub family: Family,
    /// Written for a model: what it does, when to use it, what comes back.
    pub description: &'static str,
    /// JSON schema of the arguments (an object).
    pub input: &'static str,
    /// JSON schema of the result's JSON part.
    pub output: &'static str,
    /// It changes the open cut (or opens another): one undoable step in
    /// the CUT page, autosaved, and a diff event.
    pub mutates: bool,
    /// It returns pictures (PNG) besides its JSON.
    pub images: bool,
    /// The lane or API that backs it, named in the "not available yet"
    /// error while Stage has no implementation registered.
    pub backed_by: &'static str,
    /// It makes something (a video, a render, a batch, an export, a new
    /// document): the chat parks the call as a settings card the person
    /// edits and confirms. Every request has a [`Layout`].
    pub request: bool,
    /// How a request's settings card is presented; `None` for the tools
    /// the chat runs without a card.
    pub layout: Option<Layout>,
}

/// How a request's settings card is presented. Anything not listed gets the
/// defaults (the title from the tool's name, the fields in schema order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub title: &'static str,
    pub confirm: &'static str,
    /// Arguments shown first, in this order, with their labels.
    pub order: &'static [(&'static str, &'static str)],
    /// Arguments the card does not show (passed through if prefilled).
    pub hide: &'static [&'static str],
}

impl ToolSpec {
    pub fn input_schema(&self) -> Value {
        crate::schema::parse(self.input).unwrap_or_else(|error| panic!("{}: input schema: {error}", self.name))
    }

    /// It reports `errors` and `warnings` about a document (the Generate
    /// loop's gate before `finish`).
    pub fn inspects(&self) -> bool {
        self.name == "inspect"
    }

    pub fn output_schema(&self) -> Value {
        crate::schema::parse(self.output).unwrap_or_else(|error| panic!("{}: output schema: {error}", self.name))
    }
}
