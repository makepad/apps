//! Publishing a run (EDITS.md §2, §6.5): the script as an `AppRun` asset
//! (`run.splash`, Source/Text), then the MP4 and run.json as its render
//! (`publish_render`: a `Video` whose one dependency is that exact script
//! revision, under `<namespace>/render/<asset id>`). And the freshness
//! check the UI shows: `stale` never re-runs anything.

use crate::doc::RunOutput;
use crate::exec::RunResult;
use crate::identity::{cache_key, locate, BuildIdentity};
use makepad_asset_client::{AssetClient, PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetKind, AssetRevisionRef, FileRole, MediaType, ThumbnailMedia};
use makepad_strict_json::{self as json, Value};

/// What a publish made.
#[derive(Clone, Debug, PartialEq)]
pub struct PublishedRun {
    /// The `AppRun` script revision.
    pub run: AssetRevisionRef,
    /// The render (`Video`) revision.
    pub render: AssetRevisionRef,
    /// The render alias, when it moved (a render of an older revision
    /// that finished late leaves it alone).
    pub alias: Option<String>,
}

/// Width and height of a PNG from its IHDR.
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || !png.starts_with(b"\x89PNG\r\n\x1a\n") || &png[12..16] != b"IHDR" {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]);
    Some((be(16), be(20)))
}

/// Why a publish stopped, and the script revision if that part was done:
/// retry with it as `run` to publish only the render.
#[derive(Clone, Debug, PartialEq)]
pub struct PublishFailure {
    pub run: Option<AssetRevisionRef>,
    pub message: String,
}

impl std::fmt::Display for PublishFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn refuse(message: impl Into<String>) -> PublishFailure {
    PublishFailure { run: None, message: message.into() }
}

/// Publish a finished run. `run` is the `AppRun` asset's current head
/// (None: a new asset): when its script text is `source` it is reused as
/// it is, otherwise the script is published as a new revision of that
/// asset. The run's run.json is rewritten with the script revision and its
/// final cache key before it is uploaded with the MP4 as the render.
/// Refuses a failed or validation run, one that did not build the app
/// (`--no-build` is for drafts), and one without a thumbnail
/// (`RunOptions::thumbnail`).
pub fn publish_run(
    client: &mut AssetClient,
    namespace: &str,
    run: Option<AssetRevisionRef>,
    source: &str,
    label: &str,
    result: &mut RunResult,
) -> Result<PublishedRun, PublishFailure> {
    if !result.ok() {
        return Err(refuse(format!("a failed run is not published: {}", result.failures.join("; "))));
    }
    if result.record.get("validate").and_then(Value::as_bool) == Some(true) {
        return Err(refuse("a validation run is not published"));
    }
    if !result.build.built {
        return Err(refuse("a run that did not build the app is not published: its binary may predate the sources it names"));
    }
    let png = result.thumbnail.clone().ok_or_else(|| refuse("the run has no thumbnail (run with RunOptions::thumbnail)"))?;
    let (w, h) = png_size(&png).ok_or_else(|| refuse("the thumbnail is not a PNG"))?;
    let thumbnail = || PublishThumbnail::plain(png.clone(), ThumbnailMedia::Png, w, h);

    let unchanged = match run {
        Some(head) => script_text(client, &head).map_err(refuse)?.as_deref() == Some(source.as_bytes()),
        None => false,
    };
    let run = if unchanged {
        run.unwrap()
    } else {
        let mut script = PublishBundle::new(
            namespace,
            AssetKind::AppRun,
            label,
            vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, source.as_bytes().to_vec(), None)],
            thumbnail(),
            PublishRights::generated_cc0(),
        );
        script.asset_id = run.map(|r| r.asset_id);
        script.generator = "makepad-app-run".into();
        let published = client.publish_bundle(&script).map_err(|e| refuse(format!("publish the app run: {e:?}")))?;
        AssetRevisionRef { asset_id: published.asset_id, revision: published.revision }
    };
    let failed = |message: String| PublishFailure { run: Some(run), message };

    // The cache key names the script revision the render was made from.
    let revision = run.revision.to_string();
    let key = cache_key(&revision, &result.build, &result.output);
    set_field(&mut result.record, "run_revision", json::s(&revision));
    set_field(&mut result.record, "run_asset", json::s(run.asset_id.to_string()));
    set_field(&mut result.record, "cache_key", json::s(&key));
    let run_json = result.record.to_json();
    std::fs::write(&result.run_json, &run_json).map_err(|e| failed(format!("{}: {e}", result.run_json.display())))?;

    let mp4 = std::fs::read(&result.mp4).map_err(|e| failed(format!("{}: {e}", result.mp4.display())))?;
    let mut render = PublishBundle::new(
        namespace,
        AssetKind::Video,
        format!("{label} (app run)"),
        vec![
            PublishBundleFile::bytes(FileRole::Video, MediaType::Mp4, mp4, None),
            PublishBundleFile::bytes(FileRole::Source, MediaType::Json, run_json.into_bytes(), None),
        ],
        thumbnail(),
        PublishRights::generated_cc0(),
    );
    render.media_millis = (result.frames * 1000 / result.output.fps.max(1) as u64).min(u32::MAX as u64) as u32;
    render.generator = "makepad-app-run".into();
    render.provenance = format!("app run {} of {}", run, result.build.package);
    let done = client.publish_render(run, render).map_err(|e| failed(format!("publish the render: {e:?}")))?;
    Ok(PublishedRun {
        run,
        render: AssetRevisionRef { asset_id: done.asset_id, revision: done.revision },
        alias: done.alias.map(|a| a.as_str().to_string()),
    })
}

