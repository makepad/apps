//! IMPORT CONTENT: walk files and folders and catalogue the media in them
//! WITHOUT COPYING the payloads (ported from the donor console's
//! `media_scan.rs`, deleted in ef48f3d9; the publish path is
//! `examples/local_media_import.rs`'s).
//!
//! A VJ's video folder is the thing least wanted twice: it is curated,
//! backed up and big. So each file is published as an ordinary catalog
//! asset whose heavy file is a REFERENCE (`PublishBundleFile::reference`):
//! the store hashes it where it lies and records path, size and digest, and
//! owns only what it derives -- the thumbnail, the manifest, the search row.
//! The store must admit references (`BlobRefPolicy::local_host`, which the
//! embedded store does).
//!
//! The walk is sorted by name (two runs enumerate alike), skips hidden
//! entries and symlinks, and reports recognised-but-unsupported files
//! instead of dropping them in silence. Identity is an alias derived from
//! the file's absolute path, so a second import of the same folder finds
//! each alias and skips it: re-import costs one control-plane call a file.
//! Folder names become tags, so a tree is searchable as it lands; audio
//! also carries the `music` tag the DJ library lists.

use makepad_asset_client::{AssetClient, ClientError, PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetAlias, AssetKind, DerivativePolicy, FileRole, MediaType, Redistribution, Sha256, ThumbnailMedia};
use std::path::{Path, PathBuf};

/// Namespace imported media lands in, so it can be addressed as a lane.
pub const MEDIA_NAMESPACE: &str = "vjmedia";

/// Files a run recognises but cannot publish: said, not dropped.
const KNOWN_UNSUPPORTED: [&str; 10] = ["avi", "mkv", "webm", "wmv", "flv", "gif", "tif", "tiff", "bmp", "heic"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaClass {
    Video,
    Image,
    Audio,
}

impl MediaClass {
    pub fn label(self) -> &'static str {
        match self {
            MediaClass::Video => "video",
            MediaClass::Image => "image",
            MediaClass::Audio => "audio",
        }
    }

    fn kind(self) -> AssetKind {
        match self {
            MediaClass::Video => AssetKind::Video,
            MediaClass::Image => AssetKind::Texture,
            MediaClass::Audio => AssetKind::Audio,
        }
    }

    fn role(self) -> FileRole {
        match self {
            MediaClass::Video => FileRole::Video,
            MediaClass::Image => FileRole::Texture,
            MediaClass::Audio => FileRole::Audio,
        }
    }
}

/// One recognised file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaFile {
    pub path: PathBuf,
    pub class: MediaClass,
    pub media: MediaType,
    /// File name without extension: the title.
    pub stem: String,
    /// Folder names between the root and the file: tags.
    pub dirs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedFile {
    pub path: PathBuf,
    pub why: String,
}

#[derive(Clone, Debug, Default)]
pub struct MediaScan {
    pub files: Vec<MediaFile>,
    pub skipped: Vec<SkippedFile>,
}

fn classify(ext: &str) -> Option<(MediaClass, MediaType)> {
    Some(match ext {
        "mp4" | "mov" | "m4v" => (MediaClass::Video, MediaType::Mp4),
        "png" => (MediaClass::Image, MediaType::Png),
        "jpg" | "jpeg" => (MediaClass::Image, MediaType::Jpeg),
        "wav" | "wave" => (MediaClass::Audio, MediaType::Wav),
        "mp3" => (MediaClass::Audio, MediaType::Mp3),
        "ogg" | "oga" => (MediaClass::Audio, MediaType::Ogg),
        _ => return None,
    })
}

/// Every recognised file under each of `paths` (a file stands for itself).
pub fn scan_paths(paths: &[PathBuf]) -> MediaScan {
    let mut out = MediaScan::default();
    for path in paths {
        if path.is_dir() {
            walk(path, &mut Vec::new(), &mut out);
        } else {
            consider(path, &[], &mut out);
        }
    }
    out
}

