//! The cut model's invariants, checked after every op in
//! debug builds and tests:
//!
//! 1. Every node's picture is read at most once: an effect or transition
//!    wraps one chain, and no chain is wired twice.
//! 2. Every clip, still and title reaches the picture stack through its
//!    chain of effects and transitions (nothing dangles, nothing loops).
//! 3. An effect has an input, and an effect on a clip has the clip's span
//!    and track: it has no time of its own.
//! 4. A transition joins two different clips (the latest of the chain that
//!    plays out, the earliest of the one that plays in) that overlap, and
//!    lies inside their overlap.
//! 5. The projection draws one band per effect, aligned to its clip on its
//!    clip's rail at its own level (two effects on a clip never share a
//!    place), a badge on every clip with an effect, and one tab per
//!    transition naming the clips it joins.

// Checked only in debug builds and tests.
#![cfg_attr(not(any(test, debug_assertions)), allow(dead_code))]

use super::ops::{chain_base, overlap, side_base, timed_block};
use super::project::{project, BlockKind};
use makepad_story_cut::{CutDoc, CutEval, CutNodeKind};
use std::collections::HashMap;

const EPS: f64 = 1e-6;

pub fn check(doc: &CutDoc) -> Vec<String> {
    let mut broken = Vec::new();
    let stack = doc.node(&doc.output_node).and_then(|out| match &out.kind {
        CutNodeKind::Output { frame: Some(frame), .. } => Some(frame.node.clone()),
        _ => None,
    });
    let readers = |id: &str| doc.wiring().filter(|(from, port, _, _)| *from == id && *port == "frame").map(|(_, _, to, _)| to.to_string()).collect::<Vec<_>>();
    // 1.
    for node in &doc.nodes {
        let read = readers(&node.id);
        if read.len() > 1 {
            broken.push(format!("`{}` is read {} times ({})", node.id, read.len(), read.join(", ")));
        }
    }
    // 2.
    if let Some(stack) = &stack {
        for node in doc.nodes.iter().filter(|node| timed_block(doc, &node.id).is_some()) {
            let mut current = node.id.clone();
            let mut reached = false;
            for _ in 0..=doc.nodes.len() {
                let read = readers(&current);
                if read.iter().any(|reader| reader == stack) {
                    reached = true;
                    break;
                }
                match read.as_slice() {
                    [single] if doc.generate(single).is_some() || matches!(doc.node(single).map(|node| &node.kind), Some(CutNodeKind::Fx { .. } | CutNodeKind::Transition { .. })) => current = single.clone(),
                    _ => break,
                }
            }
            if !reached {
                broken.push(format!("`{}` does not reach the picture", node.id));
            }
        }
    }
    for node in &doc.nodes {
        match &node.kind {
            // 3.
            CutNodeKind::Fx { input, .. } => {
                let Some(input) = input else {
                    broken.push(format!("effect `{}` wraps nothing", node.id));
                    continue;
                };
                if doc.node(&input.node).is_none() {
                    broken.push(format!("effect `{}` wraps a missing `{}`", node.id, input.node));
                    continue;
                }
                if let Some(base) = timed_block(doc, &chain_base(doc, &node.id)) {
                    let (own, span) = (node.span, base.span.unwrap());
                    if !own.is_some_and(|own| (own.start - span.start).abs() < EPS && (own.length - span.length).abs() < EPS) {
                        broken.push(format!("effect `{}` span {:?} is not its clip `{}`'s {:?}", node.id, own, base.id, span));
                    }
                    if node.track != base.track {
                        broken.push(format!("effect `{}` is on track {} but its clip `{}` on {}", node.id, node.track, base.id, base.track));
                    }
                }
            }
            // 4.
            CutNodeKind::Transition { a, b, .. } => {
                let (Some(a), Some(b)) = (a, b) else {
                    broken.push(format!("transition `{}` lacks an input", node.id));
                    continue;
                };
                let (base_a, base_b) = (side_base(doc, &a.node, true), side_base(doc, &b.node, false));
                let (Some(block_a), Some(block_b)) = (timed_block(doc, &base_a), timed_block(doc, &base_b)) else {
                    continue;
                };
                if base_a == base_b {
                    broken.push(format!("transition `{}` joins `{base_a}` to itself", node.id));
                    continue;
                }
                let (sa, sb) = (block_a.span.unwrap(), block_b.span.unwrap());
                let Some(over) = overlap(sa, sb) else {
                    broken.push(format!("transition `{}` joins `{base_a}` and `{base_b}`, which do not meet", node.id));
                    continue;
                };
                let span = node.span.unwrap_or(over);
                if span.start < over.start - EPS || span.end() > over.end() + EPS {
                    broken.push(format!("transition `{}` {:?} lies outside the overlap {:?}", node.id, span, over));
                }
            }
            _ => {}
        }
    }
    // 3b. A Generate has an input; one over a clip has the clip's span.
    for gen in &doc.generates {
        let Some(input) = &gen.input else {
            broken.push(format!("generate `{}` wraps nothing", gen.id));
            continue;
        };
        if doc.node(&input.node).is_none() && doc.generate(&input.node).is_none() {
            broken.push(format!("generate `{}` wraps a missing `{}`", gen.id, input.node));
            continue;
        }
        if let Some(base) = timed_block(doc, &chain_base(doc, &gen.id)) {
            let span = base.span.unwrap();
            if (gen.span.start - span.start).abs() > EPS || (gen.span.length - span.length).abs() > EPS {
                broken.push(format!("generate `{}` span {:?} is not its clip `{}`'s {:?}", gen.id, gen.span, base.id, span));
            }
        }
    }
    // 5.
    let eval = CutEval::new(doc);
    let projection = project(&eval, &HashMap::new(), &|_| false);
    let mut places = Vec::new();
    for fx in doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Fx { .. })) {
        let bands: Vec<_> = projection.blocks.iter().filter(|block| block.node == fx.id && block.kind == BlockKind::Effect).collect();
        let Some(base) = timed_block(doc, &chain_base(doc, &fx.id)) else { continue };
        let [band] = bands.as_slice() else {
            broken.push(format!("effect `{}` draws {} bands", fx.id, bands.len()));
            continue;
        };
        let span = base.span.unwrap();
        if (band.start - span.start).abs() > EPS || (band.length - span.length).abs() > EPS || band.track != base.track {
            broken.push(format!("effect `{}`'s band is not over its clip `{}`", fx.id, base.id));
        }
        let place = (band.wraps.clone(), band.level);
        if places.contains(&place) {
            broken.push(format!("effect `{}`'s band is drawn over another on `{}`", fx.id, base.id));
        }
        places.push(place);
        if !projection.block(&base.id).is_some_and(|block| block.fx) {
            broken.push(format!("clip `{}` has an effect but no FX badge", base.id));
        }
    }
    for tr in doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Transition { .. })) {
        let CutNodeKind::Transition { a: Some(a), b: Some(b), .. } = &tr.kind else { continue };
        let clips = (side_base(doc, &a.node, true), side_base(doc, &b.node, false));
        match projection.bands.iter().filter(|band| band.node == tr.id).collect::<Vec<_>>().as_slice() {
            [band] if band.clips == clips => {}
            [band] => broken.push(format!("transition `{}`'s tab names {:?}, not {:?}", tr.id, band.clips, clips)),
            bands => broken.push(format!("transition `{}` draws {} tabs", tr.id, bands.len())),
        }
    }
    broken
}

