//! KineticText: a `KineticText{preset: "<name>"}` draws its kit through the
//! kinetic text pack the host installed (makepad-content-packs; without one
//! the layer reports it needs the pack); `text` or `source: voice(@vo)` gives
//! the words, `p1..p4` the kit's first four dials, `font`/`italic` a document
//! font in place of the kit's own, `wdth wght slnt` the font's variable axes.

use super::MotionDraw;
use makepad_audio_reactive::{AudioFrame, AudioTrack, TrackPicture};
use makepad_content_packs::text::{self, TextFrame, TextView};
use makepad_motion::frame::{Affine, Item, ItemKind};
use makepad_motion::model::ShaderProgram;
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct Kinetics {
    /// Each KineticText layer's kit, by layer path (`Err`: no text pack).
    views: HashMap<String, Result<Box<dyn TextView>, String>>,
    /// The songs' analysed pictures by music id, on the host's clock
    /// ([`MotionDraw::set_audio_tracks`]), and the textures showing them.
    tracks: HashMap<String, Arc<AudioTrack>>,
    pictures: HashMap<String, TrackPicture>,
}

impl Kinetics {
    pub fn errors(&self) -> impl Iterator<Item = String> + '_ {
        self.views.iter().filter_map(|(path, view)| match view {
            Ok(view) => view.error().map(|e| format!("{path}: {e}")),
            Err(e) => Some(format!("{path}: {e}")),
        })
    }
}

fn tag(t: &str) -> u32 {
    let b = t.as_bytes();
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

impl MotionDraw {
    /// THE SONGS' PICTURES: each music id's analysis (makepad-audio-reactive
    /// `AudioTrack`, on the host's clock: an edit's, where its clip plays the
    /// track). A kit (and a vj_fx effect) then reads the sound at the
    /// frame's own time, `sound` of its item: the same picture however the
    /// frame is reached, so a locked-time render is deterministic. Without
    /// one the kits hear only the four levels.
    pub fn set_audio_tracks(&mut self, tracks: HashMap<String, Arc<AudioTrack>>) {
        let k = &mut self.shaders.kinetic;
        k.pictures.retain(|id, _| tracks.contains_key(id));
        k.tracks = tracks;
    }

    /// The sound of `item` at its moment, when its music has a track.
    pub(super) fn audio_frame(&mut self, cx: &mut Cx, item: &Item) -> Option<AudioFrame> {
        let ItemKind::Shader { sound: (music, at), .. } = &item.kind else { return None };
        let k = &mut self.shaders.kinetic;
        // A document that names no music hears the song playing then (the
        // first by id, so the choice never depends on map order).
        let id = if k.tracks.contains_key(music) {
            music.clone()
        } else {
            let mut ids: Vec<&String> = k.tracks.iter().filter(|(_, t)| t.covers(*at)).map(|(id, _)| id).collect();
            ids.sort();
            ids.first()?.to_string()
        };
        let track = k.tracks.get(&id)?.clone();
        Some(k.pictures.entry(id).or_default().frame(cx, &track, *at))
    }

    /// `item`'s KineticText on its kit, drawn into a target of `px`:
    /// `None` when `item` is no KineticText, `Some(None)` when its kit is
    /// missing or failed (the error is reported).
    pub(super) fn kinetic_effect(&mut self, cx: &mut Cx2d, item: &Item, input: &Texture, px: (u32, u32)) -> Option<Option<Texture>> {
        let ItemKind::Shader { program: ShaderProgram::Preset(name), params, local_time, beat, text: Some(words), font, audio, below, .. } = &item.kind else { return None };
        let k = &mut self.shaders.kinetic;
        let view = k.views.entry(item.path.clone()).or_insert_with(|| {
            let pack = text::pack_of(name).or_else(|| makepad_content_packs::text_packs().into_iter().next());
            pack.map(|pack| pack.view(name)).ok_or_else(|| text::missing(name))
        });
        let Ok(view) = view else { return Some(None) };
        if !view.ready(cx.cx) {
            return Some(None);
        }
        let max_dials = text::pack_of(name).map_or(0, |pack| pack.max_dials());
        let mut dials = vec![None; max_dials];
        let (mut weight, mut axes, mut overlay) = (None, Vec::new(), None);
        for (name, value) in params {
            if let Some(k) = name.strip_prefix('p').and_then(|n| n.parse::<usize>().ok()).filter(|k| (1..=dials.len()).contains(k)) {
                dials[k - 1] = (*value >= 0.0).then_some(*value as f32);
            }
            // Axes are stepped (whole units) so a keyed axis rebuilds the
            // letters at most once per unit.
            match name.as_str() {
                "backdrop" => overlay = Some(*value < 0.5),
                "wght" => weight = Some(value.round() as f32),
                "wdth" | "slnt" => axes.push((tag(name), value.round() as f32)),
                _ => {}
            }
        }
        if let Some(overlay) = overlay {
            view.set_overlay(cx.cx, overlay);
        }
        view.set_axes(cx.cx, weight, &axes);
        // A document font in place of the kit's: its file at the weight the
        // letters are built at (a static family's nearest face).
        let italic = params.iter().any(|(name, value)| name == "italic" && *value >= 0.5);
        let at_weight = view.shape_weight(cx.cx).map_or(400.0, f64::from);
        view.set_font(cx.cx, font.as_deref().and_then(|font| makepad_doc_fonts::font_file(font, at_weight, italic)));
        let t = *local_time as f32;
        // The text arrived `since` seconds ago: a function of time, so a
        // seek lands on the same frame.
        view.set_text(cx.cx, &words.text, (*local_time - words.since) as f32);
        let bpm = if *local_time > 0.0 && *beat > 0.0 { (*beat / *local_time * 60.0) as f32 } else { 120.0 };
        let sound = self.audio_frame(cx.cx, item);
        let frame = TextFrame {
            time: t,
            beat: *beat as f32,
            bpm,
            energy: audio[0].clamp(0.0, 1.0) as f32,
            bands: [audio[1].clamp(0.0, 1.0) as f32, audio[2].clamp(0.0, 1.0) as f32, audio[3].clamp(0.0, 1.0) as f32],
            dials,
            karaoke: words.voice.then_some(words.progress as f32),
            content: below.then(|| input.clone()),
            audio: sound,
        };
        self.note_kind(&format!("kinetic {}", item.path));
        let index = self.target(cx, px, "motion_kinetic");
        let target = &mut self.pool[index];
        target.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
        let Some(Ok(view)) = self.shaders.kinetic.views.get_mut(&item.path) else { return Some(None) };
        let output = view.render(cx, px, &frame);
        let first = view.scene_pass();
        if let Some(texture) = &output {
            let full = [px.0 as f64, px.1 as f64];
            self.draws.quad.quad(cx, texture, None, &Affine::IDENTITY, full, [0.0, 0.0, 1.0, 1.0], 1.0);
        }
        let target = &mut self.pool[index];
        target.end(cx);
        let id = target.pass.draw_pass_id();
        self.spans.push((first, id));
        self.passes.push(id);
        self.shaders.rendered.push(item.path.clone());
        Some(Some(self.pool[index].texture.clone()))
    }
}
