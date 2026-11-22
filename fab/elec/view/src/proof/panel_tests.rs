use fab_elec_core::{LayerId, PolySet};
use super::{clip, panel_lines, Tone};
use crate::proof::scene::{Dot, Hit, Item, Layer, Prim, ProofScene, Role};
use fab_elec_cam::{Check, CheckKind, Validation};
use fab_elec_core::Side;

#[test]
fn panel_checks_stack_counts_bom_and_clipping() {
    let validation = Validation {
        checks: vec![
            Check { file: "pos.csv".into(), kind: CheckKind::Placement, ok: true, cause: "positions match".into(), hint: String::new(), at: None, tile: None },
            Check { file: "board.zip".into(), kind: CheckKind::FileSet, ok: false, cause: "missing layer".into(), hint: "include top copper".into(), at: None, tile: None },
            Check { file: "bom.csv".into(), kind: CheckKind::Bom, ok: false, cause: "missing part".into(), hint: "include C1".into(), at: None, tile: None },
        ],
    };
    let bom = vec![
        vec!["Ref".into(), "Value".into(), "Qty".into()],
        vec!["R1,R2".into(), "10k".into(), "2".into()],
        vec!["C1".into(), "é".repeat(25), "1".into()],
        vec!["D1".into()],
    ];
    // The scene's picking fields are private; populate only its public panel data.
    let mut scene = ProofScene::default();
    scene.validation = validation;
    scene.bom = bom;
    scene.layers = vec![
        Layer { file: "bottom.gbr".into(), label: "Bottom".into(), role: Role::Copper { side: Some(Side::Bottom), inner: 0 }, items: vec![], attrs: vec![], id: LayerId::B_CU, fills: PolySet::new() },
        Layer {
            file: "top.gbr".into(), label: "Top".into(), role: Role::Copper { side: Some(Side::Top), inner: 0 },
            items: vec![Item { id: 0, prim: Prim::Disc { c: [0.0, 0.0], r: 0.5 }, attrs: None }], attrs: vec![], id: LayerId::F_CU, fills: PolySet::new(),
        },
    ];
    scene.drills = vec![
        Hit { id: 1, at: [0.0, 0.0], to: None, dia: 0.3, plated: Some(true) },
        Hit { id: 2, at: [1.0, 0.0], to: Some([2.0, 0.0]), dia: 0.5, plated: None },
    ];
    scene.dots = vec![Dot { id: 3, refdes: "C1".into(), at: [1.0, 1.0], side: Side::Top, rot: 0.0 }];

    let lines = panel_lines(&scene, 120);
    assert_eq!((lines[0].text.as_str(), lines[0].tone), ("Export: 2 checks failed", Tone::Head), "summary: count failed checks, not all checks");
    assert_eq!(lines[1..6].iter().map(|l| (l.text.as_str(), l.tone)).collect::<Vec<_>>(), vec![
        ("FAIL board.zip fileset: missing layer", Tone::Fail),
        ("     fix: include top copper", Tone::Dim),
        ("FAIL bom.csv bom: missing part", Tone::Fail),
        ("     fix: include C1", Tone::Dim),
        ("ok  pos.csv placement: positions match", Tone::Ok),
    ], "checks: failures retain validation order, each has a fix, and passes follow without fixes");
    assert_eq!(lines[6..14].iter().map(|l| (l.text.as_str(), l.tone)).collect::<Vec<_>>(), vec![
        ("", Tone::Normal),
        ("Layers", Tone::Head),
        ("Top  top.gbr  1 objects", Tone::Normal),
        ("Bottom  bottom.gbr  0 objects", Tone::Normal),
        ("Drill  2 holes (1 slots)", Tone::Normal),
        ("Placement  1 parts", Tone::Normal),
        ("", Tone::Normal),
        ("BOM", Tone::Head),
    ], "stack and counts: layers run top first with object counts, slots are counted within drills, and placement uses dots");

    let rows = &lines[14..];
    assert_eq!(rows.len(), scene.bom.len(), "BOM: retain every row including its header and a short row");
    assert_eq!(rows[0].tone, Tone::Dim, "BOM: dim the header row");
    assert!(rows[1..].iter().all(|r| r.tone == Tone::Normal), "BOM: data rows use normal tone");
    assert!(rows.iter().all(|r| r.text.chars().count() == 36), "BOM: all rows have equal character width, with each column capped at 24");
    assert_eq!(rows[0].text.find("Value"), Some(7), "BOM: header's second column follows the widest first cell and two spaces");
    assert_eq!(rows[1].text.find("10k"), Some(7), "BOM: data aligns with the header's second column");
    assert_eq!(rows[2].text.chars().skip(7).take(24).collect::<String>(), format!("{}~", "é".repeat(23)), "BOM: long Unicode cells clip by character with a tilde");
    assert_eq!(rows[2].text.chars().skip(33).collect::<String>(), "1  ", "BOM: the last column aligns after a clipped Unicode cell and is padded");
    assert_eq!(rows[3].text.trim_end(), "D1", "BOM: absent cells are padded without inventing content");

    assert_eq!(clip("abcdef", 4), "abc~", "clip: reserve the final character for the truncation marker");
    assert_eq!(clip("abc", 2), "a~", "clip: a two-character pane keeps one character and the marker");
    assert_eq!(clip("hi", 4), "hi", "clip: shorter text is unchanged");
    assert_eq!(clip("four", 4), "four", "clip: exactly fitting text has no marker");
    assert_eq!(clip("éΩλ", 2), "é~", "clip: multi-byte characters are never split");
    for chars in [0, 1, 2, 12, 24] {
        let narrow = panel_lines(&scene, chars);
        assert_eq!(narrow.len(), lines.len(), "panel clipping: width {chars} preserves the number of lines");
        for (full, cut) in lines.iter().zip(&narrow) {
            assert!(cut.text.chars().count() <= chars, "panel clipping: every line fits width {chars}");
            assert_eq!(cut.text, clip(&full.text, chars), "panel clipping: heads, checks, fixes, counts and BOM rows all use clip at width {chars}");
            assert_eq!(cut.tone, full.tone, "panel clipping: width {chars} preserves tone");
        }
    }

    let empty = panel_lines(&ProofScene::default(), 80);
    assert_eq!(empty.iter().map(|l| (l.text.as_str(), l.tone)).collect::<Vec<_>>(), vec![
        ("Export: ok", Tone::Head),
        ("", Tone::Normal),
        ("Layers", Tone::Head),
        ("", Tone::Normal),
        ("BOM", Tone::Head),
    ], "empty scene: only the summary, section heads and separators, without panicking");
}
