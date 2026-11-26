//! The Storyboard step's brief and answer (the film's style guide and one
//! scene plan per section), and each scene author's brief.

use crate::plan::{AssetRef, Production, ScenePlan, Section};

/// The director's instructions for the storyboard.
pub const SYSTEM: &str = "\
You direct a lyric video made of Motion scenes (Stage's Splash motion graphics), one scene per section of a \
song. You get the ask, the song's lyrics and its sections with their times and sung lines. Answer in exactly this \
form and nothing else (no code fences, no commentary):

STYLE:
<the style guide every scene author follows, 12-30 lines of Markdown: the film's look in a sentence; the palette \
as named #rrggbb colours with a colour arc across the sections (the warm accent saved for the payoff); the \
typefaces (bundled: Inter, Roboto Flex, IBM Plex Sans, JetBrains Mono, Playfair Display, Cormorant Garamond, \
Anton, Lexend Zetta) with sizes; the one running visual device that ties the scenes together; how a scene \
enters and leaves (on the bar line, never between); how type follows the sung words; what is off-limits>
SCENE <section name>: <the scene's title>
IDEA: <the one idea it carries, tied to its lines>
LOOK: <what is on screen and how it moves with the beat and the words, 1-3 sentences>
(one SCENE block per section, in order, using the section names exactly as given)

Rules: every scene shows its sung lines as kinetic type timed to the words; an instrumental section (no lines) \
is a title, a build or a breath, never empty. Abstract and typographic over literal; no real people, brands, \
logos or real-world conflicts.";

/// What the director is asked.
pub fn request(ask: &str, title: &str, caption: &str, sheet: &str, sections: &[Section], bpm: f64) -> String {
    let bar = if bpm > 0.0 { 240.0 / bpm } else { 2.0 };
    let mut text = format!(
        "The ask: {ask}\nThe song: \"{title}\" (the brief it was made from: {caption})\nMeasured in the song: {bpm:.0} BPM, a bar is {bar:.2} s \
         (use these, not the brief's tempo).\n\nThe lyrics (only the lines listed under the sections below are sung):\n{}\n\nThe sections:\n",
        sheet.trim()
    );
    for section in sections {
        text.push_str(&format!("- {} {:.2}-{:.2} s ({:.1} s)", section.name, section.start, section.end, section.end - section.start));
        if section.lines.is_empty() {
            text.push_str(": instrumental\n");
        } else {
            text.push_str(&format!(": {}\n", section.texts().join(" / ")));
        }
    }
    text
}

/// The director's answer: the style guide and a plan per section. Every
/// section gets a plan (one the answer left out gets a plain one).
pub fn parse(answer: &str, sections: &[Section]) -> Result<(String, Vec<ScenePlan>), String> {
    let mut style: Vec<&str> = Vec::new();
    let mut plans: Vec<ScenePlan> = Vec::new();
    let mut in_style = false;
    for raw in answer.lines() {
        let line = raw.trim_end();
        let bare = line.trim().trim_start_matches(['*', '#']).trim();
        if bare.starts_with("```") {
            continue;
        }
        if let Some(rest) = bare.strip_prefix("STYLE:") {
            in_style = true;
            if !rest.trim().is_empty() {
                style.push(rest.trim());
            }
            continue;
        }
        if let Some(rest) = bare.strip_prefix("SCENE ") {
            in_style = false;
            let (section, title) = rest.split_once(':').unwrap_or((rest, ""));
            plans.push(ScenePlan { section: section.trim().trim_matches(['*', '`']).to_string(), title: title.trim().trim_matches('*').trim().to_string(), ..ScenePlan::default() });
            continue;
        }
        if let Some(plan) = plans.last_mut() {
            if let Some(rest) = bare.strip_prefix("IDEA:") {
                plan.idea = rest.trim().to_string();
                continue;
            }
            if let Some(rest) = bare.strip_prefix("LOOK:") {
                plan.look = rest.trim().to_string();
                continue;
            }
            if !bare.is_empty() && !in_style {
                if !plan.look.is_empty() {
                    plan.look.push(' ');
                }
                plan.look.push_str(bare);
                continue;
            }
        }
        if in_style {
            style.push(line);
        }
    }
    let style = style.join("\n").trim().to_string();
    if style.lines().count() < 3 {
        return Err("the answer has no style guide (`STYLE:` and its lines)".into());
    }
    let mut out = Vec::with_capacity(sections.len());
    for section in sections {
        match plans.iter().find(|plan| plan.section == section.name) {
            Some(plan) => out.push(plan.clone()),
            None => out.push(ScenePlan {
                section: section.name.clone(),
                title: section.name.clone(),
                idea: if section.lines.is_empty() { "a breath between the sung parts".into() } else { section.texts().join(" / ") },
                look: "kinetic type of its lines on the beat, in the style guide's look".into(),
            }),
        }
    }
    if plans.is_empty() {
        return Err("the answer has no `SCENE <section>:` blocks".into());
    }
    Ok((style, out))
}

