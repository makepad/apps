//! Where a cut's exact references live on disk.
//!
//! A cut names media only as `asset(id, rev)` and `run(id, rev, render)`.
//! Before a frame is drawn every one of them is resolved,
//! on a worker, to a local file of a known kind; aliases are never
//! followed. [`StoreMedia`] resolves through the asset store (Stage);
//! [`LocalMedia`] is a fixed table (the standalone renderer, tests, CI).

#[cfg(feature = "store")]
use makepad_asset_client::resolver::{select_file, TierPreference};
#[cfg(feature = "store")]
use makepad_asset_client::AssetClient;
#[cfg(feature = "store")]
use makepad_asset_data::{AssetKind, DeviceTier, FileRole};
use makepad_asset_data::sha256::Sha256;
use makepad_asset_data::{AssetId, AssetRevisionId, AssetRevisionRef, BlobId, MediaType};
use makepad_story_edit_voice::Transcript;
use makepad_story_cut::{SpokenLine, TakeInfo};
#[cfg(feature = "store")]
use makepad_widgets::log;
use makepad_story_cut::{AssetRef, MediaRef, RunRef};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    /// An MP4 with a picture (and perhaps sound).
    Video,
    /// A PNG or JPEG.
    Image,
    /// Sound only.
    Audio,
}

/// One resolved reference: the file, what it is, and the exact identity
/// the reader names it by (revision and content digest).
#[derive(Clone, Debug)]
pub struct ResolvedMedia {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub media: MediaType,
    pub revision: AssetRevisionRef,
    pub content: BlobId,
}

/// Resolves a cut's references to local files. Blocking: called on a
/// worker, never on the UI thread.
pub trait MediaResolver: Send {
    fn resolve(&mut self, media: &MediaRef) -> Result<ResolvedMedia, String>;
    /// The Splash source of a library `VjEffect` revision.
    fn effect_source(&mut self, effect: &AssetRef) -> Result<String, String>;
    /// A voice take's length and word times (its `Lyrics` file).
    /// `None` when the resolver has none; the line then uses estimates.
    fn take_info(&mut self, _take: &AssetRef) -> Result<Option<TakeInfo>, String> {
        Ok(None)
    }
    /// The Splash source of a cut revision a compound clip nests
    /// (`cut(id, rev)`).
    fn cut_source(&mut self, edit: &AssetRef) -> Result<String, String> {
        Err(format!("nested cut {} cannot be resolved here", edit.id))
    }
    /// A transcript's timed lines (a `Data` asset of `vj-lyrics` v5 JSON).
    /// `None` when the resolver has none; its captions are empty.
    fn transcript(&mut self, _transcript: &AssetRef) -> Result<Option<Vec<SpokenLine>>, String> {
        Ok(None)
    }
    /// The Splash source of a `Motion` asset revision a `Graphic` draws.
    fn motion_source(&mut self, motion: &AssetRef) -> Result<String, String> {
        Err(format!("Motion asset {} cannot be resolved here", motion.id))
    }
    /// The bytes of a file a Motion document uses, and what they are:
    /// "png", "jpg" or "svg" (images), or "glb" / "file" (a full Scene3D's
    /// models and other files).
    fn asset_bytes(&mut self, asset: &AssetRef) -> Result<(Vec<u8>, &'static str), String> {
        Err(format!("asset {} cannot be read here", asset.id))
    }
    /// An app run render's run.json (its marks, focus track and
    /// widgets), `None` when the resolver has none.
    fn run_data(&mut self, _run: &RunRef) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
    /// A song's aligned lyrics (its music bake's `Lyrics` file, `vj-lyrics`
    /// JSON), for Motion's sung-word timing. `None` when it has none.
    fn lyrics(&mut self, _song: &AssetRef) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// The exact revision a reference plays: the asset revision itself, or a
/// run's pinned render (its `Video` asset and revision).
pub fn media_revision(media: &MediaRef) -> Result<(AssetId, AssetRevisionId), String> {
    let rev = |text: &str| text.parse::<AssetRevisionId>().map_err(|_| format!("`{text}` is not a revision id"));
    let id = |text: &str| text.parse::<AssetId>().map_err(|_| format!("`{text}` is not an asset id"));
    match media {
        MediaRef::MissingOptional(path) => Err(format!("media not present: {path} (plays silent)")),
        MediaRef::Asset(asset) => Ok((id(&asset.id)?, rev(&asset.rev)?)),
        MediaRef::Run(run) => Ok((id(&run.render_id)?, rev(&run.render)?)),
    }
}

/// Media from the asset store, by exact revision.
#[cfg(feature = "store")]
pub struct StoreMedia {
    client: AssetClient,
}

#[cfg(feature = "store")]
impl StoreMedia {
    /// `client` is a lane of the host's verified client (its cache is shared).
    pub fn new(client: AssetClient) -> Self {
        Self { client }
    }

