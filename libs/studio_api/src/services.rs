//! The control API as the chat's services: one AI service per target (the
//! CUT page, the graphic desk, DECK, SCORE, FLOW), each a family subset of
//! the catalogue, so a turn never sees all of its ~190 tools.
//!
//! A tool becomes a `ToolDef` of the chat's wire: a request (it makes
//! something: [`ToolSpec::request`]) is `Risk::Request` and travels with its
//! card layout, a tool that changes the document is `Act`, the rest `Read`.
//! This file is only the description and the answer; the host that runs the
//! tools (Stage's `studio_call`) lives in the app.

use crate::output::{CallResult, ErrorKind, Image};
use crate::spec::{Family, Layout, ToolSpec};
use makepad_ai_services::wire::{CardLayout, Risk, ServiceManifest, ToolDef, ToolResult, MAX_RESULT_IMAGE_BYTES};

/// The products a target belongs to: a window offers the targets of its
/// product and no others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Product {
    /// Makepad Story: CUT, the graphic desk, DECK.
    Story,
    /// Stage: VJ, DJ (and MIX).
    Stage,
    /// Synth: SCORE.
    Synth,
    /// Flow: the generation studio.
    Flow,
}

/// One target of the chat: a service id (`[a-z0-9_]`, what the target chip
/// names), what the person sees for it, what the model is told, and which
/// tools it offers.
pub struct Target {
    pub id: &'static str,
    /// The products that offer it.
    pub products: &'static [Product],
    pub label: &'static str,
    pub brief: &'static str,
    /// Every tool of these families ...
    pub families: &'static [Family],
    /// ... plus these by name and by name prefix ...
    pub with: &'static [&'static str],
    pub with_prefix: &'static [&'static str],
    /// ... less these by name and by name prefix.
    pub without: &'static [&'static str],
    pub without_prefix: &'static [&'static str],
}

/// The tools only the DECK page works on (the others it shares with CUT).
const DECK_TOOLS: &[&str] = &["new_deck", "add_layer", "reorder_slides", "set_deck_target", "set_deck_output_language", "deck_output_language", "add_app_slide", "preview", "open_deck", "add_slide", "move_slide", "delete_slide", "duplicate_slide", "set_slide", "set_demo", "add_app", "set_deck"];

/// The document-tree tools (Session family): a page with no document has no tree to list, read or patch.
const TREE_TOOLS: &[&str] = &["tree_list", "tree_search", "doc_read", "doc_patch", "doc_write", "doc_create", "doc_rename", "doc_delete", "doc_history", "doc_undo", "deck_output_language"];

/// The library, for the targets that take media from it.
const LIBRARY: &[&str] = &["search_library", "asset_info", "preview_asset", "import_media"];

pub const CUT: &str = "stage_cut";
pub const MOTION: &str = "stage_motion";
pub const DECK: &str = "stage_deck";
pub const SCORE: &str = "stage_score";
pub const FLOW: &str = "stage_flow";
pub const VJ: &str = "stage_vj";
pub const DJ: &str = "stage_dj";
/// The pages with no tools of their own (LIGHTS, the browsers): the session
/// and the library.
pub const PAGE: &str = "stage_page";