/// The Motion document name a section's scene is written as.
pub fn motion_name(title: &str, section: &str) -> String {
    let slug: String = title.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    let slug: Vec<&str> = slug.split('_').filter(|part| !part.is_empty()).collect();
    let slug = slug.join("_");
    let slug = if slug.is_empty() { "song".to_string() } else { slug.chars().take(24).collect() };
    format!("{slug}_{section}")
}

/// How every scene of a production is written: the rules a scene author
/// gets beside the style guide.
pub const SCENE_RULES: &str = "\
How this scene is written (on top of the authoring guide):
- Its clock: the Motion document's `t` runs from 0 at the section's start; the song second is `t + S0` \
with `let S0 = <the section's start>` written once at the top.
- Song-anchored timing: every time comes from the song timing module (`use mod.motion.modules.song.*` and \
`let S = import(asset(…))`, both given below): `song_line(S, \"<a word of the line>\", n)`, `line_start(L)`, \
`song_word_progress(L.words[i], t + S0)`, `song_beat(S, t + S0)`, `song_hit(S, \"kick\", t + S0, 0.12)`, \
`song_nearest_beat(S, …)`. Never paste word times, beat lists or lyric text into the document: the module \
holds them, and a re-timed song moves the scene with it.
- Understandable structure: named lets in one look section at the top (colours as #hex values, faces as \
`Font{…}`, sizes, durations in seconds), `///` labels on what a person would want to change, each value in \
one place; one draw or build function per kind of mark; no parallel index arrays.
- Generators over content: repeated or procedural marks come from seeded, parameterised functions (a count, a \
spread, a seed), not from long hand-written lists.
- Keep it plain: no wrapper layers or knobs for everything; one-off logic stays readable code.
- It must load and inspect clean, and read well at 1920x1080 on every frame of its span.";

/// A scene author's brief for one section.
pub fn scene_brief(section: &Section, plan: &ScenePlan, timing: &AssetRef, title: &str, width: u32, height: u32, tail: f64) -> String {
    let lines = if section.lines.is_empty() {
        "No sung lines: an instrumental stretch.".to_string()
    } else {
        format!(
            "Its sung lines, in order, with when they are sung (song seconds; key them from the module, never by hand):\n{}",
            section.lines.iter().map(|line| format!("- {:.2}-{:.2} s: {}", line.start, line.end, line.text)).collect::<Vec<_>>().join("\n")
        )
    };
    format!(
        "The scene for section `{name}` of the song \"{title}\": song seconds {start:.3} to {end:.3}, so the document is \
         {width}x{height} (`size: vec2({width}, {height})`) and lasts {len:.3} s (`duration: {len:.3}`: the section and \
         {tail:.2} s more, which the next scene dissolves over), with `let S0 = {start:.3}`.\nTitle: {scene_title}\nIdea: {idea}\nLook: {look}\n{lines}\n\nThe song timing module: \
         `use mod.motion.modules.song.*` then `let S = import(asset(\"{id}\", \"{rev}\"))`.\n\n{SCENE_RULES}",
        name = section.name,
        start = section.start,
        end = section.end,
        len = (section.end - section.start).max(0.5) + tail,
        scene_title = plan.title,
        idea = plan.idea,
        look = plan.look,
        id = timing.asset_id,
        rev = timing.revision,
    )
}

