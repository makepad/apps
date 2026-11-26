//! A render job: the cut and every file it reads, each named where it
//! already lies (an in-place folder's own files, the asset store's cached
//! blobs). Nothing is copied: the host writes this one listing, and the
//! renderer, the cut tools and the web export read the files from there.
//!
//! The listing is lines of tab-separated fields:
//!
//! ```text
//! cut       <location>                                  a standalone cut's source
//! tree      <name> <role> <root> <open>                 a tree, and the cut file drawn
//! optional  <logical path>
//! file      <kind> <logical path> <id@rev|-> <content|-> <location>
//! media     <key> <ext> <content|-> <location>          key: asset id or id@rev
//! edit      <key> <location>                            a nested cut's source
//! effect    <key> <location>                            a library effect's source
//! lyrics    <key> <location>                            word times (vj-lyrics)
//! run       <key> <location>                            an app run's run.json
//! ```
//!
//! `content` is the file's content digest (`sha256:…`), when the host knows
//! it, so the renderer never reads a whole file to name it.

use crate::media::{LocalMedia, MediaResolver, StoreMedia};
use makepad_asset_client::dto;
use makepad_asset_data::MediaType;
use makepad_story_cut::{AssetRef, CutDoc, CutNodeKind, CutTree, EffectRef, GraphicSrc, MediaRef};
use makepad_asset_data::tree::{TreeDraft, TreeFile, TreeResolver, TreeRole};
use makepad_asset_data::{AssetKind, AssetRevisionRef, BlobId};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Job {
    /// A standalone cut: its source.
    pub cut: Option<PathBuf>,
    /// A tree cut: the tree, and the file it draws.
    pub tree: Option<JobTree>,
    pub media: Vec<Located>,
    pub edits: Vec<(String, PathBuf)>,
    pub effects: Vec<(String, PathBuf)>,
    pub lyrics: Vec<(String, PathBuf)>,
    pub runs: Vec<(String, PathBuf)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JobTree {
    pub name: String,
    pub role: TreeRole,
    pub root: String,
    /// The cut file the job draws.
    pub open: String,
    pub optional: BTreeSet<String>,
    pub files: Vec<JobFile>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JobFile {
    pub kind: AssetKind,
    pub path: String,
    pub reference: Option<AssetRevisionRef>,
    pub content: Option<BlobId>,
    pub location: PathBuf,
}

/// A file the cut plays or draws, by key, with what it is (its extension:
/// a store blob has none of its own).
#[derive(Clone, Debug, PartialEq)]
pub struct Located {
    pub key: String,
    pub ext: String,
    pub content: Option<BlobId>,
    pub location: PathBuf,
}

fn role_name(role: TreeRole) -> &'static str {
    role.as_str()
}

fn role_parse(name: &str) -> Option<TreeRole> {
    Some(match name {
        "deck" => TreeRole::Deck,
        "cut" => TreeRole::Cut,
        "score" => TreeRole::Score,
        "desk" => TreeRole::Desk,
        _ => return None,
    })
}

fn field(text: &str) -> Result<(), String> {
    if text.contains(['\t', '\n']) { Err(format!("`{text}` holds a tab or a newline")) } else { Ok(()) }
}

fn location(path: &Path) -> Result<String, String> {
    let text = path.to_str().ok_or_else(|| format!("{}: not UTF-8", path.display()))?;
    field(text)?;
    Ok(text.to_string())
}

fn digest(content: &Option<BlobId>) -> String {
    content.map_or_else(|| "-".to_string(), |content| content.to_string())
}

impl Job {
    /// The listing's text.
    pub fn text(&self) -> Result<String, String> {
        let mut out = String::from("# makepad render job: every file read where it lies\n");
        if let Some(cut) = &self.cut {
            out.push_str(&format!("cut\t{}\n", location(cut)?));
        }
        if let Some(tree) = &self.tree {
            for text in [&tree.name, &tree.root, &tree.open] {
                field(text)?;
            }
            out.push_str(&format!("tree\t{}\t{}\t{}\t{}\n", tree.name, role_name(tree.role), tree.root, tree.open));
            for path in &tree.optional {
                field(path)?;
                out.push_str(&format!("optional\t{path}\n"));
            }
            for file in &tree.files {
                field(&file.path)?;
                let reference = file.reference.map_or_else(|| "-".to_string(), |reference| reference.to_string());
                out.push_str(&format!("file\t{}\t{}\t{reference}\t{}\t{}\n", dto::kind_name(file.kind), file.path, digest(&file.content), location(&file.location)?));
            }
        }
        for media in &self.media {
            field(&media.key)?;
            field(&media.ext)?;
            out.push_str(&format!("media\t{}\t{}\t{}\t{}\n", media.key, media.ext, digest(&media.content), location(&media.location)?));
        }
        for (what, list) in [("edit", &self.edits), ("effect", &self.effects), ("lyrics", &self.lyrics), ("run", &self.runs)] {
            for (key, path) in list {
                field(key)?;
                out.push_str(&format!("{what}\t{key}\t{}\n", location(path)?));
            }
        }
        Ok(out)
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(path, self.text()?).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Job, String> {
        let mut job = Job::default();
        let content = |text: &str| -> Result<Option<BlobId>, String> {
            if text == "-" { Ok(None) } else { text.parse().map(Some).map_err(|_| format!("`{text}` is not a content digest")) }
        };
        for line in text.lines().filter(|line| !line.trim().is_empty() && !line.starts_with('#')) {
            let fields: Vec<&str> = line.split('\t').collect();
            let bad = || format!("render job: `{line}`");
            match fields.as_slice() {
                ["cut", location] => job.cut = Some(PathBuf::from(location)),
                ["tree", name, role, root, open] => {
                    job.tree = Some(JobTree { name: name.to_string(), role: role_parse(role).ok_or_else(bad)?, root: root.to_string(), open: open.to_string(), optional: BTreeSet::new(), files: Vec::new() });
                }
                ["optional", path] => {
                    job.tree.as_mut().ok_or_else(bad)?.optional.insert(path.to_string());
                }
                ["file", kind, path, reference, digest, location] => {
                    let reference = match *reference {
                        "-" => None,
                        reference => {
                            let (id, rev) = reference.split_once('@').ok_or_else(bad)?;
                            Some(AssetRevisionRef { asset_id: id.parse().map_err(|_| bad())?, revision: rev.parse().map_err(|_| bad())? })
                        }
                    };
                    let file = JobFile { kind: dto::kind_parse(kind).ok_or_else(bad)?, path: path.to_string(), reference, content: content(digest)?, location: PathBuf::from(location) };
                    job.tree.as_mut().ok_or_else(bad)?.files.push(file);
                }
                ["media", key, ext, digest, location] => job.media.push(Located { key: key.to_string(), ext: ext.to_string(), content: content(digest)?, location: PathBuf::from(location) }),
                ["edit", key, location] => job.edits.push((key.to_string(), PathBuf::from(location))),
                ["effect", key, location] => job.effects.push((key.to_string(), PathBuf::from(location))),
                ["lyrics", key, location] => job.lyrics.push((key.to_string(), PathBuf::from(location))),
                ["run", key, location] => job.runs.push((key.to_string(), PathBuf::from(location))),
                _ => return Err(bad()),
            }
        }
        if job.cut.is_some() == job.tree.is_some() {
            return Err("a render job names a cut or a tree".into());
        }
        Ok(job)
    }

    pub fn read(path: &Path) -> Result<Job, String> {
        Self::parse(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)
    }

    /// The cut's source file and the media table reading every file where
    /// it lies (a tree's documents load from their locations; its media
    /// play from theirs).
    pub fn load(&self) -> Result<(PathBuf, LocalMedia), String> {
        let mut media = LocalMedia::new();
        for located in &self.media {
            media.insert_located(&located.key, &located.location, &located.ext, located.content);
        }
        for (key, path) in &self.edits {
            media.insert_edit(key, path);
        }
        for (key, path) in &self.effects {
            media.insert_effect(key, std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?);
        }
        for (key, path) in &self.lyrics {
            media.insert_lyrics(key, path);
        }
        for (key, path) in &self.runs {
            media.insert_run(key, path);
        }
        let Some(job) = &self.tree else {
            return Ok((self.cut.clone().ok_or("a render job names a cut or a tree")?, media));
        };
        let mut tree = TreeDraft { name: job.name.clone(), root: (job.role, job.root.clone()), files: BTreeMap::new(), optional: job.optional.clone() };
        let mut open = None;
        for file in &job.files {
            makepad_asset_data::tree::validate_path(&file.path)?;
            let bytes: Arc<[u8]> = if matches!(file.kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio) { Arc::from([]) }
                else { std::fs::read(&file.location).map_err(|e| format!("{}: {e}", file.location.display()))?.into() };
            if let Some(reference) = file.reference {
                let key = reference.to_string();
                let ext = file.path.rsplit_once('.').map_or("", |(_, ext)| ext);
                media.insert_located(&key, &file.location, ext, file.content);
                if file.kind == AssetKind::Cut { media.insert_edit(&key, &file.location); }
                if file.kind == AssetKind::VjEffect { media.insert_effect(&key, std::str::from_utf8(&bytes).map_err(|e| e.to_string())?); }
            }
            if file.path == job.open { open = Some(file.location.clone()); }
            tree.files.insert(file.path.clone(), TreeFile { path: file.path.clone(), kind: file.kind, reference: file.reference, base: file.reference, bytes });
        }
        let tree = Arc::new(tree);
        let file = tree.resolve("", &job.open)?;
        if file.kind != AssetKind::Cut { return Err(format!("{}: expected a cut", file.path)); }
        makepad_asset_client::tree::validate_draft(&tree)?;
        media.tree = Some((tree, job.open.clone()));
        Ok((open.ok_or_else(|| format!("no file `{}`", job.open))?, media))
    }
}

/// Every media a cut plays itself: its clips', stills', sounds' and voice
/// takes' files and its Generate segments' chosen takes (a compound clip's
/// source is a cut: its own media are its own).
pub fn doc_media(doc: &CutDoc) -> Vec<MediaRef> {
    let mut media = Vec::new();
    for node in &doc.nodes {
        match &node.kind {
            CutNodeKind::Clip { nested: true, .. } => {}
            CutNodeKind::Clip { src, .. } | CutNodeKind::Still { src, .. } | CutNodeKind::AudioClip { src, .. } => media.push(src.clone()),
            CutNodeKind::VoiceOver { lines, .. } => media.extend(lines.iter().filter_map(|line| line.take.clone()).map(MediaRef::Asset)),
            _ => {}
        }
    }
    for take in makepad_story_cut::generate::generate_media(doc) {
        if !media.contains(&take) {
            media.push(take);
        }
    }
    media
}

/// Every `asset("id", "rev")` a Splash text names.
pub fn asset_refs(text: &str) -> Vec<AssetRef> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("asset(\"") {
        rest = &rest[at + 7..];
        let Some(id_end) = rest.find('"') else { break };
        let id = &rest[..id_end];
        let after = rest[id_end + 1..].trim_start_matches(|c: char| c == ',' || c.is_whitespace());
        let Some(rev) = after.strip_prefix('"').and_then(|rev| rev.split('"').next()) else { continue };
        if id.starts_with("ast_") && rev.starts_with("arev_") {
            found.push(AssetRef { id: id.to_string(), rev: rev.to_string() });
        }
    }
    found
}

fn media_ext(media: MediaType) -> Result<&'static str, String> {
    Ok(match media {
        MediaType::Mp4 => "mp4",
        MediaType::Png => "png",
        MediaType::Jpeg => "jpg",
        MediaType::Wav => "wav",
        MediaType::Mp3 => "mp3",
        MediaType::Ogg => "ogg",
        other => return Err(format!("the renderer cannot read {other:?}")),
    })
}

