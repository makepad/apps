//! A headless request's song (`song`, `lyrics`: absolute paths): the song
//! goes into the library as music, Stage's music bake times its words
//! (local whisper, nothing downloaded), and the brief gets the song's asset
//! and its timed lines. Runs on its own thread; the brief waits for it.

use makepad_asset_client::{PublishBundle, PublishBundleFile, PublishRights};
use makepad_asset_data::{AssetKind, FileRole};
use makepad_strict_json::Value;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

/// How long to wait for the music bake's lyrics before going on without
/// word times.
const LYRICS_WAIT: Duration = Duration::from_secs(600);
const POLL: Duration = Duration::from_secs(5);

pub(super) struct Song {
    pub asset: String,
    pub revision: String,
    pub seconds: f64,
    /// The bake's `vj-lyrics` JSON, when it timed the words.
    pub lyrics_json: Option<String>,
    /// What the model reads: the song, the lyric sheet and the timed lines.
    pub brief: String,
}

pub(super) fn start(song: PathBuf, lyrics: Option<PathBuf>) -> Receiver<Result<Song, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(prepare(&song, lyrics.as_deref()));
    });
    rx
}

fn prepare(song: &Path, lyrics: Option<&Path>) -> Result<Song, String> {
    let sheet = match lyrics {
        Some(path) => std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?,
        None => String::new(),
    };
    let bytes = std::fs::read(song).map_err(|error| format!("{}: {error}", song.display()))?;
    let (media, mono, rate) = makepad_product_tools::library::decode_song(song, &bytes)?;
    let seconds = mono.len() as f64 / rate.max(1.0);
    let title = song.file_stem().map(|stem| stem.to_string_lossy().to_string()).unwrap_or_else(|| "Song".into());
    let thumbnail = makepad_story_edit_voice::publish::waveform_thumbnail(&mono)?;
    // The person's own song: it stays on this LAN, derivatives included.
    let rights = PublishRights::declared(
        "LicenseRef-user-supplied",
        "",
        song.display().to_string(),
        makepad_asset_data::Redistribution::LanLocal,
        makepad_asset_data::DerivativePolicy::LocalPreview,
    );
    let mut bundle = PublishBundle::new("stage", AssetKind::Audio, title, vec![PublishBundleFile::bytes(FileRole::Audio, media, bytes, None)], thumbnail, rights);
    bundle.media_millis = ((seconds * 1000.0).round() as u32).max(1);
    // Tagged music, so the music bake times its words on its own.
    bundle.categories = vec!["music".into()];
    bundle.tags = vec!["music".into(), "song".into()];
    let mut client = makepad_asset_connection::connect(&makepad_asset_connection::origin())?;
    let published = client.publish_bundle(&bundle).map_err(|error| format!("publish the song: {error:?}"))?;
    let asset_id = published.asset_id;
    let mut revision = published.revision.to_string();
    let mut lyrics_json = None;
    let started = Instant::now();
    while started.elapsed() < LYRICS_WAIT {
        std::thread::sleep(POLL);
        let Ok(detail) = client.asset_detail(&asset_id) else { continue };
        let Some(head) = detail.latest_published().map(|candidate| candidate.revision) else { continue };
        let Ok(manifest) = client.fetch_asset_manifest(&head) else { continue };
        let Some(file) = manifest.files.iter().find(|file| file.role == FileRole::Lyrics) else { continue };
        let Ok(json) = client.fetch_blob_bytes(&file.blob, Some(file.byte_len)) else { continue };
        revision = head.to_string();
        lyrics_json = String::from_utf8(json).ok();
        break;
    }
    let timed = lyrics_json.as_deref().map(timed_lines).unwrap_or_default();
    let mut brief = format!(
        "\n\n# The song\nThe song is in the library: `asset(\"{asset_id}\", \"{revision}\")`, {seconds:.1} s. Put it on the timeline from 0 s as `let song = AudioClip{{src: asset(\"{asset_id}\", \"{revision}\") span: [0.0, {seconds:.1}]}}`; the video follows it.\n"
    );
    if !sheet.trim().is_empty() {
        brief.push_str(&format!("\n## The lyric sheet (the words to set)\n{}\n", sheet.trim()));
    }
    if timed.is_empty() {
        brief.push_str("\n## Word times\nNo word times: the music bake did not time this song's words. Place each line at its sung onset (`analyze_music` onsets) and its words at the sung rhythm, then check them against the audio.\n");
    } else {
        brief.push_str(
            "\n## Word times (the song's aligned vocal)\nThese are `voice(@song)`: a Graphic's `Text{source: voice(@song) line: i ghost: … highlight: @active}` sets aligned line `i` (counted from 0 in this list) word by word as it is sung (the guide's §4c and §4d). Each line: its start and end in seconds, then every word with its start. The words are transcribed from the audio, so some are misheard: where the lyric sheet differs, draw the sheet's words at these times.\n",
        );
        brief.push_str(&timed);
    }
    Ok(Song { asset: asset_id.to_string(), revision, seconds, lyrics_json, brief })
}

/// `vj-lyrics` lines as compact text: `12.40–15.10 "text" — word@12.40 …`.
fn timed_lines(json: &str) -> String {
    let Ok(v) = makepad_strict_json::parse_depth(json.as_bytes(), 8) else { return String::new() };
    let num = |v: Option<&Value>| match v {
        Some(Value::F64(f)) => *f,
        Some(Value::Int(i)) => *i as f64,
        _ => 0.0,
    };
    let mut out = String::new();
    for line in v.get("lines").and_then(Value::as_arr).unwrap_or(&[]) {
        let text = line.get("text").and_then(Value::as_str).unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        let words: Vec<&str> = text.split_whitespace().collect();
        let times: Vec<f64> = line.get("w").and_then(Value::as_arr).unwrap_or(&[]).iter().map(|t| num(Some(t))).collect();
        out.push_str(&format!("- {:.2}–{:.2} \"{text}\"", num(line.get("t0")), num(line.get("t1"))));
        if times.len() == words.len() {
            out.push_str(" —");
            for (word, at) in words.iter().zip(&times) {
                out.push_str(&format!(" {word}@{at:.2}"));
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn timed_lines_list_each_word_with_its_start() {
        let json = r#"{"format":"vj-lyrics","version":4,"lines":[{"t0":1.0,"t1":3.0,"text":"A red thread","w":[1.0,1.4,1.9],"c":true},{"t0":3.5,"t1":4.0,"text":"of sun","w":[3.5],"c":false}]}"#;
        let text = super::timed_lines(json);
        assert_eq!(text, "- 1.00–3.00 \"A red thread\" — A@1.00 red@1.40 thread@1.90\n- 3.50–4.00 \"of sun\"\n");
    }
}
