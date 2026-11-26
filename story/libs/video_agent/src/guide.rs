//! What the model is told: the authoring guide (the same text as the
//! Claude Code skill Stage points at, `apps/apps/engine/motion/guide/SKILL.md`, so a person's
//! Claude Code and the in-app model learn one thing), the process, and the
//! brief.
//!
//! The system text depends only on whether the model sees pictures, never
//! on the brief, so a provider that caches the prompt prefix reuses it for
//! every generation; the brief and the documents go in the first message.

use crate::agent::{Brief, Doc, Kind};

/// The skill file, frontmatter and all.
pub const SKILL: &str = include_str!("../../../../../apps/engine/motion/guide/SKILL.md");

/// The 3D guide (lane R3) and the painting guide (Draw2D), which the
/// skill links and the model reads in full.
pub const MOTION3D: &str = include_str!("../../../../../apps/engine/motion3d/MOTION3D.md");
pub const CANVAS: &str = include_str!("../../../../../apps/engine/motion/guide/canvas.md");
/// How documents stay at 60 fps in the live preview.
pub const PERFORMANCE: &str = include_str!("../../../../../apps/engine/motion/guide/performance.md");
/// How a scene is written so it can be edited: a build program of one-record
/// calls on song anchors (the code timeline's rows; the variants' reference).
pub const PLOT: &str = include_str!("../../../../../apps/engine/motion/guide/plot.md");

/// How a deck is written (`mod.deck`): the DECK page's document.
pub const DECK: &str = include_str!("../../../../../apps/engine/motion/guide/deck.md");
/// The worked example of a deck: the Motion deck (Story's example deck), its slide list.
pub const DECK_EXAMPLE: &str = include_str!("../../../../stage/content/motion-deck/deck.splash");

/// The compact deck-editing reference: what the DECK chat reads before it
/// changes an existing deck (small enough for the AI Hub's local models).
pub const DECK_EDIT: &str = include_str!("../../../../../apps/engine/motion/guide/deck_edit.md");

/// A compact reference without its comment line.
pub fn compact(reference: &'static str) -> &'static str {
    reference.split_once("-->").map_or(reference, |(_, rest)| rest).trim_start()
}

/// The compact explainer reference: what a small-context model (the AI
/// Hub's local models, 16k tokens) gets in place of the full guide.
pub const EXPLAINER: &str = include_str!("../../../../../apps/engine/motion/guide/explainer.md");

/// The compact reference without its comment line.
pub fn compact_guide() -> &'static str {
    compact(EXPLAINER)
}

/// The system text for a small-context model: the role and process, and
/// the compact reference in place of the full guide and its references.
pub fn compact_system_prompt(brief: &Brief, vision: bool) -> String {
    let (role, process) = match brief.kind {
        Kind::Cut => (ROLE, PROCESS),
        Kind::Deck => (DECK_ROLE, DECK_PROCESS),
    };
    let deck = if brief.kind == Kind::Deck { format!("\n\n# Reference: editing a deck (compact)\n\n{}", compact(DECK_EDIT)) } else { String::new() };
    format!("{role}\n{process}\n{}\n# The authoring guide (compact)\n\n{}{deck}", if vision { LOOKING } else { BLIND }, compact_guide())
}

/// The references appended to every system text; the deck's
/// vocabulary, with its worked example, when the document is one.
fn references(kind: Kind) -> String {
    let canvas = CANVAS.split_once("-->").map_or(CANVAS, |(_, rest)| rest).trim_start();
    let performance = PERFORMANCE.trim().strip_prefix("# Fast by default").unwrap_or(PERFORMANCE).trim_start();
    let plot = PLOT.split_once("-->").map_or(PLOT, |(_, rest)| rest).trim_start();
    let plot = plot.split_once('\n').map_or(plot, |(_, rest)| rest).trim_start();
    let mut out = format!(
        "\n\n# Reference: 3D scenes (`Scene3D`)\n\n{}\n\n# Reference: painting with Draw2D\n\n{}\n\n# Reference: fast by default (60 fps in the preview)\n\n{}\n\n# Reference: editable scenes (a scene is a build program, not data)\n\n{}",
        MOTION3D.trim(),
        canvas.trim(),
        performance,
        plot.trim()
    );
    if kind == Kind::Deck {
        let deck = DECK.trim().strip_prefix("# Decks: DECK trees (`mod.deck`)").unwrap_or(DECK).trim_start();
        out.push_str(&format!("\n\n# Reference: decks (`mod.deck`)\n\n{deck}\n\n## Worked example: the Motion deck\n\n```splash\n{}\n```", DECK_EXAMPLE.trim()));
    }
    out
}