pub static TARGETS: [Target; 8] = [
    Target {
        id: CUT,
        products: &[Product::Story],
        label: "CUT",
        brief: "Stage's CUT page: the open cut, a timeline of clips, stills, titles, graphics, voice-overs and effects over a song, and the film made from it. \
Times are seconds on the cut's timeline. Read the cut (doc_read, timeline, stage_status) before changing it; every change is one undoable step. \
To make something (a video, a render, an export, a voice, subtitles, a new cut) call its tool: the person gets a settings card to edit and confirm, and nothing runs before. \
The film's editor panel (cut_panel_*) binds paths to the whole film's values: cut_panel_paths lists them, cut_panel_gesture sets one as a held gesture, cut_panel_make and cut_panel_write make the panel itself. \
The storyboard is the plan the cut is made from. Before writing a Splash document you have not written before, read the part of the authoring guide you need (authoring_guide). Look at the result with frame, strip and inspect before saying it is done.",
        families: &[Family::Session, Family::Cut, Family::Storyboard, Family::Audio, Family::Feedback, Family::Effects, Family::Assets, Family::AppRuns],
        with: &[],
        with_prefix: &["cut_panel_"],
        without: DECK_TOOLS,
        without_prefix: &["gen_", "lights_", "vj_video_", "vj_show_", "vj_deck_"],
    },
    Target {
        id: MOTION,
        products: &[Product::Story],
        label: "Graphic desk",
        brief: "Stage's graphic desk: Motion documents (a graphic is a Splash document that draws and animates over time), their variants, palette and editor panel, and the vector tools. \
Read a document (read_motion, graphic_source) before writing it; write_motion and patch_motion check that it loads and say why when it does not. \
Before writing a graphic you have not written before, read the part of the authoring guide you need (authoring_guide). \
Look at the result with frame and strip, and check it with inspect, before saying it is done.",
        families: &[Family::Motion, Family::Session],
        with: &["inspect", "frame", "strip", "onion", "critique", "search_library", "asset_info", "preview_asset", "import_media"],
        with_prefix: &[],
        without: &[],
        without_prefix: &[],
    },
    Target {
        id: DECK,
        products: &[Product::Story],
        label: "DECK",
        brief: "Stage's DECK page: the open deck, a tree of Splash files. deck.splash lists the slides; most slides are drawn by a Motion plate in slides/, the shared look (colours, fonts) is in shared/*_look.splash, and slides may host live apps. \
Before changing anything, read authoring_guide topic deck_edit once (short: how to find the file, edit it and check it). \
Find the file with tree_list (a plate's `slides` says which slide shows it), read it with doc_read (long files come in pages with an outline), and change it with doc_patch: each edit's `old` is the exact text of one whole node from doc_read, `new` replaces it. \
Slide tools (add_slide, move_slide, delete_slide, reorder_slides, set_slide, set_deck, set_deck_target, add_app_slide) change deck.splash for you; they number slides from 0, the person from 1. \
To translate the deck for the audience (\"translate the deck to Chinese\") add the zh of every shown text in the slide sources (tr({en: \"…\" zh: \"…\"})) with doc_patch; set_deck_output_language shows the output window in that language, the presenter's screen stays as written. \
Change only what was asked and keep the person's text. After a change call preview and fix every error it lists, then say in one sentence what you changed.",
        families: &[],
        with: &[
            "tree_list", "tree_search", "doc_read", "doc_patch", "doc_write", "doc_create", "doc_rename", "doc_delete", "doc_history", "doc_undo", "authoring_guide",
            "new_deck", "open_deck", "reorder_slides", "set_deck_target", "set_deck_output_language", "deck_output_language", "add_slide", "add_app_slide", "add_layer", "preview",
            "move_slide", "delete_slide", "duplicate_slide", "set_slide", "set_demo", "add_app", "set_deck",
            "inspect", "list_apps", "app_dump",
        ],
        with_prefix: &[],
        without: &[],
        without_prefix: &[],
    },
    Target {
        id: SCORE,
        products: &[Product::Synth],
        label: "SCORE",
        brief: "Stage's SCORE page: live-coded music. A score is a document the page plays; the person hears it while you edit it, and every change plays at once. Work only through the tools. \
First call score_guide once and read score.splash with doc_read(path), then make focused changes with doc_patch (small, source-preserving) or doc_write (a rewrite), and check each change with score_listen: you cannot hear, so read its levels, band balance, onset grid and notes, and fix what is off. \
Check the result with score_check. Keep what the person did not ask to change. Answer in one or two short sentences, in plain words a musician uses; never paste source into your reply. \
The score has one editor panel (a synthesiser UI written into the score): to change it, read authoring_guide (topic score_panel) and score_editor_paths, write it with score_editor_write, and check it with score_panel_status; score_editor_make makes a whole one at once. \
To write several songs, learn from the ratings, render to audio or start a new score, call score_batch, score_learn, score_render or score_new: the person gets a settings card first. The batch's songs show beside the code, where the person rates them (score_rate also rates); score_batch_status follows a batch and score_batch_stop ends it.",
        families: &[Family::Music, Family::Session],
        with: &[],
        with_prefix: &[],
        without: &[],
        without_prefix: &[],
    },
    Target {
        id: FLOW,
        products: &[Product::Flow],
        label: "FLOW",
        brief: "Stage's FLOW page: generation on the AI Hub, presets that make pictures, video, sound, music, speech, meshes and worlds. \
gen_presets lists what can be made; gen_run runs one (the person gets a settings card first) and publishes the result into the library; long jobs are followed with gen_job. \
Reuse what the library already has (search_library) instead of generating it twice.",
        families: &[],
        with: LIBRARY,
        with_prefix: &["gen_"],
        without: &[],
        without_prefix: &[],
    },
    Target {
        id: VJ,
        products: &[Product::Stage],
        label: "VJ",
        brief: "Stage's VJ page: the video decks and the effects that run on them, and the show palette, the four colours (ground, primary, secondary, tertiary) every show-following effect draws with. \
vj_palette_set picks the palette and what it follows (the person gets a settings card first); vj_palette_animate sets the palette animator's loop of colour keyframes. \
list_effects and read_effect show the effects and transitions that exist; write_effect authors one (read a similar one first, check it with effect_strip). Reuse library clips (search_library, import_media) instead of making them twice. \
For a themed VJ show, call vj_show_start. Stage continuously generates clips and mixes them on the beat. Use vj_show_steer to change direction, vj_show_status for progress and vj_show_stop to end it. \
To generate new video clips, call vj_video_generate: it opens one settings card for a single clip or a batch and publishes each finished clip straight into the VJ library. Call vj_video_status to inspect a batch or see which video models and hub nodes are ready, and vj_video_cancel to stop one clip or the whole batch. \
Use vj_deck_transport to control deck A, deck B, or both (forward, reverse, bounce, single, play/pause, speed, beat loops, IN/OUT, restart, seek), and vj_deck_status to read accepted deck transport state.",
        families: &[Family::Effects, Family::Session],
        with: LIBRARY,
        with_prefix: &["vj_video_", "vj_show_", "vj_deck_"],
        without: &[],
        without_prefix: &[],
    },
    Target {
        id: DJ,
        products: &[Product::Stage],
        label: "DJ",
        brief: "Stage's DJ page (and MIX): the two decks, the music library and the set list. dj_set_list makes, fills and reorders set lists, dj_auto turns Auto DJ (the automatic mix from deck to deck) on and sets its fade; \
music_bake analyses a track (beats, lyrics, stems; the person gets a settings card first) and music_bake_status follows it; analyze_music and beat_grid read what a baked track holds. \
Find tracks with search_library; asset ids come from there.",
        families: &[Family::Session],
        with: &["search_library", "asset_info", "preview_asset", "dj_set_list", "dj_auto", "music_bake", "music_bake_status", "analyze_music", "beat_grid"],
        with_prefix: &[],
        without: &[],
        without_prefix: &[],
    },
    Target {
        id: PAGE,
        products: &[Product::Story, Product::Stage, Product::Synth, Product::Flow],
        label: "Stage",
        brief: "A Stage page with no tools of its own, and LIGHTS: look at what Stage has open (stage_status), search the library (search_library) and show or open a file Stage wrote. \
LIGHTS: the house DMX patch (lights_fixtures), the universe by hand (lights_dmx), the camera that watches the rig (lights_camera), fog (lights_fog), a message to the person (lights_ask_user) and DMX indexing (lights_index), which learns what every channel of a fixture does from the camera and the AI hubs' vision model.",
        families: &[Family::Session],
        with: &["search_library", "asset_info"],
        with_prefix: &["lights_"],
        without: TREE_TOOLS,
        without_prefix: &[],
    },
];

