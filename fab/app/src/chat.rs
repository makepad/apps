//! The window's chat (the shared product shell's aichat dock) and the calls
//! it, external agents and Fab's own commands make into the model worker.
//!
//! One manifest (`fab_ai::manifest`) is linked into the process's service
//! registry: the docked chat's model calls it, and so does an outside agent
//! through `makepad-remote mcp <pid>`. Each call becomes a worker job; the
//! answer (and for `render`, the review sheets) comes back here and goes
//! out on the next pump.

use makepad_ai_services::pictures::{self as picture_fit, RgbaPicture};
use makepad_ai_services::wire::{ServiceManifest, TaskCard, ToolResult, MAX_RESULT_BYTES, MAX_RESULT_IMAGE_BYTES};
use makepad_ai_hub::providers::provider::MAX_TOOL_IMAGE_SIDE;
use makepad_ai_services::{Suggestion, Target};
use makepad_base64::{base64_encode, BASE64_STANDARD};
use makepad_model::RgbaImage;
use std::collections::HashMap;
use std::sync::Arc;

/// A finished call, waiting for the chat to collect it.
pub struct Answer {
    pub ok: bool,
    pub text: String,
    pub images: Vec<(String, Arc<[u8]>)>,
}

#[derive(Default)]
pub struct Calls {
    /// Calls the chat made that are still running: their tool.
    pub pending: HashMap<u64, String>,
    pub answers: HashMap<u64, Answer>,
    /// A finished image-to-mesh call's card, kept until its answer goes out.
    pub tasks: HashMap<u64, TaskCard>,
}

impl Calls {
    pub fn finish(&mut self, id: u64, answer: Answer) {
        if self.pending.remove(&id).is_some() {
            self.answers.insert(id, answer);
        }
    }
}

pub fn services() -> Vec<ServiceManifest> {
    register_elec_actions();
    vec![fab_ai::manifest()]
}

/// The electronics actions the app runs (`ElecHost::run_action`), in the action catalog once, so
/// the manifest's electronics tools offer them (`fab_ai::ELEC_TOOLS`). A lane's actions go here
/// when the app runs them. O(catalog) the first time, an atomic check after.
pub fn register_elec_actions() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        fab_elec_mech::read::register_actions();
        fab_elec_host::edit_actions::register_actions();
        fab_elec_cam::actions::register_actions();
        fab_elec_design::action::register(crate::elec_host::SIM_ACTIONS);
        fab_elec_design::action::register(crate::elec_host::FW_ACTIONS);
        fab_elec_design::action::register(crate::elec_host::SCOPE_ACTIONS.as_slice());
    });
}

/// The tool's short name, whatever form the caller used (`apply`,
/// `fab.apply`, `fab__apply`).
pub fn short_tool(tool: &str) -> &str {
    tool.strip_prefix("fab.").or_else(|| tool.strip_prefix("fab__")).unwrap_or(tool)
}

pub fn target(name: &str, parts: usize, issues: usize) -> (Target, Vec<Suggestion>) {
    let context = format!("Fab is open on the model '{name}': {parts} part(s) on the plate, {issues} with placement issues. Use the fab tools (start with fab.guide).");
    let target = Target { label: format!("Fab · {name}"), app: fab_ai::SERVICE.into(), context, attachments: Vec::new() };
    let prompt = Suggestion::prompt;
    let suggestions = if parts == 0 {
        vec![
            prompt("A wall bracket", "Model a printable wall bracket with two screw holes, 4 mm walls."),
            prompt("A cable clip", "Model a cable clip for a 6 mm cable that screws to a desk."),
        ]
    } else {
        vec![prompt("Check printability", "Render the plate and check it for thin walls, overhangs and placement issues; fix what you find.")]
    };
    (target, suggestions)
}

/// Pictures as PNGs whose base64 fits in `budget` bytes together, none more than the chat's
/// [`MAX_TOOL_IMAGE_SIDE`] pixels a side, scaled down alike as little as fits them
/// ([`picture_fit::fit_pictures`]): each a grid of equal cells (columns, rows) that stays whole
/// cells, `mark` painting on each at each size tried what
/// should stay crisp (words, marks), given its index and its cells' size.
pub fn fit_pictures(pictures: &[(RgbaImage, [u32; 2])], budget: usize, mut mark: impl FnMut(usize, &mut RgbaImage, [u32; 2])) -> Result<Vec<(Vec<u8>, [u32; 2])>, String> {
    let pictures: Vec<(RgbaPicture, [u32; 2])> = pictures.iter().map(|(image, grid)| (RgbaPicture { width: image.width, height: image.height, rgba: image.pixels.clone() }, *grid)).collect();
    picture_fit::fit_pictures(&pictures, budget, MAX_TOOL_IMAGE_SIDE, |i, picture, cell| {
        let mut image = RgbaImage { width: picture.width, height: picture.height, pixels: std::mem::take(&mut picture.rgba) };
        mark(i, &mut image, cell);
        picture.rgba = image.pixels;
    })
}