    /// A revision's file where it lies in the store's cache (fetched into
    /// it first when it is not there), and its content.
    fn cached(&mut self, file: &makepad_asset_data::AssetFile) -> Result<(PathBuf, BlobId), String> {
        let (blob, len) = (file.blob, file.byte_len);
        Ok((self.client.fetch_blob(&blob, Some(len), None).map_err(|e| e.to_string())?, blob))
    }

    /// A cut revision's source file.
    pub fn cut_file(&mut self, edit: &AssetRef) -> Result<(PathBuf, BlobId), String> {
        let (asset, revision) = media_revision(&MediaRef::Asset(edit.clone()))?;
        let manifest = self.client.fetch_asset_manifest(&revision).map_err(|e| e.to_string())?;
        if manifest.asset_id != asset || manifest.kind != AssetKind::Cut {
            return Err(format!("{} is not a revision of cut {}", edit.rev, edit.id));
        }
        let file = manifest.files.iter().find(|file| file.role == FileRole::Source).ok_or_else(|| format!("edit {} has no source", edit.id))?;
        self.cached(file)
    }

    /// A Motion asset revision's source file.
    pub fn motion_file(&mut self, motion: &AssetRef) -> Result<(PathBuf, BlobId), String> {
        let (asset, revision) = media_revision(&MediaRef::Asset(motion.clone()))?;
        let manifest = self.client.fetch_asset_manifest(&revision).map_err(|e| e.to_string())?;
        if manifest.asset_id != asset || manifest.kind != AssetKind::Motion {
            return Err(format!("{} is not a revision of Motion asset {}", motion.rev, motion.id));
        }
        let file = manifest.files.iter().find(|file| file.role == FileRole::Source).ok_or_else(|| format!("Motion asset {} has no source", motion.id))?;
        self.cached(file)
    }

    /// A library VjEffect revision's source file.
    pub fn effect_file(&mut self, effect: &AssetRef) -> Result<(PathBuf, BlobId), String> {
        let (_, revision) = media_revision(&MediaRef::Asset(effect.clone()))?;
        let manifest = self.client.fetch_asset_manifest(&revision).map_err(|e| e.to_string())?;
        if manifest.kind != AssetKind::VjEffect {
            return Err(format!("{} is not a VJ effect", effect.id));
        }
        let file = *select_file(&manifest, FileRole::Source, TierPreference::PreferWithAnyFallback(DeviceTier::High), 0).map_err(|e| e.to_string())?;
        self.cached(&file)
    }

    /// A revision's `Lyrics` file (a song's aligned words, a voice take's
    /// word times), when it has one.
    pub fn lyrics_file(&mut self, asset: &AssetRef) -> Result<Option<PathBuf>, String> {
        let (_, revision) = media_revision(&MediaRef::Asset(asset.clone()))?;
        let manifest = self.client.fetch_asset_manifest(&revision).map_err(|e| e.to_string())?;
        let Some(file) = manifest.files.iter().find(|file| file.role == FileRole::Lyrics) else { return Ok(None) };
        self.cached(file).map(|(path, _)| Some(path))
    }