/// The guide without the skill's frontmatter.
pub fn authoring_guide() -> &'static str {
    let body = SKILL.strip_prefix("---\n").and_then(|rest| rest.find("\n---\n").map(|end| &rest[end + "\n---\n".len()..]));
    body.unwrap_or(SKILL).trim_start()
}

const ROLE: &str = "\
You are the video maker inside Stage's CUT page. You make short, 2D-animation-heavy videos — kinetic \
typography, shape and SVG motion, shader backgrounds, 3D hero shots, UI mockups, app demos with \
overlays — as ordinary Splash documents: Motion documents for the graphics and one cut that places \
them on the timeline with media, voiceover, music and subtitles. The person watches every step \
(your storyboard, each call, every document change as a diff, the pictures you look at, your \
critiques) and can edit anything by hand at any time; you are told about their edits and keep them.

Work autonomously: decide and go on; ask the person only at the workflow's choice points, with `ask`, and only when the brief says someone is watching. Aim for work a senior motion \
designer would ship, not merely something that loads.
";

const DECK_ROLE: &str = "\
You are the deck maker inside Stage's DECK page. A deck is a tree of Splash documents: deck.splash lists slides, \
each a looping cut (titles, effects, motion graphics, media, live apps) with transitions between them, \
notes for the presenter and, for a live app, a scripted demo. The person watches every change as a diff \
and can edit anything by hand at any time; you are told about their edits and keep them.

Work autonomously: decide and go on. Aim for a deck a senior designer would present, not merely one that \
loads: one idea per slide, large type, few words, motion that loops cleanly.
";

const DECK_PROCESS: &str = "\
# Process
1. Read the brief and the open tree (`tree_list`, then `doc_read` with each relevant path). If it already has slides, build on them and keep \
the person's text and order.
2. Plan the slides in your reply first: a name, the one idea and the picture of each, which need a live app \
(find its registry id, and get its widget paths with `app_dump` before writing a demo), and where the \
transitions differ.
3. Write files with `doc_create` / `doc_write`, or `doc_patch` edits whose `old` is the exact text of a whole node from `doc_read`. \
Every slide's nodes are `let`s with names unique in the file; copy the structure of the worked example in \
the reference.
4. `preview` the tree: it loads every dependency and renders one native frame per slide. Fix its path diagnostics, then `inspect` it. \
Live inputs need a presentation check as well.
5. `finish` with a two-line summary. It is refused until the deck inspects clean.
";

const PROCESS: &str = "\
# Process
1. Read the brief and the documents you are given. If a cut already has content, build on it.
2. Analyse the audio you will cut to (the music's beats, bars and sections; the voiceover's lines and \
words) as soon as it exists; generate music and voice early so the cut can follow them.
3. Plan with `storyboard`, choosing from the menus in the brief:
   - a style kit, whose palette, type pairing and motion language you then use;
   - scenes with ONE idea each, cut on downbeats and timed to the voice lines and the music's sections;
   - for each scene, its layer stack, back to front: a graphic (the kit's, a generated still, or a shader
     field), illustration or instanced detail (Vector, icons, Repeat, Chart), the type (with an
     animator), and 3D where it fits (Text3D, Seq3D, a Device mockup, an Fx);
   - the media each scene needs.
   A scene with type on a flat field and nothing else is not a plan.
