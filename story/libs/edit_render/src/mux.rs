//! Lossless MP4 concatenation for segmented exports: the video samples of
//! consecutive part files (each rendered on its own, starting with a key
//! frame) back to back in one track, and the sound of one audio file as
//! the second track. No sample is decoded or changed; timestamps carry on
//! from part to part, each sample keeps its composition offset, and the
//! parts' sample descriptions must be byte-identical (they are, from one
//! encoder configuration), else the join is refused.
//!
//! Scope: unfragmented files as the platform writer produces them (`stco`
//! or `co64`, `stsc`, `stts`, optional `ctts` and `stss`, an optional edit
//! list; read by `makepad_mp4_index`). Output: `ftyp`, `mdat` (64-bit size), `moov` last;
//! a QuickTime movie for a `.mov` out (HEVC with alpha), else MPEG-4.

use makepad_mp4_index::{locate_moov, tracks, Track, TrackSample};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Every track of the MP4 at `path` (its `moov`, indexed by `makepad_mp4_index`).
pub fn read_tracks(path: &Path) -> Result<Vec<Track>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let header = locate_moov(len, &mut |at, n| {
        let mut bytes = vec![0u8; n.min((len - at.min(len)) as usize)];
        file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        Ok(bytes)
    })
    .map_err(|e| format!("{}: {e}", path.display()))?;
    let end = header.size.unwrap_or(len - header.offset);
    let mut moov = vec![0u8; (end - header.header_len) as usize];
    file.seek(SeekFrom::Start(header.offset + header.header_len)).map_err(|e| e.to_string())?;
    file.read_exact(&mut moov).map_err(|e| e.to_string())?;
    tracks(&moov).map_err(|e| format!("{}: {e}", path.display()))
}

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(body.len() + 4);
    b.extend_from_slice(&((version as u32) << 24 | flags).to_be_bytes());
    b.extend_from_slice(body);
    boxed(kind, &b)
}

fn matrix() -> Vec<u8> {
    let mut m = Vec::new();
    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        m.extend_from_slice(&v.to_be_bytes());
    }
    m
}

/// Run-length (count, value) pairs.
fn runs<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

/// One output track: its table, and where its samples come from.
struct OutTrack<'a> {
    track: &'a Track,
    /// (source file index, sample) in decode order.
    samples: Vec<(usize, TrackSample)>,
    /// Output file offset of each sample (filled while writing `mdat`).
    offsets: Vec<u64>,
    id: u32,
    /// How long it shows, in movie units: the sum of each source's own
    /// presentation (its cut's duration), so a reordering delay or an
    /// encoder's priming is skipped once, at the start, and nothing at the
    /// end is cut.
    shown: u64,
    /// H.264 NAL length size (video): encoder user-data SEI is left out of
    /// the samples, so the join does not depend on when or how many parts
    /// were encoded. The written size of each sample.
    nal_length: Option<usize>,
    sizes: Vec<u32>,
    /// One source's samples, all of them, unchanged (the sound).
    whole: bool,
    /// Where the presentation starts, in media time (the edit's
    /// `media_time`): the latest of the parts' (see `concat`).
    start: Option<i64>,
}

/// The H.264 NAL length size in a `stsd` (its `avcC`), if it has one.
fn nal_length_of(stsd: &[u8]) -> Option<usize> {
    let at = stsd.windows(4).position(|w| w == b"avcC")?;
    Some((*stsd.get(at + 4 + 4)? & 3) as usize + 1)
}

/// `sample` (length-prefixed NAL units) without its user-data-unregistered
/// SEI units (NAL type 6 whose first payload is type 5): an encoder's
/// notes, not the picture.
fn strip_user_data(sample: &[u8], length: usize, out: &mut Vec<u8>) {
    out.clear();
    let mut at = 0;
    while at + length <= sample.len() {
        let size = sample[at..at + length].iter().fold(0usize, |n, b| n << 8 | *b as usize);
        let end = (at + length + size).min(sample.len());
        let nal = &sample[at + length..end];
        let user_data = nal.len() >= 2 && nal[0] & 0x1f == 6 && nal[1] == 5;
        if !user_data {
            out.extend_from_slice(&sample[at..end]);
        }
        at = end;
    }
    if at < sample.len() {
        out.extend_from_slice(&sample[at..]);
    }
}

/// A source track's presentation length in movie units: its last edit's
/// duration, or the media's whole length.
fn shown_of(track: &Track) -> u64 {
    match track.edits.last() {
        Some((duration, media_time)) if *media_time >= 0 => *duration,
        _ => track.samples.iter().map(|s| s.duration as u64).sum::<u64>() * track.movie_timescale as u64 / track.timescale.max(1) as u64,
    }
}

