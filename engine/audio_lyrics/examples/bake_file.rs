//! Bake one audio file's lyrics and print them with their syllables:
//! `cargo run --release -p makepad-audio-lyrics --example bake_file -- <model.bin> <song> [out.json]`.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (model, song) = (&args[1], &args[2]);
    let bytes = std::fs::read(song).expect("song");
    let audio = makepad_audio_decode::decode_any(&bytes).expect("decodes");
    let left = audio.channel(0);
    let mono: Vec<f32> = if audio.channels > 1 { left.iter().zip(&audio.channel(1)).map(|(l, r)| (l + r) * 0.5).collect() } else { left };
    let rate = audio.rate as f64;
    let duration = mono.len() as f64 / rate;
    let started = std::time::Instant::now();
    let mut baker = makepad_audio_lyrics::bake::LyricsBaker::open(std::path::Path::new(model)).expect("model");
    let lyrics = baker.bake(&mono, rate, duration, "en").expect("sung words");
    eprintln!("baked {:.1} s of song in {:.1} s", duration, started.elapsed().as_secs_f64());
    for line in &lyrics.lines {
        let words: Vec<String> = line.text.split_whitespace().zip(line.syllables.iter().chain(std::iter::repeat(&Vec::new()))).map(|(w, s)| {
            if s.len() > 1 { format!("{w}{:?}", s.iter().map(|t| (t * 100.0).round() / 100.0).collect::<Vec<_>>()) } else { w.to_string() }
        }).collect();
        println!("{:7.2} {}", line.start_secs, words.join(" "));
    }
    if let Some(out) = args.get(3) {
        std::fs::write(out, lyrics.to_json("local")).expect("write");
    }
}