impl Job {
    /// A tree cut drawing `open`, whose source is `source`. Each file is
    /// read where it lies: a folder's own files (`folder`: the folder the
    /// tree was read from; a folder's media by the handles it was read
    /// with), else the store's cached blob of its revision. A document that
    /// is nowhere yet (an unsaved edit) is written under `scratch`, the one
    /// copy of it.
    pub fn from_tree(mut store: Option<&mut StoreMedia>, tree: &TreeDraft, open: &str, source: &str, folder: Option<&Path>, scratch: &Path) -> Result<Job, String> {
        let mut job = JobTree { name: tree.name.clone(), role: tree.root.0, root: tree.root.1.clone(), open: open.to_string(), optional: tree.optional.clone(), files: Vec::new() };
        if !tree.files.contains_key(open) { return Err(format!("no file `{open}`")); }
        let mut lyrics = Vec::new();
        for file in tree.files.values() {
            let media = matches!(file.kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio);
            let bytes = if file.path == open { source.as_bytes() } else { &file.bytes[..] };
            let unsaved = bytes != &file.bytes[..];
            let in_folder = folder.map(|dir| dir.join(&file.path)).filter(|path| !media && std::fs::read(path).is_ok_and(|on_disk| on_disk[..] == *bytes));
            let reference = file.reference.filter(|_| !unsaved);
            let (location, content) = match (in_folder, reference) {
                (Some(path), _) => (path, None),
                (None, Some(reference)) => match crate::media::folder::location(&reference) {
                    // A folder handle's revision is the SHA-256 of its bytes.
                    Some(path) => (path, Some(BlobId::from_bytes(*reference.revision.as_bytes()))),
                    None => {
                        let store = store.as_deref_mut().ok_or_else(|| format!("{}: not in a folder and no store to read it from", file.path))?;
                        let (path, blob) = store.tree_file(reference)?;
                        if media {
                            let song = AssetRef { id: reference.asset_id.to_string(), rev: reference.revision.to_string() };
                            if let Some(words) = store.lyrics_file(&song)? {
                                lyrics.push((reference.to_string(), words));
                            }
                        }
                        (path, Some(blob))
                    }
                },
                (None, None) if !media => {
                    let path = scratch.join("files").join(&file.path);
                    std::fs::create_dir_all(path.parent().ok_or("file has no parent")?).map_err(|e| e.to_string())?;
                    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
                    (path, None)
                }
                (None, None) => return Err(format!("{}: save the tree before rendering", file.path)),
            };
            job.files.push(JobFile { kind: file.kind, path: file.path.clone(), reference: file.reference, content, location });
        }
        Ok(Job { tree: Some(job), lyrics, ..Job::default() })
    }