fn trak(out: &OutTrack, movie_timescale: u32) -> Vec<u8> {
    let t = out.track;
    let media_duration: u64 = out.samples.iter().map(|(_, s)| s.duration as u64).sum();
    let edits: Vec<(u64, i64)> = match (out.start, t.edits.as_slice()) {
        // One presentation from where the parts' samples were aligned to
        // start it (their encoder's reordering delay), as long as the
        // parts' presentations together.
        (Some(start), _) => vec![(out.shown, start)],
        // From where the source's edit starts it (an encoder's priming).
        (None, [.., (_, media_time)]) if *media_time >= 0 => vec![(out.shown, *media_time)],
        _ => Vec::new(),
    };
    let movie_duration = edits.first().map_or(media_duration * movie_timescale as u64 / t.timescale.max(1) as u64, |(d, _)| *d);
    let video = &t.handler == b"vide";
    let mut tkhd = Vec::new();
    tkhd.extend_from_slice(&0u64.to_be_bytes()); // creation, modification
    tkhd.extend_from_slice(&out.id.to_be_bytes());
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&(movie_duration.min(u32::MAX as u64) as u32).to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 8]);
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // layer
    tkhd.extend_from_slice(&(if video { 0u16 } else { 1 }).to_be_bytes()); // alternate group
    tkhd.extend_from_slice(&(if video { 0u16 } else { 0x0100 }).to_be_bytes()); // volume
    tkhd.extend_from_slice(&0u16.to_be_bytes());
    tkhd.extend_from_slice(&matrix());
    tkhd.extend_from_slice(&t.width.to_be_bytes());
    tkhd.extend_from_slice(&t.height.to_be_bytes());
    let mut body = full(b"tkhd", 0, 3, &tkhd);
    if !edits.is_empty() {
        let mut elst = (edits.len() as u32).to_be_bytes().to_vec();
        for (duration, media_time) in &edits {
            elst.extend_from_slice(&duration.to_be_bytes());
            elst.extend_from_slice(&media_time.to_be_bytes());
            elst.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        }
        body.extend(boxed(b"edts", &full(b"elst", 1, 0, &elst)));
    }
    let mut mdhd = Vec::new();
    mdhd.extend_from_slice(&[0u8; 16]);
    mdhd.extend_from_slice(&t.timescale.to_be_bytes());
    mdhd.extend_from_slice(&media_duration.to_be_bytes());
    mdhd.extend_from_slice(&0x55c4u16.to_be_bytes()); // "und"
    mdhd.extend_from_slice(&0u16.to_be_bytes());
    let mut hdlr = vec![0u8; 4];
    hdlr.extend_from_slice(&t.handler);
    hdlr.extend_from_slice(&[0u8; 12]);
    hdlr.extend_from_slice(if video { b"VideoHandler\0" } else { b"SoundHandler\0" });
    let header = if video { full(b"vmhd", 0, 1, &[0u8; 8]) } else { full(b"smhd", 0, 0, &[0u8; 4]) };
    let dref = full(b"dref", 0, 0, &[&1u32.to_be_bytes()[..], &full(b"url ", 0, 1, &[])].concat());
    let dinf = boxed(b"dinf", &dref);
    let samples = &out.samples;
    let mut stts = Vec::new();
    let stts_runs = runs(samples.iter().map(|(_, s)| s.duration));
    stts.extend_from_slice(&(stts_runs.len() as u32).to_be_bytes());
    for (n, d) in &stts_runs {
        stts.extend_from_slice(&n.to_be_bytes());
        stts.extend_from_slice(&d.to_be_bytes());
    }
    let mut stbl = t.stsd.clone();
    stbl.extend(full(b"stts", 0, 0, &stts));
    if samples.iter().any(|(_, s)| s.cts != 0) {
        let mut ctts = Vec::new();
        let ctts_runs = runs(samples.iter().map(|(_, s)| s.cts));
        ctts.extend_from_slice(&(ctts_runs.len() as u32).to_be_bytes());
        for (n, c) in &ctts_runs {
            ctts.extend_from_slice(&n.to_be_bytes());
            ctts.extend_from_slice(&c.to_be_bytes());
        }
        let version = if samples.iter().any(|(_, s)| s.cts < 0) { 1 } else { 0 };
        stbl.extend(full(b"ctts", version, 0, &ctts));
    }
    if samples.iter().any(|(_, s)| !s.sync) {
        let sync: Vec<u32> = samples.iter().enumerate().filter(|(_, (_, s))| s.sync).map(|(i, _)| i as u32 + 1).collect();
        let mut stss = (sync.len() as u32).to_be_bytes().to_vec();
        for n in sync {
            stss.extend_from_slice(&n.to_be_bytes());
        }
        stbl.extend(full(b"stss", 0, 0, &stss));
    }
    // One sample per chunk: every sample's offset stands on its own.
    stbl.extend(full(b"stsc", 0, 0, &[&1u32.to_be_bytes()[..], &1u32.to_be_bytes(), &1u32.to_be_bytes(), &1u32.to_be_bytes()].concat()));
    let mut stsz = 0u32.to_be_bytes().to_vec();
    stsz.extend_from_slice(&(out.sizes.len() as u32).to_be_bytes());
    for size in &out.sizes {
        stsz.extend_from_slice(&size.to_be_bytes());
    }
    stbl.extend(full(b"stsz", 0, 0, &stsz));
    if out.whole {
        // Its samples as they were: their groups still apply.
        stbl.extend_from_slice(&t.groups);
    }
    let mut co64 = (out.offsets.len() as u32).to_be_bytes().to_vec();
    for offset in &out.offsets {
        co64.extend_from_slice(&offset.to_be_bytes());
    }
    stbl.extend(full(b"co64", 0, 0, &co64));
    let minf = boxed(b"minf", &[header, dinf, boxed(b"stbl", &stbl)].concat());
    let mdia = boxed(b"mdia", &[full(b"mdhd", 1, 0, &mdhd), full(b"hdlr", 0, 0, &hdlr), minf].concat());
    body.extend(mdia);
    boxed(b"trak", &body)
}

