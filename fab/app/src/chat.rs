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
    work: HashMap<u64, crate::journal::Work>,
    journal: HashMap<u64, Arc<crate::journal::Call>>,
    cancelled: std::collections::HashSet<u64>,
    /// Calls the chat made that are still running: their tool.
    pub pending: HashMap<u64, String>,
    pub answers: HashMap<u64, Answer>,
    /// A finished image-to-mesh call's card, kept until its answer goes out.
    pub tasks: HashMap<u64, TaskCard>,
}

impl Calls {
    pub(crate) fn begin(&mut self, plate: Option<&crate::journal::Plate>, by: fab_elec_design::By, id: u64, tool: &str, args: &makepad_strict_json::Value) {
        let call = plate.map(|plate| crate::journal::Call::begin(plate, by, tool, args.clone()));
        self.work.insert(id, crate::journal::Work::new(by, call.as_ref()));
        if let Some(call) = call { self.journal.insert(id, call); }
    }

    pub(crate) fn work_for(&mut self, id: u64, tool: &str) -> Option<&crate::journal::Work> {
        let work = self.work.get(&id);
        if work.is_none() {
            if self.cancelled.remove(&id) { self.pending.remove(&id); }
            makepad_widgets::log!("fab: settled tool skipped: {} (call {})", tool, id);
        }
        work
    }

    pub(crate) fn complete(&mut self, id: u64, outcome: &'static str, reply: &str) {
        if let Some(call) = self.journal.remove(&id) { call.done(outcome, reply.to_owned()); }
    }

    pub(crate) fn cancel(&mut self, id: u64) {
        self.complete(id, "cancelled", "cancelled");
        if self.work.remove(&id).is_some() { self.cancelled.insert(id); }
        self.answers.remove(&id);
    }

    pub fn finish(&mut self, id: u64, answer: Answer) {
        self.work.remove(&id);
        let cancelled = self.cancelled.remove(&id);
        if self.pending.remove(&id).is_some() && !cancelled {
            self.answers.insert(id, answer);
        }
    }
}

pub fn services() -> Vec<ServiceManifest> {
    register_elec_actions();
    vec![journal::tools(fab_ai::manifest())]
}

/// The electronics actions the app runs (`ElecHost::run_action`), in the action catalog once, so
/// the manifest's electronics tools offer them (`fab_ai::ELEC_TOOLS`). A lane's actions go here
/// when the app runs them. O(catalog) the first time, an atomic check after.
pub fn register_elec_actions() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        fab_elec_mech::read::register_actions();
        fab_elec_host::edit_actions::register_actions();
        fab_elec_design::action::register(fab_elec_host::bom_actions::ASSEMBLY_ACTIONS);
        fab_elec_design::action::register(&[fab_elec_host::bom_actions::PRICE_SET]);
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
        assert!(offered("elec_read").iter().any(|name| name == "elec.net.length"), "the board length read reaches the read tool");
        assert!(offered("elec_external").contains(&"elec.price.set".to_string()));
        assert!(offered("elec_export").contains(&"elec.export".to_string()));
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

/// The panel and both tools project the same reader pages and linked details on the worker.
pub(crate) mod journal {
    use super::{Answer, Calls, MAX_RESULT_BYTES};
    use crate::journal::{reader::{Actor, Answer as Read, Browse, DiffVersion, Filter, Key, Kind, Partner, Position, Reader, VerifiedLog}, Health, Plate};
    use crate::journal::remove::{empty_removed, remove_session, Emptied, Refusal, Removed};
    use fab::ui::journal_panel::{JournalAction, JournalDiff, JournalDiffUpdate, JournalPage, JournalRow, JournalSession, JournalSource, RecordKey, RemovedSession};
    use makepad_ai_services::wire::{Risk, ServiceManifest, ToolDef};
    use makepad_strict_json::{obj, s, Value};
    use makepad_widgets::{makepad_platform::thread::{Class, TaskPool, ThreadSpawner}, Cx, Event, NextFrame, ToUIReceiver};
    use std::{collections::HashSet, fmt::Write, io, path::PathBuf, sync::{Arc, OnceLock, atomic::{AtomicBool, AtomicUsize, Ordering}}};

    const EARLIER: &str = "This record belongs to an earlier journal of this plate. List the records again.";
    const REPLACED: &str = "This journal was replaced, so the selection was cleared.";
    const COPY_ERROR: &str = "The selected records could not be copied.";
    const UNAVAILABLE: &str = "Record unavailable.";
    const PAGE: usize = 32;
    const ACTORS: &[(&str, Actor)] = &[("person", Actor::Person), ("ai", Actor::Ai), ("agent", Actor::Agent)];
    const KINDS: &[(&str, Kind)] = &[("call", Kind::Call), ("done", Kind::Done), ("effect", Kind::Effect),
        ("history", Kind::History), ("reload", Kind::Reload), ("checkpoint", Kind::Checkpoint),
        ("lost", Kind::Lost), ("diagnostic", Kind::Diagnostic), ("other", Kind::Other)];