fn walk(dir: &Path, trail: &mut Vec<String>, out: &mut MediaScan) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        out.skipped.push(SkippedFile { path: dir.to_path_buf(), why: "unreadable folder".to_string() });
        return;
    };
    let mut names: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    names.sort();
    for path in names {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else { continue };
        if name.starts_with('.') {
            continue;
        }
        // A symlink is a second path to something walked, or a way out.
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_symlink() => continue,
            Err(_) => continue,
            _ => {}
        }
        if path.is_dir() {
            trail.push(name.to_string());
            walk(&path, trail, out);
            trail.pop();
        } else {
            consider(&path, trail, out);
        }
    }
}

fn consider(path: &Path, trail: &[String], out: &mut MediaScan) {
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default().to_ascii_lowercase();
    let Some((class, media)) = classify(&ext) else {
        if KNOWN_UNSUPPORTED.contains(&ext.as_str()) {
            out.skipped.push(SkippedFile { path: path.to_path_buf(), why: format!("{ext} is not a format the catalog publishes") });
        }
        return;
    };
    if std::fs::metadata(path).map_or(0, |meta| meta.len()) == 0 {
        out.skipped.push(SkippedFile { path: path.to_path_buf(), why: "empty file".to_string() });
        return;
    }
    let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("untitled").to_string();
    out.files.push(MediaFile { path: path.to_path_buf(), class, media, stem, dirs: trail.to_vec() });
}

/// Lowercase alphanumerics and dashes, bounded: the alias charset.
fn slug(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut dash = true;
    for ch in text.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && out.len() < max {
            out.push('-');
            dash = true;
        }
        if out.len() >= max {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "clip".to_string()
    } else {
        trimmed
    }
}