/// Join the video tracks of `parts` (in order) and the sound track of
/// `audio` (if any) into `out`. The parts' video sample descriptions must be
/// identical. `cancel` is polled between samples.
pub fn concat(parts: &[&Path], audio: Option<&Path>, out: &Path, cancel: &dyn Fn() -> bool) -> Result<(), String> {
    let mut files: Vec<std::fs::File> = Vec::new();
    let mut sources: Vec<((), Vec<Track>)> = Vec::new();
    for path in parts.iter().copied().chain(audio) {
        files.push(std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?);
        sources.push(((), read_tracks(path)?));
    }
    let video_of = |tracks: &Vec<Track>| tracks.iter().position(|t| &t.handler == b"vide");
    let first = video_of(&sources.first().ok_or("nothing to join")?.1).ok_or("the first part has no video")?;
    let mut video = OutTrack { track: &sources[0].1[first], samples: Vec::new(), offsets: Vec::new(), id: 1, shown: 0, nal_length: None, sizes: Vec::new(), whole: false, start: None };
    video.nal_length = nal_length_of(&video.track.stsd);
    // Each part's presentation starts at its own edit's media time (its
    // encoder's reordering delay, which differs with a part's length and
    // content): its samples' composition offsets move by the difference to
    // the latest of them, which starts the joined presentation, so every
    // part shows its first frame right after the previous one's last.
    let start_of = |track: &Track| track.edits.iter().rev().find(|(_, media_time)| *media_time >= 0).map_or(0, |(_, media_time)| *media_time);
    let latest = sources.iter().take(parts.len()).filter_map(|(_, tracks)| video_of(tracks).map(|at| start_of(&tracks[at]))).max().unwrap_or(0);
    video.start = Some(latest);
    for (index, (_, tracks)) in sources.iter().enumerate().take(parts.len()) {
        let track = &tracks[video_of(tracks).ok_or_else(|| format!("{}: no video", parts[index].display()))?];
        if track.stsd != video.track.stsd || track.timescale != video.track.timescale {
            return Err(format!("{}: its picture is described differently from the first part's; parts must come from one encoder setup", parts[index].display()));
        }
        if track.samples.first().is_some_and(|s| !s.sync) {
            return Err(format!("{}: does not start with a key frame", parts[index].display()));
        }
        let shift = latest - start_of(track);
        video.samples.extend(track.samples.iter().map(|s| (index, TrackSample { cts: (s.cts as i64 + shift) as i32, ..*s })));
        video.shown += shown_of(track);
    }
    let mut sound = audio.and_then(|_| {
        let index = parts.len();
        let track = sources[index].1.iter().find(|t| &t.handler == b"soun")?;
        Some(OutTrack { track, samples: track.samples.iter().map(|s| (index, *s)).collect(), offsets: Vec::new(), id: 2, shown: 0, nal_length: None, sizes: Vec::new(), whole: true, start: None })
    });
    // The movie's clock: the sound's movie timescale when there is sound
    // (as the platform writer makes it; a coarser one lets a decoder end
    // the sound a packet early), else the video's. Every presentation
    // length in it.
    let movie_timescale = sound.as_ref().map_or(video.track.movie_timescale, |sound| sound.track.movie_timescale).max(1);
    video.shown = video.shown * movie_timescale as u64 / video.track.movie_timescale.max(1) as u64;
    if let Some(sound) = &mut sound {
        sound.shown = shown_of(sound.track) * movie_timescale as u64 / sound.track.movie_timescale.max(1) as u64;
    }
    // A `.mov` out (an alpha export's, HEVC with alpha) is a QuickTime
    // movie, the container its parts are; else MPEG-4.
    let mov = out.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("mov"));
    let partial = out.with_extension(if mov { "joining.mov" } else { "joining.mp4" });
    let mut file = std::io::BufWriter::new(std::fs::File::create(&partial).map_err(|e| format!("{}: {e}", partial.display()))?);
    let ftyp = if mov {
        boxed(b"ftyp", &[&b"qt  "[..], &0u32.to_be_bytes(), b"qt  "].concat())
    } else {
        boxed(b"ftyp", &[&b"isom"[..], &0x200u32.to_be_bytes(), b"isomiso2avc1mp41"].concat())
    };
    file.write_all(&ftyp).map_err(|e| e.to_string())?;
    // A 64-bit `mdat` header (16 bytes); its size is written once the
    // samples are.
    let mdat_at = ftyp.len() as u64;
    file.write_all(&1u32.to_be_bytes()).map_err(|e| e.to_string())?;
    file.write_all(b"mdat").map_err(|e| e.to_string())?;
    file.write_all(&0u64.to_be_bytes()).map_err(|e| e.to_string())?;
    let mut at = ftyp.len() as u64 + 16;
    let mut buffer = Vec::new();
    let mut stripped = Vec::new();
    let mut copy = |track: &mut OutTrack, files: &mut Vec<std::fs::File>, file: &mut std::io::BufWriter<std::fs::File>, at: &mut u64, buffer: &mut Vec<u8>| -> Result<(), String> {
        for (source, sample) in &track.samples {
            if cancel() {
                return Err("cancelled".into());
            }
            let reader = &mut files[*source];
            buffer.resize(sample.size as usize, 0);
            reader.seek(SeekFrom::Start(sample.offset)).map_err(|e| e.to_string())?;
            reader.read_exact(buffer).map_err(|e| e.to_string())?;
            let bytes: &[u8] = match track.nal_length {
                Some(length) => {
                    strip_user_data(buffer, length, &mut stripped);
                    &stripped
                }
                None => buffer,
            };
            file.write_all(bytes).map_err(|e| e.to_string())?;
            track.offsets.push(*at);
            track.sizes.push(bytes.len() as u32);
            *at += bytes.len() as u64;
        }
        Ok(())
    };
    let result = (|| {
        copy(&mut video, &mut files, &mut file, &mut at, &mut buffer)?;
        if let Some(sound) = &mut sound {
            copy(sound, &mut files, &mut file, &mut at, &mut buffer)?;
        }
        let mdat_end = at;
        let mut traks = trak(&video, movie_timescale);
        if let Some(sound) = &sound {
            traks.extend(trak(sound, movie_timescale));
        }
        let duration = [Some(&video), sound.as_ref()].into_iter().flatten().map(|t| t.shown).max().unwrap_or(0);
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0u8; 16]);
        mvhd.extend_from_slice(&movie_timescale.to_be_bytes());
        mvhd.extend_from_slice(&duration.to_be_bytes());
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // volume
        mvhd.extend_from_slice(&[0u8; 10]);
        mvhd.extend_from_slice(&matrix());
        mvhd.extend_from_slice(&[0u8; 24]);
        mvhd.extend_from_slice(&(if sound.is_some() { 3u32 } else { 2 }).to_be_bytes());
        let moov = boxed(b"moov", &[full(b"mvhd", 1, 0, &mvhd), traks].concat());
        file.write_all(&moov).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(mdat_at + 8)).map_err(|e| e.to_string())?;
        file.write_all(&(mdat_end - mdat_at).to_be_bytes()).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())
    })();
    drop(file);
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(out);
            std::fs::rename(&partial, out).map_err(|e| format!("{}: {e}", out.display()))
        }
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::runs;

    #[test]
    fn runs_count_repeats() {
        assert_eq!(runs([5, 5, 5, 3, 5].into_iter()), vec![(3, 5), (1, 3), (1, 5)]);
        assert!(runs(std::iter::empty::<u32>()).is_empty());
    }
}

#[cfg(test)]
mod strip_tests {
    use super::strip_user_data;

    #[test]
    fn user_data_sei_goes_and_everything_else_stays() {
        let nal = |bytes: &[u8]| [&(bytes.len() as u32).to_be_bytes()[..], bytes].concat();
        let sample = [nal(&[0x06, 0x05, 1, 2, 3]), nal(&[0x06, 0x06, 9]), nal(&[0x65, 0xaa, 0xbb])].concat();
        let mut out = Vec::new();
        strip_user_data(&sample, 4, &mut out);
        assert_eq!(out, [nal(&[0x06, 0x06, 9]), nal(&[0x65, 0xaa, 0xbb])].concat());
    }
}