4. Start media generation jobs (B-roll, backgrounds, music) before you write graphics; they run in \
parallel on the AI Hub nodes while you work. Use only presets whose models are installed on a node.
5. Write each scene's Motion document, then the cut that places them (Graphic nodes with exact \
`asset(\"id\", \"rev\")` references, media clips, VoiceOver and Subtitles, transitions).
6. Speak the voiceover, then retime scenes to the spoken lines and the beat.
7. Inspect (errors must be zero), LOOK (strips of the cut and of scenes, frames of key moments, \
onion trails for movement), and `critique` what you see against the design rules. Every critique \
checks, in the pictures: frame 0 shows content and no frame is empty; every line is inside the safe \
area, also at the peak of a punch; nothing overlaps (type, subtitles, cards, labels, buttons); each \
sentence holds still and readable for at least 1.5 s; punches land on spoken words or beats; the \
video ends on a card held at full brightness; subtitles (a `Subtitles` node, kept when asked for) sit in their band clear of headlines and end before the end card, and no caption repeats or covers the headline; no text overlaps other text (stack a headline and its sub \
lines by each Text's real height, see the guide); the first frame shows the hook.
   Pacing: each sentence lands and then holds, still, for at least 1.5 s; cuts and punches fall on \
beats and downbeats (`beat(n)`, `bar(n)`), not in between; entrances ease out in 0.3–0.6 s and \
exits ease in, faster. Prefer the kit's animators to hand-keyed tweens: they carry its motion \
language and their easing is tuned. Inspect warnings about the safe area, the frame, overflow or \
clipping are fixed like errors: inspect every Motion document you wrote, not only the cut, since the \
edit's inspect does not look inside them.
8. Revise with `patch_*` calls (small, exact changes) and repeat 7–8 until the critique says ship.
9. `finish` with a two-line summary. It is refused until the cut inspects clean and, when you \
can see pictures, you looked at the finished edit after the last change.

The storyboard is the input and the cut is its output, like a spec and the code built from it. \
Your `storyboard` plan is written to the board the person sees and edits. Build the cut from the \
board, and after the write phase record which card produced which nodes with \
`storyboard_mark_built` (explicit parts, or `infer: true`).

When the person asks for a change later (\"make scene 2 punchier\"), you get their board edits with \
the request. For those, run `storyboard_apply_changes`, then do each task it returns, touching only \
the nodes that task names. For the request itself, read the current documents first (they may have \
edited them), change only what was asked, and keep their hand edits in the cut; never overwrite \
them. Then go through 7–9 again and mark the parts you built.
";

const LOOKING: &str = "\
# Looking
You can see the pictures the feedback tools return: a strip is a labelled contact sheet, an onion \
trail blends frames of one movement. Judge them like a designer — hierarchy, legibility at phone \
size, safe zones, alignment, spacing, contrast, rhythm between scenes — and say concretely what to \
change (scene, time, layer, property, value) in `critique`. Every picture also comes with its \
`exposure` numbers: the share of clipped and of near-black pixels and the mean luma, measured on \
the full frame. Use them to catch a blown-out or crushed frame, but a dark or bright look that \
the brief asks for is not a fault. A picture with a Scene3D also has `coverage`: per node, its \
share of the scene's picture at each time (what is in front counted) and its box where largest, \
or null when it was never seen.
";

const BLIND: &str = "\
# Checking without pictures
You cannot see pictures. Judge from the inspect reports, the timeline and the documents: layers \
outside the frame or the safe zone, text that overflows, spans that overlap, pacing against the \
voice and the beat. The picture tools (`frame`, `strip`) still answer with each picture's \
`exposure` numbers: the share of clipped and of near-black pixels and the mean luma, and for a \
strip its worst frames. A frame mostly near black or clipped needs a look at its colours, unless \
the brief asks for that look. With a Scene3D they also give `coverage`: per node, its share of \
the scene's picture at each time (what is in front counted) and its screen box; a hero that is \
never seen or covers a few percent needs its camera, position or scale changed. Record that \
judgement with `critique`.
";

const TEAM: &str = "\
# Directing scene authors
For a long or chaptered video (a music video, anything over about 45 s, or many scenes with one \
look) you direct instead of writing every scene yourself:
1. Listen first: the music's beats, bars and sections, the lyric or voice word timings.
2. `storyboard` the chapters against that structure, then write the `style_guide`: palette, type, \
texture, characters and props, the shared modules (write them yourself first with `write_motion`; \
authors import them and never change them), timing helpers, chapter transitions, the performance \
budget, and what is off-limits.
3. `delegate` the chapters to scene authors in one call, so they work in parallel; each writes only \
its own Motion document. Give each its span, its beats and words, and what happens. For a lyric \
film (the guide's §4c), give each its lines with their sung word times, each line's allusion and pun, and \
the one accent, line language, type voices and running device values from the style guide.
4. When they answer, place their documents on the timeline (`add_graphic`), add the transitions, \
the lyrics or subtitles and the song, then inspect and look at the whole edit.
5. Send targeted fixes back to the authors who own the scenes (`delegate` with their ids), look \
again, and finish.
For a short video with a few scenes, write everything yourself.
";

/// The director's system text: the guide, the process and the team
/// process.
pub fn director_system(vision: bool) -> String {
    format!("{ROLE}\n{PROCESS}\n{TEAM}\n{}\n# The authoring guide\n\n{}{}", if vision { LOOKING } else { BLIND }, authoring_guide(), references(Kind::Cut))
}

const AUTHOR: &str = "\
You are a scene author inside Stage's CUT page. A director gave you one scene of a video and a \
style guide. You write exactly one Motion document — yours, by the name the director gave you — \
and nothing else: the shared modules and every other document are off-limits. Follow the style \
guide exactly (palette, type, characters, timing helpers, budget); the scenes of all authors must \
look like one film. Check your document with `inspect`, look at it (`strip`, `frame`, `onion`), \
`critique` it against the guide and the design rules, fix it, and `finish` with a one-line summary \
when it is clean and good. Never ask questions; decide.
";

/// A scene author's system text: the same guide, its own role (the guide
/// comes first, so a provider that caches the prefix shares it).
pub fn author_system(vision: bool) -> String {
    format!("# The authoring guide\n\n{}{}\n\n{}\n{AUTHOR}", authoring_guide(), references(Kind::Cut), if vision { LOOKING } else { BLIND })
}

/// A scene author's first message: the style guide first (the same text
/// for every author), then its scene.
pub fn author_message(style: &str, id: &str, motion: &str, brief: &str) -> String {
    let style = if style.trim().is_empty() { "(The director wrote no style guide; follow the design rules.)" } else { style.trim() };
    format!("# Style guide\n{style}\n\n# Your scene (you are `{id}`)\nYou own the Motion document `{motion}`: create it with `write_motion` (name `{motion}`), then revise it with `patch_motion`.\n\n{}", brief.trim())
}

/// The system text for a model that does or does not see pictures, for
/// the kind of document the brief's work lands in.
pub fn system_prompt(brief: &Brief, vision: bool) -> String {
    let (role, process) = match brief.kind {
        Kind::Cut => (ROLE, PROCESS),
        Kind::Deck => (DECK_ROLE, DECK_PROCESS),
    };
    format!("{role}\n{process}\n{}\n# The authoring guide\n\n{}{}", if vision { LOOKING } else { BLIND }, authoring_guide(), references(brief.kind))
}

/// The first message: the brief, and the documents as they are.
pub fn first_message(brief: &Brief, docs: &[Doc]) -> String {
    let mut out = format!("# Brief\n{}\n\n", brief.prompt.trim());
    if brief.kind == Kind::Deck {
        if !brief.style.trim().is_empty() {
            out.push_str(&format!("- Style: {}\n", brief.style.trim()));
        }
        out.push_str("- The person is watching in the Generate panel; decide yourself.\n");
        return finish_message(out, brief, docs, "\nStart by reading the deck, then plan its slides.");
    }
    out.push_str(&format!(
        "- Output: {}×{} ({}), {} fps, about {:.0} s.\n",
        brief.width,
        brief.height,
        brief.aspect(),
        brief.fps,
        brief.length
    ));
    if !brief.style.trim().is_empty() {
        out.push_str(&format!("- Style: {}\n", brief.style.trim()));
    }
    out.push_str(if brief.interactive {
        "- The person is watching in the Generate panel: for music videos and stories, stop at each step of the workflow with `ask` (treatments, character looks, the test shot); for short pieces, decide yourself.\n"
    } else {
        "- Nobody is watching this run: decide everything yourself, never ask.\n"
    });
    out.push_str(&match brief.voice.trim() {
        "" => "- Voiceover: none.\n".to_string(),
        voice => format!("- Voiceover: yes, voice `{voice}`, with subtitles.\n"),
    });
    out.push_str(&match brief.music.trim() {
        "" | "none" => "- Music: none.\n".to_string(),
        "generate" => "- Music: generate a track to the video's length and mood on the AI Hub, then cut to it.\n".to_string(),
        music => format!("- Music: {music}\n"),
    });
    out.push_str(if brief.generate_media {
        "- Media: generate B-roll, backgrounds and textures on the AI Hub where a scene needs them; library media is fine too.\n"
    } else {
        "- Media: use only the library and what you draw; do not generate media.\n"
    });
    finish_message(out, brief, docs, "\nStart with the audio analysis you need and the storyboard.")
}

fn finish_message(mut out: String, brief: &Brief, docs: &[Doc], start: &str) -> String {
    if !brief.menus.trim().is_empty() {
        out.push_str(&format!("\n# Menus (pick from these; the guide says how to use each)\n{}\n", brief.menus.trim()));
    }
    out.push_str("\n# Documents\n");
    if docs.is_empty() {
        out.push_str("None yet.\n");
    }
    for doc in docs {
        out.push_str(&format!("\n## {} (`{}`)\n```splash\n{}\n```\n", doc.label, doc.key, doc.source.trim_end()));
    }
    out.push_str(start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief() -> Brief {
        Brief { kind: Kind::Cut, prompt: "30 s launch video for Notes".into(), width: 1080, height: 1920, fps: 60, length: 30.0, style: "bold kinetic type".into(), voice: "bm_daniel".into(), music: "generate".into(), generate_media: true, menus: "Style kits: @swiss".into(), interactive: true }
    }

    #[test]
    fn the_guide_is_the_skill_without_frontmatter() {
        assert!(SKILL.starts_with("---\n"));
        assert!(!authoring_guide().starts_with("---"));
        assert!(authoring_guide().contains("Motion{"));
        let system = system_prompt(&brief(), true);
        assert!(system.contains("# Reference: 3D scenes") && system.contains("Scene3D{") && system.contains("# Reference: painting with Draw2D") && system.contains("cam2d_apply") && system.contains("# Reference: editable scenes") && system.contains("camera_key") && !system.contains("<!--"));
    }

    /// Every `Motion{…}` example in the guide loads without an error, and
    /// evaluates.
    #[test]
    fn the_guide_examples_load() {
        let mut checked = 0;
        for block in SKILL.split("```splash").skip(1) {
            let source = block.split("```").next().unwrap_or("");
            // 3D layers measure with the GPU side (motion's `draw`), which this
            // pure test does not link; their examples are checked by their lanes.
            if !source.trim_start().starts_with("use mod.motion.*\nMotion{") || source.contains("Text3D{") || source.contains("Scene3D{") {
                continue;
            }
            // Each example loads and evaluates on its own thread with a
            // deadline, so a loader or evaluator that never returns fails
            // this test naming the example instead of hanging the run.
            let (tx, rx) = std::sync::mpsc::channel();
            let text = source.to_string();
            std::thread::spawn(move || {
                let result = makepad_motion::MotionDoc::load(&text).map(|doc| {
                    let _ = doc.eval(doc.duration * 0.5);
                });
                let _ = tx.send(result);
            });
            match rx.recv_timeout(std::time::Duration::from_secs(20)) {
                Ok(Ok(())) => checked += 1,
                Ok(Err(diagnostics)) => panic!("a guide example does not load:\n{source}\n{}", diagnostics.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")),
                Err(_) => panic!("a guide example did not load and evaluate within 20 s (a loop in the loader or evaluator?):\n{source}"),
            }
        }
        assert!(checked >= 4, "{checked} examples");
    }

    /// Every example of the compact reference loads, evaluates, and passes
    /// the layout checks it teaches; the compact system text fits a 16k
    /// context with room for the conversation.
    #[test]
    fn the_explainer_reference_examples_load_and_inspect_clean() {
        let mut checked = 0;
        for block in EXPLAINER.split("```splash").skip(1) {
            let source = block.split("```").next().unwrap_or("");
            let doc = makepad_motion::MotionDoc::load(source).unwrap_or_else(|d| panic!("an explainer example does not load:\n{source}\n{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
            let times = makepad_motion::inspect::sample_times(&doc, 10.0);
            let issues = makepad_motion::inspect::layout_checks(&doc, &times, makepad_motion::inspect::DEFAULT_MIN_TEXT_PX);
            assert!(issues.is_empty(), "an explainer example has layout warnings:\n{source}\n{issues:#?}");
            checked += 1;
        }
        assert!(checked >= 3, "{checked} examples");
        let system = compact_system_prompt(&brief(), false);
        assert!(!system.contains("<!--") && system.contains("Plot{"));
        // About 3.5 bytes a token: under 9k tokens of system text.
        assert!(system.len() < 32_000, "{} bytes", system.len());
        let mut deck = brief();
        deck.kind = Kind::Deck;
        let system = compact_system_prompt(&deck, false);
        assert!(system.contains("# Reference: editing a deck") && system.len() < 40_000, "{} bytes", system.len());
    }

    /// The deck-editing reference's slide example loads with the deck's look
    /// module stood in, and the reference stays small.
    #[test]
    fn the_deck_edit_reference_example_loads() {
        let source = DECK_EDIT.split("```splash").nth(1).and_then(|block| block.split("```").next()).unwrap_or("");
        assert!(source.contains("Math{") && source.contains("look.BONE"));
        let look = "let look = {INK: #090a0d BONE: #f0ede6 ASH: #8d919a TITLE: Font{family: \"Roboto Flex\" weight: 600}}";
        let source = source.replace("let look = import(\"shared/makepad_deck_look\")", look);
        makepad_motion::MotionDoc::load(&source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
        assert!(compact(DECK_EDIT).len() < 12_000, "{} bytes", compact(DECK_EDIT).len());
    }

    /// The painting guide's example (modules, helpers and a Draw2D layer)
    /// loads and evaluates.
    #[test]
    fn the_painting_example_loads() {
        let source = CANVAS.split("```splash").nth(1).and_then(|block| block.split("```").next()).unwrap_or("");
        assert!(source.contains("Draw2D{"));
        let doc = makepad_motion::MotionDoc::load(source).unwrap_or_else(|diagnostics| panic!("{}", diagnostics.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
        let _ = doc.eval(3.0);
    }

    #[test]
    fn the_system_text_does_not_depend_on_the_brief() {
        let mut other = brief();
        other.prompt = "something else".into();
        assert_eq!(system_prompt(&brief(), true), system_prompt(&other, true));
        assert_ne!(system_prompt(&brief(), true), system_prompt(&brief(), false));
    }

    #[test]
    fn a_deck_brief_gets_the_deck_vocabulary_and_its_own_process() {
        let mut deck = brief();
        deck.kind = Kind::Deck;
        let system = system_prompt(&deck, true);
        assert!(system.contains("deck maker") && system.contains("# Reference: decks (`mod.deck`)") && system.contains("Deck{") && system.contains("tour"));
        assert!(!system_prompt(&brief(), true).contains("mod.deck"));
        let text = first_message(&deck, &[Doc { key: "deck".into(), label: "the deck".into(), source: "Deck{}".into() }]);
        assert!(text.contains("(`deck`)") && !text.contains("1080×1920") && text.contains("plan its slides"));
    }

    #[test]
    fn the_first_message_carries_the_brief_and_documents() {
        let text = first_message(&brief(), &[Doc { key: "cut".into(), label: "the cut".into(), source: "Flow{}".into() }]);
        assert!(text.contains("1080×1920 (9:16), 60 fps, about 30 s"));
        assert!(text.contains("voice `bm_daniel`"));
        assert!(text.contains("## the cut (`cut`)\n```splash\nFlow{}\n```"));
        assert!(text.contains("# Menus (pick from these; the guide says how to use each)\nStyle kits: @swiss"));
    }
}
