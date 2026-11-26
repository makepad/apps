//! Voiceovers on the CUT page: lines as blocks on their
//! track, their words edited in the inspector, and takes spoken on the
//! media worker and pinned into the line. The text is the truth: a line
//! whose words or voice changed since its take shows as stale until it is
//! spoken again, and nothing speaks on its own.

use super::media::MediaJob;
use super::ops::CutOp;
use super::VideoCut;
use makepad_story_cut::{AssetRef, CutNodeKind, SourceTime, TakeInfo, VoiceLine, VoiceSpec};

impl VideoCut {
    pub(super) fn is_voice(&self, node: &str) -> bool {
        self.session.as_ref().and_then(|session| session.doc.node(node)).is_some_and(|node| matches!(node.kind, CutNodeKind::VoiceOver { .. }))
    }

    /// A voiceover's voice and lines, as the document has them now.
    pub(super) fn voice_of(&self, node: &str) -> Option<(VoiceSpec, Vec<VoiceLine>)> {
        match &self.session.as_ref()?.doc.node(node)?.kind {
            CutNodeKind::VoiceOver { voice, lines, .. } => Some((voice.clone(), lines.clone())),
            _ => None,
        }
    }

    /// The voiceover line a selection names: a line, or the line a
    /// caption of its subtitles shows.
    pub(super) fn selected_line(&self) -> Option<(String, usize)> {
        let selected = self.selected.as_deref()?;
        let (node, index) = super::split_selection(selected);
        let index = index?;
        if self.is_voice(node) {
            return Some((node.to_string(), index));
        }
        let session = self.session.as_ref()?;
        let CutNodeKind::Subtitles { source: Some(source), .. } = &session.doc.node(node)?.kind else { return None };
        let (eval, _) = self.eval(&session.doc);
        let caption = eval.captions(node).get(index)?.clone();
        let line = eval.voice_lines(&source.node).iter().find(|line| caption.start >= line.start - 1e-6 && caption.start < line.sound_end.max(line.end))?.index;
        Some((source.node.clone(), line))
    }

    /// A line at the playhead: on the selected voiceover, the first one,
    /// or a new one.
    pub(super) fn add_voice_line(&mut self) {
        let t = self.playhead;
        let chosen = self.selected.as_deref().map(|selected| super::split_selection(selected).0.to_string()).filter(|node| self.is_voice(node));
        let existing = chosen.or_else(|| {
            self.session.as_ref()?.doc.nodes.iter().find(|node| matches!(node.kind, CutNodeKind::VoiceOver { .. })).map(|node| node.id.clone())
        });
        match existing.and_then(|node| Some((node.clone(), self.voice_of(&node)?.1))) {
            Some((node, mut lines)) => {
                let at = lines.iter().position(|line| matches!(line.at, SourceTime::Seconds(start) if start > t)).unwrap_or(lines.len());
                lines.insert(at, VoiceLine { text: "New line".into(), at: SourceTime::Seconds(t), take: None, key: None });
                self.selected = Some(format!("{node}#{at}"));
                self.op(CutOp::SetLines { node, lines });
            }
            None => {
                // A voice that is installed here, when the default is not.
                let default = VoiceSpec::default().id;
                let voice = if self.voices.is_empty() || self.voices.contains(&default) { default } else { self.voices[0].clone() };
                self.op(CutOp::AddVoiceOver { start: t, voice });
            }
        }
    }

    pub(super) fn remove_line(&mut self, node: &str, index: usize) {
        let Some((_, mut lines)) = self.voice_of(node) else { return };
        if index >= lines.len() {
            return;
        }
        if lines.len() == 1 {
            self.op(CutOp::Delete { node: node.to_string() });
            return;
        }
        lines.remove(index);
        self.selected = None;
        self.op(CutOp::SetLines { node: node.to_string(), lines });
    }

