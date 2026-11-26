//! Storyboards: the 2D board where a person and the AI work out a video's
//! story before (and while) the cut is made from it. Time runs across,
//! rows run down (story, visuals, voiceover, music, references), and the
//! items are scene cards, short notes and references placed by time.
//!
//! The board is the input and the cut its output, as a spec is to its
//! generated code: the board records which of its revisions built the
//! edit and which edit nodes each card made ([`model::Built`]), so a
//! changed board updates only the parts of the cut that changed
//! ([`build::plan`]), mechanically where it can and as small scoped tasks
//! for the AI where it needs judgement; hand edits in the cut are kept.
//!
//! - [`model`]: the typed board and its Splash (`Storyboard::load`,
//!   `to_source`).
//! - [`splash`]: the Splash reader with byte ranges, and the patchers.
//! - [`cut`]: changes as source-preserving patches ([`edit::Op`]).
//! - [`frame`]: frame layouts (Shorts framing) with bands and safe zones.
//! - [`layout`]: the canvas's geometry: lanes, hits, marquee, ruler, snaps.
//! - [`diff`]: what changed between two boards, in words for the AI.
//! - [`build`]: provenance and the incremental update plan for the cut.
//! - [`tools`]: the AI control API's pure storyboard tools.

pub mod build;
pub mod diff;
pub mod edit;
pub mod frame;
pub mod layout;
pub mod model;
pub use makepad_splash_syntax as splash;
pub mod tools;

pub use model::{AssetRef, Body, Built, Chapter, Device, Diagnostic, Gen, Item, Link, Lyric, Media, Page, Part, Reference, Role, Row, RowKind, Scene, Storyboard, Treatment, TypeVoice};

/// A scene as the Generate agent plans it (lane M3's `storyboard` tool):
/// the fields it keeps, mapped onto a board by [`from_scenes`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlannedScene {
    pub id: String,
    pub start: f64,
    pub length: f64,
    /// The one idea (the card's intent).
    pub idea: String,
    pub look: String,
    /// The voiceover line over it: a note on the voice row.
    pub voice: String,
    pub music: String,
    pub media: String,
    pub stack: String,
}