    /// A file a Motion document uses, and what it is ("glb", "png", "jpg",
    /// "svg"): its model, else its picture, else its source.
    pub fn asset_file(&mut self, asset: &AssetRef) -> Result<(PathBuf, &'static str, BlobId), String> {
        let (_, revision) = media_revision(&MediaRef::Asset(asset.clone()))?;
        let manifest = self.client.fetch_asset_manifest(&revision).map_err(|e| e.to_string())?;
        let pick = |role| select_file(&manifest, role, TierPreference::PreferWithAnyFallback(DeviceTier::High), 0).ok().copied();
        if let Some(glb) = pick(FileRole::RenderGlb) {
            let (path, blob) = self.cached(&glb)?;
            return Ok((path, "glb", blob));
        }
        let file = pick(FileRole::Texture).or_else(|| pick(FileRole::Albedo)).or_else(|| pick(FileRole::Source))
            .ok_or_else(|| format!("{:?} asset {} has no image or source file", manifest.kind, manifest.asset_id))?;
        let kind = match file.media {
            MediaType::Png => "png",
            MediaType::Jpeg => "jpg",
            _ => "svg",
        };
        let (path, blob) = self.cached(&file)?;
        Ok((path, kind, blob))
    }

    /// A tree file's content where it lies: its media's played file, a
    /// document's source.
    pub fn tree_file(&mut self, reference: AssetRevisionRef) -> Result<(PathBuf, BlobId), String> {
        let manifest = self.client.fetch_asset_manifest(&reference.revision).map_err(|e| e.to_string())?;
        if manifest.asset_id != reference.asset_id { return Err("tree entry identity does not match its revision".into()); }
        let role = match manifest.kind {
            AssetKind::Texture => FileRole::Texture,
            AssetKind::Audio => FileRole::Audio,
            AssetKind::Video => FileRole::Video,
            _ => FileRole::Source,
        };
        let file = *select_file(&manifest, role, TierPreference::Exact(DeviceTier::Any), 0).map_err(|e| e.to_string())?;
        self.cached(&file)
    }
}

#[cfg(feature = "store")]
impl MediaResolver for StoreMedia {
    fn run_data(&mut self, run: &RunRef) -> Result<Option<Vec<u8>>, String> {
        let (_, revision) = media_revision(&MediaRef::Run(run.clone()))?;
        let manifest = self.client.fetch_asset_manifest(&revision).map_err(|e| e.to_string())?;
        let Some(file) = manifest.files.iter().find(|file| file.role == FileRole::Source && file.media == MediaType::Json) else { return Ok(None) };
        let path = self.client.fetch_blob(&file.blob, Some(file.byte_len), None).map_err(|e| e.to_string())?;
        makepad_asset_data::source::read(&path).map(Some).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn resolve(&mut self, media: &MediaRef) -> Result<ResolvedMedia, String> {
        if let Some(found) = folder::resolve(media) { return found; }
        let (asset, revision) = media_revision(media)?;
        let manifest = self.client.fetch_asset_manifest(&revision).map_err(|e| e.to_string())?;
        if asset != manifest.asset_id {
            return Err(format!("revision {revision} belongs to another asset"));
        }
        if matches!(media, MediaRef::Run(_)) && manifest.kind != AssetKind::Video {
            return Err(format!("the render {revision} of a run is not a video"));
        }
        let pick = |role| select_file(&manifest, role, TierPreference::PreferWithAnyFallback(DeviceTier::High), 0).ok();
        let (file, kind) = if let Some(file) = pick(FileRole::Video) {
            (file, MediaKind::Video)
        } else if let Some(file) = pick(FileRole::Audio) {
            (file, MediaKind::Audio)
        } else if let Some(file) = pick(FileRole::Texture).or_else(|| pick(FileRole::Albedo)) {
            (file, MediaKind::Image)
        } else {
            return Err(format!("{:?} asset {} has no video, audio or image file", manifest.kind, manifest.asset_id));
        };
        // A render reads the best copy the asset has; a lower tier is
        // said out loud (it is what a store holding only a proxy gives).
        if file.tier != DeviceTier::High && file.tier != DeviceTier::Any {
            log!("edit render: {} plays its {:?} tier copy", manifest.asset_id, file.tier);
        }
        let (blob, len, media_type) = (file.blob, file.byte_len, file.media);
        let path = self.client.fetch_blob(&blob, Some(len), None).map_err(|e| e.to_string())?;
        Ok(ResolvedMedia {
            path,
            kind,
            media: media_type,
            revision: AssetRevisionRef { asset_id: manifest.asset_id, revision },
            content: blob,
        })
    }

    fn cut_source(&mut self, edit: &AssetRef) -> Result<String, String> {
        let (path, _) = self.cut_file(edit)?;
        makepad_asset_data::source::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn transcript(&mut self, transcript: &AssetRef) -> Result<Option<Vec<SpokenLine>>, String> {
        let (_, revision) = media_revision(&MediaRef::Asset(transcript.clone()))?;
        let manifest = self.client.fetch_asset_manifest(&revision).map_err(|e| e.to_string())?;
        let Some(file) = manifest.files.iter().find(|file| file.role == FileRole::Source) else { return Ok(None) };
        let (blob, len) = (file.blob, file.byte_len);
        let path = self.client.fetch_blob(&blob, Some(len), None).map_err(|e| e.to_string())?;
        let bytes = makepad_asset_data::source::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Transcript::from_json(&bytes, None).map(|transcript| transcript.spoken_lines()))
    }

    fn lyrics(&mut self, song: &AssetRef) -> Result<Option<String>, String> {
        let Some(path) = self.lyrics_file(song)? else { return Ok(None) };
        makepad_asset_data::source::read_to_string(&path).map(Some).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn take_info(&mut self, take: &AssetRef) -> Result<Option<TakeInfo>, String> {
        let (_, revision) = media_revision(&MediaRef::Asset(take.clone()))?;
        let manifest = self.client.fetch_asset_manifest(&revision).map_err(|e| e.to_string())?;
        let Some(file) = manifest.files.iter().find(|file| file.role == FileRole::Lyrics) else { return Ok(None) };
        let (blob, len) = (file.blob, file.byte_len);
        let path = self.client.fetch_blob(&blob, Some(len), None).map_err(|e| e.to_string())?;
        let bytes = makepad_asset_data::source::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Transcript::from_json(&bytes, None).map(|transcript| transcript.take_info()))
    }

    fn motion_source(&mut self, motion: &AssetRef) -> Result<String, String> {
        if let Some(found) = folder::text(motion) { return found; }
        let (path, _) = self.motion_file(motion)?;
        makepad_asset_data::source::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn asset_bytes(&mut self, asset: &AssetRef) -> Result<(Vec<u8>, &'static str), String> {
        if let Some(found) = folder::bytes(asset) { return found; }
        let (path, kind, _) = self.asset_file(asset)?;
        Ok((makepad_asset_data::source::read(&path).map_err(|e| format!("{}: {e}", path.display()))?, kind))
    }

    fn effect_source(&mut self, effect: &AssetRef) -> Result<String, String> {
        let (path, _) = self.effect_file(effect)?;
        makepad_asset_data::source::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// A fixed table from asset id to a local file: what the standalone
/// renderer is given on its command line, or a render job lists
/// ([`crate::job`]). A file's content identity and what it is are given
/// with it, else its own bytes and its extension say.
#[derive(Clone, Debug, Default)]
pub struct LocalMedia {
    pub tree: Option<(std::sync::Arc<makepad_asset_data::tree::TreeDraft>, String)>,
    files: HashMap<String, PathBuf>,
    /// A file's extension and content digest when given with it (a store
    /// blob is named by its digest, with no extension of its own).
    known: HashMap<String, (String, Option<BlobId>)>,
    effects: HashMap<String, String>,
    lyrics: HashMap<String, PathBuf>,
    edits: HashMap<String, PathBuf>,
    runs: HashMap<String, PathBuf>,
}

impl LocalMedia {
    pub fn new() -> Self {
        Self::default()
    }

    /// A run render's run.json, by the render's asset id.
    pub fn insert_run(&mut self, render_id: impl Into<String>, path: impl Into<PathBuf>) {
        self.runs.insert(render_id.into(), path.into());
    }

    /// `asset_id` (or a run's render revision) plays `path`.
    pub fn insert(&mut self, id: impl Into<String>, path: impl Into<PathBuf>) {
        let id = id.into();
        self.known.remove(&id);
        self.files.insert(id, path.into());
    }

    /// `id` is the file at `path`, a `ext` file whose content digest is
    /// `content` when known.
    pub fn insert_located(&mut self, id: &str, path: &Path, ext: &str, content: Option<BlobId>) {
        self.files.insert(id.to_string(), path.to_path_buf());
        self.known.insert(id.to_string(), (ext.to_ascii_lowercase(), content));
    }

    /// The file `id` names (exactly `id@rev` first), its extension and its
    /// content digest when known.
    fn file(&self, id: &str, exact: &str) -> Option<(&PathBuf, String, Option<BlobId>)> {
        let key = if self.files.contains_key(exact) { exact } else { id };
        let path = self.files.get(key)?;
        Some(match self.known.get(key) {
            Some((ext, content)) => (path, ext.clone(), *content),
            None => (path, path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase(), None),
        })
    }

    /// A nested cut's Splash source file.
    pub fn insert_edit(&mut self, id: impl Into<String>, path: impl Into<PathBuf>) {
        self.edits.insert(id.into(), path.into());
    }

    /// A voice take's (or a transcript's) `vj-lyrics` word times.
    pub fn insert_lyrics(&mut self, id: impl Into<String>, path: impl Into<PathBuf>) {
        self.lyrics.insert(id.into(), path.into());
    }

    /// A library effect asset's source.
    pub fn insert_effect(&mut self, id: impl Into<String>, source: impl Into<String>) {
        self.effects.insert(id.into(), source.into());
    }
}

/// A file's content digest, read in 1 MiB pieces (never whole in memory).
pub fn file_digest(path: &Path) -> Result<BlobId, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).map_err(|e| format!("{}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(BlobId::from_bytes(hasher.finalize()))
}

/// What a local file is, by extension.
pub fn local_kind(path: &Path) -> Result<(MediaKind, MediaType), String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    kind_of(&ext).ok_or_else(|| format!("{}: unknown media type", path.display()))
}

/// What a file with extension `ext` is.
pub fn kind_of(ext: &str) -> Option<(MediaKind, MediaType)> {
    Some(match ext {
        "mp4" | "mov" | "m4v" => (MediaKind::Video, MediaType::Mp4),
        "png" => (MediaKind::Image, MediaType::Png),
        "jpg" | "jpeg" => (MediaKind::Image, MediaType::Jpeg),
        "wav" => (MediaKind::Audio, MediaType::Wav),
        "mp3" => (MediaKind::Audio, MediaType::Mp3),
        "ogg" => (MediaKind::Audio, MediaType::Ogg),
        "m4a" | "aac" => (MediaKind::Audio, MediaType::Mp4),
        _ => return None,
    })
}

impl MediaResolver for LocalMedia {
    fn run_data(&mut self, run: &RunRef) -> Result<Option<Vec<u8>>, String> {
        let Some(path) = self.runs.get(&run.render_id) else { return Ok(None) };
        makepad_asset_data::source::read(path).map(Some).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn resolve(&mut self, media: &MediaRef) -> Result<ResolvedMedia, String> {
        let key = match media {
            MediaRef::Asset(asset) => &asset.id,
            MediaRef::Run(run) => &run.render_id,
            MediaRef::MissingOptional(path) => return Err(format!("media not present: {path} (plays silent)")),
        };
        let (asset_id, revision) = media_revision(media)?;
        let exact = format!("{asset_id}@{revision}");
        let (path, ext, content) = self.file(key, &exact).ok_or_else(|| format!("no local file for {key}"))?;
        let path = path.clone();
        let (kind, media_type) = kind_of(&ext).ok_or_else(|| format!("{}: unknown media type", path.display()))?;
        Ok(ResolvedMedia {
            content: match content {
                Some(content) => content,
                None => file_digest(&path)?,
            },
            path,
            kind,
            media: media_type,
            revision: AssetRevisionRef { asset_id, revision },
        })
    }

    fn cut_source(&mut self, edit: &AssetRef) -> Result<String, String> {
        let path = self.edits.get(&format!("{}@{}", edit.id, edit.rev)).or_else(|| self.edits.get(&edit.id)).ok_or_else(|| format!("no local source for edit {}", edit.id))?;
        makepad_asset_data::source::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn transcript(&mut self, transcript: &AssetRef) -> Result<Option<Vec<SpokenLine>>, String> {
        let Some(path) = self.lyrics.get(&format!("{}@{}", transcript.id, transcript.rev)).or_else(|| self.lyrics.get(&transcript.id)) else { return Ok(None) };
        let bytes = makepad_asset_data::source::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Transcript::from_json(&bytes, None).map(|transcript| transcript.spoken_lines()))
    }

    /// `lyrics:<id>=<file>` in the table.
    fn lyrics(&mut self, song: &AssetRef) -> Result<Option<String>, String> {
        let Some(path) = self.lyrics.get(&format!("{}@{}", song.id, song.rev)).or_else(|| self.lyrics.get(&song.id)) else { return Ok(None) };
        makepad_asset_data::source::read_to_string(path).map(Some).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn take_info(&mut self, take: &AssetRef) -> Result<Option<TakeInfo>, String> {
        let Some(path) = self.lyrics.get(&format!("{}@{}", take.id, take.rev)).or_else(|| self.lyrics.get(&take.id)) else { return Ok(None) };
        let bytes = makepad_asset_data::source::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Transcript::from_json(&bytes, None).map(|transcript| transcript.take_info()))
    }

    fn effect_source(&mut self, effect: &AssetRef) -> Result<String, String> {
        self.effects.get(&format!("{}@{}", effect.id, effect.rev)).or_else(|| self.effects.get(&effect.id)).cloned().ok_or_else(|| format!("no local source for effect {}", effect.id))
    }

    /// A Motion document is a `.splash` file in the table.
    fn motion_source(&mut self, motion: &AssetRef) -> Result<String, String> {
        let path = self.files.get(&format!("{}@{}", motion.id, motion.rev)).or_else(|| self.files.get(&motion.id)).ok_or_else(|| format!("no local file for Motion asset {}", motion.id))?;
        makepad_asset_data::source::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn asset_bytes(&mut self, asset: &AssetRef) -> Result<(Vec<u8>, &'static str), String> {
        let (path, ext, _) = self.file(&asset.id, &format!("{}@{}", asset.id, asset.rev)).ok_or_else(|| format!("no local file for {}", asset.id))?;
        let kind = match ext.as_str() {
            "png" => "png",
            "jpg" | "jpeg" => "jpg",
            "svg" => "svg",
            "glb" | "gltf" => "glb",
            "hdr" | "ttf" | "otf" => "file",
            _ => return Err(format!("{}: not an image or SVG", path.display())),
        };
        Ok((makepad_asset_data::source::read(path).map_err(|e| format!("{}: {e}", path.display()))?, kind))
    }
}

/// Media of documents opened in place from a folder (Stage's bundled decks
/// and cuts): the folder reader names each media file by a handle made from
/// its folder path and its bytes (never a store revision) and records the
/// file here, and every resolver reads such a handle straight off disk.
pub mod folder {
    use super::{local_kind, AssetRef, MediaRef, ResolvedMedia};
    use makepad_asset_data::tree::{TreeDraft, TreeRole};
    use makepad_asset_data::tree::TreeFile;
    use makepad_asset_data::{AssetId, AssetKind, AssetRevisionId, AssetRevisionRef, BlobId};
    use std::collections::BTreeMap;
    use std::collections::{BTreeSet, HashMap};
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    fn files() -> &'static Mutex<HashMap<AssetRevisionRef, PathBuf>> {
        static FILES: OnceLock<Mutex<HashMap<AssetRevisionRef, PathBuf>>> = OnceLock::new();
        FILES.get_or_init(Default::default)
    }

    /// The handle of `file` read with `bytes`, recorded so resolvers read it
    /// from `file`. The same path and bytes give the same handle.
    pub fn register(file: &Path, bytes: &[u8]) -> AssetRevisionRef {
        let name = BlobId::hash_of(file.to_string_lossy().as_bytes());
        let reference = AssetRevisionRef {
            asset_id: AssetId::from_bytes(name.as_bytes()[..16].try_into().unwrap()),
            revision: AssetRevisionId::hash_of(bytes),
        };
        if let Ok(mut files) = files().lock() { files.insert(reference, file.to_path_buf()); }
        reference
    }

    /// The file a folder handle names.
    pub fn location(reference: &AssetRevisionRef) -> Option<PathBuf> {
        files().lock().ok()?.get(reference).cloned()
    }

    fn path_of(asset: &AssetRef) -> Option<PathBuf> {
        let reference = AssetRevisionRef { asset_id: asset.id.parse().ok()?, revision: asset.rev.parse().ok()? };
        files().lock().ok()?.get(&reference).cloned()
    }

    /// A folder file as a played medium (None: not a folder file).
    pub fn resolve(media: &MediaRef) -> Option<Result<ResolvedMedia, String>> {
        let MediaRef::Asset(asset) = media else { return None };
        let path = path_of(asset)?;
        Some((|| {
            let (kind, media) = local_kind(&path)?;
            let revision = AssetRevisionRef { asset_id: asset.id.parse().map_err(|_| "bad asset id")?, revision: asset.rev.parse().map_err(|_| "bad revision")? };
            // The handle's revision is the SHA-256 of the bytes, as a blob id is.
            let content = BlobId::from_bytes(*revision.revision.as_bytes());
            Ok(ResolvedMedia { path, kind, media, revision, content })
        })())
    }

    /// A folder image, SVG or model's bytes, and what it is.
    pub fn bytes(asset: &AssetRef) -> Option<Result<(Vec<u8>, &'static str), String>> {
        let path = path_of(asset)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let kind = match ext.as_str() {
            "png" => "png",
            "jpg" | "jpeg" => "jpg",
            "svg" => "svg",
            "glb" | "gltf" => "glb",
            _ => "file",
        };
        Some(makepad_asset_data::source::read(&path).map(|bytes| (bytes, kind)).map_err(|e| format!("{}: {e}", path.display())))
    }

    /// A folder text file (a Motion document's source).
    pub fn text(asset: &AssetRef) -> Option<Result<String, String>> {
        let path = path_of(asset)?;
        Some(makepad_asset_data::source::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display())))
    }

    /// A folder that holds a document tree (Stage's bundled decks and cuts,
    /// a player's document): its top-level deck files (`deck.splash` first,
    /// the root when there are several), its one cut, which of them is the
    /// tree's root, and the optional paths its `.tree` descriptors declare.
    #[derive(Clone, Debug)]
    pub struct Folder {
        pub dir: PathBuf,
        pub role: TreeRole,
        pub root: String,
        pub decks: Vec<String>,
        pub cut: Option<String>,
        pub optional: BTreeSet<String>,
    }

    impl Folder {
        /// `None` when `dir` holds no deck or cut.
        pub fn at(dir: &Path) -> Result<Option<Folder>, String> {
            let mut cuts = Vec::new();
            let mut decks: Vec<String> = Vec::new();
            for entry in makepad_asset_data::source::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))? {
                if !entry.is_file() { continue; }
                let file = entry.name;
                if file == "deck.splash" || file.ends_with(".deck.splash") { decks.push(file); }
                else if file == "cut.splash" || file.ends_with(".cut.splash") { cuts.push(file); }
            }
            if cuts.len() > 1 { return Err("more than one cut".into()); }
            let cut = cuts.pop();
            // Several decks (talks) share a folder: deck.splash is then the root.
            decks.sort_by_key(|path| (path != "deck.splash", path.clone()));
            if decks.len() > 1 && decks[0] != "deck.splash" { return Err("several decks in one folder need deck.splash as the root".into()); }
            let (role, root) = match (decks.first(), &cut) {
                (Some(deck), _) => (TreeRole::Deck, deck.clone()),
                (None, Some(cut)) => (TreeRole::Cut, cut.clone()),
                (None, None) => return Ok(None),
            };
            let optional = optional_paths(dir)?;
            Ok(Some(Folder { dir: dir.to_path_buf(), role, root, decks, cut, optional }))
        }

        /// The tree's kind of `path`: the root and the other top-level decks
        /// are decks, cuts are cuts, the rest by extension.
        pub fn kind_of(&self, path: &str) -> AssetKind {
            if self.decks.iter().any(|deck| deck == path) { return AssetKind::Deck; }
            match makepad_motion::tree::file_kind(path) {
                AssetKind::Deck => AssetKind::Motion,
                kind => kind,
            }
        }

        /// The tree as the folder holds it now, named `name`. Media files are
        /// named by a handle of their path and bytes that every resolver reads
        /// off disk ([`register`]); an optional entry is filled from the
        /// folder, else from the file `fill` names for it, else left out.
        pub fn read(&self, name: String, fill: &dyn Fn(&str) -> Option<PathBuf>) -> Result<TreeDraft, String> {
            let mut files = makepad_motion::tree::folder_files(&self.dir)?;
            for path in &self.optional { files.remove(path); }
            let mut draft = TreeDraft { name, root: (self.role, self.root.clone()), files: BTreeMap::new(), optional: self.optional.clone() };
            let mut add = |path: String, bytes: Vec<u8>, file: &Path| {
                let kind = self.kind_of(&path);
                let handle = matches!(kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio).then(|| register(file, &bytes));
                draft.files.insert(path.clone(), TreeFile { path, reference: handle, base: handle, kind, bytes: bytes.into() });
            };
            for (path, bytes) in files {
                let file = self.dir.join(&path);
                add(path, bytes, &file);
            }
            for path in &self.optional {
                let local = self.dir.join(path);
                let Some(file) = Some(local).filter(|local| makepad_asset_data::source::is_file(local)).or_else(|| fill(path)) else { continue };
                let bytes = makepad_asset_data::source::read(&file).map_err(|error| format!("{}: {error}", file.display()))?;
                add(path.clone(), bytes, &file);
            }
            Ok(draft)
        }
    }

