use makepad_motion::{inspect::*, *};

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")))
}

fn at(doc: &MotionDoc) -> Vec<LayoutIssue> {
    let issues = inspect_frame(&doc, &doc.eval(0.5), DEFAULT_MIN_TEXT_PX);
    assert!(!issues.iter().any(|i| i.code == "layout.unchecked"), "{issues:#?}");
    issues
}

fn pairs(issues: &[LayoutIssue], code: &str) -> Vec<(String, Option<String>)> {
    issues.iter().filter(|i| i.code == code).map(|i| (i.path.clone(), i.other.clone())).collect()
}

#[test]
fn glyph_overlap_ignores_whitespace_hidden_and_unrevealed_text() {
    let doc = load(include_str!("../fixtures/inspect/overlap.splash"));
    let issues = at(&doc);
    assert_eq!(pairs(&issues, "text.overlap"), vec![("layers[0]".into(), Some("layers[1]".into()))]);
    let issue = issues.iter().find(|i| i.code == "text.overlap").unwrap();
    assert_eq!((issue.from, issue.to), (0.5, 0.5));
    assert!(!issue.hint.is_empty());
}

#[test]
fn strokes_cross_glyphs_but_trimmed_transparent_and_clear_lines_do_not() {
    let doc = load(include_str!("../fixtures/inspect/strokes.splash"));
    let issues = at(&doc);
    assert_eq!(pairs(&issues, "text.line_through"), vec![
        ("layers[0]".into(), Some("layers[1]".into())),
        ("layers[0]".into(), Some("layers[6]".into())),
    ]);
}

#[test]
fn only_opaque_foreground_fills_cover_text_and_path_holes_stay_clear() {
    let doc = load(include_str!("../fixtures/inspect/fills.splash"));
    assert_eq!(pairs(&at(&doc), "text.covered"), vec![
        ("layers[1]".into(), Some("layers[2]".into())),
        ("layers[1]".into(), Some("layers[7].layers[0]".into())),
    ]);
}

#[test]
fn minimum_size_is_output_pixels_after_fit_parent_and_unit_scale() {
    let doc = load(include_str!("../fixtures/inspect/sizes.splash"));
    let small: Vec<_> = pairs(&at(&doc), "text.too_small").into_iter().map(|p| p.0).collect();
    assert_eq!(small, ["layers[0]", "layers[2]", "layers[3].layers[0]", "layers[4]"]);
    assert!(pairs(&inspect_frame(&doc, &doc.eval(0.5), 4.0), "text.too_small").is_empty());
    let stricter = inspect_frame(&doc, &doc.eval(0.5), 40.0);
    assert!(stricter.iter().any(|i| i.code == "text.too_small" && i.path == "layers[1]"));
}

#[test]
fn clipping_uses_visible_bounds_including_stroke_width_and_fitted_glyphs() {
    let doc = load(include_str!("../fixtures/inspect/clipping.splash"));
    let cut: Vec<_> = pairs(&at(&doc), "layer.clipped").into_iter().map(|p| p.0).collect();
    assert_eq!(cut, ["layers[0]", "layers[2]", "layers[3]"]);
}

#[test]
fn timeline_lists_first_last_and_gaps_and_excludes_hidden_paint() {
    let doc = load(include_str!("../fixtures/inspect/timeline.splash"));
    let times = sample_times(&doc, doc.fps);
    let visible = visibility(&doc, &times);
    let layer = |path: &str| visible.iter().find(|v| v.path == path).unwrap();
    let gapped = layer("layers[0]");
    assert_eq!(gapped.first, Some(0.0));
    assert_eq!(gapped.last, Some(2.95));
    assert_eq!(gapped.spans, [(0.0, 1.0), (2.0, 3.0)]);
    assert_eq!(gapped.gaps, [(1.0, 2.0)]);
    assert!(gapped.to_text().contains("first 0.000 s, last 2.950 s; gaps: 1.000–2.000 s"));
    assert_eq!(layer("layers[1]").spans, [(0.0, 4.0)]);
    assert!(layer("layers[1]").gaps.is_empty());
    for path in ["layers[2]", "layers[4]", "layers[5]"] {
        assert!(layer(path).first.is_none() && layer(path).spans.is_empty());
        assert!(layer(path).to_text().contains("never visible"));
    }
}

