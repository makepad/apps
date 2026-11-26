use super::lipsync::{self, Viseme};
use super::pose::{self, Joint};
use super::*;
use crate::features::{AudioFeatures, FeatureCurve, FeatureWord, VoiceFeatures};
use crate::model::{MotionContext, MotionDoc};

fn layer(walks: Vec<Walk>) -> CharacterLayer {
    CharacterLayer {
        model: Model::Rig(Arc::new(kit::character("spark").unwrap())),
        height: 600.0,
        facing: None,
        poses: vec![Cue { t: Time::seconds(0.0), name: "idle".into(), blend: 0.0 }],
        pose_blend: None,
        expressions: Vec::new(),
        costumes: Vec::new(),
        mouths: Vec::new(),
        walks,
        floats: Vec::new(),
        emotes: Vec::new(),
        vary: 0.0,
        nod: 0.0,
        bob: 0.0,
        blink: 0.0,
        seed: 1,
        sway: 0.0,
        lip_sync: None,
        lead: 1.0,
        parts: Vec::new(),
        reach: Vec::new(),
    }
}

fn solve_at(layer: &CharacterLayer, t: f64, bpm: f64) -> Solved {
    let beat = move |u: f64| u * bpm / 60.0;
    let m = Moment { t, beat: &beat, voice: None, host: 0.0, fps: 30.0, parts: &[], reach: &[], pose: None, instance: 0, arms: 0.0, hop: 0.0, nod: 0.0 };
    solve(layer, layer.rig().unwrap(), &m)
}

#[test]
fn phones_map_to_the_nine_visemes() {
    let cases = [
        ("M", Viseme::Mbp), ("B", Viseme::Mbp), ("P", Viseme::Mbp),
        ("T", Viseme::Tdskn), ("D", Viseme::Tdskn), ("S", Viseme::Tdskn), ("K", Viseme::Tdskn), ("N", Viseme::Tdskn),
        ("IY1", Viseme::Eeiy), ("IH0", Viseme::Eeiy), ("Y", Viseme::Eeiy),
        ("AA1", Viseme::Aha), ("AE", Viseme::Aha), ("AH0", Viseme::Aha),
        ("OW1", Viseme::Oh), ("AO", Viseme::Oh),
        ("UW", Viseme::Oowr), ("W", Viseme::Oowr), ("R", Viseme::Oowr),
        ("F", Viseme::Fv), ("V", Viseme::Fv),
        ("L", Viseme::Lth), ("TH", Viseme::Lth), ("DH", Viseme::Lth),
        ("sil", Viseme::Rest), ("", Viseme::Rest), ("sp", Viseme::Rest),
        // IPA.
        ("θ", Viseme::Lth), ("ʃ", Viseme::Tdskn), ("ɑː", Viseme::Aha), ("uː", Viseme::Oowr), ("m", Viseme::Mbp), ("oʊ", Viseme::Oh), ("f", Viseme::Fv),
    ];
    for (phone, want) in cases {
        assert_eq!(lipsync::viseme_of_phone(phone), want, "{phone}");
    }
    for v in Viseme::ALL {
        assert_eq!(Viseme::from_name(v.name()), Some(v));
        assert_eq!(v.fallbacks()[0], v, "a shape falls back to itself first");
        assert_eq!(*v.fallbacks().last().unwrap(), Viseme::Rest);
    }
}