    /// A line dragged to `start`: seconds move; a marker keeps its name and
    /// moves its offset, so it still follows the run.
    pub(super) fn move_line(&mut self, node: &str, index: usize, start: f64) {
        let Some((_, mut lines)) = self.voice_of(node) else { return };
        let placed = self.session.as_ref().map(|session| {
            let (eval, _) = self.eval(&session.doc);
            eval.voice_lines(node).iter().find(|line| line.index == index).map(|line| line.start)
        }).flatten();
        let Some(line) = lines.get_mut(index) else { return };
        line.at = match &line.at {
            SourceTime::Marker { name, offset } => SourceTime::Marker { name: name.clone(), offset: offset + (start - placed.unwrap_or(start)) },
            SourceTime::Seconds(_) => SourceTime::Seconds(start.max(0.0)),
        };
        self.op(CutOp::SetLines { node: node.to_string(), lines });
    }

    pub(super) fn set_line_text(&mut self, node: &str, index: usize, text: String) {
        let Some((_, mut lines)) = self.voice_of(node) else { return };
        let Some(line) = lines.get_mut(index) else { return };
        if line.text == text || text.trim().is_empty() {
            return;
        }
        line.text = text;
        self.op(CutOp::SetLines { node: node.to_string(), lines });
    }

    /// Speak `indices` of `node` (the stale ones when `None`). `voice`
    /// names another voice for the whole voiceover: it is set first and
    /// every line is spoken in it.
    pub(super) fn speak(&mut self, node: &str, indices: Option<Vec<usize>>, voice: Option<&str>) {
        let Some((mut voice_spec, lines)) = self.voice_of(node) else { return };
        let mut indices = indices;
        if let Some(id) = voice.filter(|id| !id.is_empty() && *id != voice_spec.id) {
            self.set_voice(node, id.to_string());
            voice_spec.id = id.to_string();
            indices = Some((0..lines.len()).collect());
        }
        let voice = voice_spec;
        let wanted = indices.unwrap_or_else(|| (0..lines.len()).filter(|index| lines[*index].stale(&voice)).collect());
        if wanted.is_empty() {
            self.status = "Every line's voice is up to date".into();
        }
        for index in wanted {
            let Some(line) = lines.get(index) else { continue };
            if !self.speaking.insert((node.to_string(), index)) {
                continue;
            }
            self.send_media(MediaJob::Voice { node: node.to_string(), index, voice: voice.clone(), text: line.text.clone() });
            self.status = format!("Speaking “{}”…", line.text);
        }
        self.repaint = true;
    }

    /// A take came back: pin it into its line, when the line still says
    /// what was spoken.
    pub(super) fn spoken(&mut self, node: String, index: usize, text: String, result: Result<(AssetRef, String, TakeInfo), String>) {
        self.speaking.remove(&(node.clone(), index));
        self.repaint = true;
        let (take, key, info) = match result {
            Ok(done) => done,
            Err(error) => {
                self.status = format!("Voice: {error}");
                return;
            }
        };
        self.takes.insert(take.clone(), info);
        let Some((_, mut lines)) = self.voice_of(&node) else { return };
        match lines.get_mut(index).filter(|line| line.text == text) {
            Some(line) => {
                line.take = Some(take);
                line.key = Some(key);
                self.status = format!("Voice ready: “{text}”");
                self.op(CutOp::SetLines { node, lines });
            }
            None => self.status = "The line changed while it was spoken; speak it again".into(),
        }
    }
}

impl VideoCut {
    /// The voiceover's voice, picked from the installed ones: every line
    /// becomes stale until it is spoken again.
    pub(super) fn set_voice(&mut self, node: &str, id: String) {
        let Some((mut voice, _)) = self.voice_of(node) else { return };
        if voice.id == id {
            return;
        }
        voice.id = id;
        self.op(CutOp::Param { node: node.to_string(), key: "voice".into(), value: super::ops::voice_value(&voice) });
    }
}
