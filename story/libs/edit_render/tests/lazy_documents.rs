//! A cut's later documents load ahead of the playhead while its first
//! frames draw (engine.rs): a slow document holds back exactly the frames
//! that show it until it is in, and nothing before them.

use makepad_story_cut::{AssetRef, CutDoc, MediaRef};
use makepad_story_edit_render::engine::waiting_graphic;
use makepad_story_edit_render::graphic::{scan_all, scans, sources};
use makepad_story_edit_render::{MediaResolver, ResolvedMedia};
use std::collections::HashSet;
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

const TITLE: &str = "use mod.motion.*; Motion{size: vec2(64, 36) duration: 1.0 layers: [Text{text: \"Hi\" size: 10 at: vec2(32, 18)}]}";
const PICTURE: &str = "use mod.motion.*; Motion{size: vec2(64, 36) duration: 1.0 layers: [Image{src: asset(\"ast_picaaaaaaaaaaaaaaaaaaaaaaa\", \"arev_abababababababababababababababababababababababababababababababab\") size: vec2(10, 10)}]}";
const SLOW: Duration = Duration::from_millis(600);

/// The library: the picture the second document draws is slow to read.
struct Library;

impl MediaResolver for Library {
    fn resolve(&mut self, media: &MediaRef) -> Result<ResolvedMedia, String> {
        Err(format!("{media:?} is not a video"))
    }
    fn effect_source(&mut self, effect: &AssetRef) -> Result<String, String> {
        Err(effect.id.clone())
    }
    fn motion_source(&mut self, motion: &AssetRef) -> Result<String, String> {
        match motion.id.as_str() {
            "ast_titleaaaaaaaaaaaaaaaaaaaaa" => Ok(TITLE.into()),
            "ast_pictureaaaaaaaaaaaaaaaaaaa" => Ok(PICTURE.into()),
            other => Err(other.into()),
        }
    }
    fn asset_bytes(&mut self, asset: &AssetRef) -> Result<(Vec<u8>, &'static str), String> {
        assert_eq!(asset.id, "ast_picaaaaaaaaaaaaaaaaaaaaaaa");
        std::thread::sleep(SLOW);
        Ok((b"svg".to_vec(), "svg"))
    }
}

#[test]
fn a_slow_document_holds_back_its_first_frame_until_it_is_in() {
    let cut = "use mod.flow.*\nuse mod.cut.*\nlet a = Graphic{src: asset(\"ast_titleaaaaaaaaaaaaaaaaaaaaa\", \"arev_abababababababababababababababababababababababababababababababab\") span: [0.0, 2.0]}\nlet b = Graphic{src: asset(\"ast_pictureaaaaaaaaaaaaaaaaaaa\", \"arev_abababababababababababababababababababababababababababababababab\") span: [2.0, 2.0]}\nlet main = Stack{layers: [a.out(@frame), b.out(@frame)]}\nlet out = CutOutput{frame: main.out(@frame)}\nFlow{\n    kind: @cut\n    output: {width: 64 height: 36 fps: 30}\n    duration: 4.0\n    a, b, main, out\n}\n";
    let doc = CutDoc::load(cut).unwrap_or_else(|d| panic!("{d:#?}"));
    let resolver = Mutex::new(Box::new(Library) as Box<dyn MediaResolver>);
    let have = sources(&doc, &mut makepad_story_edit_render::graphic::Shared(&resolver)).expect("sources");
    let scans = scans(&doc);
    assert_eq!(scans.iter().map(|scan| (scan.nodes.clone(), scan.first_use)).collect::<Vec<_>>(), vec![(vec!["a".to_string()], 0.0), (vec!["b".to_string()], 2.0)]);
    // The first frame's document is in; `b` loads ahead.
    let mut pending: HashSet<String> = ["b".to_string()].into_iter().collect();
    let frame = 1.0 / 30.0;
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let tx = Mutex::new(tx);
            scan_all(&doc, &scans, &[1], &have, &resolver, &|index, used| tx.lock().unwrap().send((index, used)).unwrap());
        });
        // Frames before `b` draw at once; `b`'s first frame (and the one whose
        // shutter reaches it) wait.
        assert_eq!(waiting_graphic(&doc, &pending, 0.0, frame), None);
        assert_eq!(waiting_graphic(&doc, &pending, 1.9, frame), None);
        assert_eq!(waiting_graphic(&doc, &pending, 2.0 - frame, frame).as_deref(), Some("b"));
        assert_eq!(waiting_graphic(&doc, &pending, 2.0, frame).as_deref(), Some("b"));
        assert_eq!(waiting_graphic(&doc, &pending, 3.5, frame).as_deref(), Some("b"));
        // It still waits while its picture is being read.
        assert!(rx.recv_timeout(SLOW / 2).is_err());
        assert_eq!(waiting_graphic(&doc, &pending, 2.0, frame).as_deref(), Some("b"));
        let (index, used) = rx.recv_timeout(Duration::from_secs(30)).expect("b's document");
        assert!(started.elapsed() >= SLOW);
        assert_eq!(index, 1);
        assert!(used.svgs.contains_key(&AssetRef { id: "ast_picaaaaaaaaaaaaaaaaaaaaaaa".into(), rev: "arev_abababababababababababababababababababababababababababababababab".into() }), "what `b` draws came with it");
        for node in &scans[index].nodes {
            pending.remove(node);
        }
        assert_eq!(waiting_graphic(&doc, &pending, 2.0, frame), None);
    });
}