#[test]
fn words_spell_into_phones() {
    assert_eq!(lipsync::phones_of_word("the"), vec!["TH", "EH"]);
    assert_eq!(lipsync::phones_of_word("make"), vec!["M", "EY", "K"]);
    assert_eq!(lipsync::phones_of_word("Beat!"), vec!["B", "IY", "T"]);
    assert_eq!(lipsync::phones_of_word("mouth"), vec!["M", "AW", "TH"]);
    assert_eq!(lipsync::phones_of_word("5.5"), lipsync::phones_of_word("five point five"));
    assert!(lipsync::phones_of_word("...").is_empty());
    let word = FeatureWord { text: "flow".into(), start: 1.0, end: 1.4, emphasis: 0.5, line: 0, syllables: Vec::new() };
    let shapes: Vec<Viseme> = lipsync::word_visemes(&word).iter().map(|v| v.viseme).collect();
    assert_eq!(shapes, vec![Viseme::Fv, Viseme::Lth, Viseme::Oh]);
    let spans = lipsync::word_visemes(&word);
    assert!((spans[0].start - 1.0).abs() < 1e-9 && (spans.last().unwrap().end - 1.4).abs() < 1e-9, "the shapes cover the word");
    assert!(spans[2].end - spans[2].start > spans[0].end - spans[0].start, "the vowel is the longest");
}

#[test]
fn the_mouth_follows_the_voice_one_frame_ahead_and_rests_in_pauses() {
    let word = |text: &str, start: f64, end: f64| FeatureWord { text: text.into(), start, end, emphasis: 0.5, line: 0, syllables: Vec::new() };
    let voice = VoiceFeatures {
        words: vec![word("mama", 1.0, 1.4), word("see", 2.0, 2.3)],
        lines: vec![(1.0, 2.3)],
        amplitude: FeatureCurve::new(0.0, 100.0, (0..300).map(|i| if (100..140).contains(&i) || (200..230).contains(&i) { 0.8 } else { 0.0 }).collect()),
        pitch: FeatureCurve::default(),
        pauses: vec![(1.4, 2.0)],
        phones: Vec::new(),
    };
    assert_eq!(lipsync::mouth_at(&voice, 1.01).0, Viseme::Mbp);
    assert_eq!(lipsync::mouth_at(&voice, 1.7).0, Viseme::Rest, "a pause rests");
    let (v, open) = lipsync::mouth_at(&voice, 2.2);
    assert_eq!(v, Viseme::Eeiy);
    assert!(open > 1.0, "a loud vowel opens wider: {open}");
    let track = lipsync::track(&voice, 0.0, 3.0, 30.0, 1.0);
    // With a one-frame lead the first shape appears a frame before the word.
    let first = track.iter().find(|(_, v)| *v != Viseme::Rest).unwrap();
    assert!((first.0 - (1.0 - 1.0 / 30.0)).abs() < 1e-6, "{track:?}");
    // Snaps: consecutive entries always differ.
    assert!(track.windows(2).all(|w| w[0].1 != w[1].1));
}

#[test]
fn the_walk_lands_a_step_on_every_beat() {
    let bpm = 120.0;
    let walk = Walk { start: Time::seconds(0.5), steps: 6.0, stride: 150.0, dir: 1.0, per_beat: 1.0 };
    let ch = layer(vec![walk]);
    let base = pose::preset("walk").unwrap();
    let mut last_sign = 0.0;
    for k in 1..6 {
        // Beat k after the start (beats every 0.5 s).
        let t = 0.5 + k as f64 * 0.5;
        let s = solve_at(&ch, t, bpm);
        let thigh = s.pose.angle(Joint::LegLU) - base.angle(Joint::LegLU);
        let other = s.pose.angle(Joint::LegRU) - base.angle(Joint::LegRU);
        assert!(thigh.abs() > 10.0, "legs apart on beat {k}: {thigh}");
        assert!((thigh + other).abs() < 1e-6, "the legs mirror each other");
        assert!(thigh.signum() != last_sign, "the leading leg alternates every beat");
        last_sign = thigh.signum();
        assert!((s.offset[0] - 150.0 * k as f64).abs() < 1e-6, "one stride a beat: {:?}", s.offset);
        // Halfway between beats the legs pass each other.
        let mid = solve_at(&ch, t + 0.25, bpm);
        assert!((mid.pose.angle(Joint::LegLU) - base.angle(Joint::LegLU)).abs() < 1e-6, "passing at the half beat");
        assert!(mid.pose.angle(Joint::LegLL) > 5.0 || mid.pose.angle(Joint::LegRL) > 5.0, "the swinging knee bends");
    }
    // Done: stopped where the walk ends, facing its way.
    let end = solve_at(&ch, 10.0, bpm);
    assert!((end.offset[0] - 900.0).abs() < 1e-6);
    assert_eq!(end.facing, 1.0);
    // Before it starts it stands still.
    assert_eq!(solve_at(&ch, 0.2, bpm).offset, [0.0, 0.0]);
    // A left walk mirrors the character.
    let left = layer(vec![Walk { start: Time::seconds(0.0), steps: 4.0, stride: 150.0, dir: -1.0, per_beat: 1.0 }]);
    let s = solve_at(&left, 1.0, bpm);
    assert_eq!(s.facing, -1.0);
    assert!((s.offset[0] + 300.0).abs() < 1e-6);
}

