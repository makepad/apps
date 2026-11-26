//! Headless checks for the prefabbed panels in Story's film folders. Moving
//! a bound value must change a frame, an evaluated 3D scene or a post pass in
//! at least one placement of the plate.

/// `makepad-story --panel-effects [<plate file>]` checks each content folder
/// with a cut and a `plates.json`. With a plate file it checks that plate's
/// own panel. Without one it also checks each cut's panel, when present.
/// A control that changes nothing panics with its name.
pub(crate) fn panel_effects_main(args: &[String]) -> Option<std::process::ExitCode> {
    use std::process::ExitCode;

    if args.first().map(String::as_str) != Some("--panel-effects") {
        return None;
    }
    let only = match &args[1..] {
        [] => None,
        [name] => Some(name.as_str()),
        _ => {
            eprintln!("usage: makepad-story --panel-effects [<plate file>]");
            return Some(ExitCode::from(2));
        }
    };
    let mut found = false;
    for doc in super::super::folder_docs::docs() {
        let Some(cut) = &doc.cut else { continue };
        if doc.dir.join("plates.json").is_file() && controls_have_an_effect(&doc.dir, cut, only) {
            found = true;
        }
    }
    match (found, only) {
        (false, Some(name)) => println!("story: no film found with plate {name}"),
        (false, None) => println!("story: no film found"),
        (true, Some(name)) => println!("story: every panel control changes plate {name}"),
        (true, None) => println!("story: checked panel controls in every film"),
    }
    Some(ExitCode::SUCCESS)
}

// The check is of panel activity, not of the installed display faces: the
// plates' display faces are drawn with bundled ones.
pub(crate) fn bundled_font_source(text: &str) -> String {
    text.replace("\"Archivo\"", "\"Roboto Flex\"").replace("\"Cormorant\"", "\"Cormorant Garamond\"")
}

pub(crate) fn bundled_font_modules(context: makepad_asset_data::tree::TreeContext) -> makepad_motion::modules::Modules {
    use makepad_motion::modules::{self, ModuleResolver, ModuleSource, Modules};
    struct Bundled(std::sync::Arc<dyn ModuleResolver>);
    impl ModuleResolver for Bundled {
        fn named(&self, name: &str) -> Option<ModuleSource> {
            modules::builtin(name).map(|mut source| {
                source.text = bundled_font_source(&source.text).into();
                source
            })
        }
        fn path(&self, from: &str, path: &str) -> Result<ModuleSource, String> {
            self.0.path(from, path).map(|mut source| {
                source.text = bundled_font_source(&source.text).into();
                source
            })
        }
        fn validate(&self, source: &str, file: &str) -> Result<(), Vec<String>> { self.0.validate(source, file) }
        fn document_file(&self) -> Option<&str> { self.0.document_file() }
        fn install(&self, vm: &mut makepad_widgets::makepad_script::ScriptVm, module: makepad_widgets::makepad_script::ScriptObject) { self.0.install(vm, module); }
    }
    Modules(std::sync::Arc::new(Bundled(Modules::from_tree(context).0)))
}