/// The storyboard of a planned production: the song as the music
/// reference (its tempo from the beats), the style guide as the bible's
/// page, the sections on the Sections row, every sung line on the Lyrics
/// row and one scene card per section on the Story row (its idea, look,
/// lyric and bars).
pub fn board_source(production: &Production, bpm: f64, first_downbeat: f64) -> String {
    use makepad_story_storyboard as sb;
    let duration = production.sections.last().map_or(production.song_seconds, |section| section.end).max(production.song_seconds).max(1.0);
    let mut board = sb::Storyboard {
        title: production.title.clone(),
        duration,
        bpm: (bpm > 0.0).then_some(bpm),
        beat_offset: first_downbeat.max(0.0),
        brief: production.ask.clone(),
        style: production.style.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim_start_matches('#').trim().to_string(),
        format: Some((production.width, production.height)),
        ..Default::default()
    }
    .with_default_rows();
    for kind in [sb::RowKind::Bible, sb::RowKind::Sections, sb::RowKind::Lyrics] {
        board.rows.insert(0, sb::Row { id: kind.tag().to_string(), kind, label: kind.label().to_string() });
    }
    board.rows.sort_by_key(|row| sb::RowKind::ALL.iter().position(|kind| *kind == row.kind));
    if production.song.is_set() {
        board.items.push(sb::Item {
            id: "song".into(),
            row: "music".into(),
            start: 0.0,
            length: duration,
            color: None,
            band: None,
            body: sb::Body::Ref(sb::Reference { media: Some(sb::Media::Audio), src: Some(sb::AssetRef { asset_id: production.song.asset_id.clone(), revision: production.song.revision.clone() }), label: production.title.clone(), note: production.caption.clone(), role: sb::Role::Use, ..Default::default() }),
        });
        board.music = Some("song".into());
    }
    board.items.push(sb::Item {
        id: "style_guide".into(),
        row: "bible".into(),
        start: 0.0,
        length: duration.min(12.0),
        color: None,
        band: None,
        body: sb::Body::Page(sb::Page { title: "Style guide".into(), body: production.style.clone(), refs: Vec::new() }),
    });
    let beat = if bpm > 0.0 { 60.0 / bpm } else { 0.5 };
    let bar_of = |t: f64| ((t - first_downbeat) / (4.0 * beat)).floor() as i64 + 1;
    for section in &production.sections {
        let length = (section.end - section.start).max(0.1);
        let id = format!("sec_{}", section.name);
        board.items.push(sb::Item { id: id.clone(), row: "sections".into(), start: section.start, length, color: None, band: None, body: sb::Body::Note { text: section.name.clone() } });
        for (k, line) in section.lines.iter().enumerate() {
            board.items.push(sb::Item {
                id: format!("{}_l{}", section.name, k + 1),
                row: "lyrics".into(),
                start: line.start,
                length: (line.end - line.start).max(0.1),
                color: None,
                band: None,
                body: sb::Body::Lyric(sb::Lyric { text: line.text.clone(), ..Default::default() }),
            });
        }
        let plan = production.plans.iter().find(|plan| plan.section == section.name);
        board.items.push(sb::Item {
            id: format!("scene_{}", section.name),
            row: "story".into(),
            start: section.start,
            length,
            color: None,
            band: None,
            body: sb::Body::Scene(sb::Scene {
                title: plan.map_or(section.name.clone(), |plan| plan.title.clone()),
                intent: plan.map_or(String::new(), |plan| plan.idea.clone()),
                look: plan.map_or(String::new(), |plan| plan.look.clone()),
                music: section.name.clone(),
                bars: format!("{}–{}", bar_of(section.start).max(1), bar_of(section.end - 0.01).max(1)),
                lyric: section.texts().join(" / "),
                ..Default::default()
            }),
        });
    }
    board.to_source()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(name: &str, start: f64, end: f64, lines: &[&str]) -> Section {
        Section { name: name.into(), start, end, lines: lines.iter().map(|s| crate::plan::LineSpan { text: s.to_string(), start, end }).collect() }
    }

    #[test]
    fn a_storyboard_answer_reads_as_style_and_a_plan_per_section() {
        let sections = vec![section("intro1", 0.0, 4.0, &[]), section("verse1", 4.0, 20.0, &["we ride the neon tide"]), section("chorus1", 20.0, 34.0, &["hold on"])];
        let answer = "STYLE:\n# Neon tide\n- palette: ink #0b0d12, cyan #3fe0ff, warm #ff8a3d (payoff only)\n- type: Anton for hooks, Inter 600 for verses\n\n**SCENE verse1: The Ride**\nIDEA: the city as a tide of light\nLOOK: horizontal light bands\nslide with the beat.\nSCENE chorus1: Hold\nIDEA: hold on\nLOOK: words stack and pulse on the kick";
        let (style, plans) = parse(answer, &sections).expect("reads");
        assert!(style.starts_with("# Neon tide") && style.lines().count() == 3, "{style}");
        assert_eq!(plans.len(), 3);
        assert_eq!(plans[0].section, "intro1", "a section the answer left out still gets a plan");
        assert_eq!(plans[1].title, "The Ride");
        assert_eq!(plans[1].look, "horizontal light bands slide with the beat.");
        assert_eq!(plans[2].idea, "hold on");
        assert_eq!(motion_name("Neon Tide!", "verse1"), "neon_tide_verse1");
    }

    #[test]
    fn the_board_holds_the_bible_sections_lyrics_and_a_card_per_section() {
        let mut p = Production::new("p", "a lyric video about tides", 30.0, None, 1);
        p.title = "Tides".into();
        p.song = AssetRef::new("ast_song", "arev_song");
        p.song_seconds = 30.0;
        p.style = "# Tides\n- palette: #0b0d12\n- type: Anton".into();
        p.sections = vec![section("intro1", 0.0, 4.5, &[]), section("verse1", 4.5, 30.0, &["we move with the moon"])];
        p.plans = vec![ScenePlan { section: "verse1".into(), title: "Moon".into(), idea: "pull".into(), look: "waves of type".into() }];
        let source = board_source(&p, 120.0, 0.5);
        let (board, warnings) = makepad_story_storyboard::Storyboard::load(&source).expect("loads");
        assert!(warnings.is_empty(), "{warnings:?}\n{source}");
        assert_eq!(board.items.iter().filter(|item| item.row == "story").count(), 2);
        assert_eq!(board.items.iter().filter(|item| item.row == "lyrics").count(), 1);
        assert_eq!(board.music.as_deref(), Some("song"));
        assert_eq!(board.bpm, Some(120.0));
        assert!(source.contains("waves of type"));
    }
}