#[test]
fn feet_stay_on_the_ground() {
    let rig = kit::character("pip").unwrap();
    for name in ["idle", "sit", "hero", "arms_up"] {
        let mut ch = layer(Vec::new());
        ch.model = Model::Rig(Arc::new(rig.clone()));
        ch.poses = vec![Cue { t: Time::seconds(0.0), name: name.into(), blend: 0.0 }];
        let s = solve_at(&ch, 1.0, 120.0);
        let lowest = rig.soles.iter().map(|(part, sole)| s.parts.iter().find(|(i, _, _)| i == part).unwrap().1.apply(*sole)[1]).fold(f64::MIN, f64::max);
        assert!(lowest.abs() < 1e-6, "{name}: lowest sole at {lowest}");
    }
    // Sitting drops the head.
    let head = |name: &str| {
        let mut ch = layer(Vec::new());
        ch.model = Model::Rig(Arc::new(rig.clone()));
        ch.poses = vec![Cue { t: Time::seconds(0.0), name: name.into(), blend: 0.0 }];
        let s = solve_at(&ch, 1.0, 120.0);
        let at = rig.find("head").unwrap();
        s.parts.iter().find(|(i, _, _)| *i == at).unwrap().1.apply(rig.parts[at].pivot)[1]
    };
    assert!(head("sit") > head("idle") + 50.0);
}

#[test]
fn blinks_are_seeded_and_every_few_seconds() {
    let a = face::blinks(60.0, 3.5, 7);
    assert_eq!(a, face::blinks(60.0, 3.5, 7), "the same seed blinks the same");
    assert_ne!(a, face::blinks(60.0, 3.5, 8), "another seed blinks otherwise");
    assert!(a.len() >= 12 && a.len() <= 30, "{} blinks in a minute", a.len());
    assert!(face::blinking(a[0] + 0.05, 3.5, 7));
    assert!(!face::blinking(a[0] - 0.05, 3.5, 7));
    assert!(face::blinks(60.0, 0.0, 7).is_empty());
}

#[test]
fn kit_characters_have_every_viseme_and_expression() {
    for (name, _) in kit::KITS {
        let rig = kit::character(name).unwrap();
        let mouths = rig.variants(Slot::Mouth);
        for v in Viseme::ALL {
            assert!(mouths.contains(&v.name()), "{name}: mouth_{}", v.name());
        }
        for (expression, _) in face::EXPRESSIONS {
            if *expression == "neutral" {
                continue;
            }
            assert!(mouths.contains(expression) || rig.variants(Slot::Eyes).contains(expression), "{name}: {expression}");
        }
        assert!(rig.variants(Slot::Eyes).contains(&"closed"));
        for joint in pose::Joint::ALL {
            assert!(rig.joint_part(joint).is_some(), "{name}: {}", joint.name());
        }
        assert!(rig.height > 900.0 && rig.height < 1600.0, "{name}: {}", rig.height);
        assert_eq!(rig.soles.len(), 2);
    }
}

