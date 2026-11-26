//! Publishing a finished render: an ordinary `Video` asset
//! whose one dependency is the cut revision it was rendered from, under
//! the cut's render alias, through the asset lane's render helpers, with
//! its captions. Runs on a worker (blocking client calls).
//!
//! The video is streamed from its file (never whole in memory). Every
//! Subtitles node's captions are two files of the render, `CaptionsSrt` and
//! `CaptionsVtt`, whose slot `lod` is the track index: tracks in node id
//! order ([`RenderResult::captions`]). The contract has eight slots
//! (`MAX_LOD`); tracks past them travel in the render's `Source` file, a
//! JSON document of format [`CAPTIONS_FORMAT`] ([`captions_json`]), so no
//! caption is ever dropped.

use crate::render::{Captions, RenderResult};
use makepad_asset_client::{AssetClient, PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail, PublishedBundle};
use makepad_asset_data::limits::MAX_LOD;
use makepad_asset_data::{AssetKind, AssetRevisionRef, FileRole, MediaType, ThumbnailMedia};
use makepad_widgets::log;

/// The `format` of the captions document a render's `Source` file holds.
pub const CAPTIONS_FORMAT: &str = "makepad-edit-captions";

/// Catalogue thumbnails are 256..=4096 pixels per side.
const THUMB_MIN: u32 = 256;
const THUMB_MAX: u32 = 4096;

/// Publish `render` as the render of `cut`. Its rights are the cut's own
/// (a render derives from nothing else the cut does not already name).
pub fn publish_edit_render(client: &mut AssetClient, edit: AssetRevisionRef, path: Option<&str>, render: &RenderResult, title: &str) -> Result<PublishedBundle, String> {
    let manifest = client.fetch_asset_manifest(&edit.revision).map_err(|e| e.to_string())?;
    if manifest.asset_id != edit.asset_id || manifest.kind != if path.is_some() { AssetKind::Tree } else { AssetKind::Cut } {
        return Err(format!("{} is not a revision of cut {}", edit.revision, edit.asset_id));
    }
    // The render must be of exactly this revision's source: the
    // dependency and the alias would otherwise lie.
    let cut_manifest = if let Some(path) = path {
        use makepad_asset_data::tree::TreeResolver;
        let tree = makepad_asset_client::tree::load(client, edit)?;
        let file = tree.resolve("", path)?;
        if file.kind != AssetKind::Cut { return Err(format!("{path}: expected a cut")); }
        let reference = file.reference.ok_or("the tree file has no revision")?;
        client.fetch_asset_manifest(&reference.revision).map_err(|e| e.to_string())?
    } else { manifest.clone() };
    let source = cut_manifest.files.iter().find(|file| file.role == FileRole::Source).ok_or("the cut revision has no source")?;
    if source.blob != render.cut_source {
        return Err(format!("the render was made from another source than edit revision {}", edit.revision));
    }
    let (pixels, width, height) = render.poster.as_ref().ok_or("the render kept no poster frame")?;
    let (jpeg, tw, th) = thumbnail_jpeg(pixels, *width, *height)?;
    let mut files = vec![PublishBundleFile::file(FileRole::Video, MediaType::Mp4, render.path.clone(), None)];
    let captions = render.captions();
    let slots = MAX_LOD as usize + 1;
    for (track, captions) in captions.iter().take(slots).enumerate() {
        for (role, path) in [(FileRole::CaptionsSrt, &captions.srt), (FileRole::CaptionsVtt, &captions.vtt)] {
            let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut file = PublishBundleFile::bytes(role, MediaType::Text, text, None);
            file.lod = track as u8;
            files.push(file);
        }
    }
    if captions.len() > slots {
        log!("edit render: {} caption tracks; the ones past {slots} travel in the Source document", captions.len());
        files.push(PublishBundleFile::bytes(FileRole::Source, MediaType::Json, captions_json(&captions[slots..])?.into_bytes(), None));
    }
    let mut bundle = PublishBundle::new(
        // `publish_render` files it in the cut's own namespace.
        String::new(),
        AssetKind::Video,
        title,
        files,
        PublishThumbnail::plain(jpeg, ThumbnailMedia::Jpeg, tw, th),
        PublishRights::from_manifest(&manifest.rights),
    );
    bundle.media_millis = render.duration_ms;
    bundle.generator = "makepad-edit-render".into();
    if let Some(path) = path {
        let detail = client.asset_detail(&edit.asset_id).map_err(|e| e.to_string())?;
        if detail.retired || detail.candidates.iter().any(|c| c.revision == edit.revision && c.retired_ms.is_some()) { return Err("the project revision was retired".into()); }
        let mut bundle = makepad_asset_client::render::render_bundle(edit, &detail.namespace, bundle).map_err(|e| e.to_string())?;
        bundle.alias = Some(project_render_alias(&detail.namespace, edit.asset_id, path)?);
        if let Some((_, current)) = project_render(client, edit, path)? {
            let order = |revision| detail.candidates.iter().find(|c| c.revision == revision).map(|c| (c.staged_ms, c.revision));
            if order(current.revision) > order(edit.revision) { bundle.alias = None; }
        }
        client.publish_bundle(&bundle).map_err(|e| e.to_string())
    } else { client.publish_render(edit, bundle).map_err(|e| e.to_string()) }
}