#[cfg(test)]
mod tests {
    use super::super::ops::{apply, joined, CutOp, Media, MediaKind};
    use super::super::patch::Patcher;
    use super::super::transitions::{cut_plan, find_cut_in};
    use super::*;
    use makepad_story_cut::{AssetRef, CutNode, MediaRef, SourceTime};

    const EMPTY: &str = "use mod.flow.*\nuse mod.cut.*\n\nlet main = Stack{layers: []}\nlet out = CutOutput{frame: main.out(@frame) audio: []}\n\nFlow{\n    kind: @cut\n    label: \"Test\"\n    output: {width: 1920 height: 1080 fps: 60}\n    duration: 10.0\n    bpm: 120\n    main, out\n}\n";
    /// Every test source is this long.
    const SOURCE: f64 = 6.0;

    fn video(n: u8, seconds: f64) -> Media {
        Media {
            kind: MediaKind::Video,
            src: MediaRef::Asset(AssetRef { id: makepad_asset_data::AssetId::from_bytes([n; 16]).to_string(), rev: format!("arev_{}", format!("{n:x}").repeat(64)) }),
            seconds: Some(seconds),
        }
    }

    /// Handles as the page works them out, every source `SOURCE` long.
    fn handles(node: &CutNode) -> (f64, f64) {
        let Some(span) = node.span else { return (0.0, 0.0) };
        match &node.kind {
            CutNodeKind::Clip { source_in: SourceTime::Seconds(source_in), speed, .. } => {
                let speed = speed.abs().max(1e-6);
                (source_in / speed, ((SOURCE - source_in) / speed - span.length).max(0.0))
            }
            CutNodeKind::Still { .. } | CutNodeKind::Title { .. } => (f64::INFINITY, f64::INFINITY),
            _ => (0.0, 0.0),
        }
    }