#[test]
fn a_kit_character_round_trips_through_svg() {
    let rig = kit::character("spark").unwrap();
    let markup = svg::to_svg(&rig);
    let (back, warnings) = svg::from_svg(&markup).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let names: Vec<&str> = rig.parts.iter().map(|p| p.name.as_str()).collect();
    let mut sorted: Vec<&str> = back.parts.iter().map(|p| p.name.as_str()).collect();
    let mut want = names.clone();
    sorted.sort();
    want.sort();
    assert_eq!(sorted, want);
    for part in &rig.parts {
        let other = &back.parts[back.find(&part.name).unwrap()];
        assert!((part.pivot[0] - other.pivot[0]).abs() < 0.02 && (part.pivot[1] - other.pivot[1]).abs() < 0.02, "{}", part.name);
        assert_eq!(part.shapes.len(), other.shapes.len(), "{}", part.name);
        assert_eq!(part.parent.map(|p| &rig.parts[p].name), other.parent.map(|p| &back.parts[p].name), "{}", part.name);
    }
    assert!((back.height - rig.height).abs() < 0.5);
    assert!(back.ground.abs() < 0.5);
    // Without ids it says what to do.
    assert!(svg::from_svg("<svg viewBox=\"0 0 10 10\"><rect x=\"0\" y=\"0\" width=\"5\" height=\"5\"/></svg>").unwrap_err().contains("part id"));
}