    pub(super) fn tools(manifest: ServiceManifest) -> ServiceManifest {
        let key = r#"{"type":"object","description":"A record key returned by journal_list, with folder, journal and order (offset for a damaged line).","properties":{"folder":{"type":"string"},"journal":{"type":"string","pattern":"^[0-9a-f]{16}$"},"order":{"type":"integer"},"offset":{"type":"integer"}},"required":["folder","journal"],"oneOf":[{"required":["order"]},{"required":["offset"]}]}"#;
        manifest.with_tool(ToolDef::new("journal_list",
            "Rows of this plate's local journal (who, what, when), newest first, a page at a time. No file contents. Pass before from the reply to continue.",
            format!(r#"{{"type":"object","properties":{{"actor":{{"type":"string","enum":["all","person","ai","agent"]}},"kind":{{"type":"string","enum":["all","call","done","effect","history","reload","checkpoint","lost","diagnostic","other"]}},"before":{key},"count":{{"type":"integer","minimum":1,"default":{PAGE}}}}},"additionalProperties":false}}"#), Risk::Read))
        .with_tool(ToolDef::new("journal_read",
            "One record of this plate's journal and its linked call or result, including the arguments, the reply and which documents changed. For large records, pass the cursor back and concatenate the JSON text parts.",
            format!(r#"{{"type":"object","properties":{{"record":{key},"cursor":{{"type":"object","description":"Continuation returned with a JSON text part."}}}},"required":["record"],"additionalProperties":false}}"#), Risk::Read))
        .with_tool(ToolDef::new("journal_copy",
            "The records from `from` to `to` as a replayable script: one catalog call per line, source patches for direct edits, and comment lines for what can't be replayed. For long scripts, pass the cursor back and concatenate the parts.",
            format!(r#"{{"type":"object","properties":{{"from":{key},"to":{key},"actor":{{"type":"string","enum":["all","person","ai","agent"]}},"kind":{{"type":"string","enum":["all","call","done","effect","history","reload","checkpoint","lost","diagnostic","other"]}},"cursor":{{"type":"object"}}}},"required":["from","to"],"additionalProperties":false}}"#), Risk::Read))
        .with_tool(ToolDef::new("journal_replay",
            "Rebuild the current plate through this record, inclusive, and open the copy as its own plate. A call record precedes its edit. Returns the new plate's name.",
            format!(r#"{{"type":"object","properties":{{"record":{key}}},"required":["record"],"additionalProperties":false}}"#), Risk::Request))
    }

    fn key(k: &Key) -> RecordKey {
        let (number, damaged) = match k.position { Position::Order(n) => (n, false), Position::Diagnostic(n) => (n, true) };
        RecordKey { folder: k.folder.clone(), journal: k.journal, number, damaged }
    }

    fn reader_key(k: &RecordKey) -> Key {
        Key { folder: k.folder.clone(), journal: k.journal, position: if k.damaged { Position::Diagnostic(k.number) } else { Position::Order(k.number) } }
    }

    fn key_json(k: &RecordKey) -> Value {
        obj(vec![("folder", s(&*k.folder)), ("journal", s(format!("{:016x}", k.journal))), (if k.damaged { "offset" } else { "order" }, Value::Int(k.number as i64))])
    }

    fn parse_key(v: &Value, folder: &str) -> Result<Key, &'static str> {
        if v.get("folder").and_then(Value::as_str) != Some(folder) { return Err("The record belongs to another plate.") }
        let malformed = "Expected a record key with an order or diagnostic offset.";
        let journal = v.get("journal").and_then(Value::as_str)
            .filter(|s| s.len() == 16 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
            .and_then(|s| u64::from_str_radix(s, 16).ok()).ok_or(malformed)?;
        let position = match (v.get("order"), v.get("offset")) {
            (Some(n), None) => n.as_u64().map(Position::Order),
            (None, Some(n)) => n.as_u64().map(Position::Diagnostic),
            _ => None,
        }.ok_or(malformed)?;
        Ok(Key { folder: folder.into(), journal, position })
    }

    fn name<T: Eq>(values: &[(&'static str, T)], item: &T) -> &'static str {
        values.iter().find(|(_, v)| v == item).map_or("other", |p| p.0)
    }

    fn summary(row: &crate::journal::reader::Row) -> Value {
        let key = key_json(&key(&row.key));
        let fields = |label: &str, shortened| {
            let mut fields = vec![("key", key.clone()), ("kind", s(name(KINDS, &row.kind))),
                ("actor", row.actor.map_or(Value::Null, |a| s(if a == Actor::Disk { "disk" } else { name(ACTORS, &a) }))), ("label", s(label)),
                ("time_ms", row.call_time_ms.filter(|_| row.kind == Kind::Call).map_or(Value::Null, |v| Value::Int(v as i64))),
                ("changed_files", row.changed_files.map_or(Value::Null, |n| Value::Int(n as i64)))];
            if shortened { fields.push(("label_shortened", Value::Bool(true))); }
            obj(fields)
        };
        let envelope = r#"{"rows":[],"before":}"#.len() + key.to_json().len();
        let full = fields(&row.label, false);
        if full.to_json().len() + envelope <= MAX_RESULT_BYTES { return full }
        // Even a single label can exceed the wire. Both lists show a marked UTF-8 prefix;
        // journal_read still returns the exact label. JSON escapes take at most six bytes per byte.
        let room = MAX_RESULT_BYTES.saturating_sub(envelope + fields("…", true).to_json().len());
        let mut end = (room / 6).min(row.label.len());
        while !row.label.is_char_boundary(end) { end -= 1; }
        fields(&format!("{}…", &row.label[..end]), true)
    }

    fn panel_row(row: &crate::journal::reader::Row) -> JournalRow {
        let actor = match row.actor { Some(Actor::Person) => "Person", Some(Actor::Ai) => "AI", Some(Actor::Agent) => "Agent", Some(Actor::Disk) => "Disk", None => "" };
        let mut context = match row.kind {
            Kind::Call => "Call", Kind::Done => "Result", Kind::Effect => "Edit", Kind::History => "History",
            Kind::Reload => "Reload", Kind::Checkpoint => "Checkpoint", Kind::Lost => "Lost", Kind::Diagnostic => "Damaged", Kind::Other => "Record",
        }.to_owned();
        if !actor.is_empty() { let _ = write!(context, " · {actor}"); }
        if let Some(n) = row.changed_files { let _ = write!(context, " · {n} {}", if n == 1 { "file" } else { "files" }); }
        if let Some(t) = row.call_time_ms.filter(|_| row.kind == Kind::Call) {
            let time = makepad_civil_time::local_or_utc((t / 1000) as i64);
            let _ = write!(context, " · {}", time.hms());
        }
        let title = summary(row).get("label").and_then(Value::as_str).unwrap_or_default().to_owned();
        JournalRow { key: key(&row.key), title, context }
    }

    enum Tool {
        List(Filter, Option<Key>, usize),
        Read(Key, Option<Cursor>),
        Copy { from: Key, to: Key, filter: Filter, scope: Value, cursor: Option<(usize, u64)> },
    }
    struct Cursor { offset: usize, partner: Value, end: u64 }

    fn documents(browse: &Browse<'_>, record: &Value, end: u64) -> io::Result<Option<Vec<String>>> {
        if !matches!(record.get("kind").and_then(Value::as_str), Some("call" | "done")) { return Ok(None) }
        match record.get("id").and_then(Value::as_u64) {
            Some(id) => browse.changed_by(id, end).map(Some),
            None => Ok(Some(Vec::new())),
        }
    }

    fn choice<T: Copy>(v: Option<&Value>, values: &[(&str, T)]) -> Result<Option<T>, &'static str> {
        match v {
            None => Ok(None),
            Some(v) if v.as_str() == Some("all") => Ok(None),
            Some(v) => values.iter().find(|(name, _)| Some(*name) == v.as_str()).map(|(_, t)| Some(*t)).ok_or("Unknown journal filter."),
        }
    }

    impl Tool {
        fn parse(tool: &str, args: &Value, folder: &str) -> Result<Self, &'static str> {
            let Value::Obj(fields) = args else { return Err("Expected an argument object.") };
            let list = tool == "journal_list";
            if tool == "journal_copy" {
                if fields.iter().any(|(k, _)| !matches!(k.as_str(), "actor" | "kind" | "from" | "to" | "cursor")) { return Err("Unknown journal argument.") }
                let from = parse_key(args.get("from").ok_or("Expected a from key.")?, folder)?;
                let to = parse_key(args.get("to").ok_or("Expected a to key.")?, folder)?;
                let filter = Filter { actor: choice(args.get("actor"), ACTORS)?, kind: choice(args.get("kind"), KINDS)? };
                let scope = obj(vec![("from", key_json(&key(&from))), ("to", key_json(&key(&to))),
                    ("actor", s(filter.actor.map_or("all", |a| name(ACTORS, &a)))), ("kind", s(filter.kind.map_or("all", |k| name(KINDS, &k))))]);
                let cursor = args.get("cursor").map(|c| {
                    if c.get("range") != Some(&scope) { return Err("Cursor belongs to another selection.") }
                    let offset = c.get("offset").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or("Invalid cursor offset.")?;
                    let end = c.get("end").and_then(Value::as_u64).ok_or("Invalid cursor.")?;
                    Ok((offset, end))
                }).transpose()?;
                return Ok(Self::Copy { from, to, filter, scope, cursor })
            }
            if fields.iter().any(|(k, _)| if list { !matches!(k.as_str(), "actor"|"kind"|"count"|"before") } else { !matches!(k.as_str(), "record"|"cursor") }) {
                return Err("Unknown journal argument.")
            }
            if list {
                let filter = Filter { actor: choice(args.get("actor"), ACTORS)?, kind: choice(args.get("kind"), KINDS)? };
                let before = args.get("before").map(|v| parse_key(v, folder)).transpose()?;
                let count = args.get("count").map(|v| v.as_u64().filter(|n| *n > 0).ok_or("Expected a positive count.")).transpose()?
                    .map_or(PAGE, |n| usize::try_from(n).unwrap_or(usize::MAX));
                Ok(Self::List(filter, before, count))
            } else {
                let key = parse_key(args.get("record").ok_or("Expected a record key.")?, folder)?;
                let cursor = args.get("cursor").map(|v| {
                    if v.get("record") != args.get("record") { return Err("Cursor belongs to another record.") }
                    let offset = v.get("offset").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or("Invalid cursor offset.")?;
                    let partner = v.get("partner").ok_or("Invalid cursor.")?.clone();
                    let end = v.get("end").and_then(Value::as_u64).ok_or("Invalid cursor.")?;
                    Ok(Cursor { offset, partner, end })
                }).transpose()?;
                Ok(Self::Read(key, cursor))
            }
        }

        fn run(self, browse: &mut Browse<'_>) -> Result<String, &'static str> {
            let stale = |k: &Key| Some(k.journal) != browse.journal();
            if match &self {
                Self::List(_, before, _) => before.as_ref().is_some_and(stale),
                Self::Read(k, _) => stale(k),
                Self::Copy { from, to, .. } => stale(from) || stale(to),
            } { return Err(EARLIER) }
            match self {
                Self::List(filter, before, count) => list(browse, filter, before, count),
                Self::Copy { from, to, filter, scope, cursor } => {
                    let end = cursor.map_or_else(|| browse.complete_end(), |(_, end)| end);
                    let text = browse.script(&from, &to, filter, end).map_err(|_| "Cannot copy this journal selection.")?;
                    if cursor.is_none() {
                        let whole = obj(vec![("script", s(&text))]).to_json();
                        if whole.len() <= MAX_RESULT_BYTES { return Ok(whole) }
                    }
                    parts(&text, cursor.map_or(0, |(offset, _)| offset), |offset| obj(vec![
                        ("range", scope.clone()), ("offset", Value::Int(offset as i64)), ("end", Value::Int(end as i64))]))
                }
                Self::Read(k, cursor) => {
                    let Read::Detail { record, partner } = browse.detail(k.clone()).map_err(|_| "Journal read failed.")? else { return Err("Record not found.") };
                    let current = browse.complete_end();
                    let end = cursor.as_ref().map_or(current, |c| c.end);
                    if end > current { return Err("The journal revision is no longer available.") }
                    let names = documents(browse, &record, end).map_err(|_| "Journal read failed.")?;
                    read(&k, record, partner, names, end, cursor)
                }
            }
        }
    }

    fn list(browse: &mut Browse<'_>, filter: Filter, mut before: Option<Key>, count: usize) -> Result<String, &'static str> {
        let mut text = String::from("{\"rows\":[");
        let mut last = None;
        let mut remaining = count;
        let mut more = true;
        // A large requested count still reads only the pages needed to fill one wire reply.
        'pages: while more && remaining > 0 {
            let (rows, next) = browse.page(filter, before.take(), remaining.min(PAGE)).map_err(|_| "Journal page unavailable.")?;
            more = next.is_some();
            for row in rows {
                let row_key = key(&row.key);
                let cursor = key_json(&row_key).to_json();
                let encoded = summary(&row).to_json();
                let room = MAX_RESULT_BYTES.saturating_sub(text.len() + cursor.len() + "],\"before\":}".len() + usize::from(last.is_some()));
                if encoded.len() > room {
                    if last.is_none() { return Err("The record key exceeds the reply budget.") }
                    more = true;
                    break 'pages
                }
                if last.is_some() { text.push(','); }
                text.push_str(&encoded);
                last = Some(cursor);
                before = Some(row.key);
                remaining -= 1;
            }
        }
        text.push_str("],\"before\":");
        text.push_str(if more { last.as_deref().unwrap_or("null") } else { "null" });
        text.push('}');
        Ok(text)
    }

    fn read(k: &Key, record: Value, partner: Partner, documents: Option<Vec<String>>, journal_end: u64, cursor: Option<Cursor>) -> Result<String, &'static str> {
        // A call's done can arrive between chunks. Preserve the join seen on the first read.
        let partner = match partner {
            Partner::None => Value::Null,
            Partner::NotFound => s("not found"),
            Partner::Found { key: k, record } if cursor.as_ref().is_none_or(|c| !c.partner.is_null()) => obj(vec![("key", key_json(&key(&k))), ("record", record)]),
            Partner::Found { .. } => s("not found"),
        };
        let partner_key = partner.get("key").cloned().unwrap_or(Value::Null);
        if cursor.as_ref().is_some_and(|c| c.partner != partner_key) { return Err("The linked record is no longer available.") }
        let mut fields = vec![("record", record), ("partner", partner)];
        if let Some(names) = documents { fields.push(("documents", Value::Arr(names.into_iter().map(s).collect()))); }
        let text = obj(fields).to_json();
        if cursor.is_none() && text.len() <= MAX_RESULT_BYTES { return Ok(text) }
        parts(&text, cursor.map_or(0, |c| c.offset), |offset| obj(vec![("record", key_json(&key(k))),
            ("offset", Value::Int(offset as i64)), ("partner", partner_key.clone()), ("end", Value::Int(journal_end as i64))]))
    }

    fn parts(text: &str, from: usize, continuation: impl Fn(usize) -> Value) -> Result<String, &'static str> {
        if from >= text.len() || !text.is_char_boundary(from) { return Err("Invalid cursor offset.") }
        let chunk = |end| {
            let cursor = if end == text.len() { Value::Null } else { continuation(end) };
            obj(vec![("part", s(&text[from..end])), ("cursor", cursor)]).to_json()
        };
        let mut end = from.saturating_add(MAX_RESULT_BYTES).min(text.len());
        while !text.is_char_boundary(end) { end -= 1; }
        loop {
            let encoded = chunk(end);
            if encoded.len() <= MAX_RESULT_BYTES { return Ok(encoded) }
            end -= (encoded.len() - MAX_RESULT_BYTES).min((end - from).div_ceil(2));
            while end > from && !text.is_char_boundary(end) { end -= 1; }
            if end == from { return Err("The cursor exceeds the reply budget.") }
        }
    }

    fn detail_text(key: &Key, record: &Value, partner: &Partner) -> String {
        fn section(text: &mut String, record: &Value) {
            if let Some(actor) = record.get("actor").and_then(|v| v.get("kind")).and_then(Value::as_str) {
                let _ = write!(text, "\nActor: {actor}");
            }
            for (field, label) in [("kind","Kind"), ("action","Action"), ("label","Edit"), ("args","Arguments"), ("outcome","Outcome"),
                ("reply","Reply"), ("direction","History"), ("reason","Reason"), ("cause","Cause"), ("bytes","Lost bytes"), ("at","Byte offset"),
                ("seq","Undo step"), ("seqs","Undo steps"), ("elsewhere","Effects on other plates")] {
                if let Some(value) = record.get(field).filter(|v| !v.is_null()) {
                    if let Some(s) = value.as_str() { let _ = write!(text, "\n{label}: {s}"); }
                    else { let _ = write!(text, "\n{label}: {}", value.to_json()); }
                }
            }
            for field in ["changes", "files"] {
                if let Some(files) = record.get(field).and_then(Value::as_arr) {
                    for name in files.iter().filter_map(|v| v.get("name")?.as_str()) { let _ = write!(text, "\nDocument: {name}"); }
                }
            }
        }
        let mut text = match key.position { Position::Order(n) => format!("Record {n}"), Position::Diagnostic(n) => format!("Damaged line at byte {n}") };
        section(&mut text, record);
        match partner {
            Partner::None => (),
            Partner::NotFound => text.push_str("\n\nLinked record not found."),
            Partner::Found { record, .. } => { text.push_str("\n\nLinked record"); section(&mut text, record); }
        }
        text
    }

    fn session_row(session: crate::journal::reader::sessions::Session) -> JournalSession {
        let time = session.start_ms.map(|ms| makepad_civil_time::local_or_utc((ms / 1000) as i64));
        let title = time.as_ref().map_or_else(|| "Start time unavailable".into(), |t| format!("{} · {}", t.date_text(), t.hm()));
        let copy_name = time.map_or_else(|| "Unsaved session".into(), |t| format!("Unsaved session from {}", t.hm()));
        let description = format!("{} {}{}", session.records, if session.records == 1 { "record" } else { "records" },
            if session.lost { " · Recording gap" } else { "" });
        JournalSession { folder: session.folder, token: session.token, title, description, copy_name }
    }

    enum Delivery {
        Sessions(io::Result<(Arc<[JournalSession]>, Arc<[RemovedSession]>)>),
        Removed { root: PathBuf, folder: Arc<str>, token: u64, result: Result<Removed, Refusal> },
        Emptied { root: PathBuf, result: Emptied },
        Page { revision: u64, rows: Arc<[JournalRow]>, more: bool, detail: String, count: usize, end: u64, diff_version: Option<Arc<DiffVersion>>, diffs: Option<Vec<JournalDiffUpdate>> },
        Clipboard(io::Result<String>),
        Tool(u64, Result<String, &'static str>),
        Failed(u64, io::ErrorKind),
        Settling(u64, usize),
        Rebuilt(u64, Result<ReplayCopy, String>),
    }

    fn error_reason(kind: impl Into<crate::journal::Failure>) -> &'static str {
        use crate::journal::Failure::*;
        match kind.into() {
            NoFreeLives => "this journal has no free writer lives",
            IncarnationChanged => "this plate's journal was replaced",
            Io(io::ErrorKind::StorageFull) => "the disk is full",
            Io(io::ErrorKind::PermissionDenied) => "permission was denied",
            Io(io::ErrorKind::NotFound) => "the journal file is missing",
            _ => "the journal could not be read or written",
        }
    }

    const GAP: &str = "The journal is damaged or incomplete before this record. Try an earlier record.";

    #[derive(Default)]
    struct RebuildProgress {
        total: OnceLock<usize>,
        done: AtomicUsize,
        cancelled: AtomicBool,
        #[cfg(test)]
        pause: Option<Box<dyn Fn(usize) + Send + Sync>>,
    }

    impl RebuildProgress {
        fn record(&self, done: usize, total: usize) -> io::Result<()> {
            if done == 0 { self.total.get_or_init(|| total); }
            self.done.store(done, Ordering::Relaxed);
            #[cfg(test)]
            if let Some(pause) = &self.pause { pause(done); }
            if self.cancelled.load(Ordering::Acquire) { Err(io::ErrorKind::Interrupted.into()) } else { Ok(()) }
        }
    }

    fn grouped(n: usize) -> String {
        let mut digits = n.to_string();
        for i in (1..=digits.len().saturating_sub(3)).rev().step_by(3) { digits.insert(i, ','); }
        digits
    }

    struct ReplayCopy {
        name: String, folder: PathBuf, manifest: Option<String>,
        // A completed copy in transit still belongs to its request. A cancel or
        // plate switch can beat its delivery, including dropping the receiver.
        unclaimed: Option<TaskPool>,
    }

    impl Drop for ReplayCopy {
        fn drop(&mut self) {
            if let Some(pool) = self.unclaimed.take() {
                let folder = self.folder.clone();
                match pool.submit_named(Class::Background, "discard replay copy", move || {
                    if let Err(e) = std::fs::remove_dir_all(folder) {
                        makepad_widgets::log!("could not discard replay copy: {}", e.kind());
                    }
                }) {
                    Ok(task) => task.detach(),
                    Err(e) => makepad_widgets::log!("could not queue replay cleanup: {e}"),
                }
            }
        }
    }

    impl ReplayCopy {
        fn rebuild(store: &fab_ai::plates::PlateStore, plate: &str, key: &Key, record: &Value, log: VerifiedLog, progress: &RebuildProgress, pool: TaskPool) -> Result<Self, String> {
            use crate::journal::replay::{replay_controlled, Mode, Request, REPORT_FILE};
            use std::fs;
            let Position::Order(order) = key.position else { return Err(GAP.into()) };
            let name = if record.get("kind").and_then(Value::as_str) == Some("call") {
                let action = record.get("action").and_then(Value::as_str).ok_or(GAP)?;
                format!("{plate} (before {action})")
            } else if let Some(ms) = record.get("time_ms").and_then(Value::as_u64) {
                let time = makepad_civil_time::local_or_utc((ms / 1000) as i64);
                format!("{plate} (as of {})", time.hm())
            } else {
                format!("{plate} (replay)")
            };
            let write_error = |err: io::Error| format!("The replay copy could not be written ({err}).");
            fs::create_dir_all(&store.home).map_err(write_error)?;
            // Claim a new folder exclusively, including against a simultaneous Save.
            let folder = loop {
                let mut folder = store.new_file(&name);
                folder.pop();
                match fs::create_dir(&folder) {
                    Ok(()) => break folder,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists && folder.is_dir() => continue,
                    Err(e) => return Err(write_error(e)),
                }
            };
            let journal = store.dir(&key.folder).join("journal");
            let report = replay_controlled(Request { journal: &journal, upto: Some(order), into: &folder, mode: Mode::State },
                &mut |_| {}, Some(&mut |done, total| progress.record(done, total)), Some(&log));
            let report = match report {
                Ok(report) => report,
                Err(error) => {
                    fs::remove_dir(&folder).map_err(write_error)?;
                    return Err(match error.kind() {
                        // Missing checkpoint blobs are reported in the walk as gaps.
                        // A vanished archive fails the read itself.
                        io::ErrorKind::NotFound => UNAVAILABLE.into(),
                        io::ErrorKind::InvalidData => GAP.into(),
                        _ => write_error(error),
                    })
                }
            };
            fs::remove_file(folder.join(REPORT_FILE)).map_err(write_error)?;
            if report.stopped.is_some() {
                fs::remove_dir(&folder).map_err(write_error)?;
                return Err(GAP.into())
            }
            Ok(Self { name, folder, manifest: report.manifest, unclaimed: Some(pool) })
        }
    }

    struct ReplayRequest {
        generation: u64,
        key: Key,
        call: Option<u64>,
        _notice: crate::journal::SettleNotice,
        remaining: Option<usize>,
        progress: Arc<RebuildProgress>,
        displayed: Option<usize>,
        #[cfg(test)]
        resume: Option<std::sync::mpsc::Sender<()>>,
    }

    impl Drop for ReplayRequest {
        fn drop(&mut self) { self.progress.cancelled.store(true, Ordering::Release); }
    }

    struct Location { root: PathBuf, folder: String, spawner: ThreadSpawner, pool: TaskPool }

    #[derive(Default)]
    pub(crate) struct Browser {
        // Tool requests always use this reader, which follows the plate on the bed.
        reader: Option<Reader>,
        archive: Option<(JournalSession, Reader)>,
        panel_deliveries: ToUIReceiver<Delivery>,
        // Navigation replaces the panel channel. Keep a pending move until its worker answers.
        removal: Option<ToUIReceiver<Delivery>>,
        emptying: Option<ToUIReceiver<Delivery>>,
        showing_remove_outcome: bool,
        current_health: String,
        location: Option<Location>,
        active: bool,
        deliveries: ToUIReceiver<Delivery>,
        pending: HashSet<u64>,
        // Exclusive upper key for each page after the first.
        pages: Vec<Key>,
        revision: u64,
        shown: bool,
        dirty: bool,
        in_flight: bool,
        selection_end: u64,
        diff_version: Option<Arc<DiffVersion>>,
        clipboard: Option<String>,
        located: bool,
        plate: Option<(fab_ai::plates::PlateStore, String)>,
        replay_frame: NextFrame,
        replay_generation: u64,
        replay: Option<ReplayRequest>,
        ready: Option<ReplayCopy>,
        opening: Option<(u64, ReplayCopy, Option<u64>)>,
    }

    impl Browser {
        pub fn select(&mut self, cx: &mut Cx, plate: Option<&Plate>, name: &str, calls: &mut Calls, page: &mut JournalPage) {
            self.plate = plate.map(|plate| (plate.store().clone(), name.to_owned()));
            self.select_location(cx, plate.map(Plate::reader_location), calls, page);
        }

        fn select_location(&mut self, cx: &mut Cx, location: Option<(&std::path::Path, &str)>, calls: &mut Calls, page: &mut JournalPage) {
            let old = self.location.as_ref().map(|l| (l.root.as_path(), l.folder.as_str()));
            if self.located && location == old { return }
            self.located = true;
            self.showing_remove_outcome = false;
            self.replay = None;
            self.ready = None;
            self.clipboard = None;
            self.diff_version = None;
            if old.map(|p| p.0) != location.map(|p| p.0) { self.reader = None; }
            self.location = location.map(|(root, folder)| Location { root: root.into(), folder: folder.into(), spawner: cx.thread_spawner(), pool: cx.task_pool() });
            for id in self.pending.drain() { finish(calls, id, Err("The plate changed.")); }
            self.deliveries = Default::default();
            self.panel_deliveries = Default::default();
            self.archive = None;
            self.current_health.clear();
            self.pages.clear();
            self.in_flight = false;
            self.dirty = true;
            self.revision += 1;
            page.reset();
            if self.active { self.follow_location(); }
            if self.active && self.location.is_some() && self.reader.is_none() {
                page.message = "The journal reader could not start.".into();
            }
        }

        fn follow_location(&mut self) {
            let Some(location) = &self.location else { return };
            if self.reader.is_none() { self.reader = Reader::start(&location.spawner, location.root.clone()).ok(); }
            if self.reader.as_mut().is_some_and(|r| r.select(&location.folder).is_err()) {
                self.reader = None;
            }
        }

        fn activate(&mut self) {
            if !self.active {
                self.active = true;
                self.follow_location();
            }
        }

        fn start_replay(&mut self, key: Key, call: Option<u64>) -> Result<(), &'static str> {
            if self.replay.is_some() || self.opening.is_some() { return Err("A replay copy is already in progress.") }
            if self.plate.is_none() { return Err("No current plate.") }
            let location = self.location.as_ref().ok_or("No current plate.")?;
            let (folder, reader) = match (&mut self.archive, call) {
                (Some((session, reader)), None) => (&*session.folder, Some(reader)),
                _ => (location.folder.as_str(), self.reader.as_mut()),
            };
            if &*key.folder != folder { return Err("The record belongs to another plate.") }
            let reader = reader.ok_or("Journal reader unavailable.")?;
            let dir = location.root.join(folder).join("journal");
            self.replay_generation += 1;
            let generation = self.replay_generation;
            let send = self.deliveries.sender();
            let notice: crate::journal::SettleNotice = Arc::new(move |n| { let _ = send.send(Delivery::Settling(generation, n)); });
            let observer = Arc::downgrade(&notice);
            let send = self.deliveries.sender();
            let selected = key.clone();
            reader.query(move |browse| {
                let Some(notice) = observer.upgrade() else { return };
                let validation = browse.map_err(|_| UNAVAILABLE).and_then(|b| {
                    if call.is_some() && b.journal() != Some(selected.journal) { return Err(EARLIER) }
                    let Position::Order(order) = selected.position else { return Err(GAP) };
                    b.replay_record(&selected).map(|_| order).map_err(|_| UNAVAILABLE)
                });
                match validation {
                    Ok(order) => crate::journal::watch_settlement(&dir, order, &notice),
                    Err(reason) => { let _ = send.send(Delivery::Rebuilt(generation, Err(reason.into()))); }
                }
            }).map_err(|_| "Journal reader unavailable.")?;
            self.replay = Some(ReplayRequest { generation, key, call, _notice: notice, remaining: None,
                progress: Arc::default(), displayed: None,
                #[cfg(test)] resume: None });
            if let Some(id) = call { self.pending.insert(id); }
            Ok(())
        }

        fn settled(&mut self, generation: u64, count: usize, page: &mut JournalPage) {
            let Some(request) = &mut self.replay else { return };
            if request.generation != generation || request.remaining.is_some_and(|n| n <= count) { return }
            request.remaining = Some(count);
            page.replay_waiting = count == 0 || request.call.is_none();
            page.replay_status = match count {
                0 => "Rebuilding the copy…".into(),
                1 => "Waiting for 1 earlier record to finish…".into(),
                n => format!("Waiting for {n} earlier records to finish…"),
            };
            page.revision += 1;
            if count != 0 { return }
            let send = self.deliveries.sender();
            let key = request.key.clone();
            let progress = request.progress.clone();
            let (reader, source) = match (&mut self.archive, request.call) {
                (Some((session, reader)), None) => (Some(reader), self.plate.as_ref().map(|(s, _)| (s.clone(), session.copy_name.clone()))),
                _ => (self.reader.as_mut(), self.plate.clone()),
            };
            if let (Some(reader), Some((store, plate)), Some(location)) = (reader, source, &self.location) {
                let pool = location.pool.clone();
                let send = send.clone();
                if reader.query(move |browse| {
                    if progress.cancelled.load(Ordering::Acquire) { return }
                    let Ok((record, log)) = browse.and_then(|b| b.replay_record(&key)) else {
                        let _ = send.send(Delivery::Rebuilt(generation, Err(UNAVAILABLE.into())));
                        return
                    };
                    let reply = send.clone();
                    let cleanup = pool.clone();
                    match pool.submit_named(Class::Background, "rebuild journal copy", move || {
                        if progress.cancelled.load(Ordering::Acquire) { return }
                        let result = ReplayCopy::rebuild(&store, &plate, &key, &record, log, &progress, cleanup);
                        let _ = reply.send(Delivery::Rebuilt(generation, result));
                    }) {
                        Ok(task) => task.detach(),
                        Err(_) => { let _ = send.send(Delivery::Rebuilt(generation, Err("The replay worker is unavailable.".into()))); }
                    }
                }).is_ok() { return }
            }
            let _ = send.send(Delivery::Rebuilt(generation, Err("Journal reader unavailable.".into())));
        }

        fn cancel_replay(&mut self, page: &mut JournalPage) {
            if let Some(request) = self.replay.take() {
                if let Some(id) = request.call {
                    let _ = self.deliveries.sender().send(Delivery::Tool(id, Err("Replay cancelled.")));
                }
                page.replay_waiting = false;
                page.replay_status.clear();
                page.revision += 1;
            }
        }

        fn show_progress(&mut self, page: &mut JournalPage) -> bool {
            let Some(request) = &mut self.replay else { return false };
            let Some(&total) = request.progress.total.get() else { return false };
            let done = request.progress.done.load(Ordering::Relaxed);
            if request.displayed == Some(done) { return false }
            request.displayed = Some(done);
            page.replay_status = format!("Rebuilding: {} of {} records", grouped(done), grouped(total));
            page.revision += 1;
            true
        }

        pub fn progress_frame(&mut self, cx: &mut Cx, event: &Event, shown: bool, page: &mut JournalPage) -> bool {
            let running = shown && page.drawn && self.ready.is_none() && self.replay.as_ref().is_some_and(|r| r.remaining == Some(0));
            let due = self.replay_frame.is_event(event).is_some();
            if due { self.replay_frame = NextFrame::default(); }
            if running && self.replay_frame == NextFrame::default() { self.replay_frame = cx.new_next_frame(); }
            running && due && self.show_progress(page)
        }

        fn rebuilt(&mut self, generation: u64, result: Result<ReplayCopy, String>, page: &mut JournalPage, calls: &mut Calls) {
            if self.replay.as_ref().is_none_or(|r| r.generation != generation) { return }
            page.replay_waiting = false;
            // This generation is still current. Its copy may now be opened or kept.
            let result = result.map(|mut copy| { copy.unclaimed = None; copy });
            let (message, ok, name) = match result {
                Ok(copy) if copy.manifest.is_some() => {
                    self.ready = Some(copy);
                    page.replay_status = "Copy rebuilt. Opening the plate…".into();
                    page.revision += 1;
                    return
                }
                Ok(copy) => (format!("Copy rebuilt in {}. There is no single plate file to open.", copy.folder.display()), true, Some(copy.name.clone())),
                Err(error) => (error, false, None),
            };
            let call = self.replay.take().and_then(|r| r.call);
            self.reply_replay(call, ok, name.as_deref(), &message, calls);
            page.replay_status = message;
            page.revision += 1;
        }

        fn reply_replay(&mut self, call: Option<u64>, ok: bool, name: Option<&str>, message: &str, calls: &mut Calls) {
            if let Some(id) = call {
                self.pending.remove(&id);
                let mut fields = Vec::new();
                if let Some(name) = name { fields.push(("plate", s(name))); }
                if !message.is_empty() { fields.push((if ok { "status" } else { "error" }, s(message))); }
                calls.finish(id, Answer { ok, text: obj(fields).to_json(), images: vec![] });
            }
        }

        pub fn project_to_open(&self) -> Option<(PathBuf, String)> {
            let copy = self.ready.as_ref()?;
            Some((copy.folder.join(copy.manifest.as_ref()?), copy.name.clone()))
        }

        pub fn opening(&mut self, job: Option<u64>, page: &mut JournalPage, calls: &mut Calls) {
            let Some(copy) = self.ready.take() else { return };
            let call = self.replay.take().and_then(|r| r.call);
            if let Some(id) = job {
                self.opening = Some((id, copy, call));
            } else {
                self.open_result(copy, call, false, page, calls);
            }
        }

        pub fn opened(&mut self, id: u64, ok: bool, page: &mut JournalPage, calls: &mut Calls) {
            if self.opening.as_ref().is_none_or(|o| o.0 != id) { return }
            if let Some((_, copy, call)) = self.opening.take() {
                self.open_result(copy, call, ok, page, calls);
            }
        }

        fn open_result(&mut self, copy: ReplayCopy, call: Option<u64>, ok: bool, page: &mut JournalPage, calls: &mut Calls) {
            let message = if ok { String::new() } else { format!("Copy rebuilt in {}. Fab could not open the plate.", copy.folder.display()) };
            self.reply_replay(call, ok, Some(&copy.name), &message, calls);
            page.replay_status = message;
            page.revision += 1;
        }

        pub fn tool(&mut self, calls: &mut Calls, id: u64, tool: &str, args: &Value) {
            calls.pending.insert(id, tool.into());
            self.activate();
            let folder = self.location.as_ref().map_or("", |l| l.folder.as_str());
            if tool == "journal_replay" {
                let request = match args {
                    Value::Obj(fields) if fields.len() == 1 => args.get("record").ok_or("Expected a record key.").and_then(|v| parse_key(v, folder)),
                    _ => Err("Expected a record key on this plate."),
                };
                if let Err(e) = request.and_then(|key| self.start_replay(key, Some(id))) { finish(calls, id, Err(e)); }
                return
            }
            let request = match Tool::parse(tool, args, folder) {
                Ok(request) => request,
                Err(e) => { finish(calls, id, Err(e)); return }
            };
            let Some(reader) = self.reader.as_mut() else {
                let answer = if self.location.is_some() { Err("Journal reader unavailable.") }
                    else if matches!(request, Tool::List(_, None, _)) { Ok("{\"rows\":[],\"before\":null}".into()) }
                    else { Err("Record not found.") };
                finish(calls, id, answer);
                return
            };
            let tx = self.deliveries.sender();
            if reader.query(move |browse| {
                let result = browse.map_err(|_| "Journal read failed.").and_then(|mut b| request.run(&mut b));
                let _ = tx.send(Delivery::Tool(id, result));
            }).is_ok() { self.pending.insert(id); }
            else { finish(calls, id, Err("Journal reader unavailable.")); }
        }

        fn change_source(&mut self, source: JournalSource, page: &mut JournalPage) {
            let archive = if let JournalSource::Session(session) = &source {
                let Some(location) = &self.location else { return };
                let reader = Reader::start(&location.spawner, location.root.clone()).ok()
                    .and_then(|mut r| { r.select_session(&session.folder).ok()?; Some(r) });
                let Some(reader) = reader else {
                    page.message = "The session reader could not start.".into();
                    page.revision += 1;
                    return
                };
                Some((session.clone(), reader))
            } else { None };
            self.archive = archive;
            self.showing_remove_outcome = false;
            // Outstanding tools retain their primary reader and their answer channel.
            self.panel_deliveries = Default::default();
            if self.replay.as_ref().is_some_and(|r| r.call.is_none()) {
                self.replay = None;
                self.ready = None;
            }
            self.clipboard = None;
            self.diff_version = None;
            self.pages.clear();
            self.in_flight = false;
            self.dirty = true;
            self.revision += 1;
            page.reset();
            if matches!(source, JournalSource::Plate) { page.health.clone_from(&self.current_health); }
            page.source = source;
        }

        fn refresh_sessions(&mut self, page: &mut JournalPage) {
            self.dirty = false;
            let Some(location) = &self.location else {
                page.message = "No plate store is configured.".into();
                page.revision += 1;
                return
            };
            let tx = self.panel_deliveries.sender();
            let root = location.root.clone();
            self.in_flight = self.reader.as_mut().is_some_and(|r| r.list_sessions(root.clone(), move |result| {
                let inventory = result.and_then(|sessions| {
                    let removed = crate::journal::reader::sessions::list_removed(&root)?;
                    Ok((sessions.into_iter().map(session_row).collect(), removed))
                });
                let _ = tx.send(Delivery::Sessions(inventory));
            }).is_ok());
            if !self.in_flight { page.message = "The journal reader could not start.".into(); page.revision += 1; }
        }

        fn start_empty(&mut self, page: &mut JournalPage) {
            if self.emptying.is_some() || !matches!(page.source, JournalSource::Sessions) { return }
            let (Some(location), Some((store, _))) = (&self.location, &self.plate) else { return };
            let Some(confirmed) = page.empty_confirm.take() else { return };
            let store = store.clone();
            let delivery = ToUIReceiver::default();
            let tx = delivery.sender();
            page.revision += 1;
            match location.pool.submit_named(Class::Background, "Fab journal empty removed sessions", move || {
                let result = empty_removed(&store, &confirmed, &mut |line| makepad_widgets::log!("{}", line));
                let _ = tx.send(Delivery::Emptied { root: store.root, result });
            }) {
                Ok(task) => { self.emptying = Some(delivery); task.detach(); }
                Err(_) => {
                    page.message = "Fab could not start emptying removed sessions.".into();
                    self.showing_remove_outcome = true;
                }
            }
        }

        fn start_remove(&mut self, folder: &Arc<str>, token: u64, page: &mut JournalPage) {
            if self.removal.is_some() { return }
            let JournalSource::Session(session) = &page.source else { return };
            if session.folder != *folder || session.token.is_none() { return }
            if session.token != Some(token) {
                self.remove_outcome(Err(Refusal::Changed), page);
                return
            }
            let (Some(location), Some((store, _))) = (&self.location, &self.plate) else { return };
            let store = store.clone();
            let folder = folder.clone();
            let delivery = ToUIReceiver::default();
            let tx = delivery.sender();
            page.remove_confirm = None;
            page.revision += 1;
            match location.pool.submit_named(Class::Background, "Fab journal remove session", move || {
                let result = remove_session(&store, &folder, token, &mut |line| makepad_widgets::log!("{}", line));
                let _ = tx.send(Delivery::Removed { root: store.root, folder, token, result });
            }) {
                Ok(task) => { self.removal = Some(delivery); task.detach(); }
                Err(_) => {
                    page.message = "Fab could not start the session removal.".into();
                    self.showing_remove_outcome = true;
                }
            }
        }

        fn remove_outcome(&mut self, result: Result<Removed, Refusal>, page: &mut JournalPage) {
            if result.is_ok() || matches!(result, Err(Refusal::Changed)) {
                self.change_source(JournalSource::Sessions, page);
            }
            page.remove_confirm = None;
            page.message = match result {
                Ok(moved) => moved.warning().unwrap_or_default().into(),
                Err(refused) => refused.to_string(),
            };
            // An ordinary page or inventory refresh must keep the outcome, including silent success.
            self.showing_remove_outcome = true;
            page.revision += 1;
        }

        pub fn action(&mut self, action: &JournalAction, page: &mut JournalPage) {
            let previous = (page.selected.clone(), page.through.clone());
            match action {
                JournalAction::ThisPlate => { self.change_source(JournalSource::Plate, page); return }
                JournalAction::Sessions => { self.change_source(JournalSource::Sessions, page); return }
                JournalAction::RemoveSession { folder, token } => { self.start_remove(folder, *token, page); return }
                JournalAction::EmptyRemoved => { self.start_empty(page); return }
                JournalAction::BrowseSession(folder) => {
                    if let Some(session) = page.sessions.iter().find(|s| s.folder == *folder) {
                        self.change_source(JournalSource::Session(session.clone()), page);
                    }
                    return
                }
                JournalAction::Replay if page.count == 1 => {
                    if let Some(selected) = page.selected.as_ref() {
                        match self.start_replay(reader_key(selected), None) {
                            Ok(()) => { page.replay_status = "Preparing the replay…".into(); page.replay_waiting = true; }
                            Err(e) => page.replay_status = e.into(),
                        }
                        page.revision += 1;
                    }
                    return
                }
                JournalAction::CancelReplay if page.replay_waiting => { self.cancel_replay(page); return }
                JournalAction::Copy => {
                    let reader = self.archive.as_mut().map(|(_, r)| r).or(self.reader.as_mut());
                    if let (Some(reader), Some(from)) = (reader, &page.selected) {
                        let to = reader_key(page.through.as_ref().unwrap_or(from));
                        let from = reader_key(from);
                        let filter = page_filter(page);
                        let end = self.selection_end;
                        let tx = self.panel_deliveries.sender();
                        if page.count > 0 && reader.query(move |browse| {
                            let text = browse.and_then(|b| b.script(&from, &to, filter, end));
                            let _ = tx.send(Delivery::Clipboard(text));
                        }).is_err() { page.message = COPY_ERROR.into(); page.revision += 1; }
                    }
                    return
                }
                JournalAction::Filter { actor, kind } => {
                    (page.actor, page.kind) = (*actor, *kind);
                    self.pages.clear();
                    page.selected = None;
                    page.through = None;
                }
                JournalAction::Older if page.older => {
                    let Some(last) = page.rows.last() else { return };
                    self.pages.push(reader_key(&last.key));
                }
                JournalAction::Newer if !self.pages.is_empty() => { self.pages.pop(); }
                JournalAction::Select(k) if page.rows.iter().any(|r| &r.key == k) => { page.selected = Some(k.clone()); page.through = None; }
                JournalAction::Extend(k) if page.rows.iter().any(|r| &r.key == k) => {
                    page.selected.get_or_insert_with(|| k.clone());
                    page.through = Some(k.clone());
                }
                _ => return,
            }
            if matches!(action, JournalAction::Older | JournalAction::Newer | JournalAction::Filter { .. }) {
                self.showing_remove_outcome = false;
                page.navigation += 1;
                page.rows = Arc::default();
                page.message.clear();
            }
            if page.message.starts_with(REPLACED) { page.message.clear(); }
            if (page.selected.as_ref(), page.through.as_ref()) != (previous.0.as_ref(), previous.1.as_ref()) {
                if self.replay.as_ref().is_some_and(|r| r.call.is_none() && r.remaining != Some(0)) {
                    self.cancel_replay(page);
                }
                if self.replay.is_none() && self.opening.is_none() { page.replay_status.clear(); }
                page.count = 0;
                page.set_diffs(Vec::new());
                page.detail.clear();
                self.diff_version = None;
            }
            page.revision += 1;
            self.revision += 1;
            self.dirty = true;
        }

        fn refresh(&mut self, page: &mut JournalPage) {
            if matches!(page.source, JournalSource::Sessions) { self.refresh_sessions(page); return }
            let Some(reader) = self.archive.as_mut().map(|(_, r)| r).or(self.reader.as_mut()) else {
                self.dirty = false;
                if self.location.is_some() { page.message = "The journal reader could not start.".into(); page.revision += 1; }
                else {
                    self.in_flight = self.panel_deliveries.sender().send(Delivery::Page { revision: self.revision,
                        rows: Arc::default(), more: false, detail: String::new(), count: 0, end: 0, diff_version: None, diffs: None }).is_ok();
                }
                return
            };
            let tx = self.panel_deliveries.sender();
            let revision = self.revision;
            let filter = page_filter(page);
            let before = self.pages.last().cloned();
            let selected = page.selected.as_ref().map(reader_key);
            let through = page.through.as_ref().map(reader_key);
            let shown = self.diff_version.clone();
            let result = reader.query(move |browse| {
                let result = browse.and_then(|mut browse| {
                    let (rows, next) = browse.page(filter, before, PAGE)?;
                    let end = browse.complete_end();
                    let mut count = 0;
                    let mut diff_version = None;
                    let mut diffs = Some(Vec::new());
                    let detail = if let Some(k) = selected {
                        let positions = browse.range(&k, through.as_ref().unwrap_or(&k), filter, end)?;
                        count = positions.len();
                        if let [position] = positions.as_slice() {
                            let k = Key { position: *position, ..k };
                            match browse.detail(k.clone())? {
                                Read::Detail { record, partner } => {
                                    match browse.diffs(k.position, shown.as_ref()) {
                                        Ok((version, update)) => { diff_version = Some(version); diffs = update; }
                                        Err(_) => diffs = Some(vec![JournalDiffUpdate::Prepared(JournalDiff::note("Document diff unavailable.".into()))]),
                                    }
                                    let mut detail = detail_text(&k, &record, &partner);
                                    if let Some(names) = documents(&browse, &record, browse.complete_end())? {
                                        for name in names { let _ = write!(detail, "\nDocument: {name}"); }
                                    }
                                    detail
                                }
                                _ => "Record not found.".into(),
                            }
                        } else { String::new() }
                    } else { String::new() };
                    Ok(Delivery::Page { revision, rows: rows.iter().map(panel_row).collect(), more: next.is_some(), detail, count, end, diff_version, diffs })
                });
                let _ = tx.send(result.unwrap_or_else(|e| Delivery::Failed(revision, e.kind())));
            });
            self.dirty = false;
            self.in_flight = result.is_ok();
            if result.is_err() { page.message = "Journal reader unavailable.".into(); page.revision += 1; }
        }

        pub fn take_clipboard(&mut self) -> Option<String> { self.clipboard.take() }

        pub fn update(&mut self, shown: bool, signal: bool, page: &mut JournalPage, calls: &mut Calls) -> bool {
            let old_revision = page.revision;
            if shown && page.drawn { self.activate(); }
            if signal {
                for (reader, displayed, primary) in [
                    (self.reader.as_mut(), matches!(page.source, JournalSource::Plate), true),
                    (self.archive.as_mut().map(|(_, r)| r), matches!(page.source, JournalSource::Session(_)), false),
                ] {
                    let Some(reader) = reader else { continue };
                    loop {
                        match reader.try_recv() {
                            Ok(Some(reply)) => match reply.answer {
                                Read::Changed if displayed => self.dirty = true,
                                Read::Replaced => {
                                    if self.replay.as_ref().is_some_and(|r| r.key.folder == reply.target.folder) {
                                        self.replay_generation += 1;
                                        if let Some(id) = self.replay.take().and_then(|r| r.call) {
                                            self.pending.remove(&id);
                                            finish(calls, id, Err(EARLIER));
                                        }
                                        self.ready = None;
                                        page.replay_waiting = false;
                                        page.replay_status = UNAVAILABLE.into();
                                        page.revision += 1;
                                    }
                                    if displayed {
                                        self.pages.clear();
                                        self.diff_version = None;
                                        self.selection_end = 0;
                                        self.clipboard = None;
                                        self.revision += 1;
                                        self.dirty = true;
                                        page.selected = None;
                                        page.through = None;
                                        page.rows = Arc::default();
                                        page.count = 0;
                                        page.detail.clear();
                                        page.set_diffs(Vec::new());
                                        page.message = if page.message.ends_with(COPY_ERROR) {
                                            format!("{REPLACED} {COPY_ERROR}")
                                        } else { REPLACED.into() };
                                        page.navigation += 1;
                                        page.revision += 1;
                                    }
                                }
                                Read::Health(health) if primary => {
                                    self.current_health = match health { Health::Ok => String::new(),
                                        Health::Failing(kind) => format!("Recording paused because {}. Recording will resume when it can.", error_reason(kind)) };
                                    if displayed { page.health.clone_from(&self.current_health); page.revision += 1; }
                                }
                                Read::Error(kind) if displayed => {
                                    let source = std::mem::take(&mut page.source);
                                    page.reset();
                                    page.source = source;
                                    page.message = format!("Journal unavailable because {}.", error_reason(kind));
                                    self.diff_version = None;
                                    self.revision += 1;
                                }
                                _ => (),
                            },
                            Ok(None) => break,
                            Err(_) => {
                                if displayed { page.message = "Journal reader unavailable.".into(); page.revision += 1; }
                                if self.replay.as_ref().is_some_and(|r| r.call.is_some() && primary || r.call.is_none() && displayed) {
                                    self.replay = None;
                                    page.replay_status = "Journal reader unavailable.".into();
                                    page.replay_waiting = false;
                                    self.ready = None;
                                }
                                if primary { for id in self.pending.drain() { finish(calls, id, Err("Journal reader unavailable.")); } }
                                break
                            }
                        }
                    }
                }
                while let Some(reply) = self.deliveries.try_recv().ok().or_else(|| self.panel_deliveries.try_recv().ok())
                    .or_else(|| self.removal.as_ref()?.try_recv().ok())
                    .or_else(|| self.emptying.as_ref()?.try_recv().ok()) {
                    match reply {
                        Delivery::Emptied { root, result } => {
                            self.emptying = None;
                            if self.location.as_ref().is_some_and(|l| l.root == root) && matches!(page.source, JournalSource::Sessions) {
                                self.change_source(JournalSource::Sessions, page);
                                page.message = result.message();
                                self.showing_remove_outcome = true;
                            }
                        }
                        Delivery::Removed { root, folder, token, result } => {
                            self.removal = None;
                            if self.location.as_ref().is_none_or(|l| l.root != root) { continue }
                            match &page.source {
                                JournalSource::Session(s) if s.folder == folder && s.token == Some(token) => self.remove_outcome(result, page),
                                JournalSource::Sessions if result.is_ok() || matches!(result, Err(Refusal::Changed)) => self.remove_outcome(result, page),
                                _ => (),
                            }
                        }
                        Delivery::Sessions(result) => {
                            self.in_flight = false;
                            match result {
                                Ok((sessions, removed)) => {
                                    if !self.showing_remove_outcome {
                                        page.message = if sessions.is_empty() { "No sessions without a saved plate.".into() } else { String::new() };
                                    }
                                    page.sessions = sessions;
                                    page.removed = removed;
                                }
                                Err(e) => page.message = format!("Sessions unavailable because {}.", error_reason(e.kind())),
                            }
                            page.revision += 1;
                        }
                        Delivery::Settling(generation, n) => self.settled(generation, n, page),
                        Delivery::Rebuilt(generation, result) => self.rebuilt(generation, result, page, calls),
                        Delivery::Tool(id, answer) => { if self.pending.remove(&id) { finish(calls, id, answer); } }
                        Delivery::Clipboard(result) => match result {
                            Ok(text) => self.clipboard = Some(text),
                            Err(_) => {
                                page.message = if page.message.starts_with(REPLACED) {
                                    format!("{REPLACED} {COPY_ERROR}")
                                } else { COPY_ERROR.into() };
                                page.revision += 1;
                            }
                        },
                        Delivery::Page { revision, rows, more, detail, count, end, diff_version, diffs } => {
                            self.in_flight = false;
                            if revision != self.revision { continue }
                            page.rows = rows;
                            page.detail = detail;
                            page.count = count;
                            self.selection_end = end;
                            self.diff_version = diff_version;
                            if let Some(diffs) = diffs { page.set_diffs(diffs); }
                            page.older = more;
                            page.newer = !self.pages.is_empty();
                            if !self.showing_remove_outcome && !page.message.starts_with(REPLACED) {
                                page.message = if page.rows.is_empty() {
                                    if page.actor == 0 && page.kind == 0 { "No journal records." } else { "No matching records." }.into()
                                } else { String::new() };
                            }
                            page.revision += 1;
                        }
                        Delivery::Failed(revision, kind) => {
                            self.in_flight = false;
                            if revision == self.revision { page.message = format!("Journal unavailable because {}.", error_reason(kind)); page.revision += 1; }
                        }
                    }
                }
            }
            if shown && !self.shown { self.dirty = true; }
            self.shown = shown;
            if shown && self.active && self.dirty && !self.in_flight { self.refresh(page); }
            old_revision != page.revision && shown
        }
    }

    fn page_filter(page: &JournalPage) -> Filter {
        Filter { actor: page.actor.checked_sub(1).and_then(|i| ACTORS.get(i)).map(|p| p.1),
                kind: page.kind.checked_sub(1).and_then(|i| KINDS.get(i)).map(|p| p.1) }
    }

    fn finish(calls: &mut Calls, id: u64, result: Result<String, &'static str>) {
        let ok = result.is_ok();
        let text = result.unwrap_or_else(|error| obj(vec![("error", s(error))]).to_json());
        calls.finish(id, Answer { ok, text, images: Vec::new() });
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{fs, io::Write};

        struct Fixture {
            root: PathBuf, browser: Browser, page: JournalPage, calls: Calls, cx: Cx,
        }

        impl Fixture {
            fn new() -> Self {
                Self { root: crate::journal::test_dir(), browser: Default::default(), page: Default::default(),
                    calls: Default::default(), cx: Cx::new(Box::new(|_, _| {})) }
            }
            fn select(&mut self, folder: &str) {
                self.browser.select_location(&mut self.cx, Some((&self.root, folder)), &mut self.calls, &mut self.page);
            }
            fn append(&self, folder: &str, line: &str) {
                let dir = self.root.join(folder).join("journal");
                fs::create_dir_all(&dir).unwrap();
                writeln!(fs::OpenOptions::new().create(true).append(true).open(dir.join("records.jsonl")).unwrap(), "{line}").unwrap();
            }
            fn update_until(&mut self, done: impl Fn(&Self) -> bool) {
                self.page.drawn = true;
                self.pump_until(true, done);
            }
            fn pump_until(&mut self, shown: bool, done: impl Fn(&Self) -> bool) {
                let start = std::time::Instant::now();
                loop {
                    self.browser.update(shown, true, &mut self.page, &mut self.calls);
                    if done(self) { return }
                    assert!(start.elapsed() < std::time::Duration::from_secs(5), "journal worker did not deliver the requested page or answer");
                    std::thread::yield_now();
                }
            }
            fn tool(&mut self, name: &str, args: Value) -> Value {
                self.browser.tool(&mut self.calls, 7, name, &args);
                self.pump_until(self.page.drawn, |f| f.calls.answers.contains_key(&7));
                let answer = self.calls.answers.remove(&7).unwrap();
                assert!(answer.ok, "the registered journal request should succeed");
                wire_value(answer)
            }
        }
        fn wire_value(answer: Answer) -> Value {
            assert!(answer.text.len() <= MAX_RESULT_BYTES, "Fab must produce a complete reply before the transport bounds it");
            let mut sent = crate::chat::tool_result("journal", answer);
            sent.bound();
            makepad_strict_json::parse(sent.text.as_bytes()).expect("the actual wire result must be a complete JSON object")
        }
        impl Drop for Fixture {
            fn drop(&mut self) { self.browser.reader.take(); fs::remove_dir_all(&self.root).unwrap(); }
        }
        fn record_keys(f: &Fixture) -> impl Fn(i64) -> Value {
            let journal = crate::journal::reader::test_token(&f.root, "a");
            move |order| key_json(&RecordKey { folder: "a".into(), journal, number: order as u64, damaged: false })
        }
        fn parse(text: &str) -> Value { makepad_strict_json::parse(text.as_bytes()).unwrap() }

        impl Fixture {
            fn replay_store(&mut self) -> fab_ai::plates::PlateStore {
                let store = fab_ai::plates::PlateStore {
                    root: self.root.clone(), home: self.root.join("copies"), demos: self.root.join("demos"),
                };
                self.select("a");
                self.browser.plate = Some((store.clone(), "Bracket".into()));
                store
            }

            fn press_replay(&mut self, order: u64) {
                self.update_until(|f| f.page.rows.iter().any(|r| r.key.number == order && !r.key.damaged));
                let key = self.page.rows.iter().find(|r| r.key.number == order && !r.key.damaged).unwrap().key.clone();
                self.browser.action(&JournalAction::Select(key), &mut self.page);
                self.update_until(|f| f.page.count == 1);
                self.browser.action(&JournalAction::Replay, &mut self.page);
            }

            /// Hold a real pool job between records. Disconnecting releases it
            /// even if a test assertion fails while the reader is answering.
            fn pause_rebuild(&mut self) {
                let request = self.browser.replay.as_mut().unwrap();
                let progress = Arc::get_mut(&mut request.progress).unwrap();
                let (release, wait) = std::sync::mpsc::channel();
                request.resume = Some(release);
                let wait = std::sync::Mutex::new(wait);
                progress.pause = Some(Box::new(move |done| {
                    if done == 1 { let _ = wait.lock().unwrap().recv(); }
                }));
                self.update_until(|f| f.browser.replay.as_ref().unwrap().progress.done.load(Ordering::Relaxed) == 1);
                assert!(self.browser.show_progress(&mut self.page));
                assert!(self.page.replay_waiting);
                assert!(!self.browser.show_progress(&mut self.page));
            }

            fn answer_open(&mut self, ok: bool) {
                self.browser.opening(Some(42), &mut self.page, &mut self.calls);
                self.browser.opened(42, ok, &mut self.page, &mut self.calls);
            }
        }

        #[test]
        fn journal_replay_copies_before_the_call_and_answers_only_for_the_current_plate() {
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            let store = f.replay_store();
            let source = f.root.join("a");
            let dir = crate::journal::calls_fixture(&source, false, false, false);
            fn contents(dir: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
                let mut files = std::collections::BTreeMap::new();
                for entry in fs::read_dir(dir).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_type().unwrap().is_dir() {
                        for (path, bytes) in contents(&entry.path()) { files.insert(PathBuf::from(entry.file_name()).join(path), bytes); }
                    } else { files.insert(PathBuf::from(entry.file_name()), fs::read(entry.path()).unwrap()); }
                }
                files
            }
            let before = contents(&source);
            let manifest = fs::read(source.join("piece.fab.splash")).unwrap();
            let tools = crate::chat::services().pop().unwrap();
            assert!(tools.tool("journal_replay").is_some_and(|t| t.risk == Risk::Request));
            let rec = record_keys(&f);
            let ask = |key| obj(vec![("record", key)]);
            f.browser.tool(&mut f.calls, 3, "journal_replay", &ask(obj(vec![("folder",s("b")),("order",Value::Int(4))])));
            assert!(!f.calls.answers.remove(&3).unwrap().ok);
            assert!(!store.home.exists());

            f.browser.tool(&mut f.calls, 4, "journal_replay", &ask(rec(crate::journal::at(1, 4) as i64)));
            f.pump_until(false, |f| f.browser.ready.is_some());
            let (file, name) = f.browser.project_to_open().unwrap();
            assert_eq!(name, "Bracket (before fab.workspace_edit)");
            assert_eq!(file.parent().unwrap().parent(), Some(store.home.as_path()));
            assert_eq!(contents(file.parent().unwrap()), [(PathBuf::from("piece.fab.splash"), manifest)].into());
            assert!(!f.calls.answers.contains_key(&4));
            let mut session = fab_ai::Session::new(crate::print::CheckSetup::fab_defaults().machine().unwrap(), store.home.clone());
            session.store = Some(store.clone());
            assert!(session.open(&file).is_ok());
            let copy_journal = crate::journal::Plate::opened(&mut session).unwrap();
            let (_, folder) = copy_journal.reader_location();
            assert!(!store.dir(folder).join("journal").exists());
            let flush = copy_journal.measured_writer();
            crate::journal::Call::begin(&copy_journal, fab_elec_design::By::Person, "inspect", obj(vec![])).done("ok", String::new());
            flush();
            let log = fs::read_to_string(store.dir(folder).join("journal/records.jsonl")).unwrap();
            let first = parse(log.lines().next().unwrap());
            assert_eq!(first.get("kind").and_then(Value::as_str), Some("checkpoint"));
            assert_eq!(first.get("order").and_then(Value::as_u64), Some(crate::journal::at(1, 1)));
            f.answer_open(true);
            let answer = wire_value(f.calls.answers.remove(&4).unwrap());
            assert_eq!(answer, obj(vec![("plate", s(&name))]));
            assert_eq!(contents(&source), before);

            // The effect includes the new file and gets its own reservation time.
            f.press_replay(crate::journal::at(1, 5));
            f.update_until(|f| f.browser.ready.is_some());
            let (file, name) = f.browser.project_to_open().unwrap();
            let row = fs::read_to_string(dir.join("records.jsonl")).unwrap().lines()
                .map(parse).find(|row| row.get("order").and_then(Value::as_u64) == Some(crate::journal::at(1, 5))).unwrap();
            let ms = row.get("time_ms").unwrap().as_u64().unwrap();
            assert_eq!(name, format!("Bracket (as of {})", makepad_civil_time::local_or_utc((ms / 1000) as i64).hm()));
            assert_eq!(fs::read(file.with_file_name("kept.txt")).unwrap(), b"recorded effect");
            f.answer_open(true);
            assert_eq!(contents(&source), before);
        }

        #[test]
        fn journal_replay_waits_for_the_earlier_reservation_and_can_cancel() {
            use crate::journal::{Binding, Call};
            use fab_elec_design::By;
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            let store = f.replay_store();
            let live = Plate::testing(&store.dir("a").join("journal"), "part.fab.splash", &Default::default());
            live.attach(&f.cx.thread_spawner());
            let first = Call::begin(&live, By::Person, "fab.inspect", obj(vec![]));
            let earlier = Binding::new(first.clone());
            first.done("ok", String::new()); // Order 3 is held by this ticket.
            let next = Call::begin(&live, By::Person, "fab.params", obj(vec![]));
            let later = Binding::new(next.clone());
            next.done("ok", String::new());  // Order 5 must not hold a replay through 4.
            f.press_replay(crate::journal::at(1, 4));
            f.update_until(|f| f.page.replay_status.contains("1 earlier record"));
            assert!(f.page.replay_waiting && !store.home.exists());
            f.browser.action(&JournalAction::CancelReplay, &mut f.page);
            assert!(f.browser.replay.is_none() && !f.page.replay_waiting);
            f.browser.action(&JournalAction::Replay, &mut f.page);
            f.update_until(|f| f.page.replay_status.contains("1 earlier record"));
            f.browser.action(&JournalAction::Select(RecordKey { folder: "a".into(), journal: f.page.rows[0].key.journal, number: crate::journal::at(1, 1), damaged: false }), &mut f.page);
            assert!(f.browser.replay.is_none());
            f.press_replay(crate::journal::at(1, 4));
            f.update_until(|f| f.page.replay_status.contains("1 earlier record"));
            drop(earlier);
            f.update_until(|f| f.browser.ready.is_some());
            assert_eq!(f.browser.project_to_open().unwrap().1, "Bracket (before fab.params)");
            f.answer_open(true);
            drop(later);
            f.update_until(|f| f.page.rows.iter().any(|r| r.key.number == crate::journal::at(1, 5)));
        }

        #[test]
        fn journal_page_and_tool_answer_during_a_rebuild() {
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            f.replay_store();
            crate::journal::calls_fixture(&f.root.join("a"), false, false, false);
            f.press_replay(crate::journal::at(1, 5));
            f.pause_rebuild();
            assert_eq!(f.page.replay_status, "Rebuilding: 1 of 5 records");
            assert_eq!(grouped(1_234_567), "1,234,567");
            f.browser.action(&JournalAction::Filter { actor: 0, kind: 2 }, &mut f.page);
            f.update_until(|f| !f.page.rows.is_empty());
            assert!(f.page.rows.iter().all(|row| row.context.starts_with("Result")));
            let rec = record_keys(&f);
            let read = f.tool("journal_read", obj(vec![("record", rec(crate::journal::at(1, 4) as i64))]));
            assert_eq!(read.get("record").unwrap().get("action").unwrap().as_str(), Some("fab.workspace_edit"));
            assert!(f.browser.project_to_open().is_none());
            f.browser.replay.as_mut().unwrap().resume.take().unwrap().send(()).unwrap();
            f.update_until(|f| f.browser.ready.is_some());
            let (manifest, _) = f.browser.project_to_open().unwrap();
            assert_eq!(fs::read(manifest.with_file_name("kept.txt")).unwrap(), b"recorded effect");
            f.answer_open(true);
            assert!(f.page.replay_status.is_empty());
        }

        #[test]
        fn journal_cancel_during_a_rebuild_removes_the_claimed_copy() {
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            let store = f.replay_store();
            crate::journal::calls_fixture(&f.root.join("a"), false, false, false);
            f.press_replay(crate::journal::at(1, 5));
            f.pause_rebuild();
            let worker = Arc::downgrade(&f.browser.replay.as_ref().unwrap().progress);
            assert_eq!(fs::read_dir(&store.home).unwrap().count(), 1);
            f.browser.action(&JournalAction::CancelReplay, &mut f.page);
            assert!(f.page.replay_status.is_empty() && !f.page.replay_waiting);
            f.update_until(|_| worker.upgrade().is_none());
            assert_eq!(fs::read_dir(&store.home).unwrap().count(), 0);
            assert!(f.browser.project_to_open().is_none() && f.page.replay_status.is_empty());

            // A click may also arrive after the worker sent the completed copy
            // and before the UI drains its delivery. That copy is still private.
            f.press_replay(crate::journal::at(1, 5));
            f.pause_rebuild();
            let queued = std::mem::take(&mut f.browser.deliveries);
            f.browser.replay.as_mut().unwrap().resume.take().unwrap().send(()).unwrap();
            f.update_until(|f| Arc::strong_count(&f.browser.replay.as_ref().unwrap().progress) == 1);
            assert_eq!(fs::read_dir(&store.home).unwrap().count(), 1);
            f.browser.deliveries = queued;
            f.browser.action(&JournalAction::CancelReplay, &mut f.page);
            f.update_until(|_| fs::read_dir(&store.home).unwrap().next().is_none());
            assert!(f.browser.project_to_open().is_none() && f.page.replay_status.is_empty());
        }

        #[test]
        fn journal_replay_keeps_an_ambiguous_copy_and_leaves_no_folder_for_a_gap() {
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            let store = f.replay_store();
            let journal = store.dir("a").join("journal");
            let manifest = b"use mod.fab.*";
            let hash = makepad_core_util::sha256::sha256_hex(manifest);
            fs::create_dir_all(journal.join("blobs")).unwrap();
            fs::write(journal.join("blobs").join(&hash), manifest).unwrap();
            let file = |n| obj(vec![("name", s(n)), ("hash", s(&hash))]);
            f.append("a", &obj(vec![("order",Value::Int(1)),("kind",s("checkpoint")),
                ("files", Value::Arr(vec![file("a.fab.splash"),file("b.fab.splash")]))]).to_json());
            f.append("a", r#"{"order":2,"id":2,"kind":"call","action":"fab.inspect","time_ms":60000}"#);
            f.append("a", r#"{"order":3,"kind":"effect","call":2,"changes":[]}"#);
            f.press_replay(3); // Old effect: the call has a time but this record has none.
            f.update_until(|f| f.page.replay_status.contains("no single plate file"));
            let folder = store.home.join("Bracket (replay)");
            assert!(f.page.replay_status.contains(folder.to_str().unwrap()));
            assert!(folder.join("a.fab.splash").is_file() && folder.join("b.fab.splash").is_file());
            assert!(f.browser.project_to_open().is_none());
            f.append("a", r#"{"order":5,"kind":"lost","cause":"unwritten","from":4,"to":4,"count":1}"#);
            f.append("a", r#"{"order":6,"kind":"reload","changes":[]}"#);
            f.press_replay(6);
            f.update_until(|f| f.page.replay_status.contains("earlier record") && !f.page.replay_waiting);
            assert!(f.page.replay_status.contains("damaged or incomplete"));
            assert!(f.browser.project_to_open().is_none());
            assert_eq!(fs::read_dir(&store.home).unwrap().count(), 1);

            // A later checkpoint supersedes the gap. Its manifest asks the normal
            // project loader for a sidecar that was never in the journal.
            let needs_script = b"use mod.fab.*\nscript(\"missing.cad.splash\", name: \"part\")\n";
            let hash = makepad_core_util::sha256::sha256_hex(needs_script);
            fs::write(journal.join("blobs").join(&hash), needs_script).unwrap();
            f.append("a", &obj(vec![("order",Value::Int(7)),("kind",s("checkpoint")),("manifest",s("part.fab.splash")),
                ("files", Value::Arr(vec![obj(vec![("name",s("part.fab.splash")),("hash",s(hash))])]))]).to_json());
            f.press_replay(7);
            f.update_until(|f| f.browser.ready.is_some());
            let (file, _) = f.browser.project_to_open().unwrap();
            let mut session = fab_ai::Session::new(crate::print::CheckSetup::fab_defaults().machine().unwrap(), store.home.clone());
            let open = session.open(&file);
            assert!(open.as_ref().err().is_some_and(|e| e.contains("missing.cad.splash")));
            f.answer_open(open.is_ok());
            assert!(f.page.replay_status.contains("rebuilt") && f.page.replay_status.contains("could not open"));
            assert!(f.page.replay_status.contains(file.parent().unwrap().to_str().unwrap()) && file.exists());
        }

        #[test]
        fn journal_tab_and_tools_share_rows_links_filters_and_plate_selection() {
            let _guard = crate::cx_shared();
            let manifest = crate::chat::services().pop().unwrap();
            for tool in ["journal_list", "journal_read", "journal_copy"] {
                assert!(manifest.tool(tool).is_some_and(|t| t.risk == Risk::Read));
            }
            let mut f = Fixture::new();
            f.select("empty");
            f.browser.update(false, true, &mut f.page, &mut f.calls);
            f.browser.update(true, false, &mut f.page, &mut f.calls);
            assert!(f.browser.reader.is_none(), "selecting a plate before the tab draws must not start the worker");
            f.page.drawn = true;
            f.browser.update(true, false, &mut f.page, &mut f.calls);
            assert!(f.browser.reader.is_some(), "the first visible frame starts the reader");
            f.tool("journal_list", obj(vec![]));
            assert!(!f.root.join("empty").exists());
            f.append("a", r#"{"kind":"call","order":1,"id":1,"action":"fab.place","actor":{"kind":"agent"},"time_ms":60000,"args":{"x":2}}"#);
            f.append("a", r#"{"kind":"effect","order":4,"call":1,"actor":{"kind":"agent"},"label":"Move","changes":[{"name":"plate.fab.splash"}]}"#);
            f.append("a", "damaged");
            f.append("a", r#"{"kind":"lost","order":5,"cause":"torn"}"#);
            f.select("a");
            f.update_until(|f| f.page.rows.len() == 4);
            let rec = record_keys(&f);
            let first = f.tool("journal_list", obj(vec![("count", Value::Int(2))]));
            let rows = first.get("rows").unwrap().as_arr().unwrap();
            assert_eq!(rows[0].get("kind").unwrap().as_str(), Some("diagnostic"));
            assert_eq!(rows[1].get("kind").unwrap().as_str(), Some("lost"));
            assert_eq!(f.page.rows[0].title, "Damaged journal line");
            assert_eq!(f.page.rows[1].title, "Recording gap");
            for (row, ui) in rows.iter().zip(f.page.rows.iter()) { assert_eq!(row.get("key"), Some(&key_json(&ui.key))); }
            let next = f.tool("journal_list", obj(vec![("before", first.get("before").unwrap().clone())]));
            assert_eq!(next.get("rows").unwrap().as_arr().unwrap()[0].get("key"), Some(&rec(4)));
            let call_row = &next.get("rows").unwrap().as_arr().unwrap()[1];
            assert_eq!(call_row.get("time_ms").and_then(Value::as_u64), Some(60_000));
            assert_eq!(f.page.rows[3].context, format!("Call · Agent · {}", makepad_civil_time::local_or_utc(60).hms()));
            let names = Value::Arr(vec![s("plate.fab.splash")]);
            let call = f.tool("journal_read", obj(vec![("record", rec(1))]));
            assert_eq!(call.get("documents"), Some(&names), "the call lists the files its effects changed");
            let effect = f.page.rows[2].key.clone();
            f.browser.action(&JournalAction::Select(effect.clone()), &mut f.page);
            f.update_until(|f| f.page.detail.contains("plate.fab.splash"));
            assert!(f.page.detail.contains("fab.place"));
            let detail = f.tool("journal_read", obj(vec![("record", rec(4))]));
            assert_eq!(detail.get("partner").unwrap().get("key"), Some(&rec(1)));
            f.append("a", r#"{"kind":"done","order":3,"id":1,"outcome":"ok","reply":"Moved"}"#);
            // A read of the updated file wakes the panel immediately like a durable notification.
            f.tool("journal_list", obj(vec![]));
            f.update_until(|f| f.page.rows.len() == 5);
            assert_eq!(f.page.selected, Some(effect), "a late result must keep the selected effect");
            f.browser.action(&JournalAction::Filter { actor: 3, kind: 2 }, &mut f.page);
            f.update_until(|f| f.page.rows.len() == 1);
            let filtered = f.tool("journal_list", parse(r#"{"actor":"agent","kind":"done"}"#));
            let done = &filtered.get("rows").unwrap().as_arr().unwrap()[0];
            assert_eq!(done.get("actor").unwrap().as_str(), Some("agent"));
            assert_eq!(done.get("key"), Some(&key_json(&f.page.rows[0].key)));
            assert!(f.page.rows[0].context.contains("Agent"));
            assert!(f.page.rows[0].title.contains("fab.place"));
            f.browser.action(&JournalAction::Select(f.page.rows[0].key.clone()), &mut f.page);
            f.update_until(|f| f.page.detail.contains("plate.fab.splash"));
            let done = f.tool("journal_read", obj(vec![("record", rec(3))]));
            assert_eq!(done.get("documents"), Some(&names), "the result lists the same files as the call and panel");
            f.browser.tool(&mut f.calls, 12, "journal_read", &obj(vec![("record", rec(4))]));
            f.append("b", r#"{"kind":"checkpoint","order":1,"files":[]}"#);
            f.select("b");
            assert!(!f.calls.answers.remove(&12).unwrap().ok, "switch settles an old tool request");
            assert!(f.page.rows.is_empty() && f.page.selected.is_none());
            f.update_until(|f| f.page.rows.len() == 1);
            assert_eq!(&*f.page.rows[0].key.folder, "b");
            assert!(Tool::parse("journal_read", &obj(vec![("record", rec(4))]), "b").is_err());
        }

        #[test]
        fn journal_replacement_invalidates_detail_copy_replay_and_tool_keys() {
            use std::sync::mpsc;
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            let store = f.replay_store();
            for (folder, tag) in [("a", "a"), ("b", "b")] {
                let dir = store.dir(folder).join("journal");
                let hash = makepad_core_util::sha256::sha256_hex(tag.as_bytes());
                fs::create_dir_all(dir.join("blobs")).unwrap();
                fs::write(dir.join("blobs").join(&hash), tag).unwrap();
                fs::write(dir.join("incarnation"), tag.repeat(32)).unwrap();
                let file = obj(vec![("name", s("piece.fab.splash")), ("hash", s(hash))]);
                f.append(folder, &obj(vec![("kind", s("checkpoint")), ("order", Value::Int(1)),
                    ("files", Value::Arr(vec![file]))]).to_json());
            }
            let dir = store.dir("a").join("journal");
            let next = store.dir("b").join("journal");
            assert_eq!(fs::metadata(dir.join("records.jsonl")).unwrap().len(),
                fs::metadata(next.join("records.jsonl")).unwrap().len(), "length cannot distinguish these fixtures");
            f.update_until(|f| f.page.rows.len() == 1);
            let old = f.page.rows[0].key.clone();
            f.browser.action(&JournalAction::Select(old.clone()), &mut f.page);
            f.browser.action(&JournalAction::Extend(old.clone()), &mut f.page);
            f.update_until(|f| f.page.count == 1 && f.page.through.is_some());

            // Hold a Browse after its scan. The pool also gets exactly the log that
            // settled() would send it, before any of the opens under test.
            let (validated, handle) = mpsc::channel();
            let (resume, gate) = mpsc::channel();
            let (result, inspected) = mpsc::channel();
            let k = reader_key(&old);
            f.browser.reader.as_mut().unwrap().query(move |browse| {
                let mut b = browse.unwrap();
                validated.send(b.replay_record(&k).unwrap()).unwrap();
                if gate.recv().is_err() { return }
                let detail_missing = matches!(b.detail(k.clone()), Ok(Read::NotFound));
                let copy = b.script(&k, &k, Filter::default(), b.complete_end());
                result.send((detail_missing, copy.err().map(|e| e.kind()))).unwrap();
            }).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let (record, log) = loop {
                f.browser.update(true, true, &mut f.page, &mut f.calls);
                if let Ok(pair) = handle.try_recv() { break pair }
                assert!(std::time::Instant::now() < deadline, "reader must deliver the validated source");
                std::thread::yield_now();
            };
            f.browser.action(&JournalAction::Replay, &mut f.page);
            f.browser.action(&JournalAction::Copy, &mut f.page);
            let archived = f.root.join("archived");
            // Windows permits renaming the shared-open file itself, but can refuse
            // a rename of its parent while the file is still inside it.
            #[cfg(windows)]
            fs::rename(dir.join("records.jsonl"), f.root.join("held-log")).unwrap();
            fs::rename(&dir, &archived).unwrap();
            fs::rename(&next, &dir).unwrap();
            #[cfg(windows)]
            fs::rename(f.root.join("held-log"), archived.join("records.jsonl")).unwrap();
            resume.send(()).unwrap();

            let (missing, copy_error) = inspected.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert!(missing, "detail validated against A must never return B's record");
            assert_eq!(copy_error, Some(io::ErrorKind::NotFound), "copy must refuse B's bytes under A's offsets");
            let pool = f.cx.task_pool();
            let cleanup = pool.clone();
            let target = store.clone();
            let k = reader_key(&old);
            let mut job = pool.submit_named(Class::Background, "verify replay handoff", move || {
                ReplayCopy::rebuild(&target, "Original", &k, &record, log, &RebuildProgress::default(), cleanup).err()
            }).unwrap();
            f.update_until(|_| job.is_finished());
            let reason = job.try_take().unwrap().unwrap();
            assert_eq!(reason.as_deref(), Some(UNAVAILABLE), "a pool job validated on A must refuse the replacement");
            assert_eq!(fs::read_dir(&store.home).unwrap().count(), 0, "replay must not publish a copy of B");

            f.update_until(|f| f.page.rows.first().is_some_and(|row| row.key.journal != old.journal));
            assert_eq!(f.page.replay_status, UNAVAILABLE, "the replay press must report the vanished record");
            assert!(f.page.selected.is_none() && f.page.through.is_none(), "both endpoints of the old selection must be cleared");
            assert!(f.page.message.contains(REPLACED), "the tab must explain why it cleared the selection");
            assert!(f.browser.take_clipboard().is_none(), "the pending copy must never reach the clipboard");
            let stale = key_json(&old);
            for (tool, args) in [
                ("journal_list", obj(vec![("before", stale.clone())])),
                ("journal_read", obj(vec![("record", stale.clone())])),
                ("journal_copy", obj(vec![("from", stale.clone()), ("to", stale.clone())])),
                ("journal_replay", obj(vec![("record", stale)])),
            ] {
                f.browser.tool(&mut f.calls, 9, tool, &args);
                f.update_until(|f| f.calls.answers.contains_key(&9));
                let error = wire_value(f.calls.answers.remove(&9).unwrap());
                assert_eq!(error.get("error").and_then(Value::as_str), Some(EARLIER), "{tool} must ask for fresh keys");
            }
            let unbound = obj(vec![("folder", s("a")), ("order", Value::Int(1))]);
            assert_eq!(parse_key(&unbound, "a").unwrap_err(), "Expected a record key with an order or diagnostic offset.",
                "a key without a token is malformed");

            // Only the incarnation changes this time. Observe the reader's reply directly.
            fs::write(dir.join("incarnation"), "c".repeat(32)).unwrap();
            let reader = f.browser.reader.as_mut().unwrap();
            reader.request(crate::journal::reader::Request::Count(Filter::default())).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if reader.try_recv().unwrap().is_some_and(|r| matches!(r.answer, Read::Replaced)) { break }
                assert!(std::time::Instant::now() < deadline, "changing the incarnation alone must deliver Replaced");
                std::thread::yield_now();
            }
        }

        #[test]
        fn journal_list_pages_by_encoded_bytes_and_continues_after_the_last_key() {
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            let label = "\"é\\\n".repeat(MAX_RESULT_BYTES / 16);
            for n in 1..=5 {
                f.append("a", &obj(vec![("kind", s("effect")), ("order", Value::Int(n)), ("label", s(&label))]).to_json());
            }
            f.select("a");
            let mut args = vec![("count", Value::Int(500))];
            assert!(f.browser.reader.is_none());
            let mut orders = vec![];
            loop {
                let result = f.tool("journal_list", obj(args.clone()));
                let rows = result.get("rows").unwrap().as_arr().unwrap();
                assert!(!rows.is_empty() && rows.len() < 5);
                orders.extend(rows.iter().map(|r| r.get("key").unwrap().get("order").unwrap().as_i64().unwrap()));
                let before = result.get("before").unwrap();
                if before.is_null() { break }
                assert_eq!(Some(before), rows.last().unwrap().get("key"));
                args = vec![("before", before.clone()), ("count", Value::Int(500))];
            }
            assert_eq!(orders, [5, 4, 3, 2, 1]);
            assert!(f.browser.reader.is_some() && !f.page.drawn, "list requests are served without opening the tab");
            let label = "界".repeat(MAX_RESULT_BYTES);
            f.append("a", &obj(vec![("kind", s("effect")), ("order", Value::Int(6)), ("label", s(&label))]).to_json());
            let result = f.tool("journal_list", obj(vec![]));
            let row = &result.get("rows").unwrap().as_arr().unwrap()[0];
            assert_eq!(row.get("label_shortened").and_then(Value::as_bool), Some(true));
            f.update_until(|f| f.page.rows.first().is_some_and(|r| r.key.number == 6));
            assert_eq!(row.get("label").and_then(Value::as_str), Some(f.page.rows[0].title.as_str()));
        }

        #[test]
        fn journal_read_cursor_recovers_the_exact_record_and_original_partner() {
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            let content = "\n🙂\"\\".repeat(MAX_RESULT_BYTES / 4);
            let record = obj(vec![("kind", s("call")), ("order", Value::Int(1)), ("id", Value::Int(1)), ("args", s(content))]);
            f.append("a", &record.to_json());
            f.select("a");
            assert!(f.browser.reader.is_none());
            let rec = record_keys(&f);
            for already_done in [false, true] {
                let mut args = vec![("record", rec(1))];
                let mut text = String::new();
                loop {
                    let result = f.tool("journal_read", obj(args.clone()));
                    assert!(result.get("part").is_some(), "oversized records use the continuation shape");
                    text.push_str(result.get("part").unwrap().as_str().unwrap());
                    let cursor = result.get("cursor").unwrap();
                    if cursor.is_null() { break }
                    assert_eq!(cursor.get("offset").unwrap().as_u64(), Some(text.len() as u64));
                    if !already_done && args.len() == 1 {
                        f.append("a", r#"{"kind":"effect","order":3,"call":1,"changes":[{"name":"base.splash"}]}"#);
                        f.append("a", r#"{"kind":"done","order":2,"id":1,"outcome":"ok","reply":"complete"}"#);
                    }
                    args = vec![("record", rec(1)), ("cursor", cursor.clone())];
                }
                let dto = parse(&text);
                assert_eq!(dto.get("record"), Some(&record));
                let names = if already_done { vec![s("base.splash")] } else { vec![] };
                assert_eq!(dto.get("documents"), Some(&Value::Arr(names)), "a late effect must not rewrite a continued read");
                let partner = dto.get("partner").unwrap();
                if already_done { assert_eq!(partner.get("key"), Some(&rec(2))); }
                else { assert_eq!(partner.as_str(), Some("not found"), "a late done must not rewrite a continued read"); }
            }
            assert!(f.browser.reader.is_some() && !f.page.drawn, "read requests are served without opening the tab");
        }
        #[test]
        fn journal_filtered_range_copies_invocations_and_verified_changes_across_pages() {
            use makepad_core_util::sha256::sha256_hex;
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            let append = |f: &Fixture, mut fields: Vec<(&str, Value)>, order: i64, kind: &str| {
                fields.extend([("kind", s(kind)), ("order", Value::Int(order))]);
                f.append("a", &obj(fields).to_json());
            };
            let blob = |bytes: &[u8]| {
                let blobs = f.root.join("a/journal/blobs");
                fs::create_dir_all(&blobs).unwrap();
                let hash = sha256_hex(bytes);
                fs::write(blobs.join(&hash), bytes).unwrap();
                s(hash)
            };
            let initial = blob("é0\n".as_bytes());
            let binary_before = blob(&[0, 1]);
            let binary_after = blob(&[0, 2]);
            append(&f, vec![("files", Value::Arr(vec![
                obj(vec![("name", s("part.splash")), ("hash", initial)]),
                obj(vec![("name", s("mesh.bin")), ("hash", binary_before.clone())])]))], 1, "checkpoint");
            let args = obj(vec![("value", s("3"))]);
            append(&f, vec![("id", Value::Int(2)), ("actor", obj(vec![("kind",s("ai"))])), ("action", s("fab.elec_edit")),
                ("args", obj(vec![("action", s("elec.field.set")), ("args", args.clone())]))], 2, "call");
            let change = |old: &str, new: &str| obj(vec![("name",s("part.splash")),
                ("before",s(sha256_hex(format!("é{old}\n").as_bytes()))), ("after",s(sha256_hex(format!("é{new}\n").as_bytes()))),
                ("blob",Value::Bool(false)), ("edits", Value::Arr(vec![obj(vec![("start",Value::Int(2)), ("end",Value::Int((2 + old.len()) as i64)), ("text",s(new))])]))]);
            for (n, (old, new)) in [("0","1"),("1","2"),("2","3")].iter().enumerate() {
                append(&f, vec![("call", Value::Int(2)), ("changes",Value::Arr(vec![change(old,new)]))], 3 + n as i64, "effect");
            }
            append(&f, vec![("id",Value::Int(2)),("outcome",s("ok"))], 6, "done");
            append(&f, vec![("id",Value::Int(7)),("action",s("fab.place")),("args",obj(vec![]))], 7, "call");
            append(&f, vec![("id",Value::Int(7)),("outcome",s("refused"))], 8, "done");
            append(&f, vec![("changes",Value::Arr(vec![change("3","界")]))], 9, "effect");
            f.append("origin", r#"{"kind":"call","order":2,"id":2,"ref":"foreign","action":"fab.import","args":{"new_plate":true}}"#);
            append(&f, vec![("call",Value::Null),("call_journal",s("origin")),("call_ref",s("foreign")),("changes",Value::Arr(vec![change("界","4")]))], 10, "effect");
            append(&f, vec![("changes",Value::Arr(vec![obj(vec![("name",s("mesh.bin")),("before",binary_before.clone()),
                ("after",binary_after.clone()), ("blob",Value::Bool(true)),("edits",Value::Arr(vec![]))])]))], 11, "effect");
            // A reserved order can reach disk after the panel has opened its diff.
            for n in (12..=40).filter(|&n| n != 30) { append(&f, vec![("changes",Value::Arr(vec![]))], n, "history"); }
            let rec = record_keys(&f);
            let ask = |from,to| obj(vec![("from",rec(from)),("to",rec(to))]);
            f.select("a");
            assert!(f.page.message.is_empty(), "wait for the first page before showing an empty state");
            f.update_until(|f| f.page.rows.len() == PAGE);
            let copy = f.tool("journal_copy", ask(2,6));
            let line = copy.get("script").unwrap().as_str().unwrap();
            let (name,json) = line.split_once(' ').unwrap();
            assert_eq!(name, "elec.field.set");
            assert!(parse(json) == args && line.lines().count() == 1, "call, three effects and result are one invocation");
            assert!(f.tool("journal_copy", ask(8,7)).get("script").unwrap().as_str().unwrap().starts_with("# refused fab.place"));
            let key_at = |f: &Fixture, order| f.page.rows.iter().find(|r| r.key.number == order && !r.key.damaged).unwrap().key.clone();
            let anchor = key_at(&f,10);
            f.browser.action(&JournalAction::Select(anchor.clone()), &mut f.page);
            f.update_until(|f| f.page.count == 1);
            let session = f.page.diff_session(0).expect("the selected text effect has an editor");
            let version = f.browser.diff_version.clone().unwrap();
            append(&f, vec![("changes", Value::Arr(vec![]))], 41, "history");
            f.update_until(|f| f.page.rows.first().is_some_and(|r| r.key.number == 41));
            assert!(Arc::ptr_eq(&version, f.browser.diff_version.as_ref().unwrap()), "an unrelated append skips diff preparation");
            assert_eq!(f.page.diff_session(0), Some(session), "refresh must preserve the editor being scrolled");
            let end = f.browser.selection_end;
            append(&f, vec![("changes", Value::Arr(vec![]))], 30, "history");
            f.update_until(|f| f.browser.selection_end > end);
            assert_eq!(f.page.diff_session(0), Some(session), "rechecking a late unrelated record preserves the same editor");
            f.browser.action(&JournalAction::Older, &mut f.page);
            f.update_until(|f| f.page.newer && f.page.rows.last().unwrap().key.number == 1);
            let other = key_at(&f,2);
            f.browser.action(&JournalAction::Extend(other.clone()), &mut f.page);
            f.update_until(|f| f.page.count == 9);
            assert!(f.page.selected.as_ref() == Some(&anchor) && f.page.through.as_ref() == Some(&other));
            f.browser.action(&JournalAction::Copy, &mut f.page);
            f.update_until(|f| f.browser.clipboard.is_some());
            let clipboard = f.browser.take_clipboard().unwrap();
            let result = f.tool("journal_copy", ask(10,2));
            assert!(result.get("script").unwrap().as_str() == Some(&clipboard), "the press and the tool copy exactly the same range");
            let lines: Vec<_> = clipboard.lines().collect();
            assert_eq!(lines.len(),4);
            let mut bytes = "é3\n".as_bytes().to_vec();
            for (line, expected) in lines[2..].iter().zip(["é界\n", "é4\n"]) {
                let patch = parse(line.strip_prefix("patch ").expect("local direct and joined foreign effects must both be source patches"));
                assert_eq!(patch.get("doc").and_then(Value::as_str),Some("part.splash"));
                assert!(patch.get("before").and_then(Value::as_str) == Some(&sha256_hex(&bytes)));
                let e = &patch.get("edits").unwrap().as_arr().unwrap()[0];
                let start = e.get("start").unwrap().as_u64().unwrap() as usize;
                let end = e.get("end").unwrap().as_u64().unwrap() as usize;
                bytes.splice(start..end,e.get("text").unwrap().as_str().unwrap().bytes());
                assert!(bytes == expected.as_bytes(), "the offsets must apply to the previous UTF-8 bytes");
            }
            let foreign = f.tool("journal_copy", ask(10,10));
            assert!(foreign.get("script").and_then(Value::as_str) == Some(lines[3]));
            let filtered = f.tool("journal_copy", obj(vec![("from",rec(2)),("to",rec(10)),("actor",s("ai")),("kind",s("done"))]));
            assert!(filtered.get("script").and_then(Value::as_str) == Some(lines[0]));
            let (tx,rx) = std::sync::mpsc::channel();
            f.browser.reader.as_mut().unwrap().query(move |b| {
                let b = b.unwrap();
                let single = |order| {
                    let (_, updates) = b.diffs(Position::Order(order), None).unwrap();
                    let mut updates = updates.unwrap();
                    assert_eq!(updates.len(), 1, "one changed document must produce one viewer");
                    let JournalDiffUpdate::Prepared(diff) = updates.pop().unwrap() else { panic!("new selection needs a diff") };
                    diff
                };
                let endpoints = |order| {
                    let (document, _, _) = single(order).text.expect("expected a text diff");
                    let rows = document.as_text().as_lines();
                    (rows[document.metadata().old_to_row[0]].clone(), rows[document.metadata().new_to_row[0]].clone())
                };
                let binary = single(11);
                let ok = [2,6].iter().all(|&order| endpoints(order) == ("é0".into(), "é3".into()))
                    && endpoints(4) == ("é1".into(), "é2".into()) && endpoints(9) == ("é3".into(), "é界".into())
                    && binary.text.is_none() && binary.label.contains("mesh.bin")
                    && [binary_before,binary_after].iter().all(|v| binary.label.contains(v.as_str().unwrap()));
                tx.send(ok).unwrap();
            }).unwrap();
            assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), "the prepared diff must preserve both revisions and binary hashes");

            // Another linked effect adds a file that sorts first. Keep the
            // existing document's editor as it moves into the second row.
            let call_key = key_at(&f, 2);
            f.browser.action(&JournalAction::Select(call_key), &mut f.page);
            f.update_until(|f| f.page.diff_session(0).is_some());
            let session = f.page.diff_session(0).unwrap();
            let end = f.browser.selection_end;
            append(&f, vec![("call", Value::Int(2)), ("changes", Value::Arr(vec![obj(vec![
                ("name", s("assembly.splash")), ("before", Value::Null), ("after", s(sha256_hex("é0\n".as_bytes()))),
                ("blob", Value::Bool(true))])]))], 42, "effect");
            f.update_until(|f| f.browser.selection_end > end);
            assert!(f.page.diff_session(0).is_none(), "a created file has an operation and hashes");
            assert_eq!(f.page.diff_session(1), Some(session), "a new file in the same call preserves the unchanged editor");

        }

        #[test]
        fn journal_copy_preserves_file_operations_and_rechecks_late_effects() {
            use makepad_core_util::sha256::sha256_hex;
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            let name = "new.splash";
            f.append("a", r#"{"order":1,"kind":"checkpoint","files":[]}"#);
            let blobs = f.root.join("a/journal/blobs");
            fs::create_dir_all(&blobs).unwrap();
            let hash = |text: &str| s(sha256_hex(text.as_bytes()));
            for text in ["first", "second", "late"] {
                fs::write(blobs.join(sha256_hex(text.as_bytes())), text).unwrap();
            }
            let add = |f: &Fixture, order: i64, before: Value, after: Value| {
                let change = obj(vec![("name", s(name)), ("before", before), ("after", after), ("blob", Value::Bool(true))]);
                f.append("a", &obj(vec![("order",Value::Int(order)), ("kind",s("effect")), ("changes",Value::Arr(vec![change]))]).to_json());
            };
            add(&f, 2, Value::Null, hash("first"));
            add(&f, 4, hash("first"), hash("second"));
            add(&f, 5, hash("second"), Value::Null);
            add(&f, 6, Value::Null, hash("missing"));
            f.append("a", r#"{"order":7,"id":7,"kind":"call","action":"elec.place","args":[{"name":"board","value":"main"}]}"#);
            f.append("a", r#"{"order":8,"id":7,"kind":"done","outcome":"ok"}"#);
            f.append("a", r#"{"order":9,"id":9,"kind":"call","action":"fab.place","args":{}}"#);
            f.append("a", r#"{"order":10,"id":9,"kind":"done","outcome":"cancelled"}"#);
            f.append("a", r#"{"order":11,"kind":"effect","changes":"malformed"}"#);
            f.select("a");
            let rec = record_keys(&f);
            let args = |from,to| obj(vec![("from",rec(from)),("to",rec(to))]);
            let result = f.tool("journal_copy", args(2,6));
            let mut lines = result.get("script").unwrap().as_str().unwrap().lines();
            let creation = parse(lines.next().unwrap().strip_prefix("patch ").unwrap());
            assert!(creation.get("op") == Some(&s("create")) && creation.get("text") == Some(&s("first")));
            assert!(lines.next().unwrap().starts_with("patch "));
            let removal = parse(lines.next().unwrap().strip_prefix("patch ").unwrap());
            assert!(removal.get("op") == Some(&s("remove")) && removal.get("before") == Some(&hash("second")));
            assert!(lines.next().unwrap().starts_with("# Missing journal blob") && lines.next().is_none());

            let result = f.tool("journal_copy", args(7,11));
            let mut lines = result.get("script").unwrap().as_str().unwrap().lines();
            let catalog = parse(lines.next().unwrap().strip_prefix("elec.place ").unwrap());
            assert!(catalog.get("board") == Some(&s("main")), "a person's catalog arguments are named");
            assert!(lines.next().unwrap().starts_with("# cancelled fab.place"));
            assert!(lines.next().unwrap().starts_with("# Malformed record") && lines.next().is_none());

            // The selected effect was valid even though an earlier order had not arrived.
            // Once that order changes the same document, the old starting hash cannot match.
            assert!(f.tool("journal_copy", args(4,4)).get("script").unwrap().as_str().unwrap().starts_with("patch "));
            f.update_until(|f| f.page.rows.iter().any(|r| r.key.number == 4));
            let selected = f.page.rows.iter().find(|r| r.key.number == 4).unwrap().key.clone();
            f.browser.action(&JournalAction::Select(selected), &mut f.page);
            f.update_until(|f| f.page.diff_session(0).is_some());
            add(&f, 3, hash("first"), hash("late"));
            f.update_until(|f| f.page.rows.iter().any(|r| r.key.number == 3));
            assert!(f.page.diff_session(0).is_none(), "a newly invalid revision must replace the previously shown editor");
            let stale = f.tool("journal_copy", args(4,4));
            assert!(stale.get("script").unwrap().as_str().unwrap().starts_with("# Malformed record"),
                "a late earlier edit must invalidate the previously verified revision");
        }

        #[test]
        fn journal_copy_continuations_keep_the_range_and_its_first_outcomes() {
            let _guard = crate::cx_shared();
            let mut f = Fixture::new();
            let args = obj(vec![("text", s("界\n\"".repeat(MAX_RESULT_BYTES / 3)))]);
            f.append("a", &obj(vec![("kind",s("call")),("id",Value::Int(1)),("order",Value::Int(1)),("action",s("fab.script")),("args",args.clone())]).to_json());
            f.append("a", r#"{"kind":"done","id":1,"order":2,"outcome":"ok"}"#);
            f.append("a", r#"{"kind":"call","id":3,"order":3,"action":"fab.place","args":{}}"#);
            f.append("a", r#"{"kind":"lost","order":5}"#);
            f.append("a", "damaged");
            f.select("a");
            let rec = record_keys(&f);
            let endpoints = vec![("from",rec(1)),("to",rec(5))];
            let mut params = endpoints.clone();
            let mut script = String::new();
            loop {
                let reply = f.tool("journal_copy", obj(params));
                script.push_str(reply.get("part").unwrap().as_str().unwrap());
                let cursor = reply.get("cursor").unwrap();
                if cursor.is_null() { break }
                if script.len() == reply.get("part").unwrap().as_str().unwrap().len() {
                    f.append("a", r#"{"kind":"done","id":3,"order":4,"outcome":"ok"}"#);
                    let wrong = obj(vec![("from",rec(1)),("to",rec(3)),("cursor",cursor.clone())]);
                    assert!(Tool::parse("journal_copy", &wrong, "a").is_err());
                }
                params = endpoints.clone();
                params.push(("cursor",cursor.clone()));
            }
            let mut lines = script.lines();
            assert!(parse(lines.next().unwrap().strip_prefix("fab.script ").unwrap()) == args);
            assert!(lines.next().unwrap().contains("no recorded outcome"), "a continued script must keep the original outcome");
            assert!(lines.next().unwrap().starts_with("# Lost") && lines.next().is_none());
            let latest = f.tool("journal_copy",obj(vec![("from",rec(3)),("to",rec(4))]));
            assert!(latest.get("script").unwrap().as_str().unwrap().starts_with("fab.place "));
            let damaged = f.tool("journal_list",obj(vec![("kind",s("diagnostic"))]));
            let key = damaged.get("rows").unwrap().as_arr().unwrap()[0].get("key").unwrap().clone();
            assert!(f.tool("journal_copy",obj(vec![("from",key.clone()),("to",key)])).get("script").unwrap().as_str().unwrap().starts_with("# Damaged"));
        }

    }
}