    /// A cut's history, as the page keeps it: sources, undo and redo.
    struct History {
        patcher: Patcher,
        source: String,
        undo: Vec<String>,
        redo: Vec<String>,
        log: Vec<String>,
    }

    impl History {
        fn new() -> Self {
            Self { patcher: Patcher::new().unwrap(), source: EMPTY.into(), undo: Vec::new(), redo: Vec::new(), log: Vec::new() }
        }
        fn doc(&self) -> CutDoc {
            CutDoc::load(&self.source).unwrap()
        }
        /// `apply` checks the invariants itself (and panics in tests).
        fn op(&mut self, op: CutOp) -> bool {
            self.log.push(format!("{op:?}"));
            match apply(&self.patcher, &self.source, &op) {
                Ok((source, _)) => {
                    self.undo.push(std::mem::replace(&mut self.source, source));
                    self.redo.clear();
                    true
                }
                Err(error) => {
                    self.log.push(format!("  refused: {error}"));
                    false
                }
            }
        }
        fn undo(&mut self, redo: bool) {
            self.log.push(if redo { "redo".into() } else { "undo".into() });
            let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
            if let Some(source) = from.pop() {
                to.push(std::mem::replace(&mut self.source, source));
            }
            self.assert_ok();
        }
        fn assert_ok(&self) {
            let broken = check(&self.doc());
            assert!(broken.is_empty(), "{}\nafter:\n{}\n{}", broken.join("\n"), self.log.join("\n"), self.source);
        }
        /// cmd-T on the cut at `a`'s end.
        fn cut(&mut self, a: &str) -> bool {
            let doc = self.doc();
            let Some(node) = doc.node(a).cloned() else { return false };
            let Some(span) = node.span else { return false };
            let Some((a, b, _)) = find_cut_in(&doc, node.track, span.end(), 1.0 / 60.0) else { return false };
            match cut_plan(&doc, "101_trans_dissolve", &a, &b, 1.0, &handles, &|id| id.to_string()) {
                Ok(plan) => self.op(plan.op),
                Err(error) => {
                    self.log.push(format!("cut refused: {error}"));
                    false
                }
            }
        }
    }