const DOC: &str = r#"
use mod.motion.*; Motion{
    size: vec2(1280, 720)
    duration: 10.0
    bpm: 96
    layers: [
        Rect{size: vec2(1280, 720) fill: #202028}
        Character{
            name: "spark"
            model: @spark
            look: {top: #d9d2c0 accessories: [@medallion @cap]}
            at: vec2(200, 680)
            height: 420
            walk: {start: beat(0) steps: 6 stride: 90}
            pose: [[0, @idle], [beat(7), @on_mic, 0.3]]
            expression: [[0, @deadpan], [word(@vo, 2), @smile]]
            nod: 5
            blink: 3.0
            seed: 4
            lip_sync: voice(@vo)
            parts: {head: {rotate: tween(0, 4, 5, 6)}}
        }
    ]
}
"#;

fn doc() -> MotionDoc {
    let doc = MotionDoc::load(DOC).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    let features = AudioFeatures::synthetic("yo check the flow on the beat", 5.0, 2.5, 96.0, 12.0);
    doc.with_context(MotionContext::analyzed(features, None, 0.0)).unwrap().0
}

#[test]
fn a_character_frame_is_deterministic() {
    let a = doc();
    for t in [0.0, 1.3, 4.6, 5.5, 8.25] {
        let f1 = a.eval(t);
        let f2 = a.eval(t);
        assert!(f1.errors.is_empty(), "{:?}", f1.errors);
        assert_eq!(f1.hash(), f2.hash(), "t = {t}");
        assert_eq!(f1.hash(), doc().eval(t).hash(), "a fresh load gives the same frame at {t}");
    }
    assert_ne!(a.eval(1.3).hash(), a.eval(1.4).hash(), "it moves");
}

#[test]
fn a_character_document_walks_talks_and_changes_expression() {
    let d = doc();
    let paths = |t: f64| -> Vec<String> { d.eval(t).walk().into_iter().map(|(_, item)| item.path.clone()).collect() };
    let group = |t: f64| d.eval(t).items[1].clone();
    // Walking: the character moves right on the beat grid (96 bpm: a beat is 0.625 s).
    let x = |t: f64| match &group(t).kind {
        crate::frame::ItemKind::Group { children, .. } => children.iter().find(|c| c.path.contains(".torso.")).unwrap().world.e,
        _ => panic!(),
    };
    assert!((x(0.625 * 4.0) - x(0.625 * 2.0) - 180.0).abs() < 1e-6, "two steps of 90: {} → {}", x(1.25), x(2.5));
    // Deadpan, then smiling once the voice's third word is said.
    assert!(paths(1.0).iter().any(|p| p.contains("mouth_deadpan")));
    let third = d.context.signals.voice("vo").unwrap().words[2].start;
    let later = third + 0.02;
    assert!(paths(later).iter().any(|p| p.contains("brows_smile")), "{:?}", paths(later));
    // Talking: inside a word the mouth is a viseme.
    let word = &d.context.signals.voice("vo").unwrap().words[1];
    let mid = (word.start + word.end) * 0.5;
    let mouth: Vec<String> = paths(mid).into_iter().filter(|p| p.contains(".mouth_")).collect();
    assert!(mouth.iter().any(|p| Viseme::ALL.iter().any(|v| p.contains(&format!("mouth_{}.", v.name())))), "{mouth:?}");
    // On the mic after beat 7.
    assert!(paths(6.0).iter().any(|p| p.contains(".mic.")));
    assert!(!paths(1.0).iter().any(|p| p.contains(".mic.")));
}

#[test]
fn character_mistakes_teach() {
    let err = |src: &str| MotionDoc::load(src).err().map(|d| d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    let e = err(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1 layers: [Character{model: @sprak}]}"#);
    assert!(e.contains("spark"), "{e}");
    let e = err(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1 layers: [Character{model: @spark pose: @on_mike}]}"#);
    assert!(e.contains("on_mic"), "{e}");
    let e = err(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1 layers: [Character{model: @spark look: {head: @triangle}}]}"#);
    assert!(e.contains("@round"), "{e}");
    let e = err(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1 layers: [Character{}]}"#);
    assert!(e.contains("model"), "{e}");
}

#[test]
fn costumes_floats_emotes_and_crowd_variants() {
    let src = r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 8.0 bpm: 120 layers: [
        Character{model: @pip at: vec2(300, 650) height: 400
            costume: [[0, @none], [2.0, @crown, @shades]]
            float: {start: 4.0 end: 6.0 height: 100 bob: 10}
            emote: [[1.0, @sweat, 1.0], [3.0, @dizzy]]}
        Repeat{count: 4 layout: @line step: {x: 200} at: vec2(700, 650)
            layer: Character{model: @spark height: 200 vary: 1.0 seed: 3 nod: 4}}
    ]}"#;
    let doc = MotionDoc::load(src).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    let paths = |t: f64| -> Vec<String> { doc.eval(t).walk().into_iter().map(|(_, item)| item.path.clone()).collect() };
    let has = |t: f64, part: &str| paths(t).iter().any(|p| p.contains(part));
    assert!(!has(1.0, "costume_crown") && has(2.5, "costume_crown.") && has(2.5, "costume_shades."));
    assert!(has(1.5, "emote_sweat") && !has(2.5, "emote_sweat") && has(3.5, "emote_dizzy"));
    // Floating lifts the feet off the ground.
    let feet_y = |t: f64| {
        let frame = doc.eval(t);
        frame.walk().into_iter().filter(|(_, i)| i.path.starts_with("layers[0].foot_")).map(|(_, i)| i.world.f).fold(f64::MIN, f64::max)
    };
    assert!(feet_y(1.0) - feet_y(5.0) > 60.0, "{} vs {}", feet_y(1.0), feet_y(5.0));
    assert!((feet_y(1.0) - feet_y(7.5)).abs() < 1.0, "back down after the float");
    // The crowd's copies differ in colour, the same way every frame.
    let frame = doc.eval(1.0);
    let torso_fill = |copy: usize| {
        frame.walk().into_iter().find(|(_, i)| i.path.contains(&format!("#{copy}")) && i.path.contains(".torso.0")).map(|(_, i)| match &i.kind {
            crate::frame::ItemKind::Path { fill, .. } => fill.unwrap(),
            _ => panic!(),
        })
    };
    let fills: Vec<_> = (0..4).filter_map(torso_fill).collect();
    assert_eq!(fills.len(), 4, "{:?}", frame.walk().into_iter().map(|(_, i)| i.path.clone()).filter(|p| p.contains("torso")).collect::<Vec<_>>());
    assert!(fills.windows(2).any(|w| w[0] != w[1]), "copies vary: {fills:?}");
    assert_eq!(frame.hash(), doc.eval(1.0).hash());
    // A varied colour keeps its alpha; inks stay ink.
    let v = Variation::of(1.0, 3, 2);
    assert_eq!(v.color([0.05, 0.05, 0.05, 1.0]), [0.05, 0.05, 0.05, 1.0]);
    assert_eq!(v.color([0.9, 0.4, 0.2, 0.5])[3], 0.5);
}

#[test]
fn one_model_placed_in_every_shot() {
    // The model sheet once (a `let`), each shot places it.
    let src = r#"
use mod.motion.*; let spark = Character{model: @spark look: {top: #3c78d8 accessories: [@cap]} height: 300 blink: 0}
Motion{size: vec2(1280, 720) duration: 4.0 layers: [
    spark{at: vec2(300, 650) pose: @hero}
    spark{at: vec2(900, 650) pose: @point span: [2.0, 2.0]}
]}"#;
    let doc = MotionDoc::load(src).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    let rigs: Vec<&Rig> = doc.layers.iter().filter_map(|l| match &l.kind {
        crate::model::LayerKind::Character(ch) => ch.rig(),
        _ => None,
    }).collect();
    assert_eq!(rigs.len(), 2);
    assert_eq!(rigs[0], rigs[1], "the same model in both shots");
    assert!(rigs[0].find("cap").is_some());
    assert!(catalog().contains("@on_mic") && catalog().contains("@sweat") && catalog().contains("@crown"));
    let svg = kit_svg("pip", &["head=@square".into(), "top=#ff0000".into()]).unwrap();
    assert!(svg.contains("id=\"mouth_AHA\"") && svg.contains("#ff0000"));
    assert!(kit_svg("pip", &["head=@hexagon".into()]).is_err());
}

#[test]
fn the_motion_skill_documents_the_kit_and_its_example_loads() {
    let skill = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/guide/SKILL.md")).unwrap();
    let section = skill.split("## 2g.").nth(1).expect("the Characters section").split("\n## ").next().unwrap();
    for (name, _) in kit::KITS.iter().chain(pose::POSES).chain(face::EXPRESSIONS).chain(kit::COSTUMES).chain(emote::EMOTES) {
        assert!(section.contains(&format!("@{name}")), "the skill names @{name}");
    }
    for v in Viseme::ALL {
        assert!(section.contains(v.name()), "{}", v.name());
    }
    let example = section.split("```").nth(1).unwrap();
    let doc = MotionDoc::load(example).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    let features = AudioFeatures::synthetic("yo", 5.0, 2.0, 96.0, 10.0);
    let doc = doc.with_context(MotionContext::analyzed(features, None, 0.0)).unwrap().0;
    assert!(doc.eval(5.1).errors.is_empty());
    let crowd = section.lines().find(|l| l.contains("Repeat{count")).unwrap();
    let crowd = crowd.split('`').nth(1).unwrap();
    MotionDoc::load(&format!("use mod.motion.*; Motion{{size: vec2(1280, 720) duration: 2 layers: [{crowd}]}}")).unwrap_or_else(|d| panic!("{d:?}"));
}

#[test]
fn timed_phones_drive_the_mouth_when_the_take_has_them() {
    use crate::features::FeaturePhone;
    let phone = |text: &str, start: f64, end: f64| FeaturePhone { text: text.into(), start, end };
    let word = FeatureWord { text: "see".into(), start: 1.0, end: 1.5, emphasis: 0.5, line: 0, syllables: Vec::new() };
    let mut voice = VoiceFeatures { words: vec![word], lines: vec![(1.0, 1.5)], ..Default::default() };
    // Spelling: S then EE over the word's span.
    assert_eq!(lipsync::mouth_at(&voice, 1.05).0, Viseme::Tdskn);
    // Kokoro's clock says the `s` is long and the vowel is late: it wins.
    voice.phones = vec![phone("s", 1.0, 1.3), phone("iː", 1.3, 1.45)];
    assert_eq!(lipsync::mouth_at(&voice, 1.2).0, Viseme::Tdskn);
    assert_eq!(lipsync::mouth_at(&voice, 1.35).0, Viseme::Eeiy);
    assert_eq!(lipsync::mouth_at(&voice, 1.47).0, Viseme::Rest, "between phonemes the mouth rests");
    // Kokoro's IPA as the speech crate writes it.
    for (p, v) in [("eɪ", Viseme::Eeiy), ("aɪ", Viseme::Aha), ("oʊ", Viseme::Oh), ("aʊ", Viseme::Aha), ("ɔɪ", Viseme::Oh), ("ʤ", Viseme::Tdskn), ("ɹ", Viseme::Oowr), ("ð", Viseme::Lth), ("ᵊ", Viseme::Aha)] {
        assert_eq!(lipsync::viseme_of_phone(p), v, "{p}");
    }
}

#[test]
fn crowd_members_are_varied_copies_that_react() {
    let mut ch = layer(Vec::new());
    ch.vary = 0.9;
    let beat = |u: f64| u * 2.0;
    let feet = |items: &[crate::frame::Item]| items.iter().filter(|i| i.path.contains(".foot_")).map(|i| i.world.f).fold(f64::MIN, f64::max);
    let (a, _) = member_items(&ch, 0, [100.0, 500.0], 0.5, 1.0, &beat, &[], 0.0, 0.0, 0.0, None, "crowd", &Affine::IDENTITY);
    let (b, _) = member_items(&ch, 1, [100.0, 500.0], 0.5, 1.0, &beat, &[], 0.0, 0.0, 0.0, None, "crowd", &Affine::IDENTITY);
    assert!(!a.is_empty() && a.iter().all(|i| i.path.starts_with("crowd#0.")));
    assert_ne!(format!("{:?}", a.iter().map(|i| &i.kind).collect::<Vec<_>>()), format!("{:?}", b.iter().map(|i| &i.kind).collect::<Vec<_>>()), "copies differ");
    let (hopped, _) = member_items(&ch, 0, [100.0, 500.0], 0.5, 1.0, &beat, &[], 1.0, 40.0, 0.0, None, "crowd", &Affine::IDENTITY);
    assert!((feet(&a) - feet(&hopped) - 20.0).abs() < 1.0, "a 40-unit hop at half scale lifts the feet 20: {} {}", feet(&a), feet(&hopped));
    assert_eq!(a, member_items(&ch, 0, [100.0, 500.0], 0.5, 1.0, &beat, &[], 0.0, 0.0, 0.0, None, "crowd", &Affine::IDENTITY).0, "the same every time");
}

#[test]
fn a_crowd_of_a_character() {
    let src = r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 4.0 bpm: 120 layers: [
        Crowd{count: 24 rows: 2 at: vec2(0, 380) size: vec2(1280, 340) seed: 3 nod: 1 hop: 0.05
              arms: keys([1.0, 0], [1.5, 1]) member: Character{model: @pip height: 300 vary: 0.8 costume: @party_hat}}
    ]}"#;
    let doc = MotionDoc::load(src).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    let frame = doc.eval(2.0);
    let members: std::collections::HashSet<String> = frame.walk().into_iter().filter_map(|(_, i)| i.path.split_once('.').and_then(|(_, rest)| rest.split('.').next().map(str::to_string))).filter(|p| p.contains('#')).collect();
    let heads = frame.walk().into_iter().filter(|(_, i)| i.path.contains(".head.0")).count();
    assert_eq!(heads, 24, "every member is a character: {members:?}");
    assert!(frame.walk().iter().any(|(_, i)| i.path.contains("costume_party_hat")));
    assert_eq!(frame.hash(), doc.eval(2.0).hash(), "exact twice");
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1 layers: [Crowd{count: 2 member: Rect{}}]}"#).is_err());
}