impl Target {
    /// Whether `spec` is one of this target's tools.
    pub fn has(&self, spec: &ToolSpec) -> bool {
        let name = spec.name;
        let by_family = self.families.contains(&spec.family) && !self.without.contains(&name) && !self.without_prefix.iter().any(|prefix| name.starts_with(prefix));
        by_family || self.with.contains(&name) || self.with_prefix.iter().any(|prefix| name.starts_with(prefix))
    }

    /// The service of this target over `specs` (the tools the host
    /// implements: the table's and the ones it adds).
    pub fn manifest<'a>(&self, specs: impl IntoIterator<Item = &'a ToolSpec>) -> Result<ServiceManifest, String> {
        let mut manifest = ServiceManifest::new(self.id, format!("Stage {}", self.label), self.brief);
        for spec in specs.into_iter().filter(|spec| self.has(spec)) {
            manifest = manifest.with_tool(tool_def(spec));
        }
        manifest.validate()?;
        Ok(manifest)
    }
}

/// The targets a window of `products` offers.
pub fn targets_of(products: &[Product]) -> impl Iterator<Item = &'static Target> + '_ {
    TARGETS.iter().filter(move |target| target.products.iter().any(|product| products.contains(product)))
}

/// The target a service id names.
pub fn target(id: &str) -> Option<&'static Target> {
    TARGETS.iter().find(|target| target.id == id)
}