#[test]
fn sampling_includes_nested_span_boundaries_between_grid_points() {
    let doc = load(include_str!("../fixtures/inspect/timeline.splash"));
    let times = sample_times(&doc, 2.0);
    for t in [1.0, 1.05, 1.09, 2.0, 3.0] {
        assert!(times.iter().any(|s| (s - t).abs() < 1e-9), "missing {t}: {times:?}");
    }
    assert!(times.windows(2).all(|p| p[0] < p[1]));
    let visible = visibility(&doc, &times);
    let child = visible.iter().find(|v| v.path == "layers[3].layers[0]").unwrap();
    assert!((child.first.unwrap() - 1.05).abs() < 1e-9);
    assert!((child.spans[0].1 - 1.09).abs() < 1e-9);
}

#[test]
fn findings_are_deterministic_and_disjoint_runs_are_not_joined() {
    let mut doc = load(include_str!("../fixtures/inspect/overlap.splash"));
    let timeline = load(include_str!("../fixtures/inspect/timeline.splash"));
    doc.layers[1].opacity = timeline.layers[0].opacity.clone();
    doc.duration = 4.0;
    let times = sample_times(&doc, 4.0);
    let issues = layout_checks(&doc, &times, DEFAULT_MIN_TEXT_PX);
    let overlaps: Vec<_> = issues.iter().filter(|i| i.code == "text.overlap").collect();
    assert_eq!(overlaps.len(), 2, "{issues:#?}");
    assert_eq!((overlaps[0].from, overlaps[1].from), (0.0, 2.0));
    assert!(overlaps[0].to < 1.0 && overlaps[1].to < 3.0);
    let backwards: Vec<_> = times.iter().rev().copied().chain([f64::NAN, -1.0, 4.0, 0.0]).collect();
    assert_eq!(issues, layout_checks(&doc, &backwards, DEFAULT_MIN_TEXT_PX));
    for t in [2.5, 0.5, 1.5, 0.5] {
        let frame = doc.eval(t);
        assert_eq!(inspect_frame(&doc, &frame, 16.0), inspect_frame(&doc, &doc.eval(t), 16.0));
    }
}

#[test]
fn math_pieces_are_text_and_draw2d_paint_is_checked() {
    let doc = load(include_str!("../fixtures/inspect/math_draw2d.splash"));
    let issues = at(&doc);
    // The formula is one text, crossed by the path and by the Draw2D stem;
    // the Draw2D text run is crossed by the path below it.
    assert_eq!(pairs(&issues, "text.line_through"), vec![
        ("layers[0]".into(), Some("layers[1]".into())),
        ("layers[0]".into(), Some("layers[2]".into())),
        ("layers[2]".into(), Some("layers[4]".into())),
    ]);
    assert!(pairs(&issues, "text.overlap").is_empty(), "{issues:#?}");
    // Math pieces are not reported as fills covering text.
    assert!(pairs(&issues, "text.covered").is_empty(), "{issues:#?}");
}

/// The continuity check: hard cuts in and out are found at their frame,
/// once per popping parent; fades, slides in from the edge, growth, a
/// `hard_cut: true` layer, frame 0 and tiny sparks are not pops.
#[test]
fn layers_that_pop_in_or_out_without_an_entrance_or_exit_are_found_at_their_frame() {
    let doc = load(include_str!("../fixtures/inspect/pops.splash"));
    let pops: Vec<(String, &str, f64)> = layout_checks(&doc, &sample_times(&doc, 10.0), DEFAULT_MIN_TEXT_PX)
        .into_iter()
        .filter(|i| i.code.starts_with("layer.pop"))
        .map(|i| (i.path.clone(), i.code, (i.from * 30.0).round()))
        .collect();
    assert_eq!(pops, vec![
        ("layers[0]".to_string(), "layer.pop_in", 30.0),
        ("layers[4]".to_string(), "layer.pop_in", 60.0),
        ("layers[0]".to_string(), "layer.pop_out", 75.0),
        ("layers[4]".to_string(), "layer.pop_out", 90.0),
    ], "{pops:#?}");
    let all = pop_checks(&doc, &sample_times(&doc, 10.0));
    let first = &all[0];
    assert_eq!((first.code, first.path.as_str(), first.from), ("layer.pop_in", "layers[0]", 1.0));
    assert!(first.message.contains("`pop`") && first.message.contains("frame 30"), "{}", first.message);
    assert!(first.hint.contains("keys([0.00, 0], [0.35, 1, @ease_out])") && first.hint.contains("hard_cut: true"), "{}", first.hint);
    let out = all.iter().find(|i| i.code == "layer.pop_out" && i.path == "layers[0]").unwrap();
    assert!(out.hint.contains("ending at 1.50 s"), "{}", out.hint);
}
