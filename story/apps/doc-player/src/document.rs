//! Open a document and the dependency graph its source names. Directory
//! enumeration is deliberately absent: unrelated media, helpers and old
//! versions never become browser downloads. Media keeps the shared folder
//! reader's content handles and the renderer's ordinary fixed media table.

use crate::options::Options;
use makepad_asset_data::source;
use makepad_asset_data::tree::TreeDraft;
use makepad_asset_data::AssetKind;
use makepad_story_cut::CutDoc;
use makepad_story_deck::{DeckDoc, SlideCut};
use makepad_story_edit_render::LocalMedia;
use std::sync::Arc;

pub enum Document {
    /// A cut, or a Motion document as the one graphic of a cut (as a deck's
    /// Motion slide is).
    Cut(Arc<CutDoc>),
    Deck(Arc<DeckDoc>),
}

pub struct Opened {
    pub document: Document,
    /// The folder's media by handle (each read where it lies).
    pub media: LocalMedia,
    /// The document's path in its tree.
    pub path: String,
}

/// Open `options.document` in its folder's tree, read through the one
/// source (`makepad_asset_data::source`: files on disk natively, the page's
/// files in the browser). In the browser a read the page has not fetched yet
/// fails; the page fetches what the run wanted and opens again
/// (`crate::web_source`).
pub fn open(cx: &mut makepad_widgets::Cx, options: &Options) -> Result<Opened, String> {
    use makepad_story_edit_render::media::folder;
    use makepad_asset_data::tree::{reference_path, Tree, TreeEntry, TreeFile, TreeRole};
    use makepad_widgets::makepad_script::syntax;
    use std::collections::{BTreeMap, BTreeSet};
    let file = source::canonicalize(&options.document).map_err(|e| format!("{}: {e}", options.document.display()))?;
    let dir = match &options.tree {
        Some(dir) => source::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?,
        None => file.parent().ok_or("the document has no folder")?.to_path_buf(),
    };
    let path = file.strip_prefix(&dir).map_err(|_| format!("{} is not inside the tree {}", file.display(), dir.display()))?
        .to_str().ok_or("the document's path is not UTF-8")?.replace('\\', "/");
    let name = dir.file_name().and_then(|name| name.to_str()).unwrap_or("document").to_string();
    let mut optional = BTreeSet::new();
    if let Some(descriptor) = &options.tree_descriptor {
        let declaration = Tree::parse(&source::read_to_string(descriptor).map_err(|e| format!("{}: {e}", descriptor.display()))?)?;
        if declaration.root.is_some() || declaration.entries.values().any(|e| !matches!(e, TreeEntry::Optional(None))) {
            return Err("a player tree descriptor declares optional paths only".into());
        }
        optional = declaration.optional_paths();
    }
    // An explicit fill names a dependency even without a descriptor. It never
    // causes an unrelated file to be read: only references below consume it.
    for (logical, _) in &options.fills {
        makepad_asset_data::tree::validate_path(logical)?;
        if options.tree_descriptor.is_some() && !optional.contains(logical) {
            return Err(format!("--fill {logical}: the descriptor declares no such optional entry"));
        }
        optional.insert(logical.clone());
    }
    let role = if makepad_motion::tree::file_kind(&path) == AssetKind::Deck { TreeRole::Deck } else { TreeRole::Cut };
    let mut tree = TreeDraft { name, root: (role, path.clone()), files: BTreeMap::new(), optional };
    let mut pending = vec![(path.clone(), "document argument".to_string())];
    let mut seen = BTreeSet::new();
    let mut waiting = Vec::new();
    // The font families the documents name (`Font{family: …}`): in the
    // browser their files are fetched with the documents.
    let mut families = BTreeSet::new();
    while let Some((logical, requester)) = pending.pop() {
        if !seen.insert(logical.clone()) { continue; }
        let location = options.fills.iter().find(|(p, _)| p == &logical).map(|(_, file)| file.clone()).unwrap_or_else(|| dir.join(&logical));
        let bytes = match source::read(&location) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && tree.optional.contains(&logical) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => { waiting.push(format!("{logical}, requested by {requester}: {e}")); continue; }
            Err(e) => return Err(format!("{logical}, requested by {requester}: {e}")),
        };
        let kind = makepad_motion::tree::file_kind(&logical);
        if logical.ends_with(".splash") {
            let text = std::str::from_utf8(&bytes).map_err(|e| format!("{logical}: {e}"))?;
            let parsed = syntax::parse(text).map_err(|e| format!("{logical}: {e}"))?;
            let mut references = Vec::new();
            parsed.walk(&mut |_, node| {
                if let Some((_, fields)) = node.object() {
                    for field in fields.iter().filter(|f| matches!(f.name.as_deref(), Some("src" | "graphic"))) {
                        if let Some(path) = field.value.string() { references.push(path.to_string()); }
                    }
                    // A deck slide's `link` enters another deck file of the tree.
                    for field in fields.iter().filter(|f| f.name.as_deref() == Some("link")) {
                        if let Some(path) = field.value.string().filter(|path| path.ends_with(".splash")) { references.push(path.to_string()); }
                    }
                    for field in fields.iter().filter(|f| f.name.as_deref() == Some("family")) {
                        if let Some(name) = field.value.string() { families.insert(name.to_string()); }
                    }
                }
                if let syntax::Kind::Call { callee, args, .. } = &node.kind {
                    let callee = callee.ident().unwrap_or("");
                    let name = ["mod.cut.", "mod.deck.", "mod.motion.", "mod.forms."].iter().find_map(|prefix| callee.strip_prefix(*prefix)).unwrap_or(callee);
                    if matches!(name, "graphic" | "import") {
                        if let [arg] = args.as_slice() {
                            if let Some(path) = arg.string() { references.push(path.to_string()); }
                        }
                    }
                }
            });
            for reference in references {
                let mut target = reference_path(&logical, &reference)?;
                if !target.rsplit('/').next().unwrap_or("").contains('.') { target.push_str(".splash"); }
                pending.push((target, logical.clone()));
            }
        }
        let handle = matches!(kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio).then(|| folder::register(&location, &bytes));
        tree.files.insert(logical.clone(), TreeFile { path: logical, kind, reference: handle, base: handle, bytes: bytes.into() });
    }
    // The named families' faces (and the default's, Inter) are the page's
    // files (a downloadable one's `fonts/…`, a bundled one's crate resource):
    // fetched now, so a document's load on the page's own thread (no
    // workers) finds them (a read there cannot wait). Natively they are the
    // installed files.
    #[cfg(target_arch = "wasm32")]
    for name in families.iter().map(String::as_str).chain(["Inter"]) {
        let Some(family) = makepad_doc_fonts::family(name) else { continue };
        for file in family.files.iter().filter(|file| matches!(file.role, makepad_doc_fonts::FontFileRole::Face { .. })) {
            let path = match file.place {
                makepad_doc_fonts::Place::Download { cache_as, .. } => std::path::PathBuf::from(cache_as),
                makepad_doc_fonts::Place::Resource(resource) => makepad_doc_fonts::resource_path(resource),
            };
            match source::read(&path) {
                Ok(bytes) => {
                    // The source reader and the renderer have separate caches.
                    // A first seek can draw a new face before an async resource
                    // request finishes, briefly substituting Inter. Publish the
                    // prefetched package face before any document frame draws.
                    if let makepad_doc_fonts::Place::Resource(resource) = file.place {
                        if let Some((krate, path)) = resource.split_once(':') {
                            let dependency = format!("{krate}/{path}");
                            if cx.get_dependency(&dependency).is_err() {
                                cx.provide_dependency(&dependency, std::rc::Rc::new(bytes));
                            }
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => waiting.push(format!("{}, the font {name}: {e}", path.display())),
                // Not on the page: the document says so when it draws.
                _ => {}
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (families, cx);
    if !waiting.is_empty() { return Err(waiting.join("\n")); }
    let tree = Arc::new(tree);
    let media = folder::local_media(&tree);
    let document = load(&tree, &path)?;
    Ok(Opened { document, media, path })
}

/// The document at `path` of `tree`, by its kind there.
pub fn load(tree: &Arc<TreeDraft>, path: &str) -> Result<Document, String> {
    let file = tree.files.get(path).ok_or_else(|| format!("no file `{path}` in the tree"))?;
    let lines = |diags: Vec<String>| diags.iter().map(|diag| format!("{path}:{diag}")).collect::<Vec<_>>().join("\n");
    match file.kind {
        AssetKind::Cut => CutDoc::load_tree(file.text()?, tree.context(path))
            .map(|doc| Document::Cut(Arc::new(doc)))
            .map_err(|diags| lines(diags.iter().map(ToString::to_string).collect())),
        AssetKind::Deck => {
            let doc = DeckDoc::load_tree(file.text()?, tree.context(path)).map_err(|diags| lines(diags.iter().map(ToString::to_string).collect()))?;
            if let Some(slide) = doc.slides.iter().find(|slide| !matches!(slide.cut, SlideCut::Inline(_))) {
                return Err(format!("{path}: slide `{}` plays a library cut; a player plays tree documents only", slide.name));
            }
            Ok(Document::Deck(Arc::new(doc)))
        }
        AssetKind::Motion => {
            // A Motion document draws at its own size, as the one graphic of
            // a cut (`CutDoc::from_graphic`, as a deck's Motion slide).
            let context = makepad_motion::MotionContext { modules: makepad_motion::modules::Modules::from_tree(tree.context(path)), ..Default::default() };
            let motion = makepad_motion::MotionDoc::load_with(file.text()?, &[], context).map_err(|diags| lines(diags.iter().map(ToString::to_string).collect()))?;
            let size = (motion.size[0].round().max(2.0) as u32 & !1, motion.size[1].round().max(2.0) as u32 & !1);
            CutDoc::from_graphic(file, tree.context(path), size)
                .map(|doc| Document::Cut(Arc::new(doc)))
                .map_err(|diags| lines(diags.iter().map(ToString::to_string).collect()))
        }
        kind => Err(format!("{path}: a {kind:?} file is not a document a player plays")),
    }
}

/// In the browser a font file named by the build machine's path (a crate's
/// resources, as the text builders name them) is read as the page's
/// `<crate>/resources/…` file (fetched with the document, [`open`]).
#[cfg(target_arch = "wasm32")]
pub fn install_font_reader() {
    use std::sync::{Arc, Mutex, OnceLock};
    fn roots() -> &'static Mutex<Vec<(String, std::path::PathBuf)>> {
        static ROOTS: OnceLock<Mutex<Vec<(String, std::path::PathBuf)>>> = OnceLock::new();
        ROOTS.get_or_init(Default::default)
    }
    makepad_doc_fonts::observe_roots(|krate, dir| roots().lock().unwrap_or_else(|e| e.into_inner()).push((krate.to_string(), dir.to_path_buf())));
    makepad_doc_fonts::set_font_reader(Box::new(move |path| {
        let key = roots().lock().unwrap_or_else(|e| e.into_inner()).iter().find_map(|(krate, dir)| {
            path.strip_prefix(dir).ok().map(|rel| std::path::Path::new(krate).join(rel))
        })?;
        source::read(&key).ok().map(Arc::from)
    }));
}