/// Whether `frame` draws the items `base` draws. A 3D scene's item holds
/// its scene, signals and modules by identity (its document's VM, its
/// resolver), so two loads of one text never compare equal by them:
/// they are taken as the base's, and what the scene draws is compared by
/// [`SceneCache::looks`].
fn alike(frame: &makepad_motion::MotionFrame, base: &makepad_motion::MotionFrame) -> bool {
    use makepad_motion::{Item, ItemKind};
    fn adopt(items: &mut [Item], base: &[Item]) {
        for (item, like) in items.iter_mut().zip(base) {
            match (&mut item.kind, &like.kind) {
                (ItemKind::Scene3dDoc { scene, signals, modules, layers, .. }, ItemKind::Scene3dDoc { scene: s, signals: g, modules: m, layers: l, .. }) => {
                    (*scene, *signals, *modules) = (s.clone(), g.clone(), m.clone());
                    for ((_, a), (_, b)) in layers.iter_mut().zip(l) {
                        adopt(a, b);
                    }
                }
                (ItemKind::Group { children, mask, .. }, ItemKind::Group { children: c, mask: m, .. }) => {
                    adopt(children, c);
                    if let (Some(a), Some(b)) = (mask, m) {
                        adopt(std::slice::from_mut(&mut **a), std::slice::from_ref(&**b));
                    }
                }
                (ItemKind::Fly { inner, .. }, ItemKind::Fly { inner: i, .. }) | (ItemKind::Anchored { inner, .. }, ItemKind::Anchored { inner: i, .. }) => {
                    adopt(std::slice::from_mut(&mut **inner), std::slice::from_ref(&**i));
                }
                _ => {}
            }
        }
    }
    if frame == base {
        return true;
    }
    let mut frame = frame.clone();
    adopt(&mut frame.items, &base.items);
    for ((_, a), (_, b)) in frame.data_layers.iter_mut().zip(&base.data_layers) {
        adopt(a, b);
    }
    frame == *base
}

/// A document's 3D scenes as the drawing side keeps them: each scene an
/// item shows loaded once and evaluated at each frame's time. A value
/// set in the document while one is loaded is not in it: drop it then.
#[derive(Default)]
struct SceneCache(std::collections::HashMap<(String, u64), Result<makepad_stage_motion3d::doc::Scene3dDoc, String>>);

impl SceneCache {
    /// What each 3D scene of `frame` draws, evaluated at its item's time
    /// (a hash of the evaluated scene; its load's errors where it does
    /// not load).
    fn looks(&mut self, frame: &makepad_motion::MotionFrame) -> Vec<u64> {
        use std::hash::Hasher;
        struct Hash(std::collections::hash_map::DefaultHasher);
        impl std::fmt::Write for Hash {
            fn write_str(&mut self, s: &str) -> std::fmt::Result {
                self.0.write(s.as_bytes());
                Ok(())
            }
        }
        let hash = |args: std::fmt::Arguments| {
            let mut hash = Hash(Default::default());
            std::fmt::write(&mut hash, args).expect("a hash takes any text");
            hash.0.finish()
        };
        frame.walk().into_iter().filter_map(|(_, item)| {
            let makepad_motion::ItemKind::Scene3dDoc { scene, size, local_time, edit_offset, .. } = &item.kind else { return None };
            // (A template's scene is its text and inputs; an object's,
            // the object in the document's VM.)
            let loaded = self.0.entry((item.path.clone(), hash(format_args!("{scene:?}")))).or_insert_with(|| {
                let resolver = makepad_stage_motion3d::doc::FileResolver::default();
                makepad_motion::scene::load_scene(&item.kind, &item.path, &resolver).expect("a scene item").map_err(|errors| format!("{errors:?}"))
            });
            Some(match loaded {
                Ok(scene) => {
                    let input = makepad_stage_motion3d::eval::EvalInput { t: *local_time as f32, aspect: (size[0] / size[1].max(1e-9)) as f32, audio: &makepad_stage_motion3d::anim::NoAudio, edit_offset: *edit_offset as f32 };
                    hash(format_args!("{:?}", scene.eval(&input, &mut Default::default())))
                }
                Err(errors) => hash(format_args!("{errors}")),
            })
        }).collect()
    }
}

