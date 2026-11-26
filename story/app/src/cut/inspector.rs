//! The CUT page's inspector (design §3.5, §4.4): a header (the role chip,
//! the name, the kind line) and folding sections for what the selection
//! is. With nothing selected it is the Project inspector: EDIT (format,
//! rate, duration, tempo, the viewer's safe zone), MOBILE for vertical
//! edits, and a SUMMARY of what the cut holds and what is stale. For a
//! block: TIMING (start, duration, source in as timecode; the track as a
//! stepper), LOOK (preset or text, fit and subtitle style as segmented
//! controls, the animatable picture values with key navigation), AUDIO
//! (volume in dB, pan L/C/R, fades, ducking as a switch with its depth),
//! LINE and VOICE for a voiceover line, KEYFRAMES and SOURCE.
//!
//! Values are right-aligned in their units and scrub when dragged; a field
//! writes when let go, typed or stepped, each as one cut. An animatable
//! value that has keys writes the key at the playhead (a new key when there
//! is none there); the diamond adds or removes that key, the arrows go to
//! the previous and next key.

use super::controls::{CutScrub, CutSegmented, CutSwitch, Format, ScrubAction, SegmentAction, SwitchAction};
use super::ops::CutOp;
use super::patch::Value;
use super::project::preset_label;
use makepad_library_browser::lanes::LibLane;
use super::timeline::{timecode_full};
use makepad_library_browser::draw::{CutRole};
use super::{split_selection, PageLane, VideoCut};
use makepad_flow::Literal;
use makepad_story_cut::reframe::SafeZone;
use makepad_story_cut::{CutNodeKind, EffectRef, Fit, MediaRef, SourceTime};
use makepad_widgets::*;

/// Sections: container, header, body, open at first.
const SECTIONS: [(LiveId, LiveId, LiveId, bool); 9] = [
    (live_id!(cut_s_project), live_id!(cut_h_project), live_id!(cut_b_project), true),
    (live_id!(cut_s_summary), live_id!(cut_h_summary), live_id!(cut_b_summary), true),
    (live_id!(cut_s_voice), live_id!(cut_h_voice), live_id!(cut_b_voice), true),
    (live_id!(cut_s_speak), live_id!(cut_h_speak), live_id!(cut_b_speak), true),
    (live_id!(cut_s_timing), live_id!(cut_h_timing), live_id!(cut_b_timing), true),
    (live_id!(cut_s_look), live_id!(cut_h_look), live_id!(cut_b_look), true),
    (live_id!(cut_s_audio), live_id!(cut_h_audio), live_id!(cut_b_audio), true),
    (live_id!(cut_s_keys), live_id!(cut_h_keys), live_id!(cut_b_keys), false),
    (live_id!(cut_s_source), live_id!(cut_h_source), live_id!(cut_b_source), false),
];

/// Parameter rows: row, name, value, key navigation. Rows 0–3 are LOOK's,
/// 4–5 AUDIO's.
const ROWS: [(LiveId, LiveId, LiveId, LiveId); 6] = [
    (live_id!(cut_r0), live_id!(cut_n0), live_id!(cut_v0), live_id!(cut_k0)),
    (live_id!(cut_r1), live_id!(cut_n1), live_id!(cut_v1), live_id!(cut_k1)),
    (live_id!(cut_r2), live_id!(cut_n2), live_id!(cut_v2), live_id!(cut_k2)),
    (live_id!(cut_r3), live_id!(cut_n3), live_id!(cut_v3), live_id!(cut_k3)),
    (live_id!(cut_r4), live_id!(cut_n4), live_id!(cut_v4), live_id!(cut_k4)),
    (live_id!(cut_r5), live_id!(cut_n5), live_id!(cut_v5), live_id!(cut_k5)),
];

const TIMING_FIELDS: [LiveId; 4] = [live_id!(cut_start), live_id!(cut_length), live_id!(cut_in), live_id!(cut_track)];

const ROLE_ICONS: [LiveId; 10] = [
    live_id!(cut_role_video), live_id!(cut_role_image), live_id!(cut_role_app), live_id!(cut_role_title), live_id!(cut_role_fx),
    live_id!(cut_role_trans), live_id!(cut_role_audio), live_id!(cut_role_voice), live_id!(cut_role_caps), live_id!(cut_role_project),
];

const SAFE_ZONES: [SafeZone; 4] = [SafeZone::None, SafeZone::Reels, SafeZone::TikTok, SafeZone::Shorts];

/// A parameter's keys as written: `(t, v, ease)`, or `None` for a
/// constant.
fn key_points(literal: &Literal) -> Option<Vec<(f64, f64, Option<String>)>> {
    let Literal::Arr(points) = literal else { return None };
    if points.is_empty() {
        return None;
    }
    points.iter().map(|point| match point {
        Literal::Arr(items) => match items.as_slice() {
            [Literal::Num(t), Literal::Num(v), rest @ ..] => Some((*t, *v, rest.iter().find_map(|item| match item {
                Literal::Id(ease) if ease != "pin" => Some(ease.clone()),
                _ => None,
            }))),
            _ => None,
        },
        _ => None,
    }).collect()
}

/// A value at `local` seconds: its constant, or its keys linearly between.
fn value_at(literal: &Literal, local: f64, default: f64) -> f64 {
    match literal {
        Literal::Num(value) => *value,
        literal => match key_points(literal) {
            Some(points) => {
                let next = points.partition_point(|point| point.0 <= local);
                match (next.checked_sub(1).map(|index| &points[index]), points.get(next)) {
                    (Some(a), Some(b)) => a.1 + (b.1 - a.1) * ((local - a.0) / (b.0 - a.0)).clamp(0.0, 1.0),
                    (Some(a), None) => a.1,
                    (None, Some(b)) => b.1,
                    (None, None) => default,
                }
            }
            None => default,
        },
    }
}

fn keys_value(points: &[(f64, f64, Option<String>)]) -> Value {
    let points = points.iter().map(|(t, v, ease)| (*t, *v, ease.as_deref())).collect::<Vec<_>>();
    Value::keys(&points)
}

/// How a parameter row shows its value.
fn format_of(key: &str) -> Format {
    match key {
        "opacity" => Format::Percent,
        "gain" => Format::Decibel,
        "pan" => Format::Pan,
        "font_size" => Format::Pixels,
        _ => Format::Dial,
    }
}

fn scrub(cx: &mut Cx, ui: &WidgetRef, id: &[LiveId], format: Format, min: f64, max: f64, value: f64) {
    if let Some(mut field) = ui.widget(cx, id).borrow_mut::<CutScrub>() {
        field.configure(cx, format, min, max);
        field.set_value(cx, value);
        field.set_keyed(cx, false);
        field.set_disabled(cx, false);
    }
}