/// How much a tool can break: a request is parked as a settings card, a
/// change to the document runs at once (it is one undo step), the rest look.
pub fn risk(spec: &ToolSpec) -> Risk {
    if spec.request {
        Risk::Request
    } else if spec.mutates {
        Risk::Act
    } else {
        Risk::Read
    }
}

pub fn card_layout(layout: &Layout) -> CardLayout {
    let mut out = CardLayout::new(layout.title, layout.confirm);
    for (arg, label) in layout.order {
        out = out.field(*arg, *label);
    }
    for arg in layout.hide {
        out = out.hidden(*arg);
    }
    out
}

/// One catalogue tool as the chat's tool.
pub fn tool_def(spec: &ToolSpec) -> ToolDef {
    let def = ToolDef::new(spec.name, spec.description, spec.input, risk(spec));
    match &spec.layout {
        Some(layout) => def.with_layout(card_layout(layout)),
        None => def,
    }
}

/// The transcript line of a result: where it is when it has an address or a
/// folder (the web export), else what the tool said about it.
fn note_of(json: &makepad_strict_json::Value) -> String {
    let text = |key: &str| json.get(key).and_then(|value| value.as_str());
    match (text("local_url"), text("out_dir")) {
        (Some(url), _) => match text("lan_url") {
            Some(lan) => format!("done · {url} · on the network {lan}"),
            None => format!("done · {url}"),
        },
        (None, Some(dir)) => format!("done · {dir}"),
        _ => ["note", "state", "message"].iter().find_map(|key| text(key)).unwrap_or("").to_string(),
    }
}

