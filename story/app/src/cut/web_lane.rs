//! The library's Web lane (WB1): the grabs of YouTube and other page
//! addresses (an in/out range optional) and live streams. They are asked for
//! in the chat (the `web_grab_start` card, chip "Grab from the web…") or by
//! pasting an address on the timeline. The grabs themselves record through
//! a browser root, which Story does not have yet; this page only shows them:
//!
//! - The lane lists the library's finished grabs (Video assets tagged
//!   `web-grab`, ordinary store rows: they drag onto the timeline like any
//!   video) and, after them, the grabs still under way (a progress badge;
//!   they refuse a drag and say why).
//! - The bar over the list (shown only in this lane, derived from the lane
//!   on every library paint) says what the selected grab is doing and
//!   stops it.
//! - A URL pasted on the timeline (⌘V) grabs it and lands the clip at the
//!   playhead when it is done.
//! - Live streams on the timeline block a render (it is locked-time): the
//!   chat says so and offers a chip to grab them ([`VideoCut::stream_blockers`]).

use makepad_library_browser::list::{CardKind, ListRow};
use super::{PageLane, VideoCut};
use makepad_story_cut::{CutNodeKind, MediaRef};
use makepad_widgets::*;

/// A grab as this page shows it (filled in by the grabs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WebJobRow {
    pub job: u64,
    pub title: String,
    pub detail: String,
    /// 0..1 when known.
    pub progress: Option<f64>,
    pub recording: bool,
    pub stream: bool,
    pub ended: bool,
    pub failed: Option<String>,
    /// The clip, once there is one (a stream: its newest snapshot).
    pub asset: Option<(String, String)>,
    pub seconds: Option<f64>,
}

/// What the page asks of the grabs.
#[derive(Clone, Debug, PartialEq)]
pub enum WebRequest {
    Grab { url: String, range: String, land_at: Option<f64> },
    /// Stop a grab (its pieces are dropped) or freeze a stream (kept).
    Stop(u64),
}

#[derive(Default)]
pub struct WebLane {
    pub jobs: Vec<WebJobRow>,
    /// Streams still live: `(asset id, job)`.
    pub live: Vec<(String, u64)>,
    pub requests: Vec<WebRequest>,
    /// This build has no browser: the lane says so.
    pub unavailable: Option<String>,
    note: String,
}

/// Whether pasted text looks like a web address worth handing to the grabs
/// (they parse it for real and say why when it is not one).
pub fn looks_like_url(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && !text.contains(char::is_whitespace) && (text.contains("://") || text.starts_with("youtu.be/") || text.starts_with("www.") || text.starts_with("youtube.com/"))
}

impl VideoCut {
    /// The grabs under way, as rows after the store's (they refuse drags).
    pub(super) fn web_rows(&self) -> Vec<ListRow> {
        if self.lib.lane != Some(PageLane::Web) {
            return Vec::new();
        }
        self.web.jobs.iter().filter(|job| !job.ended || job.failed.is_some()).map(|job| {
            let badge = match (&job.failed, job.stream, job.progress) {
                (Some(_), _, _) => ("FAILED".to_string(), 0x3a2a2aff, 0xd98b8bff),
                (None, true, _) => ("LIVE".to_string(), 0x5a1f24ff, 0xffb4b4ff),
                (None, false, Some(progress)) => (format!("{:.0}%", progress * 100.0), 0x1f2f4aff, 0xb4d0ffff),
                (None, false, None) => ("REC".to_string(), 0x1f2f4aff, 0xb4d0ffff),
            };
            ListRow {
                title: job.title.clone(),
                detail: job.failed.clone().unwrap_or_else(|| job.detail.clone()),
                kind: CardKind::Video,
                seconds: job.seconds,
                badge: Some(badge),
                ..ListRow::default()
            }
        }).collect()
    }

    /// The job of a row after the store's rows, if it is one.
    pub(super) fn web_job_at(&self, index: usize) -> Option<&WebJobRow> {
        if self.lib.lane != Some(PageLane::Web) {
            return None;
        }
        let at = index.checked_sub(self.lib.rows.len())?;
        self.web.jobs.iter().filter(|job| !job.ended || job.failed.is_some()).nth(at)
    }