/// A PNG's size from its header; None: no PNG.
fn png_size(png: &[u8]) -> Option<[u32; 2]> {
    let at = |k: usize| Some(u32::from_be_bytes(png.get(k..k + 4)?.try_into().ok()?));
    png.starts_with(b"\x89PNG\r\n\x1a\n").then_some(())?;
    Some([at(16)?, at(20)?])
}

/// The reply for the chat (or an outside agent): bounded text and pictures
/// within the reply's picture budget, none more than [`MAX_TOOL_IMAGE_SIDE`] pixels a side.
pub fn tool_result(call_id: &str, answer: Answer) -> ToolResult {
    let mut text = answer.text;
    if text.len() > MAX_RESULT_BYTES {
        let mut end = MAX_RESULT_BYTES - 64;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("… [result truncated]");
    }
    let mut result = if answer.ok { ToolResult::ok(call_id, text, "") } else { ToolResult::failed(call_id, text) };
    // Their labels are in the reply's text, in this order. Each gets a fair share of the budget:
    // the smaller first, so what one leaves goes to the rest; one over its share shrinks to fit
    // it, as one larger than the chat takes does. None is dropped to make room for another. (A
    // render's sheets come fitted together.)
    let base64 = |png: &[u8]| String::from_utf8(base64_encode(png, &BASE64_STANDARD)).unwrap_or_default();
    let mut images: Vec<String> = answer.images.iter().map(|(_, png)| base64(png)).collect();
    let mut order: Vec<usize> = (0..images.len()).collect();
    order.sort_by_key(|&i| images[i].len());
    let mut budget = MAX_RESULT_IMAGE_BYTES;
    let mut fitted = vec![false; images.len()];
    for (k, &i) in order.iter().enumerate() {
        let share = budget / (images.len() - k);
        let large = png_size(&answer.images[i].1).is_none_or(|[w, h]| w.max(h) > MAX_TOOL_IMAGE_SIDE);
        if images[i].len() > share || large {
            match picture_fit::decode_png(&answer.images[i].1).and_then(|image| picture_fit::fit_pictures(&[(image, [1, 1])], share, MAX_TOOL_IMAGE_SIDE, |_, _, _| {})) {
                Ok(pngs) => images[i] = base64(&pngs[0].0),
                Err(e) => {
                    result.text.push_str(&format!("\n[picture {} left out: {e}]", i + 1));
                    continue;
                }
            }
        }
        budget -= images[i].len();
        fitted[i] = true;
    }
    result.images = images.into_iter().zip(fitted).filter_map(|(data, fitted)| fitted.then_some(data)).collect();
    result
}

