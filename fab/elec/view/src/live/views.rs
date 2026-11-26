//! What an edit from outside the electronics editors says to the shell: where it came from,
//! which editors it changed, and the cue that names it. Nothing here runs per frame.

/// An editor whose content an edit changed ([`ElecOutsideEdit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElecEditor {
    Schematic,
    Board,
    Firmware,
    Bench,
}

impl ElecEditor {
    /// The editor that shows a design document, by its name's kind: a circuit or its sheets in the
    /// Schematic, a board's layout on the Board, firmware in the Firmware, a bench in the Bench.
    /// None for the documents no one editor shows (parts, blocks, system links, cables, export
    /// profiles, custom views, requirements: those have their own panel, not an editor of the design).
    pub fn of_doc(name: &str) -> Option<Self> {
        use fab_elec_lang::DocKind as K;
        match K::of_name(name)? {
            K::Circuit | K::Sheets => Some(Self::Schematic),
            K::Board => Some(Self::Board),
            K::Firmware => Some(Self::Firmware),
            K::Bench => Some(Self::Bench),
            K::System | K::Cable | K::Part | K::Blocks | K::Profile | K::View | K::Requirements => None,
        }
    }
}

/// Where an outside edit came from, as far as the app knows: what shows it names only that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditOrigin {
    /// The plate's own files, read again (the plate on the bed reloaded or reopened).
    Disk,
    /// Another writer in the app changed the bed's documents, not the electronics editors.
    Elsewhere,
    /// An AI tool's commit (the engine's `By::Ai`).
    Ai,
    /// A tool call from outside the conversation.
    Agent,
}

impl EditOrigin {
    /// Caller shown in the pane for an electronics tool's edit or flash request.
    pub fn tool(by: fab_elec_design::By) -> Option<Self> {
        use fab_elec_design::By;
        match by {
            By::Ai { .. } => Some(Self::Ai),
            By::Agent { .. } => Some(Self::Agent),
            By::Person | By::Disk => None,
        }
    }

    /// The cue's words for an edit labelled `label`: the origin, as far as it is known, before it.
    pub fn cue(self, label: &str) -> String {
        match self {
            EditOrigin::Ai => format!("✦ AI: {label}"),
            EditOrigin::Agent => format!("Agent: {label}"),
            EditOrigin::Disk => format!("Changed on disk: {label}"),
            EditOrigin::Elsewhere => format!("Changed elsewhere: {label}"),
        }
    }
}

/// An edit that came from outside the app's electronics editors (not a key or drag in them): where
/// it came from, the editors whose content it changed, each once, and where in a firmware document
/// it landed. Sent where the electronics engine takes those documents in; the shell decides what
/// follows (Follow AI, the flashes).
#[derive(Clone, Debug, PartialEq)]
pub struct ElecOutsideEdit {
    pub origin: EditOrigin,
    /// What changed in a history entry's words ("R1 to 22k", "14 parts and 6 tracks"); the cue
    /// puts the origin before it ([`EditOrigin::cue`]). An AI commit's is its own label.
    pub label: String,
    /// Where it landed in the design the views now show: what the flash lights.
    pub sites: Vec<fab_elec_core::Sel>,
    pub editors: Vec<ElecEditor>,
    /// Each changed firmware document and its changed lines (0-based, of the new text).
    pub lines: Vec<(String, std::ops::Range<usize>)>,
}

/// The lines `after` changed against `before`: one 0-based range of `after`'s lines between the
/// texts' common head and tail (a pure deletion gives the line after it, so its place still
/// shows). None when their lines are equal. O(lines).
pub fn changed_lines(before: &str, after: &str) -> Option<std::ops::Range<usize>> {
    let (a, b): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if head == a.len() && head == b.len() {
        return None;
    }
    let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
    let end = b.len() - tail;
    Some(if end > head { head..end } else { head..head + 1 })
}

/// How long an outside edit's cue shows, seconds: whole, then fading over its last [`CUE_FADE`].
pub const CUE_SECONDS: f64 = 4.0;
pub const CUE_FADE: f64 = 1.5;

/// An outside edit's cue (the caption shows it beside the workspaces): its words, then the edits
/// after it from the same origin while it shows, counted.
#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub origin: EditOrigin,
    pub label: String,
    pub more: usize,
    pub at: f64,
}

impl Cue {
    /// `✦ AI: R1 to 22k`, `Changed on disk: R1 to 22k and 4 more`.
    pub fn words(&self) -> String {
        if self.more == 0 { self.origin.cue(&self.label) } else { self.origin.cue(&format!("{} and {} more", self.label, self.more)) }
    }

    /// `edit` heard at `now` (seconds): one from the same origin while `cue` shows counts as one
    /// more, any other starts its own. An edit without words changes nothing.
    pub fn hear(cue: &mut Option<Cue>, edit: &ElecOutsideEdit, now: f64) {
        if edit.label.is_empty() {
            return;
        }
        match cue {
            Some(cue) if cue.origin == edit.origin && now - cue.at < CUE_SECONDS => {
                cue.more += 1;
                cue.at = now;
            }
            _ => *cue = Some(Cue { origin: edit.origin, label: edit.label.clone(), more: 0, at: now }),
        }
    }

    /// How much of it shows at `now`: whole, then fading over its last [`CUE_FADE`] seconds;
    /// none once it has gone.
    pub fn alpha(&self, now: f64) -> Option<f32> {
        let age = now - self.at;
        (age < CUE_SECONDS).then(|| ((CUE_SECONDS - age) / CUE_FADE).clamp(0.0, 1.0) as f32)
    }
}