    fn fx_of(doc: &CutDoc, clip: &str) -> Vec<String> {
        doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Fx { .. }) && chain_base(doc, &node.id) == clip).map(|node| node.id.clone()).collect()
    }

    /// The user's report: two clips on V1, cmd-T by overlap-by-trimming,
    /// Pixel Dissolve on clip 2, then more drags and effects. The effect
    /// band stayed behind when the clip moved.
    #[test]
    fn the_reported_sequence_keeps_effects_on_their_clips() {
        let mut h = History::new();
        assert!(h.op(CutOp::Add { media: video(1, SOURCE), start: 0.0, track: None }));
        assert!(h.op(CutOp::Add { media: video(2, SOURCE), start: 6.0, track: None }));
        assert!(h.cut("clip1"), "{}", h.log.join("\n"));
        // No handles: clip2 moved 1 s earlier to make the overlap.
        let doc = h.doc();
        assert_eq!(doc.node("clip2").unwrap().span.unwrap().start, 5.0);
        assert_eq!(joined(&doc, "clip2").len(), 1);
        assert!(h.op(CutOp::Effect { node: "clip2".into(), effect: "13_pixel_dissolve".into() }));
        // Drag clip2 along (still overlapping), then far away: its effect follows.
        assert!(h.op(CutOp::Move { node: "clip2".into(), start: 5.2, track: 0 }));
        assert!(h.op(CutOp::Effect { node: "clip2".into(), effect: "31_reel".into() }));
        assert_eq!(fx_of(&h.doc(), "clip2").len(), 2);
        assert!(h.op(CutOp::Move { node: "clip2".into(), start: 14.0, track: 0 }));
        let doc = h.doc();
        assert!(joined(&doc, "clip2").is_empty());
        for fx in fx_of(&doc, "clip2") {
            assert_eq!(doc.node(&fx).unwrap().span, doc.node("clip2").unwrap().span);
        }
        // Dragging an effect's band moves its clip.
        let fx = fx_of(&doc, "clip2")[0].clone();
        assert!(h.op(CutOp::Move { node: fx, start: 16.0, track: 1 }));
        let doc = h.doc();
        assert_eq!((doc.node("clip2").unwrap().span.unwrap().start, doc.node("clip2").unwrap().track), (16.0, 1));
        assert!(h.op(CutOp::Trim { node: "clip2".into(), start: 16.5, length: 3.0 }));
        assert!(h.op(CutOp::Split { node: "clip2".into(), t: 18.0 }));
        let doc = h.doc();
        let halves: Vec<_> = doc.nodes.iter().filter(|node| timed_block(&doc, &node.id).is_some() && node.track == 1).map(|node| node.id.clone()).collect();
        assert_eq!(halves.len(), 2);
        for half in &halves {
            assert_eq!(fx_of(&doc, half).len(), 2, "each half keeps both effects");
        }
        for _ in 0..h.undo.len() {
            h.undo(false);
        }
        assert_eq!(h.source, EMPTY);
        while !h.redo.is_empty() {
            h.undo(true);
        }
    }

    /// Transitions on both ends of a clip, then the middle clip cut, moved
    /// and deleted.
    #[test]
    fn a_clip_between_two_transitions() {
        let mut h = History::new();
        for (n, start) in [(1, 0.0), (2, 4.0), (3, 8.0)] {
            assert!(h.op(CutOp::Add { media: video(n, 4.0), start, track: Some(0) }));
        }
        // 4 s of 6 s sources from 0: 2 s handle past each out, none before in.
        assert!(h.cut("clip1"), "{}", h.log.join("\n"));
        assert!(h.cut("clip2"), "{}", h.log.join("\n"));
        let doc = h.doc();
        assert_eq!(joined(&doc, "clip2").len(), 2);
        assert!(h.op(CutOp::Effect { node: "clip2".into(), effect: "15_acid_bloom".into() }));
        let middle = doc.node("clip2").unwrap().span.unwrap();
        assert!(h.op(CutOp::Split { node: "clip2".into(), t: middle.start + middle.length * 0.5 }));
        let doc = h.doc();
        let new = doc.nodes.iter().find(|node| timed_block(&doc, &node.id).is_some() && !["clip1", "clip2", "clip3"].contains(&node.id.as_str())).unwrap().id.clone();
        assert_eq!(joined(&doc, "clip2").iter().map(|(_, other)| other.as_str()).collect::<Vec<_>>(), vec!["clip1"]);
        assert_eq!(joined(&doc, &new).iter().map(|(_, other)| other.as_str()).collect::<Vec<_>>(), vec!["clip3"]);
        assert!(h.op(CutOp::Delete { node: "clip2".into() }));
        assert!(h.op(CutOp::Delete { node: new }));
        assert!(!h.doc().nodes.iter().any(|node| matches!(node.kind, CutNodeKind::Transition { .. } | CutNodeKind::Fx { .. })));
        for _ in 0..h.undo.len() {
            h.undo(false);
        }
    }

    /// Random sequences of every timeline op, undo and redo; the invariants
    /// hold after each step.
    #[test]
    fn random_op_sequences_keep_the_invariants() {
        let effects = ["13_pixel_dissolve", "15_acid_bloom", "31_reel"];
        for seed in 1..=(std::env::var("EDIT_FUZZ_SEEDS").ok().and_then(|n| n.parse().ok()).unwrap_or(12u64)) {
            let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut next = move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            let mut h = History::new();
            for step in 0..45 {
                let doc = h.doc();
                let blocks: Vec<CutNode> = doc.nodes.iter().filter_map(|node| timed_block(&doc, &node.id)).collect();
                let fx: Vec<String> = doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Fx { .. })).map(|node| node.id.clone()).collect();
                let trs: Vec<CutNode> = doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Transition { .. })).cloned().collect();
                let pick = |n: u64, len: usize| (n % len.max(1) as u64) as usize;
                let r = next();
                let f = |n: u64| (n % 1000) as f64 / 1000.0;
                let choice = if blocks.len() < 2 { 0 } else { r % 13 };
                match choice {
                    0 => {
                        let length = 1.0 + f(next()) * 5.0;
                        let start = (f(next()) * 16.0 * 4.0).round() / 4.0;
                        let track = if next() % 2 == 0 { None } else { Some((next() % 3) as u32) };
                        h.op(CutOp::Add { media: video((next() % 5) as u8 + 1, length), start, track });
                    }
                    1 | 2 => {
                        let block = &blocks[pick(next(), blocks.len())];
                        let span = block.span.unwrap();
                        let start = (span.start + (f(next()) - 0.5) * 8.0).max(0.0);
                        let track = if next() % 3 == 0 { (next() % 3) as u32 } else { block.track };
                        h.op(CutOp::Move { node: block.id.clone(), start, track });
                    }
                    3 => {
                        let block = &blocks[pick(next(), blocks.len())];
                        let span = block.span.unwrap();
                        let d = (f(next()) - 0.5) * 2.0;
                        h.op(CutOp::Trim { node: block.id.clone(), start: span.start + d, length: span.length - d });
                    }
                    4 => {
                        let block = &blocks[pick(next(), blocks.len())];
                        let span = block.span.unwrap();
                        h.op(CutOp::Trim { node: block.id.clone(), start: span.start, length: span.length + (f(next()) - 0.5) * 2.0 });
                    }
                    5 => {
                        let block = &blocks[pick(next(), blocks.len())];
                        let span = block.span.unwrap();
                        h.op(CutOp::Split { node: block.id.clone(), t: span.start + span.length * (0.1 + 0.8 * f(next())) });
                    }
                    6 | 7 => {
                        // On a clip, or on an effect (a second one on the same clip).
                        let node = if !fx.is_empty() && next() % 3 == 0 { fx[pick(next(), fx.len())].clone() } else { blocks[pick(next(), blocks.len())].id.clone() };
                        h.op(CutOp::Effect { node, effect: effects[pick(next(), effects.len())].into() });
                    }
                    8 | 9 => {
                        let block = &blocks[pick(next(), blocks.len())];
                        h.cut(&block.id);
                    }
                    10 => {
                        // Two tracks that overlap: the older transition.
                        let a = &blocks[pick(next(), blocks.len())];
                        if let Some(b) = blocks.iter().find(|b| b.track != a.track && overlap(a.span.unwrap(), b.span.unwrap()).is_some()) {
                            h.op(CutOp::Transition { a: a.id.clone(), b: b.id.clone(), effect: "102_trans_wipe".into() });
                        }
                    }
                    11 => {
                        let mut all: Vec<String> = blocks.iter().map(|block| block.id.clone()).collect();
                        all.extend(fx.iter().cloned());
                        all.extend(trs.iter().map(|tr| tr.id.clone()));
                        h.op(CutOp::Delete { node: all[pick(next(), all.len())].clone() });
                    }
                    _ => {
                        if !trs.is_empty() && next() % 2 == 0 {
                            // A transition's edges dragged.
                            let tr = &trs[pick(next(), trs.len())];
                            let span = tr.span.unwrap();
                            let d = (f(next()) - 0.5) * 1.0;
                            h.op(CutOp::Trim { node: tr.id.clone(), start: span.start - d, length: span.length + 2.0 * d });
                        } else {
                            let redo = next() % 2 == 0;
                            h.undo(redo);
                        }
                    }
                }
                let _ = step;
                h.assert_ok();
            }
            // Everything undoes to the empty edit, and redoes to the end.
            while !h.redo.is_empty() {
                h.undo(true);
            }
            let end = h.source.clone();
            while !h.undo.is_empty() {
                h.undo(false);
            }
            assert_eq!(h.source, EMPTY);
            while !h.redo.is_empty() {
                h.undo(true);
            }
            assert_eq!(h.source, end);
        }
    }
}