fn scrub_commits(cx: &mut Cx, ui: &WidgetRef, id: &[LiveId], actions: &Actions) -> Option<f64> {
    let field = ui.widget(cx, id);
    actions.filter_widget_actions_cast::<ScrubAction>(field.widget_uid()).filter_map(|action| match action {
        ScrubAction::Commit(value) => Some(value),
        _ => None,
    }).last()
}

impl VideoCut {
    /// The graph literal of `key` on `node`.
    fn literal(&self, node: &str, key: &str) -> Option<Literal> {
        let graph_node = self.session.as_ref()?.doc.graph.nodes.iter().find(|graph_node| graph_node.id == node)?;
        graph_node.params.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone())
    }

    /// Seconds from the node's span start at the playhead: the time its
    /// keys are written in.
    fn local(&self, node: &str) -> f64 {
        let start = self.session.as_ref().and_then(|session| session.doc.node(node)).and_then(|node| node.span).map_or(0.0, |span| span.start);
        let fps = self.fps().max(1.0);
        ((self.playhead - start) * fps).round() / fps
    }

    /// The parameter rows the node shows: `(key, label, min, max, default)`.
    fn param_rows(kind: &CutNodeKind) -> [Option<(&'static str, &'static str, f64, f64, f64)>; 6] {
        let mut rows = [None; 6];
        match kind {
            CutNodeKind::Clip { .. } => {
                rows[0] = Some(("opacity", "Opacity", 0.0, 1.0, 1.0));
                rows[4] = Some(("gain", "Volume", 0.0, 4.0, 1.0));
                rows[5] = Some(("pan", "Pan", -1.0, 1.0, 0.0));
            }
            CutNodeKind::Still { .. } => rows[0] = Some(("opacity", "Opacity", 0.0, 1.0, 1.0)),
            CutNodeKind::Title { .. } | CutNodeKind::Subtitles { .. } => rows[0] = Some(("font_size", "Size", 8.0, 400.0, 64.0)),
            CutNodeKind::Fx { .. } => {
                for (index, (key, label)) in [("p1", "Dial 1"), ("p2", "Dial 2"), ("p3", "Dial 3"), ("p4", "Dial 4")].into_iter().enumerate() {
                    rows[index] = Some((key, label, -1.0, 1.0, -1.0));
                }
            }
            CutNodeKind::Transition { .. } => {
                for (index, (key, label)) in [("p1", "Dial 1"), ("p2", "Dial 2"), ("p3", "Dial 3")].into_iter().enumerate() {
                    rows[index] = Some((key, label, -1.0, 1.0, -1.0));
                }
            }
            CutNodeKind::AudioClip { .. } | CutNodeKind::VoiceOver { .. } => {
                rows[4] = Some(("gain", "Volume", 0.0, 4.0, 1.0));
                rows[5] = Some(("pan", "Pan", -1.0, 1.0, 0.0));
            }
            _ => {}
        }
        rows
    }

    /// The header: the role chip (its strip colour and kind glyph), the
    /// name, and the kind line.
    fn header(&self, cx: &mut Cx, ui: &WidgetRef, role: Option<(LiveId, u32)>, name: &str, kind: &str) {
        let p = makepad_look::palette::get(cx);
        let (icon, strip) = role.unwrap_or((live_id!(cut_role_project), p.raised));
        for id in ROLE_ICONS {
            ui.widget(cx, &[id]).set_visible(cx, id == icon);
        }
        let mut chip = ui.widget(cx, ids!(cut_role_chip));
        let color = Vec4f::from_u32(strip);
        script_apply_eval!(cx, chip, {draw_bg +: {color: #(color)}});
        ui.label(cx, ids!(cut_node)).set_text(cx, name);
        ui.label(cx, ids!(cut_node_kind)).set_text(cx, kind);
        if let Some(mut tip) = ui.widget(cx, ids!(cut_node_tip)).borrow_mut::<makepad_widgets::tip::Tip>() {
            tip.text = if name.chars().count() > 28 { name.to_string() } else { String::new() };
        }
    }

    fn show_sections(&mut self, cx: &mut Cx, ui: &WidgetRef, shown: &[LiveId]) {
        for (container, head, body, _) in SECTIONS {
            let open = self.sections.get(&head).copied().unwrap_or(true);
            ui.view(cx, &[container]).set_visible(cx, shown.contains(&container));
            ui.view(cx, &[body]).set_visible(cx, open);
            ui.view(cx, &[head, live_id!(open)]).set_visible(cx, open);
            ui.view(cx, &[head, live_id!(closed)]).set_visible(cx, !open);
        }
    }

    /// A Motion graphic selected (in the tree or on the timeline): its
    /// name, its timing, and its document with the inputs the cut gives it.
    fn graphic_inspector(&mut self, cx: &mut Cx, ui: &WidgetRef, graphic: &makepad_story_cut::GraphicNode) {
        let Some(session) = &self.session else { return };
        let fps = session.doc.output.fps;
        let duration = session.doc.duration;
        let label = self.projection.blocks.iter().find(|block| block.node == graphic.id).map_or_else(|| graphic.id.clone(), |block| block.label.clone());
        self.header(cx, ui, Some((live_id!(cut_role_title), CutRole::TITLE.strip)), &label, &format!("Graphic · V{}", graphic.track + 1));
        let span = graphic.span.unwrap_or(makepad_story_cut::Span { start: 0.0, length: duration });
        scrub(cx, ui, ids!(cut_start), Format::Time { fps }, 0.0, 3600.0, span.start);
        scrub(cx, ui, ids!(cut_length), Format::Time { fps }, 1.0 / fps.max(1.0), 3600.0, span.length);
        scrub(cx, ui, ids!(cut_track), Format::Track { letter: 'V' }, 0.0, 15.0, graphic.track as f64);
        ui.view(cx, ids!(cut_track_row)).set_visible(cx, true);
        ui.view(cx, ids!(cut_in_row)).set_visible(cx, false);
        // SOURCE: the document, and the inputs the cut sets on it.
        let document = match &graphic.src {
            makepad_story_cut::GraphicSrc::Asset(asset) => format!("Motion {} · revision {}…", asset.id, &asset.rev[..asset.rev.len().min(12)]),
            makepad_story_cut::GraphicSrc::Builtin(name) => format!("Built-in {name}"),
            makepad_story_cut::GraphicSrc::Tree(tree) => tree.document.clone(),
        };
        let inputs = graphic.params.iter().map(|(name, value)| format!("{name}: {}", input_text(value))).collect::<Vec<_>>();
        let fit = match graphic.fit { Fit::Fill => "fill", Fit::Fit => "fit", Fit::None => "1:1" };
        let mut detail = vec![document, format!("Sits in the frame: {fit}")];
        detail.extend(inputs);
        ui.label(cx, ids!(cut_src_title)).set_text(cx, &label);
        ui.label(cx, ids!(cut_src_detail)).set_text(cx, &detail.join("\n"));
        ui.button(cx, ids!(cut_reveal)).set_text(cx, "Open graphic");
        let mut shown = Vec::new();
        if graphic.span.is_some() {
            shown.push(live_id!(cut_s_timing));
        }
        shown.push(live_id!(cut_s_source));
        self.show_sections(cx, ui, &shown);
    }

    /// Nothing selected: the cut itself.
    fn project_inspector(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(session) = &self.session else {
            self.header(cx, ui, None, "No cut open", "Choose a format to start");
            self.show_sections(cx, ui, &[]);
            return;
        };
        let doc = session.doc.clone();
        let fps = doc.output.fps;
        let (w, h) = (doc.output.width, doc.output.height);
        let aspect = super::aspect_name(w, h);
        self.header(cx, ui, None, &doc.label, &format!("Edit · {aspect} · {w}×{h}"));
        ui.label(cx, ids!(cut_p_format)).set_text(cx, &format!("{aspect} · {w}×{h}"));
        {
            use super::rates;
            let preset = rates::preset(fps).filter(|_| !self.fps_custom);
            ui.drop_down(cx, ids!(cut_p_fps)).set_selected_item(cx, preset.unwrap_or(rates::RATES.len()));
            ui.view(cx, ids!(cut_p_fps_box)).set_visible(cx, preset.is_none());
            let custom = ui.text_input(cx, ids!(cut_p_fps_custom));
            if preset.is_none() && custom.text().is_empty() {
                custom.set_text(cx, &rates::label(fps));
            }
        }
        scrub(cx, ui, ids!(cut_p_duration), Format::Time { fps }, 1.0 / fps.max(1.0), 3600.0, doc.duration);
        scrub(cx, ui, ids!(cut_p_bpm), Format::Pixels, 20.0, 300.0, doc.bpm);
        if let Some(mut field) = ui.widget(cx, ids!(cut_p_bpm)).borrow_mut::<CutScrub>() {
            field.configure(cx, Format::Bpm, 20.0, 300.0);
        }
        let platform = self.platform();
        if let Some(mut segmented) = ui.widget(cx, ids!(cut_p_safe)).borrow_mut::<CutSegmented>() {
            segmented.set_active(cx, SAFE_ZONES.iter().position(|zone| *zone == platform).unwrap_or(0));
        }
        // SUMMARY: counts per kind, and what is stale or missing.
        let mut counts: Vec<(&str, usize)> = Vec::new();
        let mut stale = Vec::new();
        for node in &doc.nodes {
            let noun = match &node.kind {
                CutNodeKind::Clip { src: MediaRef::Run(_), .. } => "app clips",
                CutNodeKind::Clip { nested: true, .. } => "compound clips",
                CutNodeKind::Clip { .. } => "video clips",
                CutNodeKind::Still { .. } => "stills",
                CutNodeKind::Title { .. } => "titles",
                CutNodeKind::Fx { .. } => "effects",
                CutNodeKind::Transition { .. } => "transitions",
                CutNodeKind::AudioClip { .. } => "sounds",
                CutNodeKind::VoiceOver { voice, lines, .. } => {
                    let n = lines.iter().filter(|line| line.stale(voice)).count();
                    if n > 0 {
                        stale.push(format!("● {n} voiceover line{} to speak", if n == 1 { "" } else { "s" }));
                    }
                    "voiceover lines"
                }
                CutNodeKind::Subtitles { .. } => "subtitle tracks",
                _ => continue,
            };
            let add = match &node.kind {
                CutNodeKind::VoiceOver { lines, .. } => lines.len(),
                _ => 1,
            };
            match counts.iter_mut().find(|(name, _)| *name == noun) {
                Some((_, n)) => *n += add,
                None => counts.push((noun, add)),
            }
        }
        // Graphics and generators live beside `nodes` in the loaded cut.
        if !doc.graphics.is_empty() { counts.push(("graphics", doc.graphics.len())); }
        if !doc.generates.is_empty() { counts.push(("generative segments", doc.generates.len())); }
        let broken = self.projection.blocks.iter().filter(|block| block.broken).count();
        if broken > 0 {
            stale.push(format!("● {broken} clip{} with removed media", if broken == 1 { "" } else { "s" }));
        }
        // A key/value list, then what is stale as a link that selects it.
        let noun = |name: &str| {
            let mut chars = name.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        };
        // Keys left, values right-aligned to the field edge.
        let mut pairs: Vec<(String, String)> = counts.iter().map(|(name, n)| (noun(name), n.to_string())).collect();
        if !pairs.is_empty() {
            pairs.push(("Length".into(), timecode_full(doc.duration, fps)));
        }
        const SUM_ROWS: [LiveId; 13] = [live_id!(cut_sum_0), live_id!(cut_sum_1), live_id!(cut_sum_2), live_id!(cut_sum_3), live_id!(cut_sum_4), live_id!(cut_sum_5), live_id!(cut_sum_6), live_id!(cut_sum_7), live_id!(cut_sum_8), live_id!(cut_sum_9), live_id!(cut_sum_10), live_id!(cut_sum_11), live_id!(cut_sum_12)];
        for (index, id) in SUM_ROWS.into_iter().enumerate() {
            let pair = pairs.get(index);
            ui.view(cx, &[id]).set_visible(cx, pair.is_some());
            if let Some((key, value)) = pair {
                ui.label(cx, &[id, live_id!(key)]).set_text(cx, key);
                ui.label(cx, &[id, live_id!(value)]).set_text(cx, value);
            }
        }
        ui.label(cx, ids!(cut_summary)).set_visible(cx, pairs.is_empty());
        ui.label(cx, ids!(cut_summary)).set_text(cx, "Nothing on the timeline yet");
        ui.button(cx, ids!(cut_summary_stale)).set_visible(cx, !stale.is_empty());
        ui.button(cx, ids!(cut_summary_stale)).set_text(cx, &stale.join("  "));
        self.show_sections(cx, ui, &[live_id!(cut_s_project), live_id!(cut_s_summary)]);
    }

    pub(super) fn inspector(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if self.sections.is_empty() {
            self.sections = SECTIONS.iter().map(|(_, head, _, open)| (*head, *open)).collect();
        }
        let revision = self.session.as_ref().map_or(0, |session| session.revision);
        let key = (self.selected.clone().unwrap_or_default(), revision);
        // Keyed values change along the playhead: refill on a new frame.
        let frame = (self.playhead * self.fps().max(1.0)).round() as i64;
        if Some(&key) == self.inspected.as_ref() && self.playing.is_none() && frame == self.inspected_frame {
            return;
        }
        self.inspected_frame = frame;
        self.inspected = Some(key);
        let selected = self.selected.clone();
        let node = self.session.as_ref().zip(selected.as_deref()).and_then(|(session, id)| session.doc.node(split_selection(id).0).cloned());
        let graphic = self.session.as_ref().zip(selected.as_deref()).and_then(|(session, id)| session.doc.graphic(split_selection(id).0).cloned());
        ui.button(cx, ids!(cut_delete)).set_visible(cx, node.is_some() || graphic.is_some());
        if let Some(graphic) = graphic {
            self.graphic_inspector(cx, ui, &graphic);
            ui.redraw(cx);
            return;
        }
        let Some(node) = node else {
            self.project_inspector(cx, ui);
            ui.redraw(cx);
            return;
        };
        let line = self.selected_line();
        let doc = self.session.as_ref().unwrap().doc.clone();
        let fps = doc.output.fps;
        let lane_row = |track: u32, letter: char| format!("{letter}{}", track + 1);

        // The header.
        let caption = selected.as_deref().and_then(|id| split_selection(id).1).filter(|_| matches!(node.kind, CutNodeKind::Subtitles { .. }));
        let (role, title, kind) = match &node.kind {
            CutNodeKind::Clip { src: MediaRef::Run(_), .. } => ((live_id!(cut_role_app), CutRole::APP_RUN.strip), super::project::media_label(match &node.kind { CutNodeKind::Clip { src, .. } => src, _ => unreachable!() }, &self.titles), format!("Clip · App run · {}", lane_row(node.track, 'V'))),
            CutNodeKind::Clip { src, nested: true, .. } => ((live_id!(cut_role_project), CutRole::VIDEO.strip), super::project::media_label(src, &self.titles), format!("Compound · Edit · {}", lane_row(node.track, 'V'))),
            CutNodeKind::Clip { src, .. } => ((live_id!(cut_role_video), CutRole::VIDEO.strip), super::project::media_label(src, &self.titles), format!("Clip · Video · {}", lane_row(node.track, 'V'))),
            CutNodeKind::Still { src, .. } => ((live_id!(cut_role_image), CutRole::VIDEO.strip), super::project::media_label(src, &self.titles), format!("Still · Image · {}", lane_row(node.track, 'V'))),
            CutNodeKind::AudioClip { src, .. } => ((live_id!(cut_role_audio), CutRole::AUDIO.strip), super::project::media_label(src, &self.titles), format!("Audio · {}", lane_row(node.track, 'A'))),
            CutNodeKind::Fx { effect, .. } => ((live_id!(cut_role_fx), CutRole::EFFECT.strip), super::project::effect_label(effect), format!("Fx · {}", super::project::effect_label(effect))),
            CutNodeKind::Transition { effect, .. } => ((live_id!(cut_role_trans), CutRole::TRANSITION.strip), preset_label(effect), "Transition".to_string()),
            CutNodeKind::Title { text, .. } => ((live_id!(cut_role_title), CutRole::TITLE.strip), text.clone(), format!("Title · {}", lane_row(node.track, 'V'))),
            CutNodeKind::VoiceOver { lines, .. } => match &line {
                Some((_, index)) => ((live_id!(cut_role_voice), CutRole::VOICE.strip), lines.get(*index).map(|line| line.text.clone()).unwrap_or_default(), format!("VoiceOver line {} of {} · VO", index + 1, lines.len())),
                None => ((live_id!(cut_role_voice), CutRole::VOICE.strip), "Voiceover".to_string(), format!("VoiceOver · {} lines · VO", lines.len())),
            },
            CutNodeKind::Subtitles { .. } => {
                let count = self.projection.blocks.iter().filter(|block| block.owner == node.id).count();
                let words = caption.and_then(|index| self.projection.blocks.iter().find(|block| block.node == format!("{}#{index}", node.id)).map(|block| block.label.clone()));
                ((live_id!(cut_role_caps), CutRole::CAPTION.wash), words.unwrap_or_else(|| "Subtitles".into()), match caption {
                    Some(index) => format!("Caption {} of {count} · CC", index + 1),
                    None => format!("Subtitles · {count} captions · CC"),
                })
            }
            _ => ((live_id!(cut_role_project), 0), node.id.clone(), node.kind.type_name().to_string()),
        };
        self.header(cx, ui, Some(role), &title, &kind);

        // TIMING.
        let timed = node.span.is_some() && line.is_none() && caption.is_none();
        let span = node.span.unwrap_or(makepad_story_cut::Span { start: 0.0, length: doc.duration });
        scrub(cx, ui, ids!(cut_start), Format::Time { fps }, 0.0, 3600.0, span.start);
        scrub(cx, ui, ids!(cut_length), Format::Time { fps }, 1.0 / fps.max(1.0), 3600.0, span.length);
        let letter = match &node.kind {
            CutNodeKind::AudioClip { .. } => 'A',
            _ => 'V',
        };
        scrub(cx, ui, ids!(cut_track), Format::Track { letter }, 0.0, 15.0, node.track as f64);
        let trackable = !matches!(node.kind, CutNodeKind::Fx { .. } | CutNodeKind::Transition { .. });
        ui.view(cx, ids!(cut_track_row)).set_visible(cx, trackable);
        let source_in = match &node.kind {
            CutNodeKind::Clip { source_in: SourceTime::Seconds(seconds), .. } | CutNodeKind::AudioClip { source_in: SourceTime::Seconds(seconds), .. } => Some(*seconds),
            _ => None,
        };
        scrub(cx, ui, ids!(cut_in), Format::Time { fps }, 0.0, 3600.0, source_in.unwrap_or(0.0));
        ui.view(cx, ids!(cut_in_row)).set_visible(cx, source_in.is_some());

        // LOOK and AUDIO: parameter rows with key navigation.
        let local = self.local(&node.id);
        let rows = Self::param_rows(&node.kind);
        self.rows_shown = rows.iter().map(|row| row.map(|(key, _, min, max, _)| (key.to_string(), min, max))).collect();
        let half = 0.5 / fps.max(1.0);
        for (index, row) in rows.iter().enumerate() {
            let (row_id, name_id, value_id, key_id) = ROWS[index];
            ui.view(cx, &[row_id]).set_visible(cx, row.is_some());
            let Some((key, label, min, max, default)) = row else { continue };
            ui.label(cx, &[name_id]).set_text(cx, label);
            let literal = self.literal(&node.id, key).unwrap_or(Literal::Num(*default));
            let points = key_points(&literal);
            scrub(cx, ui, &[value_id], format_of(key), *min, *max, value_at(&literal, local, *default));
            if let Some(mut field) = ui.widget(cx, &[value_id]).borrow_mut::<CutScrub>() {
                field.set_keyed(cx, points.is_some());
            }
            let animatable = makepad_story_cut::animatable(node.kind.type_name(), key);
            ui.widget(cx, &[key_id]).set_visible(cx, animatable);
            let on = points.as_ref().is_some_and(|points| points.iter().any(|point| (point.0 - local).abs() <= half));
            let before = points.as_ref().is_some_and(|points| points.iter().any(|point| point.0 < local - half));
            let after = points.as_ref().is_some_and(|points| points.iter().any(|point| point.0 > local + half));
            let (on, keyed) = (if on { 1.0 } else { 0.0 }, if points.is_some() { 1.0 } else { 0.0 });
            let mut diamond = ui.widget(cx, &[key_id, live_id!(key)]);
            script_apply_eval!(cx, diamond, {draw_bg +: {on: #(on) keyed: #(keyed)}});
            diamond.redraw(cx);
            ui.widget(cx, &[key_id, live_id!(prev)]).set_visible(cx, points.is_some());
            ui.widget(cx, &[key_id, live_id!(next)]).set_visible(cx, points.is_some());
            ui.button(cx, &[key_id, live_id!(prev)]).set_enabled(cx, before);
            ui.button(cx, &[key_id, live_id!(next)]).set_enabled(cx, after);
        }
        let (value_name, value) = match &node.kind {
            CutNodeKind::Fx { effect, .. } => (Some("Preset"), match effect {
                EffectRef::Preset(name) => name.clone(),
                EffectRef::Asset(asset) => format!("{}:{}", asset.id, asset.rev),
            }),
            CutNodeKind::Transition { effect, .. } => (Some("Preset"), effect.clone()),
            CutNodeKind::Title { text, .. } => (Some("Text"), text.clone()),
            _ => (None, String::new()),
        };
        ui.view(cx, ids!(cut_value_row)).set_visible(cx, value_name.is_some());
        ui.label(cx, ids!(cut_value_name)).set_text(cx, value_name.unwrap_or(""));
        ui.text_input(cx, ids!(cut_value)).set_text(cx, &value);
        let fit = match &node.kind {
            CutNodeKind::Clip { fit, .. } | CutNodeKind::Still { fit, .. } => Some(*fit),
            _ => None,
        };
        ui.view(cx, ids!(cut_fit_row)).set_visible(cx, fit.is_some());
        // A compound clip: its cut's own captions, off unless asked for.
        let nested_subs = match &node.kind {
            CutNodeKind::Clip { nested: true, subtitles, .. } => Some(*subtitles),
            _ => None,
        };
        ui.view(cx, ids!(cut_nsubs_row)).set_visible(cx, nested_subs.is_some());
        if let (Some(on), Some(mut switch)) = (nested_subs, ui.widget(cx, ids!(cut_nsubs)).borrow_mut::<CutSwitch>()) {
            switch.set_on(cx, on);
        }
        if let (Some(fit), Some(mut segmented)) = (fit, ui.widget(cx, ids!(cut_fit)).borrow_mut::<CutSegmented>()) {
            segmented.set_active(cx, match fit { Fit::Fill => 0, Fit::Fit => 1, Fit::None => 2 });
        }
        // Subtitles: film lines (the default) or karaoke.
        let style = match &node.kind {
            CutNodeKind::Subtitles { style, .. } => Some(style.starts_with("karaoke")),
            _ => None,
        };
        ui.view(cx, ids!(cut_style_row)).set_visible(cx, style.is_some());
        if let (Some(karaoke), Some(mut segmented)) = (style, ui.widget(cx, ids!(cut_style)).borrow_mut::<CutSegmented>()) {
            segmented.set_active(cx, usize::from(karaoke));
        }
        let look = value_name.is_some() || fit.is_some() || style.is_some() || rows[..4].iter().any(Option::is_some);

        // AUDIO.
        let shape = match &node.kind {
            CutNodeKind::Clip { shape, audio: true, .. } | CutNodeKind::AudioClip { shape, .. } | CutNodeKind::VoiceOver { shape, .. } => Some(shape.clone()),
            _ => None,
        };
        ui.view(cx, ids!(cut_fade_rows)).set_visible(cx, shape.is_some());
        ui.view(cx, ids!(cut_duck_row)).set_visible(cx, shape.is_some() && !matches!(node.kind, CutNodeKind::VoiceOver { .. }));
        ui.view(cx, ids!(cut_depth_row)).set_visible(cx, shape.as_ref().is_some_and(|shape| shape.duck.is_some()) && !matches!(node.kind, CutNodeKind::VoiceOver { .. }));
        if let Some(shape) = &shape {
            scrub(cx, ui, ids!(cut_fade_in), Format::Time { fps }, 0.0, 10.0, shape.fade_in);
            scrub(cx, ui, ids!(cut_fade_out), Format::Time { fps }, 0.0, 10.0, shape.fade_out);
            let depth = shape.duck.as_ref().map_or(-12.0, |duck| duck.depth_db);
            scrub(cx, ui, ids!(cut_depth), Format::Decibel, 10f64.powf(-40.0 / 20.0), 1.0, 10f64.powf(depth / 20.0));
            let voices = doc.nodes.iter().any(|other| matches!(other.kind, CutNodeKind::VoiceOver { .. }) && other.id != node.id);
            if let Some(mut switch) = ui.widget(cx, ids!(cut_duck)).borrow_mut::<CutSwitch>() {
                switch.set_on(cx, shape.duck.is_some());
                switch.set_disabled(cx, shape.duck.is_none() && !voices);
                switch.set_text(cx, if voices || shape.duck.is_some() { "Duck under voiceover" } else { "Duck under voiceover (add one first)" });
            }
        }

        // LINE and VOICE.
        if let Some((voice_node, index)) = &line {
            let (voice, lines) = self.voice_of(voice_node).unwrap_or_default();
            let engine = voice.engine.id().to_string();
            let (eval, _) = self.eval(&doc);
            let start = eval.voice_lines(voice_node).iter().find(|placed| placed.index == *index).map_or(0.0, |placed| placed.start);
            scrub(cx, ui, ids!(cut_line_at), Format::Time { fps }, 0.0, 3600.0, start);
            if let Some(this) = lines.get(*index) {
                if let (SourceTime::Marker { .. }, Some(mut field)) = (&this.at, ui.widget(cx, ids!(cut_line_at)).borrow_mut::<CutScrub>()) {
                    field.set_disabled(cx, true);
                }
                ui.text_input(cx, ids!(cut_line_text)).set_text(cx, &this.text);
                let speaking = self.speaking.contains(&(voice_node.clone(), *index));
                let p = makepad_look::palette::get(cx);
                let (dot, state) = match (speaking, this.take.is_some(), this.stale(&voice)) {
                    (true, _, _) => (p.muted, "Speaking…".to_string()),
                    (false, false, _) => (p.amber, "No voice yet — ask for it in the chat (Voice the lines)".to_string()),
                    (false, true, true) => (p.amber, "Stale — the words changed since the take (Voice the lines)".to_string()),
                    (false, true, false) => (p.live, format!("Voice up to date · {engine} {}", voice.id)),
                };
                let mut dot_view = ui.widget(cx, ids!(cut_voice_dot));
                let color = Vec4f::from_u32(dot);
                script_apply_eval!(cx, dot_view, {draw_bg +: {color: #(color)}});
                let mut state_label = ui.widget(cx, ids!(cut_voice_state));
                let ink = Vec4f::from_u32(if dot == p.amber { p.warm } else { p.text });
                script_apply_eval!(cx, state_label, {draw_text +: {color: #(ink)}});
                ui.label(cx, ids!(cut_voice_state)).set_text(cx, &state);
            }
        }

        // KEYFRAMES: every keyed parameter.
        let keyed = doc.graph.nodes.iter().find(|graph_node| graph_node.id == node.id).map(|graph_node| {
            graph_node.params.iter().filter_map(|(key, value)| key_points(value).filter(|_| makepad_story_cut::animatable(node.kind.type_name(), key)).map(|points| {
                let times = points.iter().map(|point| timecode_full(span.start + point.0, fps)).collect::<Vec<_>>().join("  ");
                format!("{key} · {} keys · {times}", points.len())
            })).collect::<Vec<_>>()
        }).unwrap_or_default();
        ui.label(cx, ids!(cut_keys)).set_text(cx, &keyed.join("\n"));

        // SOURCE.
        let media = match &node.kind {
            CutNodeKind::Clip { src, .. } | CutNodeKind::Still { src, .. } | CutNodeKind::AudioClip { src, .. } => Some(src.clone()),
            _ => None,
        };
        if let Some(media) = &media {
            let (asset, revision) = match media {
                MediaRef::MissingOptional(path) => (path.clone(), String::new()),
                MediaRef::Asset(asset) => (asset.id.clone(), asset.rev.clone()),
                MediaRef::Run(run) => (run.render_id.clone(), run.render.clone()),
            };
            ui.label(cx, ids!(cut_src_title)).set_text(cx, &super::project::media_label(media, &self.titles));
            let nested = matches!(node.kind, CutNodeKind::Clip { nested: true, .. });
            ui.button(cx, ids!(cut_reveal)).set_text(cx, if nested { "Open cut" } else { "Reveal in library" });
            let mut detail = if matches!(media, MediaRef::MissingOptional(_)) {
                vec![format!("media not present: {asset} (plays silent)")]
            } else { vec![asset.clone(), format!("revision {}…", &revision[..revision.len().min(17)])] };
            if let Some(row) = self.lib.rows.iter().find(|row| row.reference.asset_id.to_string() == asset) {
                if let Some(seconds) = row.seconds {
                    detail.push(format!("{} long", timecode_full(seconds, fps)));
                }
            }
            if let Some((thumb, _)) = self.lib.thumbs.get(&asset) {
                detail.push(format!("picture {}:{}", thumb.width, thumb.height));
            }
            ui.label(cx, ids!(cut_src_detail)).set_text(cx, &detail.join("\n"));
        }

        // Which sections show, in the fixed order of the page.
        let mut shown = Vec::new();
        if line.is_some() {
            shown.extend([live_id!(cut_s_voice), live_id!(cut_s_speak)]);
        }
        if timed {
            shown.push(live_id!(cut_s_timing));
        }
        if look && line.is_none() {
            shown.push(live_id!(cut_s_look));
        }
        if shape.is_some() || rows[4].is_some() {
            shown.push(live_id!(cut_s_audio));
        }
        if !keyed.is_empty() {
            shown.push(live_id!(cut_s_keys));
        }
        if media.is_some() {
            shown.push(live_id!(cut_s_source));
        }
        self.show_sections(cx, ui, &shown);
        ui.redraw(cx);
    }

    /// A parameter row let go at `value`: the key at the playhead when the
    /// value is keyed, else the constant.
    fn set_param(&mut self, row: usize, value: f64) {
        let Some(Some((key, min, max))) = self.rows_shown.get(row).cloned() else { return };
        let Some(node) = self.selected.as_deref().map(|id| split_selection(id).0.to_string()) else { return };
        let value = value.clamp(min, max);
        let local = self.local(&node);
        let written = match self.literal(&node, &key).as_ref().and_then(key_points) {
            Some(mut points) => {
                let half = 0.5 / self.fps().max(1.0);
                match points.iter_mut().find(|point| (point.0 - local).abs() <= half) {
                    Some(point) => point.1 = value,
                    None => {
                        let at = points.partition_point(|point| point.0 < local);
                        points.insert(at, (local, value, None));
                    }
                }
                keys_value(&points)
            }
            None => Value::num(value),
        };
        self.op(CutOp::Param { node, key, value: written });
    }

    /// The diamond: add the key at the playhead, or remove it.
    fn toggle_key(&mut self, row: usize) {
        let Some(Some((key, _, _))) = self.rows_shown.get(row).cloned() else { return };
        let Some(node) = self.selected.as_deref().map(|id| split_selection(id).0.to_string()) else { return };
        let local = self.local(&node);
        let literal = self.literal(&node, &key).unwrap_or(Literal::Null);
        let default = Self::param_rows(&self.session.as_ref().and_then(|session| session.doc.node(&node)).map(|node| node.kind.clone()).unwrap_or(CutNodeKind::Stack { layers: Vec::new() }))
            .iter().flatten().find(|row| row.0 == key).map_or(0.0, |row| row.4);
        let current = value_at(&literal, local, default);
        let half = 0.5 / self.fps().max(1.0);
        let written = match key_points(&literal) {
            Some(mut points) => match points.iter().position(|point| (point.0 - local).abs() <= half) {
                Some(index) => {
                    points.remove(index);
                    if points.is_empty() { Value::num(current) } else { keys_value(&points) }
                }
                None => {
                    let at = points.partition_point(|point| point.0 < local);
                    points.insert(at, (local, current, None));
                    keys_value(&points)
                }
            },
            None => keys_value(&[(local.max(0.0), current, None)]),
        };
        self.op(CutOp::Param { node, key, value: written });
    }

    /// The arrows: the playhead to the row's previous or next key.
    fn seek_key(&mut self, row: usize, forward: bool) {
        let Some(Some((key, _, _))) = self.rows_shown.get(row).cloned() else { return };
        let Some(node) = self.selected.as_deref().map(|id| split_selection(id).0.to_string()) else { return };
        let Some(points) = self.literal(&node, &key).as_ref().and_then(key_points) else { return };
        let local = self.local(&node);
        let half = 0.5 / self.fps().max(1.0);
        let target = if forward {
            points.iter().map(|point| point.0).find(|t| *t > local + half)
        } else {
            points.iter().rev().map(|point| point.0).find(|t| *t < local - half)
        };
        let start = self.session.as_ref().and_then(|session| session.doc.node(&node)).and_then(|node| node.span).map_or(0.0, |span| span.start);
        if let Some(t) = target {
            self.seek(start + t);
            self.inspected = None;
        }
    }

    /// A timing field let go: its value as the cut it changes.
    fn timing_changed(&mut self, field: LiveId, value: f64) {
        let Some(session) = &self.session else { return };
        if let Some(graphic) = self.selected.as_ref().and_then(|id| session.doc.graphic(split_selection(id).0)).cloned() {
            let Some(span) = graphic.span else { return };
            let op = if field == live_id!(cut_start) {
                CutOp::Move { node: graphic.id, start: value.max(0.0), track: graphic.track }
            } else if field == live_id!(cut_track) {
                CutOp::Move { node: graphic.id, start: span.start, track: value.round().max(0.0) as u32 }
            } else if field == live_id!(cut_length) {
                CutOp::Trim { node: graphic.id, start: span.start, length: value }
            } else {
                return;
            };
            self.op(op);
            return;
        }
        let Some(node) = self.selected.as_ref().and_then(|id| session.doc.node(split_selection(id).0)).cloned() else { return };
        let Some(span) = node.span else { return };
        // A transition keeps its centre and its clips round it.
        if matches!(node.kind, CutNodeKind::Transition { .. }) && field == live_id!(cut_length) {
            let centre = span.start + span.length * 0.5;
            self.retime_transition(&node.id, centre - value * 0.5, value);
            return;
        }
        let op = if field == live_id!(cut_start) {
            CutOp::Move { node: node.id, start: value.max(0.0), track: node.track }
        } else if field == live_id!(cut_track) {
            CutOp::Move { node: node.id, start: span.start, track: value.round().max(0.0) as u32 }
        } else if field == live_id!(cut_length) {
            CutOp::Param { node: node.id, key: "span".into(), value: Value::span(span.start, value.max(1.0 / session.doc.output.fps)) }
        } else {
            CutOp::Param { node: node.id, key: "in".into(), value: Value::num(value.max(0.0)) }
        };
        self.op(op);
    }

    /// Show the selected clip's media in the library.
    fn reveal(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(session) = &self.session else { return };
        let Some(node) = self.selected.as_ref().and_then(|id| session.doc.node(split_selection(id).0)).cloned() else { return };
        let (lane, media) = match &node.kind {
            CutNodeKind::Clip { src: MediaRef::Run(run), .. } => (LibLane::AppRuns, run.id.clone()),
            CutNodeKind::Clip { src: MediaRef::Asset(asset), .. } => (LibLane::Video, asset.id.clone()),
            CutNodeKind::Still { src, .. } => (LibLane::Image, super::media_id(src).to_string()),
            CutNodeKind::AudioClip { src, .. } => (LibLane::Audio, super::media_id(src).to_string()),
            _ => return,
        };
        self.lib.search.clear();
        self.lib.widget(cx, ui, live_id!(lib_search)).as_text_input().set_text(cx, "");
        self.reveal = Some(media);
        if self.lib.lane == Some(PageLane::Store(lane)) {
            self.query();
        } else {
            self.set_lane(cx, ui, PageLane::Store(lane));
        }
    }

    fn segment_picked(cx: &mut Cx, ui: &WidgetRef, id: &[LiveId], actions: &Actions) -> Option<usize> {
        let widget = ui.widget(cx, id);
        actions.filter_widget_actions_cast::<SegmentAction>(widget.widget_uid()).filter_map(|action| match action {
            SegmentAction::Selected(index) => Some(index),
            _ => None,
        }).last()
    }

    pub(super) fn inspector_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        for (_, head, _, _) in SECTIONS {
            if ui.button(cx, &[head, live_id!(head)]).clicked(actions) {
                let open = self.sections.entry(head).or_insert(true);
                *open = !*open;
                self.inspected = None;
                self.repaint = true;
            }
        }
        // The Project inspector.
        if let Some(value) = scrub_commits(cx, ui, ids!(cut_p_duration), actions) {
            self.op(CutOp::Header { key: "duration".into(), value: Value::num(value.max(1.0 / self.fps().max(1.0))) });
        }
        if let Some(value) = scrub_commits(cx, ui, ids!(cut_p_bpm), actions) {
            self.op(CutOp::Header { key: "bpm".into(), value: Value::num(value.round().clamp(20.0, 300.0)) });
        }
        if let Some(index) = Self::segment_picked(cx, ui, ids!(cut_p_safe), actions) {
            let zone = SAFE_ZONES[index.min(3)];
            self.platform = Some(zone);
            // Subtitles keep clear of the platform's UI too, and the choice
            // is saved with them.
            let subtitles = self.session.as_ref().map(|session| session.doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Subtitles { .. })).map(|node| node.id.clone()).collect::<Vec<_>>()).unwrap_or_default();
            for node in subtitles {
                self.op(CutOp::Param { node, key: "safe".into(), value: Value::id(zone.id()) });
            }
            self.repaint = true;
        }
        // The cut's frame rate: a preset, or a typed one (Enter).
        {
            use super::rates;
            let mut set = None;
            if let Some(item) = ui.drop_down(cx, ids!(cut_p_fps)).changed(actions) {
                self.fps_custom = item >= rates::RATES.len();
                set = rates::RATES.get(item).map(|(_, fps)| *fps);
                if self.fps_custom {
                    ui.text_input(cx, ids!(cut_p_fps_custom)).set_text(cx, "");
                    self.repaint = true;
                }
            }
            if let Some((text, _)) = ui.text_input(cx, ids!(cut_p_fps_custom)).returned(actions) {
                match rates::parse(&text) {
                    Some(fps) => set = Some(fps),
                    None => self.status = format!("“{text}” is not a frame rate (1 to 1000, e.g. 72 or 59.94)"),
                }
            }
            if let (Some(fps), Some(doc)) = (set, self.session.as_ref().map(|session| session.doc.clone())) {
                if (fps - doc.output.fps).abs() > 1e-6 {
                    self.op(CutOp::Header { key: "output".into(), value: super::ops::output_value(&doc, fps, doc.output.loudness) });
                    self.status = format!("The cut is now {} fps", rates::label(fps));
                    self.status_sticky = true;
                }
                self.repaint = true;
            }
        }
        if ui.button(cx, ids!(cut_summary_stale)).clicked(actions) {
            // The first line to speak, or the first removed clip.
            let session = self.session.as_ref().map(|session| session.doc.clone());
            let line = session.as_ref().and_then(|doc| doc.nodes.iter().find_map(|node| match &node.kind {
                CutNodeKind::VoiceOver { voice, lines, .. } => lines.iter().position(|line| line.stale(voice)).map(|index| format!("{}#{index}", node.id)),
                _ => None,
            }));
            let broken = self.projection.blocks.iter().find(|block| block.broken).map(|block| block.node.clone());
            if let Some(target) = line.or(broken) {
                self.selected = Some(target);
                self.repaint = true;
            }
        }
        for field in TIMING_FIELDS {
            if let Some(value) = scrub_commits(cx, ui, &[field], actions) {
                self.timing_changed(field, value);
            }
        }
        for (index, (_, _, value_id, key_id)) in ROWS.into_iter().enumerate() {
            if let Some(value) = scrub_commits(cx, ui, &[value_id], actions) {
                self.set_param(index, value);
            }
            if ui.button(cx, &[key_id, live_id!(key)]).clicked(actions) {
                self.toggle_key(index);
            }
            if ui.button(cx, &[key_id, live_id!(prev)]).clicked(actions) {
                self.seek_key(index, false);
            }
            if ui.button(cx, &[key_id, live_id!(next)]).clicked(actions) {
                self.seek_key(index, true);
            }
        }
        if let Some((value, _)) = ui.text_input(cx, ids!(cut_value)).returned(actions) {
            if let Some(node) = self.selected.as_deref().map(|id| split_selection(id).0.to_string()) {
                let key = match self.session.as_ref().and_then(|session| session.doc.node(&node)).map(|node| &node.kind) {
                    Some(CutNodeKind::Fx { .. } | CutNodeKind::Transition { .. }) => Some("effect"),
                    Some(CutNodeKind::Title { .. }) => Some("text"),
                    _ => None,
                };
                if let Some(key) = key {
                    self.op(CutOp::Param { node, key: key.into(), value: Value::string(&value) });
                }
            }
        }
        let node = self.selected.as_deref().map(|id| split_selection(id).0.to_string());
        if let (Some(item), Some(node)) = (Self::segment_picked(cx, ui, ids!(cut_fit), actions), node.clone()) {
            self.op(CutOp::Param { node, key: "fit".into(), value: Value::id(["fill", "fit", "none"][item.min(2)]) });
        }
        if let (Some(item), Some(node)) = (Self::segment_picked(cx, ui, ids!(cut_style), actions), node.clone()) {
            let style = if item == 1 { Value::string("karaoke/pop") } else { Value::id("movie") };
            self.op(CutOp::Param { node, key: "style".into(), value: style });
        }
        let shape_of = |this: &Self, node: &str| this.session.as_ref().and_then(|session| session.doc.node(node)).and_then(|node| match &node.kind {
            CutNodeKind::Clip { shape, .. } | CutNodeKind::AudioClip { shape, .. } | CutNodeKind::VoiceOver { shape, .. } => Some(shape.clone()),
            _ => None,
        });
        for (field, fade_in) in [(live_id!(cut_fade_in), true), (live_id!(cut_fade_out), false)] {
            if let (Some(value), Some(node)) = (scrub_commits(cx, ui, &[field], actions), node.clone()) {
                if let Some(shape) = shape_of(self, &node) {
                    let (fade_in, fade_out) = if fade_in { (value, shape.fade_out) } else { (shape.fade_in, value) };
                    self.op(CutOp::Fade { node, fade_in, fade_out });
                }
            }
        }
        let voices_but = |this: &Self, node: &str| this.session.as_ref().map(|session| session.doc.nodes.iter().filter(|other| matches!(other.kind, CutNodeKind::VoiceOver { .. }) && other.id != node).map(|other| other.id.clone()).collect::<Vec<_>>()).unwrap_or_default();
        let nsubs = ui.widget(cx, ids!(cut_nsubs));
        for action in actions.filter_widget_actions_cast::<SwitchAction>(nsubs.widget_uid()) {
            if let (SwitchAction::Toggled(on), Some(node)) = (action, node.clone()) {
                self.op(CutOp::Param { node, key: "subtitles".into(), value: Value::num(if on { 1.0 } else { 0.0 }) });
            }
        }
        let duck = ui.widget(cx, ids!(cut_duck));
        for action in actions.filter_widget_actions_cast::<SwitchAction>(duck.widget_uid()) {
            if let (SwitchAction::Toggled(on), Some(node)) = (action, node.clone()) {
                let under = on.then(|| voices_but(self, &node));
                self.op(CutOp::Duck { node, under, depth_db: None });
            }
        }
        if let (Some(value), Some(node)) = (scrub_commits(cx, ui, ids!(cut_depth), actions), node.clone()) {
            let under = shape_of(self, &node).and_then(|shape| shape.duck).map(|duck| duck.under).unwrap_or_else(|| voices_but(self, &node));
            self.op(CutOp::Duck { node, under: Some(under), depth_db: Some(20.0 * value.max(1e-5).log10()) });
        }
        if let Some((text, _)) = ui.text_input(cx, ids!(cut_line_text)).returned(actions) {
            if let Some((node, index)) = self.selected_line() {
                self.set_line_text(&node, index, text);
            }
        }
        if let Some(value) = scrub_commits(cx, ui, ids!(cut_line_at), actions) {
            if let Some((node, index)) = self.selected_line() {
                self.move_line(&node, index, value);
            }
        }
        if ui.button(cx, ids!(cut_line_add)).clicked(actions) {
            self.add_voice_line();
        }
        if ui.button(cx, ids!(cut_reveal)).clicked(actions) {
            let nested = self.selected.as_deref().map(|id| split_selection(id).0.to_string())
                .filter(|node| self.session.as_ref().and_then(|session| session.doc.node(node)).is_some_and(|node| node.kind.nested_edit().is_some()));
            let graphic = self.selected.as_deref().map(|id| split_selection(id).0.to_string())
                .filter(|node| self.session.as_ref().is_some_and(|session| session.doc.graphic(node).is_some()));
            match (nested, graphic) {
                (Some(node), _) => self.open_nested(&node),
                (None, Some(node)) => {
                    super::motion_edit::open(self, &node);
                }
                (None, None) => self.reveal(cx, ui),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_values_interpolate_and_constants_hold() {
        let keys = Literal::Arr(vec![
            Literal::Arr(vec![Literal::Num(0.0), Literal::Num(0.0)]),
            Literal::Arr(vec![Literal::Num(2.0), Literal::Num(1.0), Literal::Id("ease_out".into())]),
        ]);
        assert_eq!(key_points(&keys).unwrap()[1].2.as_deref(), Some("ease_out"));
        assert_eq!(value_at(&keys, 1.0, 9.0), 0.5);
        assert_eq!(value_at(&keys, 5.0, 9.0), 1.0);
        assert_eq!(value_at(&Literal::Num(0.3), 1.0, 9.0), 0.3);
        assert_eq!(value_at(&Literal::Null, 1.0, 9.0), 9.0);
        assert!(keys_value(&key_points(&keys).unwrap()).text.starts_with("keys([0.0, 0.0], [2.0, 1.0, @ease_out])"));
    }
}

/// An input value of a graphic, as one short line.
fn input_text(value: &makepad_story_cut::graphic::InputValue) -> String {
    use makepad_story_cut::graphic::InputValue;
    match value {
        InputValue::Text(text) | InputValue::Markup(text) => {
            let line: String = text.chars().take(40).collect();
            if text.chars().count() > 40 { format!("{line}…") } else { line }
        }
        InputValue::Number(n) => format!("{n}"),
        InputValue::Bool(on) => on.to_string(),
        InputValue::Vec2([x, y]) => format!("{x}, {y}"),
        InputValue::Asset(asset) => asset.id.clone(),
        InputValue::List(items) => format!("{} items", items.len()),
        InputValue::Empty => "none".into(),
        other => format!("{other:?}"),
    }
}