/// One plate file of the film, compared: its text and the modules it
/// imports, its load as each of its plates in the film (the base), the
/// base's frames and 3D scenes as a comparison first needs them, and
/// what each moved value found (aliases with the same moves share it).
struct Plate<'a> {
    file: &'a str,
    snapshot: &'a makepad_motion::tree::MemoryTree,
    /// Each of its plates in the film: its params, where it starts (a
    /// control may matter in one plate of a shared file only).
    placings: Vec<(Vec<(String, makepad_motion::InputValue)>, f64)>,
    text: String,
    code: makepad_motion::schema::code::CodeOutline,
    /// (name, path, text) and its outline.
    imported: Vec<(String, String, String)>,
    outlines: Vec<makepad_motion::schema::code::CodeOutline>,
    base: Vec<makepad_motion::MotionDoc>,
    /// Every frame of each placing (a flash lasts a frame or two), and
    /// the order they are compared in: coarse to fine (bit-reversed
    /// indices), so a value that matters in part of a plate shows within
    /// a few frames.
    times: Vec<Vec<f64>>,
    orders: Vec<Vec<usize>>,
    base_posts: Vec<String>,
    base_frames: Vec<Vec<std::sync::OnceLock<makepad_motion::MotionFrame>>>,
    base_scene_docs: Vec<std::sync::Mutex<SceneCache>>,
    base_scenes: Vec<Vec<std::sync::OnceLock<Vec<u64>>>>,
    /// By site and moves: `None` (a move changed a frame) or the moves
    /// that do not load.
    found: std::sync::Mutex<std::collections::BTreeMap<(usize, usize, usize, Vec<String>), std::sync::Arc<std::sync::OnceLock<Option<Vec<String>>>>>>,
}

impl<'a> Plate<'a> {
    fn new(file: &'a str, snapshot: &'a makepad_motion::tree::MemoryTree, dir: &std::path::Path, placings: Vec<(Vec<(String, makepad_motion::InputValue)>, f64)>) -> Self {
        use makepad_motion::tree::TreeResolver;
        let text = bundled_font_source(&std::fs::read_to_string(dir.join(file)).unwrap());
        let code = makepad_motion::schema::code::outline(&text);
        let imported: Vec<(String, String, String)> = text.lines().filter_map(|line| {
            let (name, value) = line.trim_start().strip_prefix("let ")?.split_once('=')?;
            let path = value.trim().strip_prefix("import(\"")?.split('"').next()?;
            let module = snapshot.resolve(file, path).unwrap();
            Some((name.trim().to_string(), module.path.clone(), bundled_font_source(module.text().unwrap())))
        }).collect();
        let outlines = imported.iter().map(|(_, _, t)| makepad_motion::schema::code::outline(t)).collect();
        let mut plate = Plate { file, snapshot, placings, text, code, imported, outlines, base: Vec::new(), times: Vec::new(), orders: Vec::new(), base_posts: Vec::new(), base_frames: Vec::new(), base_scene_docs: Vec::new(), base_scenes: Vec::new(), found: Default::default() };
        plate.base = plate.load(&plate.text, &plate.imported).unwrap_or_else(|e| panic!("{file} does not load: {e}"));
        plate.times = plate.base.iter().map(|b| (0..(b.duration * b.fps).ceil().max(1.0) as usize).map(|k| (k as f64 + 0.5) / b.fps).collect()).collect();
        plate.orders = plate.times.iter().map(|ts| {
            let mut order: Vec<usize> = (0..ts.len()).collect();
            order.sort_by_key(|k| (*k as u32).reverse_bits());
            order
        }).collect();
        plate.base_posts = plate.base.iter().map(|doc| format!("{:?}", doc.post)).collect();
        plate.base_frames = plate.times.iter().map(|ts| ts.iter().map(|_| Default::default()).collect()).collect();
        plate.base_scene_docs = plate.base.iter().map(|_| Default::default()).collect();
        plate.base_scenes = plate.times.iter().map(|ts| ts.iter().map(|_| Default::default()).collect()).collect();
        plate
    }

    /// The file loaded as each of its plates (as the film loads it), from
    /// `doc` and the modules' texts `mods`.
    fn load(&self, doc: &str, mods: &[(String, String, String)]) -> Result<Vec<makepad_motion::MotionDoc>, String> {
        self.placings.iter().map(|(params, offset)| {
            let mut tree = makepad_asset_data::tree::TreeDraft::from_memory(self.snapshot)?;
            tree.write(self.file, makepad_asset_data::AssetKind::Motion, doc.as_bytes().into())?;
            for (_, path, text) in mods {
                tree.write(path, makepad_asset_data::AssetKind::Motion, text.as_bytes().into())?;
            }
            let tree = std::sync::Arc::new(tree);
            let modules = bundled_font_modules(tree.context(self.file));
            let context = makepad_motion::MotionContext { offset: *offset, modules, ..Default::default() };
            makepad_motion::editable(|| makepad_motion::MotionDoc::load_with(doc, &makepad_story_cut::graphic::motion_inputs(params), context)).map_err(|d| d.iter().find(|d| d.is_error()).map(|d| format!("{:?}", d)).unwrap_or_default())
        }).collect()
    }

