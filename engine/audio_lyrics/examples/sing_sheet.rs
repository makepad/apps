//! Time a known lyric sheet in a song and print each line with how it was
//! timed and how far it sits from the heard words:
//! `cargo run --release -p makepad-audio-lyrics --example sing_sheet -- <model.bin> <song> <sheet.txt> [--check]`.
//!
//! `--listen <from> <to>` prints what whisper hears in that span alone, with
//! its word times (a second opinion on one line's place).
//!
//! `--check` spot-checks every line against the audio: the line's own span
//! is cut out and transcribed alone, and the words heard there are compared
//! with the sheet's (word error rate per line); a line timed in the wrong
//! place hears other words or none.

use makepad_audio_lyrics::sung::{fold, match_words, Sheet};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (model_path, song, sheet_path) = (&args[1], &args[2], &args[3]);
    let check = args.iter().any(|arg| arg == "--check");
    let bytes = std::fs::read(song).expect("song");
    let audio = makepad_audio_decode::decode_any(&bytes).expect("decodes");
    let left = audio.channel(0);
    let mono: Vec<f32> = if audio.channels > 1 { left.iter().zip(&audio.channel(1)).map(|(l, r)| (l + r) * 0.5).collect() } else { left };
    let rate = audio.rate as f64;
    let duration = mono.len() as f64 / rate;
    let sheet = Sheet::parse(&std::fs::read_to_string(sheet_path).expect("sheet"));
    if let Some(at) = args.iter().position(|arg| arg == "--listen") {
        let (from, to): (f64, f64) = (args[at + 1].parse().expect("from"), args[at + 2].parse().expect("to"));
        let model = makepad_ai_speech::whisper::WhisperModel::load_file(model_path).expect("model");
        let mut state = makepad_ai_speech::whisper::WhisperState::new(&model);
        let samples_16k = makepad_audio_lyrics::align::resample(&mono, rate, 16_000.0);
        let (a, b) = ((from * 16_000.0) as usize, ((to * 16_000.0) as usize).min(samples_16k.len()));
        let mut params = makepad_ai_speech::whisper::WhisperParams::default();
        params.language = "en".into();
        params.temperature = 0.0;
        params.no_timestamps = false;
        for segment in state.transcribe_aligned(&model, &samples_16k[a..b], &params) {
            let words: Vec<String> = segment.words.iter().map(|w| format!("{}@{:.2}", w.text.trim(), from + w.start_ms as f64 / 1000.0)).collect();
            println!("{}", words.join(" "));
        }
        return;
    }
    let started = std::time::Instant::now();
    let mut baker = makepad_audio_lyrics::bake::LyricsBaker::open(std::path::Path::new(model_path)).expect("model");
    let sung = baker.align_sheet(&mono, rate, duration, &sheet, "en");
    eprintln!("timed {:.1} s of song in {:.1} s: {} of {} sheet words matched to {} heard", duration, started.elapsed().as_secs_f64(), sung.matched_words, sung.sheet_words, sung.heard_words);
    eprintln!("heard: {}", sung.heard_text);
    let mut whisper = check.then(|| {
        let model = makepad_ai_speech::whisper::WhisperModel::load_file(model_path).expect("model");
        let state = makepad_ai_speech::whisper::WhisperState::new(&model);
        (model, state)
    });
    let samples_16k = makepad_audio_lyrics::align::resample(&mono, rate, 16_000.0);
    let (mut errors, mut total) = (0usize, 0usize);
    for line in &sung.lines {
        let agreement = line.agreement.map_or("   -  ".to_string(), |a| format!("{:5.0}ms", a * 1000.0));
        let mut verdict = String::new();
        if let Some((model, state)) = whisper.as_mut() {
            let a = ((line.start() - 0.15).max(0.0) * 16_000.0) as usize;
            let b = (((line.end() + 0.15) * 16_000.0) as usize).min(samples_16k.len());
            let mut params = makepad_ai_speech::whisper::WhisperParams::default();
            params.language = "en".into();
            params.temperature = 0.0;
            params.single_segment = true;
            let segments = state.transcribe(model, &samples_16k[a..b], &params);
            let heard: Vec<String> = segments.iter().flat_map(|s| s.text.split_whitespace().map(fold).collect::<Vec<_>>()).filter(|w| !w.is_empty()).collect();
            let words: Vec<String> = line.words.iter().map(|w| fold(&w.w)).collect();
            let map = match_words(&words, &heard);
            let exact = map.iter().enumerate().filter(|(i, m)| m.is_some_and(|j| heard[j] == words[*i])).count();
            let wer = (words.len() - exact) as f64 / words.len().max(1) as f64;
            errors += words.len() - exact;
            total += words.len();
            verdict = format!(" wer {:4.0}% [{}]", wer * 100.0, heard.join(" "));
        }
        let how = if line.forced { "forced" } else { "heard " };
        println!("{:7.2}-{:6.2} {how} match {:2}/{:2} agree {agreement}{verdict}  | {}", line.start(), line.end(), line.matched, line.words.len(), line.text());
    }
    for (block, text) in &sung.unsung {
        println!("  unsung (block {block}): {text}");
    }
    if total > 0 {
        println!("spot check: {} of {} words heard again in their line's span ({:.1}% word error)", total - errors, total, 100.0 * errors as f64 / total as f64);
    }
}