/// A finished call as the chat's answer, and the pictures it made (the
/// host hands them to the model with the answer).
pub fn answer(call_id: &str, result: &CallResult) -> (ToolResult, Vec<Image>) {
    match result {
        Ok(output) => {
            let text = output.json.to_json();
            let mut reply = ToolResult::ok(call_id, text, note_of(&output.json));
            let mut budget = MAX_RESULT_IMAGE_BYTES;
            for image in &output.images {
                // The local panel receives the original PNG. Remote clients
                // receive images that fit the service's result budget.
                let bytes = image.png.len().div_ceil(3).saturating_mul(4);
                if bytes > budget {
                    reply.text.push_str("\n…[a picture exceeds the remote result budget]");
                    continue;
                }
                budget -= bytes;
                reply.images.push(crate::base64::encode(&image.png));
            }
            (reply, output.images.clone())
        }
        Err(error) => {
            let message = format!("{}: {}", error.kind.id(), error.message);
            let result = match error.kind {
                ErrorKind::Failed => ToolResult::failed(call_id, message),
                ErrorKind::NotAvailable => ToolResult::unavailable(call_id, message),
                ErrorKind::UnknownTool | ErrorKind::InvalidArgs | ErrorKind::NotFound | ErrorKind::Refused => ToolResult::refused(call_id, message),
            };
            (result, Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::TOOLS;
    use makepad_ai_services::wire::MAX_TOOLS;

    #[test]
    fn every_target_is_a_valid_service_and_no_turn_sees_the_whole_catalogue() {
        for target in &TARGETS {
            let manifest = target.manifest(TOOLS.iter()).unwrap_or_else(|error| panic!("{}: {error}", target.id));
            assert!(!manifest.tools.is_empty(), "{}", target.id);
            assert!(manifest.tools.len() <= MAX_TOOLS && manifest.tools.len() < TOOLS.len() * 2 / 3, "{}: {} of {} tools", target.id, manifest.tools.len(), TOOLS.len());
        }
    }

    #[test]
    fn vj_service_manifest_exposes_vj_video_tools_and_not_flow_gen() {
        let manifest = target(VJ).unwrap().manifest(TOOLS).unwrap();
        for (name, risk, has_layout) in [
            ("vj_video_generate", Risk::Request, true),
            ("vj_video_status", Risk::Read, false),
            ("vj_video_cancel", Risk::Act, false),
            ("vj_show_start", Risk::Request, true),
            ("vj_show_steer", Risk::Act, false),
            ("vj_show_stop", Risk::Act, false),
            ("vj_show_status", Risk::Read, false),
            ("vj_deck_transport", Risk::Act, false),
            ("vj_deck_status", Risk::Read, false),
        ] {
            let tool = manifest.tool(name).unwrap_or_else(|| panic!("VJ is missing {name}"));
            assert_eq!((tool.risk, tool.layout.is_some()), (risk, has_layout), "{name}");
        }
        assert!(manifest.tools.iter().all(|tool| !tool.name.starts_with("gen_")));
        for other in TARGETS.iter().filter(|t| t.id != VJ) {
            let manifest = other.manifest(TOOLS).unwrap();
            assert!(manifest.tools.iter().all(|tool| !tool.name.starts_with("vj_video_")), "{} offers VJ video tools", other.id);
            assert!(manifest.tools.iter().all(|tool| !tool.name.starts_with("vj_deck_")), "{} offers VJ deck tools", other.id);
            assert!(manifest.tools.iter().all(|tool| !tool.name.starts_with("vj_show_")), "{} offers VJ show tools", other.id);
        }
    }

    #[test]
    fn every_tool_is_offered_by_some_target() {
        for spec in TOOLS.iter() {
            assert!(TARGETS.iter().any(|target| target.has(spec)), "{} is in no target", spec.name);
        }
    }

    #[test]
    fn requests_are_exactly_the_tools_with_a_card_layout() {
        for spec in TOOLS.iter() {
            assert_eq!(spec.request, spec.layout.is_some(), "{}", spec.name);
            assert_eq!(risk(spec) == Risk::Request, spec.request, "{}", spec.name);
        }
        assert!(TOOLS.iter().filter(|spec| spec.request).count() >= 27);
        let make = crate::find("make_video").unwrap();
        assert_eq!(risk(make), Risk::Request);
        assert_eq!(tool_def(make).layout.unwrap().title, "Make a video");
        assert_eq!(risk(crate::find("add_clip").unwrap()), Risk::Act);
        assert_eq!(risk(crate::find("timeline").unwrap()), Risk::Read);
    }

    #[test]
    fn a_result_with_an_address_or_a_folder_says_where() {
        use crate::output::ToolOutput;
        let parse = |text: &str| makepad_strict_json::parse(text.as_bytes()).unwrap();
        let note = |text: &str| answer("c1", &Ok(ToolOutput::json(parse(text)))).0.note;
        assert_eq!(note(r#"{"out_dir": "/m/Film web", "local_url": "http://127.0.0.1:9/", "lan_url": "http://192.0.2.2:9/"}"#), "done · http://127.0.0.1:9/ · on the network http://192.0.2.2:9/");
        assert_eq!(note(r#"{"out_dir": "/m/Film web"}"#), "done · /m/Film web");
        assert_eq!(note(r#"{"started": true, "state": "rendering"}"#), "rendering");
        let (failed, _) = answer("c2", &Err(crate::output::ToolError::refused("no cut is open")));
        assert_eq!((failed.outcome, failed.text.as_str()), (makepad_ai_services::wire::ToolOutcome::Refused, "refused: no cut is open"));
    }
}