    /// Why a grab row cannot be dropped on the timeline yet.
    pub(super) fn web_drop_refusal(&self, index: usize) -> Option<String> {
        let job = self.web_job_at(index)?;
        Some(match (&job.failed, job.stream) {
            (Some(why), _) => format!("This grab failed: {why}"),
            (None, true) => "A live stream: Snapshot it from the chat to drop what it has so far, or Stop to keep it as a clip".into(),
            (None, false) => "Still grabbing: it drops onto the timeline once it is done".into(),
        })
    }

    /// The Web bar: shown only in this lane (from the lane, every paint).
    pub(super) fn paint_web_bar(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let web = self.lib.lane == Some(PageLane::Web);
        ui.view(cx, ids!(cut_web_bar)).set_visible(cx, web);
        if !web {
            return;
        }
        let selected = self.lib.list_selected.and_then(|index| self.web_job_at(index)).cloned();
        let note = match (&self.web.unavailable, &selected) {
            (Some(why), _) => why.clone(),
            (None, Some(job)) => format!("{} · {}", job.title, job.failed.clone().unwrap_or_else(|| job.detail.clone())),
            (None, None) if !self.web.note.is_empty() => self.web.note.clone(),
            (None, None) => "Grabs record in real time in a background tab of Stage's browser · protected (DRM) video is refused".into(),
        };
        ui.label(cx, ids!(cut_web_note)).set_text(cx, &note);
        let stop = selected.as_ref().is_some_and(|job| !job.ended);
        ui.widget(cx, ids!(cut_web_stop)).set_disabled(cx, !stop);
        ui.button(cx, ids!(cut_web_stop)).set_text(cx, if selected.as_ref().is_some_and(|job| job.stream) { "Stop (keep)" } else { "Stop" });
    }

    pub(super) fn handle_web_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        if ui.button(cx, ids!(cut_web_stop)).clicked(actions) {
            if let Some(job) = self.lib.list_selected.and_then(|index| self.web_job_at(index)).map(|job| job.job) {
                self.web.requests.push(WebRequest::Stop(job));
            }
        }
    }

    /// Text pasted on the timeline: a web address becomes a grab that
    /// lands at the playhead. False when it is not an address.
    pub(super) fn paste_web_url(&mut self, text: &str) -> bool {
        if !looks_like_url(text) {
            return false;
        }
        self.web.requests.push(WebRequest::Grab { url: text.trim().to_string(), range: String::new(), land_at: Some(self.playhead) });
        self.status = format!("Grabbing {} · it lands here when it is done (Web lane shows progress)", text.trim());
        self.repaint = true;
        true
    }

    /// Clips on the timeline that show a live stream (node, job): an
    /// export refuses while any remain.
    pub(super) fn stream_blockers(&self) -> Vec<(String, u64)> {
        let Some(session) = &self.session else { return Vec::new() };
        let clips: Vec<(String, String)> = session.doc.nodes.iter().filter_map(|node| match &node.kind {
            CutNodeKind::Clip { src: MediaRef::Asset(asset), .. } => Some((node.id.clone(), asset.id.clone())),
            _ => None,
        }).collect();
        let live = &self.web.live;
        clips.into_iter().filter_map(|(node, asset)| live.iter().find(|(id, _)| *id == asset).map(|(_, job)| (node, *job))).collect()
    }
}

/// What the chat says while live streams block a render.
pub fn stream_reason(count: usize) -> String {
    match count {
        1 => "1 live web stream is on the timeline. Exports are locked-time: grab it into a clip first.".into(),
        n => format!("{n} live web streams are on the timeline. Exports are locked-time: grab them into clips first."),
    }
}

#[cfg(test)]
mod tests {
    use super::looks_like_url;

    #[test]
    fn pasted_addresses_are_recognised() {
        for url in ["https://www.youtube.com/watch?v=Ghdj3BOjgh4", "youtu.be/Ghdj3BOjgh4", "www.youtube.com/shorts/x", "http://example.com/v.mp4"] {
            assert!(looks_like_url(url), "{url}");
        }
        for text in ["", "Title text", "hello world", "V1"] {
            assert!(!looks_like_url(text), "{text}");
        }
    }
}