    /// The texts a path can name a value in: the plate's, then its
    /// modules' by their names.
    fn files(&self) -> Vec<makepad_motion::paths::File<'_>> {
        std::iter::once(makepad_motion::paths::File { name: "", text: &self.text, code: &self.code })
            .chain(self.imported.iter().zip(&self.outlines).map(|((name, _, text), code)| makepad_motion::paths::File { name, text, code }))
            .collect()
    }

    fn base_frame(&self, p: usize, k: usize) -> &makepad_motion::MotionFrame {
        self.base_frames[p][k].get_or_init(|| self.base[p].eval(self.times[p][k]))
    }

    fn base_scene(&self, p: usize, k: usize) -> &Vec<u64> {
        self.base_scenes[p][k].get_or_init(|| self.base_scene_docs[p].lock().unwrap().looks(self.base_frame(p, k)))
    }

    /// A frame draws what the base's frame `k` draws: its items (a 3D
    /// scene's by its place, time and pictures), then what each of its
    /// 3D scenes draws (only where the items are the same and the base's
    /// frame has a scene).
    fn same(&self, p: usize, k: usize, frame: &makepad_motion::MotionFrame, scenes: &mut SceneCache) -> bool {
        alike(frame, self.base_frame(p, k)) && {
            let base = self.base_scene(p, k);
            base.is_empty() || scenes.looks(frame) == *base
        }
    }

    /// The first difference from the base: a placing's post passes (no
    /// frame), or a frame among the first `frames` of each placing's
    /// order. `scenes`: the documents' scenes as loaded so far.
    fn difference(&self, docs: &[makepad_motion::MotionDoc], scenes: &mut [SceneCache], frames: usize) -> Option<(usize, Option<usize>)> {
        if let Some(p) = docs.iter().zip(&self.base_posts).position(|(doc, post)| format!("{:?}", doc.post) != *post) {
            return Some((p, None));
        }
        docs.iter().zip(scenes).enumerate().find_map(|(p, (doc, scenes))| self.orders[p].iter().take(frames).find(|&&k| !self.same(p, k, &doc.eval(self.times[p][k]), scenes)).map(|&k| (p, Some(k))))
    }

    /// Whether moving the value at `path` (for a film control, the one
    /// `film_path` reaches) changes the plate: `None` where it is not a
    /// value here, else what it found. Its moves stay within what
    /// `controls` let it reach. `docs`, `scenes`: this worker's own load
    /// of the plate; moved numbers and colours are set in it and set
    /// back.
    fn check(&self, docs: &[makepad_motion::MotionDoc], scenes: &mut [SceneCache], film_path: &str, path: &str, controls: &[String]) -> Option<Result<(), String>> {
        use makepad_edit_values::ValueKind as K;
        let file = self.file;
        let files = self.files();
        let control_path = if film_path.is_empty() { path } else { film_path };
        let site = makepad_motion::paths::resolve_exact(&files, path).ok()?;
        let t = site.text.trim();
        // Moved down and up (a value past where it matters one way may
        // matter the other): dead only when no move changes a frame.
        let moves: Vec<String> = match site.kind {
            K::Number | K::Time | K::Angle | K::Length => {
                let digits: String = t.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
                let unit = &t[digits.len()..];
                let v: f64 = digits.parse().unwrap_or(0.0);
                let whole = !digits.contains('.');
                // Within what the panel lets the control reach: a
                // choice's other choices, a slider's ends, else a small
                // step either way, then a large one.
                let spec = |key: &str| controls.iter().filter(|c| c.contains(&format!("path: \"{control_path}\""))).find_map(|c| {
                    let rest = c.split(&format!(" {key}: ")).nth(1)?;
                    let rest = rest.strip_prefix('"').unwrap_or(rest);
                    Some(rest.split(|ch: char| ch == '"' || ch == '}' || (key != "choices" && ch == ' ')).next()?.to_string())
                });
                let ns: Vec<f64> = if let Some(choices) = spec("choices") {
                    choices.split_whitespace().filter_map(|c| c.parse().ok()).collect()
                } else if let (Some(min), Some(max)) = (spec("min").and_then(|m| m.parse().ok()), spec("max").and_then(|m| m.parse().ok())) {
                    vec![min, max]
                } else if v == 0.0 {
                    if whole { vec![1.0, -1.0] } else { vec![0.05, -0.05] }
                } else if whole {
                    vec![(v * 1.02).round().max(v + 1.0), (v * 0.98).round().min(v - 1.0)]
                } else if v.abs() < 1.0 / docs[0].fps {
                    // (Under a frame: a floor, a step. A move by a part
                    // of it shows in no frame; by a frame and by ten, it
                    // can.)
                    vec![v * 1.02, v * 0.98, 1.0 / docs[0].fps, 10.0 / docs[0].fps]
                } else {
                    vec![v * 1.02, v * 0.98, v * 1.5, v * 0.5]
                };
                ns.into_iter().map(|n| format!("{}{unit}", makepad_motion::patch::fmt_num(if whole { n.round() } else { n }))).filter(|m| m != t).collect()
            }
            K::Colour => vec!["#21c8a0".to_string()],
            K::Bool => vec![if t == "true" { "false".into() } else { "true".into() }],
            K::Ease => vec![if t == "@linear" { "@ease_in_quad".into() } else { "@linear".into() }],
            K::Font => vec!["Font{family: \"Roboto Flex\" weight: 900 axes: {wdth: 25}}".into(), "Font{family: \"JetBrains Mono\" weight: 100}".into()],
            _ => return None,
        };
        // Aliases with the same moves ask the same question: asked once.
        let key = (site.file, site.range.start, site.range.end, moves.clone());
        let found = self.found.lock().unwrap().entry(key).or_default().clone();
        let found = found.get_or_init(|| {
            // A plain number or a `#colour` is set in the loaded plate
            // and set back after (the editor's live drag), each move
            // compared at a few frames; then each move loads the plate
            // again with the moved text and compares every frame (a
            // unit, a choice of ease or font, a value the load computed
            // with or copied out: post passes, counts).
            use makepad_motion::schema::{CodeFile, Literal};
            let literal = |written: &str| match site.kind {
                K::Colour if written.starts_with('#') => makepad_motion::patch::literal_color(written).map(|c| Literal::Color(c.map(|x| x as f32))),
                K::Number | K::Time | K::Angle | K::Length => written.parse::<f64>().ok().map(Literal::Number),
                _ => None,
            };
            let code_file = if site.file == 0 { CodeFile::Doc } else { CodeFile::Module(self.imported[site.file - 1].1.clone()) };
            let source = files[site.file].text;
            for moved in &moves {
                let (Some(original), Some(value)) = (literal(t), literal(moved)) else { continue };
                let reaches: Vec<_> = docs.iter().map(|doc| doc.patch_literal(&code_file, source, site.range.clone(), value)).collect();
                let patched = reaches.iter().all(|r| r.places > 0 && r.reload.is_empty());
                // (Scenes loaded before the patch hold the values before it.)
                scenes.iter_mut().for_each(|s| s.0.clear());
                // (What the editor draws after a patch: the model read
                // again from the patched VM, as `MotionDoc::reread`.)
                let witness = if patched { self.difference(&reread(file, docs), scenes, 16) } else { None };
                for (doc, reach) in docs.iter().zip(&reaches) {
                    let back = doc.patch_literal(&code_file, source, site.range.clone(), original);
                    assert_eq!(back.places, reach.places, "{file}: `{path}` = {t} -> {moved}: set back");
                }
                scenes.iter_mut().for_each(|s| s.0.clear());
                if let Some((p, frame)) = witness {
                    // Set back, what differed is the base's again: a
                    // later control does not inherit this move.
                    let back = reread(file, &docs[p..=p]).remove(0);
                    match frame {
                        Some(k) => assert!(self.same(p, k, &back.eval(self.times[p][k]), &mut scenes[p]), "{file}: `{path}` = {t} -> {moved}: set back, frame {k} differs"),
                        None => assert_eq!(format!("{:?}", back.post), self.base_posts[p], "{file}: `{path}` = {t} -> {moved}: set back"),
                    }
                    return None;
                }
            }
            let mut unloaded = Vec::new();
            for moved in &moves {
                let changed = format!("{}{moved}{}", &files[site.file].text[..site.range.start], &files[site.file].text[site.range.end..]);
                let loaded = if site.file == 0 {
                    self.load(&changed, &self.imported)
                } else {
                    let mut mods = self.imported.clone();
                    mods[site.file - 1].2 = changed;
                    self.load(&self.text, &mods)
                };
                match loaded {
                    Ok(docs) => {
                        if self.difference(&docs, &mut docs.iter().map(|_| Default::default()).collect::<Vec<_>>(), usize::MAX).is_some() {
                            return None;
                        }
                    }
                    Err(e) => unloaded.push(format!("{moved}: {e}")),
                }
            }
            Some(unloaded)
        });
        Some(match found {
            None => Ok(()),
            Some(unloaded) => {
                let fails = if unloaded.is_empty() { String::new() } else { format!(" ({} does not load)", unloaded.join(", ")) };
                Err(format!("{file}: `{path}` = {t} -> {} changes no frame{fails}", moves.join(" or ")))
            }
        })
    }
}