/// Pictures as the chat's provider takes them, through [`tool_result`] and then the provider's
/// check of a tool's pictures: their sizes, or why it refuses them.
#[cfg(test)]
pub fn as_the_provider_takes(pictures: Vec<(String, Vec<u8>)>) -> Result<Vec<[u32; 2]>, String> {
    use makepad_ai_hub::providers::provider::{validate_tool_images, ToolImage};
    let images = pictures.into_iter().map(|(label, png)| (label, Arc::from(png))).collect();
    let result = tool_result("call", Answer { ok: true, text: String::new(), images });
    let images: Vec<ToolImage> = result.images.iter().map(|b| ToolImage { label: "picture".into(), png: Arc::from(makepad_base64::base64_decode(b.as_bytes()).unwrap()) }).collect();
    validate_tool_images(&images)?;
    Ok(images.iter().map(|i| png_size(&i.png).expect("a PNG")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action the app registers is offered by exactly one electronics tool, the one its
    /// risk maps to (the board read by fab.board): a lane's new ActionDef reaches the AI with no
    /// manifest edit, and none is offered twice or by a tool that cannot run it.
    #[test]
    fn every_electronics_action_is_offered_by_exactly_one_tool() {
        register_elec_actions();
        let manifest = fab_ai::manifest();
        let offered = |tool: &str| -> Vec<String> {
            let Some(def) = manifest.tool(tool) else { return Vec::new() };
            let schema = makepad_model::json::parse(def.parameters.as_bytes()).expect("the tool's schema parses");
            let action = schema.get("properties").and_then(|props| props.get("action")).expect("the tool takes an action");
            match action.get("enum") {
                Some(makepad_model::json::Value::Arr(names)) => names.iter().filter_map(|name| name.as_str()).filter(|name| *name != "help").map(str::to_string).collect(),
                _ => panic!("{tool}'s action is an enum"),
            }
        };
        let board = fab_elec_mech::read::ACTIONS[0].name;
        assert!(manifest.tool("board").is_some(), "the board read stays fab.board");
        let catalog = fab_elec_design::action::catalog();
        assert!(catalog.iter().any(|def| def.name == "elec.value.set"), "the app registers its edit actions");
        for def in catalog {
            let tools: Vec<&str> = fab_ai::ELEC_TOOLS.iter().map(|(tool, _, _)| *tool).filter(|tool| offered(tool).iter().any(|name| name == def.name)).collect();
            if def.name == board {
                assert!(tools.is_empty(), "the board read is fab.board's alone");
            } else {
                assert_eq!(tools, vec![fab_ai::elec_tool_of(def.risk).expect("every registered action has a tool for its risk")],
                    "{} is offered once, by the tool its risk maps to", def.name);
            }
        }
    }

    /// A render-like picture: a soft gradient under a dither of a few levels, which a PNG can
    /// hardly compress (as the review's sheets are).
    fn dithered(side: u32, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        let mut pixels = Vec::with_capacity((side * side * 4) as usize);
        for y in 0..side {
            for x in 0..side {
                for c in 0..3 {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    let base = 60 + (x + y + c * 40) * 120 / (2 * side);
                    pixels.push((base as i32 + (state % 5) as i32 - 2) as u8);
                }
                pixels.push(255);
            }
        }
        RgbaImage { width: side, height: side, pixels }.to_png(&makepad_model::Limits::default()).unwrap()
    }

    fn side_of(data: &str) -> u32 {
        let png = makepad_base64::base64_decode(data.as_bytes()).unwrap();
        RgbaImage::from_png(&png, &makepad_model::Limits::default()).unwrap().width
    }

    #[test]
    fn every_picture_comes_back_within_the_budget_at_a_fair_size() {
        let small = dithered(64, 9);
        let images: Vec<(String, Arc<[u8]>)> = [dithered(1024, 1), dithered(1024, 2), small.clone(), dithered(1024, 3)].into_iter().enumerate().map(|(i, png)| (format!("{i}"), Arc::from(png))).collect();
        assert!(images.iter().map(|(_, png)| png.len()).sum::<usize>() > 2 * MAX_RESULT_IMAGE_BYTES, "the pictures are well over the budget");
        let result = tool_result("call", Answer { ok: true, text: "sheets".into(), images });
        assert_eq!(result.images.len(), 4, "{}", result.text);
        assert!(result.images.iter().map(String::len).sum::<usize>() <= MAX_RESULT_IMAGE_BYTES);
        // the small one fits its share as it is; the large ones all shrink alike
        assert_eq!(makepad_base64::base64_decode(result.images[2].as_bytes()).unwrap(), small);
        let sides: Vec<u32> = [0, 1, 3].map(|i| side_of(&result.images[i])).to_vec();
        println!("sides {sides:?}");
        assert!(sides.iter().all(|&s| s >= 400 && s * 10 >= sides[0] * 9 && s * 9 <= sides[0] * 10), "{sides:?}");
    }

    /// A picture within the byte budget but wider than the chat takes comes back as wide as it
    /// takes, its shape kept.
    #[test]
    fn a_picture_wider_than_the_chat_takes_shrinks_to_its_side() {
        let png = RgbaImage { width: 1500, height: 600, pixels: [90, 120, 150, 255].repeat(1500 * 600) }.to_png(&makepad_model::Limits::default()).unwrap();
        assert!(picture_fit::base64_len(png.len()) < MAX_RESULT_IMAGE_BYTES, "within the byte budget");
        assert_eq!(as_the_provider_takes(vec![("wide".into(), png)]), Ok(vec![[MAX_TOOL_IMAGE_SIDE, 410]]));
    }

    #[test]
    fn a_picture_that_fits_comes_back_as_it_was() {
        let png = dithered(512, 4);
        let result = tool_result("call", Answer { ok: true, text: String::new(), images: vec![("one".into(), Arc::from(png.clone()))] });
        assert_eq!(result.images.len(), 1);
        assert_eq!(makepad_base64::base64_decode(result.images[0].as_bytes()).unwrap(), png);
    }

    #[test]
    fn shrunk_sheets_shrink_alike_keep_whole_cells_and_their_marks_size() {
        let limits = makepad_model::Limits::default();
        let sheets: Vec<(RgbaImage, [u32; 2])> = [(1024, 5), (1024, 6)].map(|(side, seed)| (RgbaImage::from_png(&dithered(side, seed), &limits).unwrap(), [2, 2])).to_vec();
        let mut marked = Vec::new();
        let pngs = fit_pictures(&sheets, 400_000, |i, image, cell| {
            marked.push((i, cell));
            for x in 0..24 {
                image.pixels[x * 4..x * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        })
        .unwrap();
        let bytes: usize = pngs.iter().map(|(png, _)| picture_fit::base64_len(png.len())).sum();
        println!("tries {marked:?}, {bytes} bytes");
        assert!(bytes <= 400_000 && bytes * 100 >= 400_000 * 97, "{bytes} bytes fill the budget within 3%");
        let out: Vec<RgbaImage> = pngs.iter().map(|(png, _)| RgbaImage::from_png(png, &limits).unwrap()).collect();
        let cell = marked.last().unwrap().1;
        assert!(cell[0] < 512 && cell[0] == cell[1], "{marked:?}");
        for image in &out {
            assert_eq!((image.width, image.height), (cell[0] * 2, cell[1] * 2), "both sheets alike, whole cells");
            assert!((0..24).all(|x| image.pixels[x * 4..x * 4 + 3] == [255, 0, 0]), "the mark is drawn on the shrunk sheet at its own size");
        }
    }
}
