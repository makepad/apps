//! The in-flight tool state.
//!
//! Every *result* a tool produces lives in `AppState` — measurements,
//! `scene_state.section`. What lives here is the half-finished interaction:
//! the measurement being placed, the snap under the cursor, a section-handle
//! drag, the section animate-in.
//!
//! It is shared between two owners that never see each other: `ToolSet`
//! (inside the viewport's input chain) and `FabToolOverlay` (the 2D overlay
//! widget). Both run on the UI thread, so this is a thread-local singleton
//! rather than a field. Nothing survives a reload (`reset_for_scene`).

use crate::api::*;
use makepad_widgets_core::*;
use std::cell::RefCell;

/// A measurement being placed: the committed points and the live preview.
#[derive(Clone, Debug, Default)]
pub struct MeasureDraft {
    pub kind: MeasureKind,
    pub points: Vec<Vec3f>,
    /// How each point was snapped (same length as `points`), for the glyphs.
    pub snaps: Vec<SnapKind>,
    pub normals: Vec<Vec3f>,
    /// Where the cursor currently is, snapped. Drives the rubber band.
    pub preview: Option<SnapHit>,
}

impl MeasureDraft {
    pub fn clear(&mut self) {
        self.points.clear();
        self.snaps.clear();
        self.normals.clear();
        self.preview = None;
    }

    pub fn undo(&mut self) {
        self.points.pop();
        self.snaps.pop();
        self.normals.pop();
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// Which draggable thing the pointer is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionHandle {
    /// Offset handle of `section.planes[i]`.
    Plane(usize),
    /// Face `0..6` of the section box: −X +X −Y +Y −Z +Z.
    BoxFace(usize),
}

#[derive(Clone, Copy, Debug)]
pub struct SectionDrag {
    pub handle: SectionHandle,
    /// Screen position where the drag started.
    pub start: DVec2,
    /// Section value at drag start (offset in meters / box face coordinate).
    pub start_value: f32,
    /// Screen-space direction (unit) the handle moves along, and how many
    /// points one meter of movement covers.
    pub axis_screen: DVec2,
    pub points_per_meter: f64,
}

/// An endpoint edit stays local until release, when it becomes one model transaction.
#[derive(Clone, Debug)]
pub struct MeasureDrag {
    pub index: usize,
    pub point: usize,
    pub start: DVec2,
    pub original: Measurement,
    pub preview: Measurement,
    pub plane: Plane,
    pub moved: bool,
}

/// Section animate-in: eases the whole `SectionState` from `from` to `to`.
#[derive(Clone, Debug)]
pub struct SectionAnim {
    pub from: SectionState,
    pub to: SectionState,
    /// 0..1.
    pub t: f32,
    pub duration: f32,
}

#[derive(Clone, Debug, Default)]
pub struct ToolSession {
    pub paint: super::paint::PaintDraft,
    pub sculpt: super::sculpt::SculptDraft,
    pub measure: MeasureDraft,
    pub measure_drag: Option<MeasureDrag>,
    pub fit: super::fit::FitDraft,
    /// Snap under the cursor right now (drives the snap glyph).
    pub snap_cursor: Option<SnapHit>,
    pub section_drag: Option<SectionDrag>,
    pub section_hover: Option<SectionHandle>,
    pub section_anim: Option<SectionAnim>,
    /// One-line tool feedback drawn under the view label.
    pub hint: String,
    /// What a draft showing skipped and what to pick for it (a statement that lost its
    /// selector), or once a pick named it again, what that pick wrote (in a script fn's code,
    /// for every call): drawn where the hint goes while no hint is. The app sets and clears it
    /// with the draft and the next build; a new scene does not.
    pub draft: String,
    /// The area being drawn in the Sliced view (no trees on the bed, print settings on a layer).
    pub area: Option<super::areas::Draft>,
    /// The marker's marks for the AI (not reset by a rebuild: a mark follows its part).
    pub marker: super::annotate::Marker,
}

impl ToolSession {
    /// Wipe everything that refers to a scene (called when one is loaded).
    pub fn reset_for_scene(&mut self) {
        self.paint = Default::default();
        self.sculpt.reset_for_scene();
        self.measure.clear();
        self.measure_drag = None;
        self.fit.clear();
        self.snap_cursor = None;
        self.section_drag = None;
        self.section_hover = None;
        self.section_anim = None;
        self.hint.clear();
        self.area = None;
    }

    /// True while something wants per-frame updates.
    pub fn wants_frames(&self) -> bool {
        self.section_anim.is_some()
    }
}

thread_local! {
    static SESSION: RefCell<ToolSession> = RefCell::new(ToolSession::default());
}

/// Borrow the session. Never nest these calls.
pub fn with<R>(f: impl FnOnce(&mut ToolSession) -> R) -> R {
    SESSION.with(|s| f(&mut s.borrow_mut()))
}