fn project_render_alias(namespace: &str, project: makepad_asset_data::AssetId, path: &str) -> Result<makepad_asset_data::AssetAlias, String> {
    makepad_asset_data::tree::validate_path(path)?;
    let key = makepad_asset_data::BlobId::hash_of(format!("{project}/{path}").as_bytes());
    format!("{namespace}/render/{key}").parse().map_err(|e| format!("render alias: {e}"))
}

pub fn project_render(client: &mut AssetClient, project: AssetRevisionRef, path: &str) -> Result<Option<(AssetRevisionRef, AssetRevisionRef)>, String> {
    let detail = client.asset_detail(&project.asset_id).map_err(|e| e.to_string())?;
    let alias = project_render_alias(&detail.namespace, project.asset_id, path)?;
    let head = match client.resolve_alias(&alias) {
        Ok(head) => head,
        Err(makepad_asset_client::ClientError::NotFound { .. }) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let render = AssetRevisionRef { asset_id: head.asset_id, revision: head.head_revision };
    let manifest = client.fetch_asset_manifest(&render.revision).map_err(|e| e.to_string())?;
    match manifest.dependencies.as_slice() {
        [source] if manifest.kind == AssetKind::Video && source.asset_id == project.asset_id => Ok(Some((render, *source))),
        _ => Err("the project render has a different source".into()),
    }
}

/// `{"format": "makepad-edit-captions", "version": 1, "tracks": [{"node",
/// "srt", "vtt"}]}`: caption tracks' SRT and WebVTT text, as the renderer
/// wrote them beside the video (the tracks past the caption slots).
pub fn captions_json(captions: &[Captions]) -> Result<String, String> {
    let mut tracks = Vec::new();
    for track in captions {
        let read = |path: &std::path::Path| std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()));
        tracks.push(format!(
            "{{\"node\":{},\"srt\":{},\"vtt\":{}}}",
            json_string(&track.node), json_string(&read(&track.srt)?), json_string(&read(&track.vtt)?)
        ));
    }
    Ok(format!("{{\"format\":{},\"version\":1,\"tracks\":[{}]}}", json_string(CAPTIONS_FORMAT), tracks.join(",")))
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The poster as a JPEG within the catalogue's size range (nearest-pixel
/// scaled when it is outside it), and its size.
pub fn thumbnail_jpeg(pixels: &[u32], width: u32, height: u32) -> Result<(Vec<u8>, u32, u32), String> {
    if width == 0 || height == 0 || pixels.len() != (width * height) as usize {
        return Err("empty poster".into());
    }
    let short = width.min(height) as f64;
    let long = width.max(height) as f64;
    let scale = if short < THUMB_MIN as f64 { THUMB_MIN as f64 / short } else if long > THUMB_MAX as f64 { THUMB_MAX as f64 / long } else { 1.0 };
    let (tw, th) = (((width as f64 * scale).round() as u32).clamp(THUMB_MIN, THUMB_MAX), ((height as f64 * scale).round() as u32).clamp(THUMB_MIN, THUMB_MAX));
    let mut bgra = Vec::with_capacity((tw * th * 4) as usize);
    for y in 0..th {
        let sy = ((y as u64 * height as u64) / th as u64) as u32;
        for x in 0..tw {
            let sx = ((x as u64 * width as u64) / tw as u64) as u32;
            bgra.extend_from_slice(&pixels[(sy * width + sx) as usize].to_le_bytes());
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 88)
        .encode(&bgra, tw as u16, th as u16, jpeg_encoder::ColorType::Bgra)
        .map_err(|e| format!("thumbnail: {e}"))?;
    Ok((out, tw, th))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_land_in_the_catalogue_range() {
        let small = vec![0xff20_4060u32; 64 * 36];
        let (jpeg, w, h) = thumbnail_jpeg(&small, 64, 36).unwrap();
        assert!(jpeg.starts_with(&[0xff, 0xd8]));
        assert_eq!((w, h), (455, 256));
        let tall = vec![0xff00_0000u32; 1080 * 1920];
        assert_eq!(thumbnail_jpeg(&tall, 1080, 1920).map(|(_, w, h)| (w, h)).unwrap(), (1080, 1920));
    }

    #[test]
    fn captions_travel_as_one_json_document() {
        let dir = std::env::temp_dir().join(format!("edit_render_captions_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (srt, vtt) = (dir.join("a.srt"), dir.join("a.vtt"));
        std::fs::write(&srt, "1\n00:00:00,300 --> 00:00:01,900\nSay \"hi\"\n").unwrap();
        std::fs::write(&vtt, "WEBVTT\n").unwrap();
        let json = captions_json(&[Captions { node: "subs".into(), srt: srt.clone(), vtt: vtt.clone() }]).unwrap();
        assert_eq!(
            json,
            "{\"format\":\"makepad-edit-captions\",\"version\":1,\"tracks\":[{\"node\":\"subs\",\"srt\":\"1\\n00:00:00,300 --> 00:00:01,900\\nSay \\\"hi\\\"\\n\",\"vtt\":\"WEBVTT\\n\"}]}"
        );
        let _ = std::fs::remove_file(&srt);
        let _ = std::fs::remove_file(&vtt);
        let _ = std::fs::remove_dir(&dir);
    }
}