    /// The optional paths a folder's `.tree` descriptors declare (they
    /// declare nothing else).
    pub fn optional_paths(dir: &Path) -> Result<BTreeSet<String>, String> {
        let mut optional = BTreeSet::new();
        let mut entries = makepad_asset_data::source::read_dir(dir).map_err(|e| e.to_string())?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in entries {
            if !entry.is_file() || !entry.name.ends_with(".tree") { continue; }
            let path = dir.join(&entry.name);
            let tree = makepad_asset_data::tree::Tree::parse(&makepad_asset_data::source::read_to_string(&path).map_err(|e| e.to_string())?)?;
            if tree.root.is_some() || tree.entries.values().any(|entry| !matches!(entry, makepad_asset_data::tree::TreeEntry::Optional(None))) {
                return Err(format!("{}: folder descriptors declare optional paths only", path.display()));
            }
            for path in tree.optional_paths() {
                if !optional.insert(path.clone()) { return Err(format!("duplicate optional path `{path}`")); }
            }
        }
        Ok(optional)
    }

    /// Every media file of `tree` that has a folder handle, as a fixed table
    /// (each read where it lies, its handle's revision as its content), the
    /// way a render job lists them (a render job's `Job::load`).
    pub fn local_media(tree: &TreeDraft) -> super::LocalMedia {
        let mut media = super::LocalMedia::new();
        for file in tree.files.values() {
            let Some(reference) = file.reference else { continue };
            let Some(path) = location(&reference) else { continue };
            let ext = file.path.rsplit_once('.').map_or("", |(_, ext)| ext);
            media.insert_located(&reference.to_string(), &path, ext, Some(BlobId::from_bytes(*reference.revision.as_bytes())));
        }
        media
    }
}
