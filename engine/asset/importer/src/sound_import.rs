//! Sound-library import: a curated folder of one-shots and loops →
//! `AssetKind::Audio` assets a game plays by alias (`game.sfx_sample`,
//! `game.sfx`, `game.ambience`).
//!
//! The folder carries its own index, `sounds.tsv` (header line, then one row
//! per take): `alias  file  title  tags` (tab-separated; further columns are
//! ignored). The alias IS the name a game writes (`sfx/footstep/stone-1`),
//! its first segment is the namespace, and the tags (comma-separated: the
//! category, the games that use it) are catalog labels, so the library is
//! searchable by `game.find_model(q, {kind: "sound"})`.
//!
//! Idempotent like the music import: a take whose head revision already holds
//! the same audio bytes and picture is skipped; changed bytes publish a new
//! revision of the SAME asset id, so every game that names the alias gets the
//! new take without an edit.

use crate::music_import::{measure, track_picture, Container, TrackOutcome};
use crate::thumbs::encode_jpeg_bgra;
use makepad_asset_client::util::sanitize_text;
use makepad_asset_client::{AssetClient, ClientError, PublishFile, PublishRequest, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetAlias, AssetKind, BlobId, FileRole, ThumbnailMedia};
use std::path::Path;
use std::str::FromStr;

/// Catalog category every library take carries.
pub const SOUND_CATEGORY: &str = "sound";
const MAX_TITLE: usize = 200;

/// One row of `sounds.tsv`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundRow {
    pub alias: String,
    pub file: String,
    pub title: String,
    pub tags: Vec<String>,
}

/// Parse the index. A malformed row is an error naming its line: a library
/// that publishes half its takes would leave games naming missing sounds.
pub fn parse_index(text: &str) -> Result<Vec<SoundRow>, String> {
    let mut rows = Vec::new();
    for (n, line) in text.lines().enumerate().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 4 {
            return Err(format!("sounds.tsv line {}: want alias, file, title, tags", n + 1));
        }
        let alias = cols[0].trim().to_string();
        if alias.split('/').count() < 2 {
            return Err(format!("sounds.tsv line {}: alias '{alias}' has no namespace", n + 1));
        }
        let tags = cols[3]
            .split(',')
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .take(makepad_asset_data::limits::MAX_LABELS)
            .collect();
        rows.push(SoundRow { alias, file: cols[1].trim().to_string(), title: cols[2].trim().to_string(), tags });
    }
    Ok(rows)
}

/// Rights for the generated library: made on our own fleet from text
/// prompts, so free to use, share and change. A caller who knows better
/// terms passes them in.
pub fn generated_rights() -> PublishRights {
    PublishRights::declared(
        "Generated (sa3-sfx, Stability AI Community License)",
        "",
        "Sandbox sound library, generated on the Makepad AI hub",
        makepad_asset_data::Redistribution::Allowed,
        makepad_asset_data::DerivativePolicy::Allowed,
    )
}

#[derive(Clone, Debug, Default)]
pub struct SoundReport {
    pub published: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// Publish every take the index lists.
pub fn import_sounds(client: &mut AssetClient, dir: &Path, rights: &PublishRights, log: bool) -> Result<SoundReport, String> {
    let index = std::fs::read_to_string(dir.join("sounds.tsv")).map_err(|e| format!("{}: {e}", dir.join("sounds.tsv").display()))?;
    let rows = parse_index(&index)?;
    let mut report = SoundReport::default();
    for row in &rows {
        match publish_row(client, dir, row, rights) {
            Ok(outcome) => {
                if log {
                    eprintln!("[sound-import] {:?} {}", outcome, row.alias);
                }
                match outcome {
                    TrackOutcome::Published => report.published.push(row.alias.clone()),
                    TrackOutcome::Updated => report.updated.push(row.alias.clone()),
                    TrackOutcome::Unchanged => report.unchanged.push(row.alias.clone()),
                }
            }
            Err(error) => {
                if log {
                    eprintln!("[sound-import] FAILED {}: {error}", row.alias);
                }
                report.failed.push((row.alias.clone(), error));
            }
        }
    }
    Ok(report)
}

fn publish_row(client: &mut AssetClient, dir: &Path, row: &SoundRow, rights: &PublishRights) -> Result<TrackOutcome, String> {
    let bytes = std::fs::read(dir.join(&row.file)).map_err(|e| format!("read {}: {e}", row.file))?;
    let container = Container::from_name_mime(&row.file, "").ok_or_else(|| format!("{}: not wav/ogg/mp3", row.file))?;
    let measured = measure(&bytes, container).ok_or_else(|| "unmeasurable duration".to_string())?;
    let picture = track_picture(&bytes, container, &measured, &row.alias);
    let thumbnail_bytes = encode_jpeg_bgra(&picture.pixels, picture.width, picture.height)?;
    let alias = AssetAlias::from_str(&row.alias).map_err(|e| format!("alias {}: {e}", row.alias))?;
    let audio_blob = BlobId::hash_of(&bytes);
    let thumbnail_blob = BlobId::hash_of(&thumbnail_bytes);
    let existing = match client.resolve_alias(&alias) {
        Ok(head) => {
            let manifest = crate::readable_head(client, &head.head_revision).map_err(|e| format!("head manifest: {e}"))?;
            let unchanged = manifest.is_some_and(|m| {
                m.files.iter().any(|f| f.role == FileRole::Audio && f.blob == audio_blob)
                    && m.thumbnail.is_some_and(|t| t.blob == thumbnail_blob)
            });
            if unchanged {
                return Ok(TrackOutcome::Unchanged);
            }
            Some(head.asset_id)
        }
        Err(ClientError::NotFound { .. }) => None,
        Err(error) => return Err(format!("alias probe: {error}")),
    };
    let namespace = row.alias.split('/').next().unwrap_or("sfx");
    let mut request = PublishRequest::new(
        namespace,
        AssetKind::Audio,
        sanitize_text(&row.title, MAX_TITLE),
        PublishFile { bytes, media: container.media(), role: FileRole::Audio, media_millis: measured.millis, dims: None },
        PublishThumbnail {
            bytes: thumbnail_bytes,
            media: ThumbnailMedia::Jpeg,
            width: picture.width as u32,
            height: picture.height as u32,
            views: picture.views,
        },
    );
    request.alias = Some(alias);
    request.asset_id = existing;
    request.categories = vec![SOUND_CATEGORY.into()];
    request.tags = row.tags.clone();
    request.description = sanitize_text(&format!("{} — sound library take ({})", row.title, row.tags.join(", ")), 400);
    request.generator = "sound_import".into();
    request.backend = "asset-importer".into();
    request.model = "sa3-sfx".into();
    request.provenance = sanitize_text(&format!("sound-import {}", row.file), 400);
    request.rights = rights.clone();
    client.publish_artifact(&request).map_err(|e| format!("publish: {e}"))?;
    Ok(if existing.is_some() { TrackOutcome::Updated } else { TrackOutcome::Published })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_index_reads_rows_and_refuses_bad_ones() {
        let text = "alias\tfile\ttitle\ttags\tseconds\n\
                    sfx/footstep/stone-1\tfootstep/stone-1.wav\tFootstep on stone 1\tsfx,footstep,castle\t0.31\n\n";
        let rows = parse_index(text).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].alias, "sfx/footstep/stone-1");
        assert_eq!(rows[0].tags, vec!["sfx", "footstep", "castle"]);
        assert!(parse_index("h\nsfx/x\tf.wav\n").is_err(), "a short row names its line");
        assert!(parse_index("h\nnonamespace\tf.wav\tt\ttag\n").is_err());
    }
}
