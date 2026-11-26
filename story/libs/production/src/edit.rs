//! The cut's source: the song from 0 s and one Graphic per section's
//! scene on the section's span, joined by a transition on each section cut.
//! A transition runs from the cut for [`transition_secs`] into the next
//! section, over the outgoing scene's tail (each scene is written that much
//! longer than its section), so every scene's clock stays the song's: a
//! scene placed at its section's start reads song time as `t + S0`.

use crate::plan::{AssetRef, Section};

/// One scene on the timeline.
pub struct Placed<'a> {
    pub section: &'a Section,
    pub motion: &'a AssetRef,
}

/// The cut's node id for a section's scene.
pub fn node_id(section: &str) -> String {
    let id: String = section.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    format!("s_{id}")
}

fn t(x: f64) -> String {
    format!("{x:.3}")
}

/// How long a cut's transition lasts: one beat, within 0.3 to 0.8 s.
pub fn transition_secs(bpm: f64) -> f64 {
    if bpm <= 0.0 {
        return 0.5;
    }
    ((60.0 / bpm) * 1000.0).round().clamp(300.0, 800.0) / 1000.0
}

/// The transition on the section cuts: the dissolve (no preset).
pub const TRANSITION: &str = "";

/// The tempo the beats keep: the median beat length.
pub fn bpm(beats: &[f64]) -> f64 {
    let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).filter(|gap| *gap > 0.2 && *gap < 2.0).collect();
    if gaps.is_empty() {
        return 120.0;
    }
    gaps.sort_by(f64::total_cmp);
    (60.0 / gaps[gaps.len() / 2] * 100.0).round() / 100.0
}

#[allow(clippy::too_many_arguments)]
pub fn source(title: &str, song: &AssetRef, duration: f64, bpm: f64, scenes: &[Placed], width: u32, height: u32, fps: u32) -> String {
    let tail = transition_secs(bpm);
    let mut out = String::from("use mod.flow.*\nuse mod.cut.*\n\n");
    out.push_str(&format!("// \"{}\": one scene per section of the song, a transition on each cut.\n", title.replace('"', "'")));
    let mut ids = Vec::new();
    for (k, placed) in scenes.iter().enumerate() {
        let id = node_id(&placed.section.name);
        let mut length = (placed.section.end - placed.section.start).max(1.0 / fps.max(1) as f64);
        if k + 1 < scenes.len() {
            length += tail;
        }
        out.push_str(&format!(
            "let {id} = Graphic{{src: asset(\"{}\", \"{}\") span: [{}, {}] fit: @fill}}\n",
            placed.motion.asset_id,
            placed.motion.revision,
            t(placed.section.start),
            t(length)
        ));
        ids.push(id);
    }
    // Each cut: the picture so far into the next scene.
    let mut transitions = Vec::new();
    let mut picture = ids.first().map(|id| format!("{id}.out(@frame)")).unwrap_or_default();
    for (k, placed) in scenes.iter().enumerate().skip(1) {
        let id = format!("cut{k}");
        out.push_str(&format!(
            "let {id} = Transition{{a: {picture} b: {}.out(@frame) effect: \"{TRANSITION}\" span: [{}, {}]}}\n",
            ids[k],
            t(placed.section.start),
            t(tail)
        ));
        picture = format!("{id}.out(@frame)");
        transitions.push(id);
    }
    out.push_str(&format!("let song = AudioClip{{src: asset(\"{}\", \"{}\") span: [0.0, {}] fade_out: 0.5}}\n", song.asset_id, song.revision, t(duration)));
    // A list input takes each connection with its slot id.
    out.push_str(&format!("let main = Stack{{layers: [conn(1, {picture})]}}\n"));
    out.push_str("let out = CutOutput{frame: main.out(@frame) audio: [conn(1, song.out(@audio))]}\n\n");
    let mut nodes = ids.clone();
    nodes.extend(transitions);
    out.push_str(&format!(
        "Flow{{\n    kind: @cut\n    label: \"{}\"\n    output: {{width: {width} height: {height} fps: {fps}}}\n    duration: {}\n    bpm: {bpm}\n    {}, song, main, out\n}}\n",
        title.replace('"', "'"),
        t(duration),
        nodes.join(", ")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_edit_places_each_scene_on_its_section_over_the_song() {
        let sections = vec![
            Section { name: "intro1".into(), start: 0.0, end: 4.5, lines: Vec::new() },
            Section { name: "verse1".into(), start: 4.5, end: 20.0, lines: Vec::new() },
        ];
        let rev = format!("arev_{}", "d".repeat(64));
        let refs = vec![AssetRef::new("ast_aaaaaaaaaaaaaaaaaaaaaaaaaa", rev.clone()), AssetRef::new("ast_clonebaaaaaaaaaaaaaaaaaaaa", rev.clone())];
        let placed: Vec<Placed> = sections.iter().zip(&refs).map(|(section, motion)| Placed { section, motion }).collect();
        let source = source("Neon \"Tide\"", &AssetRef::new("ast_clonecaaaaaaaaaaaaaaaaaaaa", rev.clone()), 20.0, bpm(&[0.0, 0.5, 1.0, 1.5]), &placed, 1920, 1080, 60);
        // The first scene runs a beat past its section, under the transition.
        assert!(source.contains(&format!("let s_intro1 = Graphic{{src: asset(\"ast_aaaaaaaaaaaaaaaaaaaaaaaaaa\", \"{rev}\") span: [0.000, 5.000] fit: @fill}}")), "{source}");
        assert!(source.contains(&format!("let s_verse1 = Graphic{{src: asset(\"ast_clonebaaaaaaaaaaaaaaaaaaaa\", \"{rev}\") span: [4.500, 15.500] fit: @fill}}")), "{source}");
        assert!(source.contains("let cut1 = Transition{a: s_intro1.out(@frame) b: s_verse1.out(@frame) effect: \"\" span: [4.500, 0.500]}"), "{source}");
        assert!(source.contains("let main = Stack{layers: [conn(1, cut1.out(@frame))]}"), "{source}");
        assert!(source.contains("audio: [conn(1, song.out(@audio))]"), "{source}");
        assert!(source.contains("bpm: 120\n"), "{source}");
        assert!(source.contains("label: \"Neon 'Tide'\""), "{source}");
        assert!(source.contains("s_intro1, s_verse1, cut1, song, main, out"), "{source}");
        let doc = makepad_story_cut::CutDoc::load(&source).unwrap_or_else(|error| panic!("{error:?}\n{source}"));
        assert!(doc.graphic("s_verse1").is_some() && doc.node("song").is_some() && doc.node("cut1").is_some());
    }
}