    /// A standalone library cut, `source`: of revision `saved` when it is
    /// one (its source is read from the store), else written under
    /// `scratch`. Its nested cuts, library effects, Motion documents, the
    /// pictures those use, its songs' lyrics and every media file, at any
    /// depth, each where the store keeps it.
    pub fn from_cut(store: &mut StoreMedia, saved: Option<&AssetRef>, source: &str, scratch: &Path) -> Result<Job, String> {
        let mut job = Job::default();
        job.cut = Some(match saved {
            Some(cut) => store.cut_file(cut)?.0,
            None => {
                std::fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
                let path = scratch.join("cut.splash");
                std::fs::write(&path, source).map_err(|e| format!("{}: {e}", path.display()))?;
                path
            }
        });
        let diagnostics = |diags: Vec<makepad_story_cut::Diagnostic>| diags.iter().map(|diag| diag.message.clone()).collect::<Vec<_>>().join("; ");
        let root = CutDoc::load(source).map_err(diagnostics)?;
        let tree = CutTree::load(source, None, &mut |edit| store.cut_source(edit)).map_err(diagnostics)?;
        let mut edits: Vec<(&AssetRef, &CutDoc)> = tree.edits.iter().collect();
        edits.sort_by(|a, b| (&a.0.id, &a.0.rev).cmp(&(&b.0.id, &b.0.rev)));
        let mut media = doc_media(&root);
        for (edit, doc) in &edits {
            job.edits.push((edit.id.clone(), store.cut_file(edit)?.0));
            media.extend(doc_media(doc));
        }
        let docs: Vec<&CutDoc> = std::iter::once(&root).chain(edits.iter().map(|(_, doc)| *doc)).collect();
        // Library effects (`Fx{effect: asset(…)}`).
        let mut effects: Vec<AssetRef> = Vec::new();
        for doc in &docs {
            for node in &doc.nodes {
                if let CutNodeKind::Fx { effect: EffectRef::Asset(asset), .. } = &node.kind {
                    if !effects.contains(asset) {
                        effects.push(asset.clone());
                    }
                }
            }
        }
        for effect in &effects {
            job.effects.push((effect.id.clone(), store.effect_file(effect)?.0));
        }
        // Graphics' Motion documents, and the pictures they (and the cuts) name.
        let mut motions: Vec<AssetRef> = Vec::new();
        for doc in &docs {
            for graphic in &doc.graphics {
                if let GraphicSrc::Asset(asset) = &graphic.src {
                    if !motions.contains(asset) {
                        motions.push(asset.clone());
                    }
                }
            }
        }
        let mut texts: Vec<String> = docs.iter().map(|doc| doc.source.clone()).collect();
        texts[0] = source.to_string();
        for motion in &motions {
            let (path, blob) = store.motion_file(motion)?;
            texts.push(std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?);
            job.media.push(Located { key: motion.id.clone(), ext: "splash".into(), content: Some(blob), location: path });
        }
        if !motions.is_empty() {
            let mut known: Vec<String> = media.iter().map(|media| match media {
                MediaRef::Asset(asset) => asset.id.clone(),
                MediaRef::Run(run) => run.render_id.clone(),
                MediaRef::MissingOptional(path) => path.clone(),
            }).chain(motions.iter().map(|motion| motion.id.clone())).chain(effects.iter().map(|effect| effect.id.clone())).collect();
            for text in &texts {
                for asset in asset_refs(text) {
                    if known.contains(&asset.id) {
                        continue;
                    }
                    known.push(asset.id.clone());
                    // Not a picture (a clip's own media, a font): nothing to add.
                    if let Ok((path, kind, blob)) = store.asset_file(&asset) {
                        job.media.push(Located { key: asset.id.clone(), ext: kind.into(), content: Some(blob), location: path });
                    }
                }
            }
        }
        // Songs' aligned lyrics, for lyric-locked graphics.
        let mut songs: Vec<AssetRef> = Vec::new();
        for doc in &docs {
            for node in &doc.nodes {
                if let CutNodeKind::AudioClip { src: MediaRef::Asset(song), .. } = &node.kind {
                    if !songs.contains(song) {
                        songs.push(song.clone());
                    }
                }
            }
        }
        for song in &songs {
            if let Ok(Some(path)) = store.lyrics_file(song) {
                job.lyrics.push((song.id.clone(), path));
            }
        }
        media.sort();
        media.dedup();
        for media in &media {
            let key = match media {
                MediaRef::MissingOptional(_) => continue,
                MediaRef::Asset(asset) => asset.id.clone(),
                MediaRef::Run(run) => run.render_id.clone(),
            };
            let resolved = store.resolve(media)?;
            let ext = media_ext(resolved.media).map_err(|error| format!("{key}: {error}"))?;
            job.media.push(Located { key, ext: ext.into(), content: Some(resolved.content), location: resolved.path });
        }
        Ok(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_reads_back_as_written() {
        let reference: AssetRevisionRef = AssetRevisionRef { asset_id: "ast_o46tbvnz6nbjdfbf34u3zb4esu".parse().unwrap(), revision: format!("arev_{}", "ab".repeat(32)).parse().unwrap() };
        let content = Some(BlobId::hash_of(b"bytes"));
        let job = Job {
            cut: None,
            tree: Some(JobTree {
                name: "Harbour film".into(), role: TreeRole::Cut, root: "film.cut.splash".into(), open: "film.cut.splash".into(),
                optional: ["media/song.mp3".to_string()].into_iter().collect(),
                files: vec![
                    JobFile { kind: AssetKind::Cut, path: "film.cut.splash".into(), reference: None, content: None, location: "/films/harbour/film.cut.splash".into() },
                    JobFile { kind: AssetKind::Audio, path: "media/song.mp3".into(), reference: Some(reference), content, location: "/cache/ab/cd".into() },
                ],
            }),
            media: vec![Located { key: "ast_x".into(), ext: "png".into(), content: None, location: "/cache/x".into() }],
            edits: vec![("ast_e".into(), "/cache/e".into())],
            effects: Vec::new(),
            lyrics: vec![("ast_s".into(), "/cache/l".into())],
            runs: Vec::new(),
        };
        assert_eq!(Job::parse(&job.text().unwrap()).unwrap(), job);
        assert!(Job { cut: Some("/a\tb".into()), ..Job::default() }.text().is_err());
        assert!(Job::parse("media\tx\n").is_err());
    }
}