/// A board from the Generate agent's plan: one scene card per scene on the
/// story row (titled by its id when the plan gives no title), its voice
/// line as a note on the voice row over the scene (inset a little, so
/// lines do not butt), the title, style and kit on the board.
pub fn from_scenes(title: &str, style: &str, kit: &str, scenes: &[PlannedScene]) -> String {
    let duration = scenes.iter().map(|scene| scene.start + scene.length).fold(0.0, f64::max).max(1.0);
    let mut board = Storyboard { title: title.to_string(), duration, style: style.to_string(), kit: kit.to_string(), ..Default::default() }.with_default_rows();
    for (index, scene) in scenes.iter().enumerate() {
        let id = if splash::valid_name(&scene.id) && board.item(&scene.id).is_none() && board.row(&scene.id).is_none() { scene.id.clone() } else { board.fresh_id("scene") };
        let title = if scene.id.is_empty() { format!("Scene {}", index + 1) } else { scene.id.replace('_', " ") };
        board.items.push(Item {
            id: id.clone(),
            row: "story".into(),
            start: scene.start.max(0.0),
            length: scene.length.max(0.1),
            color: None,
            band: None,
            body: Body::Scene(Scene {
                title,
                intent: scene.idea.clone(),
                look: scene.look.clone(),
                kit: kit.to_string(),
                music: scene.music.clone(),
                media: scene.media.clone(),
                stack: scene.stack.clone(),
                ..Default::default()
            }),
        });
        if !scene.voice.trim().is_empty() {
            let inset = (scene.length * 0.05).min(0.25);
            let note = board.fresh_id(&format!("{id}_vo"));
            board.items.push(Item { id: note, row: "voice".into(), start: scene.start.max(0.0) + inset, length: (scene.length - 2.0 * inset).max(0.1), color: None, band: None, body: Body::Note { text: scene.voice.trim().to_string() } });
        }
    }
    board.to_source()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic board: a 20 s product launch with five scenes,
    /// voiceover, four references and music with beats.
    pub const LAUNCH: &str = include_str!("../fixtures/launch.splash");

    #[test]
    fn the_launch_board_loads_and_round_trips() {
        let (board, warnings) = Storyboard::load(LAUNCH).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(board.scenes().len(), 5);
        assert_eq!(board.items.iter().filter(|item| matches!(item.body, Body::Ref(_)) && item.row == "refs").count(), 4);
        assert_eq!(board.bpm, Some(120.0));
        assert_eq!(board.music.as_deref(), Some("song"));
        // Canonical writing loads to the same board, and writing that again
        // gives the same text.
        let written = board.to_source();
        let (again, _) = Storyboard::load(&written).unwrap();
        assert_eq!(again, board);
        assert_eq!(again.to_source(), written);
        assert_eq!(board.revision(), again.revision());
        assert_eq!(board.dependencies().len(), 3);
    }

    pub const SHOWREEL: &str = include_str!("../fixtures/showreel_short.splash");

    #[test]
    fn the_showreel_short_plans_chapters_on_the_beat_in_the_shorts_frame() {
        let (board, warnings) = Storyboard::load(SHOWREEL).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(board.chapters().len(), 9);
        // Every chapter starts and ends on the beat (or half-beat) grid.
        assert!(board.chapters().iter().all(|chapter| board.on_beat(chapter.start) && board.on_beat(chapter.end())));
        let frame = board.frame().unwrap();
        // Everything with words sits in a band that may hold type, or is
        // the series tag / the floor note (decoration).
        for item in &board.items {
            if let Some(band) = item.band.as_deref().and_then(|band| frame.band(band)) {
                assert!(band.text || matches!(band.id, "tag" | "floor"), "{}", item.id);
            }
        }
        let written = board.to_source();
        assert_eq!(Storyboard::load(&written).unwrap().0, board);
    }

    pub const LYRIC_FILM: &str = include_str!("../fixtures/lyric_film.splash");

    #[test]
    fn a_lyric_film_has_lines_voices_and_running_devices() {
        let (board, warnings) = Storyboard::load(LYRIC_FILM).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(board.lyrics().len(), 8);
        assert_eq!(board.voices.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), ["slam", "wall", "mono", "serif", "hand"]);
        assert_eq!(board.voices, model::lyric_film_voices().into_iter().map(|mut v| { v.role = board.voice(&v.id).unwrap().role.clone(); v }).collect::<Vec<_>>());
        let devices = board.devices();
        let tide = &devices.iter().find(|(name, _)| name == "Tide").unwrap().1;
        assert_eq!(tide.iter().map(|(_, _, value)| value.as_str()).collect::<Vec<_>>(), ["0.04", "0.15", "0.16 → 0.17"]);
        assert_eq!(board.line_number("l4"), Some(4));
        let written = board.to_source();
        assert_eq!(Storyboard::load(&written).unwrap().0, board);
        // Lines are parts of the cut; devices are plans.
        let built = crate::build::infer(&board, &[], AssetRef { asset_id: "e".into(), revision: "r".into() });
        let (_, unbuilt) = crate::build::stale(&Storyboard { built: Some(built), ..board.clone() });
        assert_eq!(unbuilt.len(), 8);
        assert!(unbuilt.iter().all(|id| id.starts_with('l')));
    }

    #[test]
    fn a_written_board_is_splash() {
        // Every board evaluates in the real Splash VM, with the storyboard
        // types as plain prototypes.
        use makepad_script::*;
        let (board, _) = Storyboard::load(LAUNCH).unwrap();
        let (reel, _) = Storyboard::load(SHOWREEL).unwrap();
        let (film, _) = Storyboard::load(LYRIC_FILM).unwrap();
        for source in [LAUNCH.to_string(), board.to_source(), SHOWREEL.to_string(), reel.to_source(), LYRIC_FILM.to_string(), film.to_source()] {
            let mut host = Box::new(ScriptVmHost::new((), ()));
            let bx = Box::new(ScriptVmBase::new());
            let mut vm = ScriptVm {
                host: host.host_context(),
                bx,
            };
            let module = vm.new_module(id!(storyboard));
            for name in ["Row", "Scene", "Chapter", "Treatment", "Page", "Lyric", "Device", "Voice", "Note", "Ref", "Link", "Storyboard", "Built", "Part"] {
                let proto = vm.bx.heap_mut().new_object();
                vm.bx.heap_mut().set_value_def(module, LiveId::from_str(name).into(), proto.into());
            }
            let prelude = "mod.storyboard.asset = fn(id, rev) { {asset: id rev: rev} }\nnil\n";
            vm.capture_errors();
            vm.eval(ScriptMod::new("", "", "mod.storyboard", 0, 0, &prelude, &[]));
            let code = format!("use mod.pod.*; {}", source.replacen("use mod.storyboard.*", "use mod.storyboard.*;", 1));
            vm.eval(ScriptMod::new("", "", "board.splash", 0, 0, &code, &[]));
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "{errors:?}");
        }
    }

    #[test]
    fn a_generate_plan_becomes_a_board() {
        let scenes = [
            PlannedScene { id: "hook".into(), start: 0.0, length: 3.5, idea: "one tap".into(), voice: "Ideas move fast.".into(), stack: "plate · type".into(), ..Default::default() },
            PlannedScene { id: "demo".into(), start: 3.5, length: 6.0, idea: "the app".into(), ..Default::default() },
        ];
        let source = from_scenes("Notes", "warm", "@swiss", &scenes);
        let (board, warnings) = Storyboard::load(&source).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(board.scenes().iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), ["hook", "demo"]);
        assert_eq!(board.duration, 9.5);
        let note = board.items.iter().find(|item| item.row == "voice").unwrap();
        assert_eq!((note.id.as_str(), note.headline().as_str()), ("hook_vo1", "Ideas move fast."));
        assert!(note.start > 0.0 && note.end() < 3.5);
    }
}