/// The `Source`/`Text` bytes of an `AppRun` revision.
fn script_text(client: &mut AssetClient, run: &AssetRevisionRef) -> Result<Option<Vec<u8>>, String> {
    let manifest = client.fetch_asset_manifest(&run.revision).map_err(|e| format!("read the app run {run}: {e:?}"))?;
    if manifest.kind != AssetKind::AppRun || manifest.asset_id != run.asset_id {
        return Err(format!("{run} is not an app run revision of that asset"));
    }
    match manifest.files.iter().find(|f| f.role == FileRole::Source && f.media == MediaType::Text) {
        Some(file) => client.fetch_blob_bytes(&file.blob, None).map(Some).map_err(|e| format!("read the app run script: {e:?}")),
        None => Ok(None),
    }
}

fn set_field(record: &mut Value, key: &str, value: Value) {
    if let Value::Obj(fields) = record {
        match fields.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value,
            None => fields.push((key.to_string(), value)),
        }
    }
}

/// The cache key a new run of the same script (revision `run_head`) would
/// use now, from a run.json: the app's binary on disk and its sources'
/// commits and uncommitted changes, the recorded output, the current
/// Makepad commit. An app not built yet never matches. Reads git and hashes
/// the binary and the trees' changes: call it off the UI thread.
pub fn current_key(record: &Value, run_head: &str) -> Result<String, String> {
    let app = record.get("app").and_then(|a| a.get("id")).and_then(Value::as_str).ok_or("run.json has no app id")?;
    let output = record.get("output").and_then(output_from_json).ok_or("run.json has no output")?;
    let (entry, root, binary) = locate(app)?;
    let identity = if binary.is_file() {
        BuildIdentity::current(entry, &root, &binary, true)?
    } else {
        BuildIdentity::with_binary(entry, &root, "unbuilt".into(), false)?
    };
    Ok(cache_key(run_head, &identity, &output))
}

/// Whether a run's render is stale: its recorded cache key is not the one
/// a new run of revision `run_head` would use (§6.5). Pure; see
/// `current_key` for the other half.
pub fn is_stale(record: &Value, current_key: &str) -> bool {
    record.get("cache_key").and_then(Value::as_str) != Some(current_key)
}

/// `is_stale(record, current_key(record, run_head))`. Never re-runs.
pub fn stale(record: &Value, run_head: &str) -> Result<bool, String> {
    Ok(is_stale(record, &current_key(record, run_head)?))
}

pub fn output_from_json(v: &Value) -> Option<RunOutput> {
    let n = |k: &str| v.get(k).and_then(crate::remote::num);
    Some(RunOutput { width: n("width")? as u32, height: n("height")? as u32, dpi_scale: n("dpi_scale")?, fps: n("fps")? as u32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_png_header_gives_its_size_and_staleness_is_a_key_compare() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&1080u32.to_be_bytes());
        png.extend_from_slice(&1920u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((1080, 1920)));
        assert_eq!(png_size(b"nope"), None);
        let record = json::obj(vec![("cache_key", json::s("abc"))]);
        assert!(!is_stale(&record, "abc"));
        assert!(is_stale(&record, "abd"));
        let out = json::obj(vec![("width", Value::Int(8)), ("height", Value::Int(4)), ("dpi_scale", Value::F64(2.0)), ("fps", Value::Int(30))]);
        assert_eq!(output_from_json(&out), Some(RunOutput { width: 8, height: 4, dpi_scale: 2.0, fps: 30 }));
    }
}