/// The documents' models read again from their VMs, as an editor does
/// after a patch.
fn reread(file: &str, docs: &[makepad_motion::MotionDoc]) -> Vec<makepad_motion::MotionDoc> {
    docs.iter().map(|doc| doc.reread().unwrap_or_else(|e| panic!("{file}: a reread fails: {e:?}"))).collect()
}

/// Check the plates placed by `cut` in `dir`. Return false when the manifest
/// has no plate matching the requested path, file name or stem.
pub(crate) fn controls_have_an_effect(dir: &std::path::Path, cut: &str, only: Option<&str>) -> bool {
    let plates: std::collections::BTreeMap<String, String> = match makepad_strict_json::parse_depth(std::fs::read(dir.join("plates.json")).unwrap().as_slice(), 4).unwrap() {
        makepad_strict_json::Value::Obj(pairs) => pairs.iter().filter_map(|(k, v)| Some((k.to_string(), v.as_str()?.to_string()))).collect(),
        _ => panic!("plates.json is an object"),
    };
    let mut files: Vec<_> = plates.values().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    if let Some(name) = only {
        let name = std::ffi::OsStr::new(name);
        files.retain(|file| {
            let path = std::path::Path::new(file);
            path.as_os_str() == name || path.file_name() == Some(name) || path.file_stem() == Some(name)
        });
        if files.is_empty() { return false; }
    }
    makepad_editor_panel::install_doc(makepad_stage_motion_pickers::motion_doc());
    makepad_doc_fonts::register_root("makepad_widgets", makepad_widgets::widgets_dir());
    let snapshot = makepad_motion::tree::read_folder(dir, cut, makepad_asset_data::tree::TreeRole::Cut).unwrap();
    // Each file's plates in the film, from the film's edit: `// <plate>
    // (<file>): ...` then its `Graphic{... span: [start, length] ...
    // params: {k: v ...}}`.
    let mut placed: std::collections::BTreeMap<String, Vec<(Vec<(String, makepad_motion::InputValue)>, f64)>> = Default::default();
    let edit = std::fs::read_to_string(dir.join(cut)).unwrap();
    let mut lines = edit.lines();
    while let Some(line) = lines.next() {
        let Some(id) = line.strip_prefix("// ").and_then(|l| l.split(' ').next()) else { continue };
        let Some(file) = plates.get(id) else { continue };
        let Some(graphic) = lines.next() else { break };
        let after = |key: &str| graphic.split(key).nth(1);
        let Some(start) = after("span: [").and_then(|r| r.split(',').next()?.trim().parse::<f64>().ok()) else { continue };
        let params = after("params: {").and_then(|r| r.split('}').next()).map_or(Vec::new(), |r| {
            let words: Vec<&str> = r.split_whitespace().collect();
            words.chunks(2).filter_map(|kv| Some((kv.first()?.trim_end_matches(':').to_string(), makepad_motion::InputValue::Number(kv.get(1)?.parse().ok()?)))).collect()
        });
        placed.entry(file.clone()).or_default().push((params, start));
    }
    // The film's own panel: `look.X` (a module the plates share) and
    // `*:X` (every plate that has X) and `p_plate:X` (one plate's) reach
    // into the plate files; it has an effect when any plate it reaches
    // changes. (`edit.…` moves clips: no plate value.)
    let film_panel = if only.is_none() {
        let code = makepad_motion::schema::code::outline(&edit);
        makepad_editor_panel::let_region(&edit, &code, "cut_panel").map_or("", |region| &edit[region])
    } else {
        ""
    };
    let controls_of = |panel: &str| -> Vec<String> { panel.split("Panel").skip(1).map(|c| c.split('}').next().unwrap_or(c).to_string()).collect() };
    let paths_of = |panel: &str| -> Vec<String> {
        let mut paths: Vec<String> = panel.split("path: \"").skip(1).filter_map(|p| p.split('"').next().map(str::to_string)).collect();
        paths.sort();
        paths.dedup();
        paths
    };
    let film_controls = controls_of(film_panel);
    let film_paths: Vec<String> = paths_of(film_panel).into_iter().filter(|p| !p.starts_with("edit.")).collect();
    // Each plate file loaded once; plates without a 3D scene (cheap to
    // compare) first.
    files.sort_by_cached_key(|file| std::fs::read_to_string(dir.join(file)).unwrap().contains("Scene3D"));
    let compared: Vec<Plate> = std::thread::scope(|scope| {
        let handles: Vec<_> = files.iter().map(|&file| {
            let (snapshot, placings) = (&snapshot, placed.get(file).cloned().unwrap_or_else(|| vec![(Vec::new(), 0.0)]));
            scope.spawn(move || Plate::new(file, snapshot, dir, placings))
        }).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // What to try: (plate, film control or "", the path in the plate,
    // the controls that give its range). A film control is tried first
    // in the plates whose text names its value, then in every other
    // plate it reaches while it changed none.
    let own_panels: Vec<Option<String>> = compared.iter().map(|plate| makepad_editor_panel::panel_region(&plate.text, &plate.code).map(|r| plate.text[r].to_string())).collect();
    let own_controls: Vec<Vec<String>> = own_panels.iter().map(|panel| panel.as_deref().map_or(Vec::new(), controls_of)).collect();
    let film_targets = |i: usize, named: bool| {
        let ids: Vec<&String> = plates.iter().filter(|(_, f)| f.as_str() == compared[i].file).map(|(id, _)| id).collect();
        film_paths.iter().filter(|film_path| compared[i].text.contains(film_path.rsplit(['.', ':']).next().unwrap_or(film_path)) == named).filter_map(|film_path| {
            let inner = if film_path.starts_with("look.") {
                film_path.clone()
            } else if let Some(x) = film_path.strip_prefix("*:") {
                x.to_string()
            } else {
                let (node, x) = film_path.split_once(':')?;
                ids.iter().any(|id| node == format!("p_{id}")).then(|| x.to_string())?
            };
            Some((i, film_path.clone(), inner, film_controls.as_slice()))
        }).collect::<Vec<_>>()
    };
    let mut first: Vec<(usize, String, String, &[String])> = Vec::new();
    for i in 0..compared.len() {
        first.extend(own_panels[i].as_deref().map_or(Vec::new(), paths_of).into_iter().map(|path| (i, String::new(), path, own_controls[i].as_slice())));
        first.extend(film_targets(i, true));
    }
    let later: Vec<(usize, String, String, &[String])> = (0..compared.len()).flat_map(|i| film_targets(i, false)).collect();
    // The film's controls that changed some plate: not tried again.
    let film_alive: std::sync::Mutex<std::collections::BTreeSet<String>> = Default::default();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    // Every target in one queue, plate by plate: each worker takes the
    // next as it is free (a few take seconds: a font loads, a value
    // shows late) and loads a plate of its own when it moves to it.
    let run = |queue: &[(usize, String, String, &[String])]| -> Vec<(String, Option<Result<(), String>>)> {
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads.min(queue.len())).map(|_| scope.spawn(|| {
                let mut out = Vec::new();
                let mut held: Option<(usize, Vec<makepad_motion::MotionDoc>, Vec<SceneCache>)> = None;
                while let Some((i, film_path, path, controls)) = queue.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                    if !film_path.is_empty() && film_alive.lock().unwrap().contains(film_path) {
                        continue;
                    }
                    let plate = &compared[*i];
                    if !matches!(&held, Some((j, ..)) if j == i) {
                        // (The plate it held goes before the next loads.)
                        drop(held.take());
                        let docs = plate.load(&plate.text, &plate.imported).unwrap_or_else(|e| panic!("{} does not load: {e}", plate.file));
                        let mut scenes: Vec<SceneCache> = docs.iter().map(|_| Default::default()).collect();
                        // A second load of the same text draws what the
                        // first does (else every move would differ).
                        // So does the model read again from its VM.
                        for (p, (doc, again)) in docs.iter().zip(reread(plate.file, &docs)).enumerate() {
                            let k = plate.orders[p][0];
                            assert!(plate.same(p, k, &doc.eval(plate.times[p][k]), &mut scenes[p]), "{}: a second load differs from the first at frame {k}", plate.file);
                            assert!(plate.same(p, k, &again.eval(plate.times[p][k]), &mut scenes[p]) && format!("{:?}", again.post) == plate.base_posts[p], "{}: a reread differs from its load at frame {k}", plate.file);
                        }
                        held = Some((*i, docs, scenes));
                    }
                    let (_, docs, scenes) = held.as_mut().expect("held above");
                    let found = plate.check(docs, scenes, film_path, path, controls);
                    if !film_path.is_empty() && matches!(found, Some(Ok(()))) {
                        film_alive.lock().unwrap().insert(film_path.clone());
                    }
                    out.push((film_path.clone(), found));
                }
                out
            })).collect();
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
        })
    };
    let mut dead = Vec::new();
    let mut checked = 0;
    // film path -> (plates reached, plates where it changed a frame, why not)
    let mut film_reach: std::collections::BTreeMap<String, (usize, usize, Vec<String>)> = film_paths.iter().map(|p| (p.clone(), Default::default())).collect();
    // (The film's controls that changed a plate in the first queue are
    // not tried in the second.)
    let found_first = run(&first);
    let found_later = run(&later);
    for (film_path, found) in found_first.into_iter().chain(found_later) {
        let Some(found) = found else { continue };
        if film_path.is_empty() {
            checked += 1;
            if let Err(why) = found {
                dead.push(why);
            }
        } else {
            let reach = film_reach.get_mut(&film_path).unwrap();
            reach.0 += 1;
            match found {
                Ok(()) => reach.1 += 1,
                Err(why) => reach.2.push(why),
            }
        }
    }
    for (film_path, (reached, changed, why)) in &film_reach {
        checked += 1;
        if *changed == 0 {
            dead.push(format!("film: `{film_path}` reaches {reached} plate value(s) and changes no frame{}", why.iter().map(|w| format!("\n    {w}")).collect::<String>()));
        }
    }
    assert!(checked > 0);
    dead.sort();
    assert!(dead.is_empty(), "controls that change nothing ({} of {checked}):\n{}", dead.len(), dead.join("\n"));
    true
}