fn hex8(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    hash.finalize()[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// The stable alias of the file at this exact path: by path, not content,
/// because hashing gigabytes to learn a name is the cost this avoids.
pub fn alias_for(path: &Path) -> Option<AssetAlias> {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let stem = abs.file_stem().and_then(|stem| stem.to_str()).unwrap_or("clip");
    AssetAlias::new(format!("{MEDIA_NAMESPACE}/{}-{}", slug(stem, 48), hex8(abs.to_string_lossy().as_bytes()))).ok()
}

/// The tags an imported file carries: its folder names, `local`, and for
/// audio the `music` tag the DJ library lists.
pub fn import_tags(file: &MediaFile) -> Vec<String> {
    let mut tags: Vec<String> = file.dirs.iter().map(|dir| slug(dir, 32)).collect();
    tags.push("local".to_string());
    if file.class == MediaClass::Audio {
        tags.push("music".to_string());
    }
    tags.dedup();
    tags
}

/// The rights of an import of the operator's own files: no licence is
/// claimed for content whose provenance is unknown, the source says where
/// it came from, nothing is cleared for redistribution.
pub fn personal_rights(root: &Path) -> PublishRights {
    PublishRights {
        license: "LicenseRef-Personal-Library".to_string(),
        license_revision: String::new(),
        terms_digest: None,
        terms_url: String::new(),
        credits: String::new(),
        source: format!("local import: {}", root.display()),
        source_archive: None,
        redistribution: Redistribution::Forbidden,
        derivatives: DerivativePolicy::Allowed,
    }
}

/// The picture of one file and its playback length in ms. A video costs
/// one decoded frame and no full read; an image or a sound is read only to
/// derive the thumbnail -- those bytes are never uploaded.
fn thumbnail_of(file: &MediaFile, abs: &Path) -> Result<(PublishThumbnail, u32, Option<(u32, u32)>), String> {
    match file.class {
        MediaClass::Video => {
            let probe = crate::videothumb::probe_video(abs)?;
            let thumb = PublishThumbnail::plain(probe.thumbnail_jpeg, ThumbnailMedia::Jpeg, crate::thumbs::THUMB_DIM as u32, crate::thumbs::THUMB_DIM as u32);
            Ok((thumb, probe.duration_ms, None))
        }
        MediaClass::Image => {
            let bytes = std::fs::read(abs).map_err(|error| format!("read: {error}"))?;
            let dims = crate::thumbs::png_dims(&bytes).or_else(|| crate::thumbs::jpeg_dims(&bytes)).ok_or("unreadable image size")?;
            let (thumb, media, width, height) = crate::import::usable_image_thumb(&bytes).ok_or("unusable image")?;
            Ok((PublishThumbnail::plain(thumb, media, width, height), 0, Some(dims)))
        }
        MediaClass::Audio => {
            let bytes = std::fs::read(abs).map_err(|error| format!("read: {error}"))?;
            let pcm = crate::thumbs::decode_audio(&bytes, file.media)?;
            let thumb = crate::thumbs::audio_thumbnail_jpeg(&pcm)?;
            let millis = crate::thumbs::audio_millis(&bytes, file.media).unwrap_or(0);
            Ok((PublishThumbnail { bytes: thumb.bytes, media: ThumbnailMedia::Jpeg, width: thumb.width, height: thumb.height, views: thumb.views }, millis, None))
        }
    }
}

/// What became of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileOutcome {
    Published,
    /// Its alias already exists: imported before.
    Present,
    Failed(String),
}

/// Publish one file by reference, unless it is in the catalog already.
pub fn import_file(client: &mut AssetClient, file: &MediaFile, rights: &PublishRights) -> FileOutcome {
    let abs = std::path::absolute(&file.path).unwrap_or_else(|_| file.path.clone());
    let Some(alias) = alias_for(&abs) else { return FileOutcome::Failed("no alias for this path".to_string()) };
    match client.resolve_alias(&alias) {
        Err(ClientError::NotFound { .. }) => {}
        Ok(_) => return FileOutcome::Present,
        Err(error) => return FileOutcome::Failed(format!("alias: {error}")),
    }
    let (thumb, millis, dims) = match thumbnail_of(file, &abs) {
        Ok(found) => found,
        Err(error) => return FileOutcome::Failed(error),
    };
    let class = file.class;
    let mut bundle = PublishBundle::new(
        MEDIA_NAMESPACE,
        class.kind(),
        file.stem.clone(),
        vec![PublishBundleFile::reference(class.role(), file.media, abs, dims)],
        thumb,
        rights.clone(),
    );
    bundle.alias = Some(alias);
    bundle.media_millis = millis;
    bundle.categories = vec!["imported".to_string(), class.label().to_string()];
    bundle.tags = import_tags(file);
    match client.publish_bundle(&bundle) {
        Ok(_) => FileOutcome::Published,
        Err(error) => FileOutcome::Failed(format!("publish: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_walk_finds_media_skips_hidden_and_reports_the_unsupported() {
        let root = std::env::temp_dir().join(format!("media-import-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sets/opener")).unwrap();
        std::fs::write(root.join("sets/opener/b.mp4"), b"x").unwrap();
        std::fs::write(root.join("sets/a.png"), b"x").unwrap();
        std::fs::write(root.join("sets/clip.mkv"), b"x").unwrap();
        std::fs::write(root.join("sets/notes.txt"), b"x").unwrap();
        std::fs::write(root.join("sets/empty.wav"), b"").unwrap();
        std::fs::write(root.join(".hidden.mp4"), b"x").unwrap();
        let scan = scan_paths(&[root.clone()]);
        let names: Vec<&str> = scan.files.iter().map(|file| file.stem.as_str()).collect();
        assert_eq!(names, ["a", "b"], "sorted, hidden left out");
        assert_eq!(scan.files[1].dirs, ["sets", "opener"]);
        assert_eq!(scan.files[1].class, MediaClass::Video);
        let why: Vec<&str> = scan.skipped.iter().map(|s| s.why.as_str()).collect();
        assert_eq!(why, ["mkv is not a format the catalog publishes", "empty file"]);
        assert_eq!(import_tags(&scan.files[1]), ["sets", "opener", "local"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_alias_follows_the_path() {
        let a = alias_for(Path::new("/media/sets/Opener Loop.mp4")).unwrap();
        let b = alias_for(Path::new("/media/other/Opener Loop.mp4")).unwrap();
        assert!(a.as_str().starts_with("vjmedia/opener-loop-"), "{}", a.as_str());
        assert_ne!(a, b, "the same name in another folder is another file");
        assert_eq!(alias_for(Path::new("/media/sets/Opener Loop.mp4")), Some(a));
    }
}
