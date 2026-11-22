use super::*;
use crate::csg::V2;
use fab_materials::MaterialId;
use makepad_csg_mesh::{validate::{pieces, validate_mesh}, volume::mesh_volume};
use std::collections::BTreeMap;

struct Case {
    name: String,
    p: WindowParams,
    expect_warnings: i32,
}

fn cases() -> Vec<Case> {
    let mut all = Vec::new();
    for screen in 1..Screen::NAMES.len() as i32 {
        let mut p = WindowParams { screen, ..WindowParams::default() };
        apply_screen(&mut p);
        p.name = format!("screen_{screen}");
        all.push(Case { name: p.name.clone(), p, expect_warnings: 0 });
    }
    let ring = || {
        let mut p = WindowParams { screen: Screen::LedRing25.index(), ..WindowParams::default() };
        apply_screen(&mut p);
        p
    };
    let mut add = |name: &str, p, expect_warnings| all.push(Case { name: name.into(), p, expect_warnings });

    let mut p = ring();
    p.seg_shape = SegShape::Wedge.index();
    p.seg_ink = SegInk::Diffuser.index();
    add("ring_wedge_diffuser", p, 0);

    let mut p = ring();
    p.seg_count = 16;
    p.seg_width = 2.4;
    p.seg_length = 3.0;
    p.seg_round = SegRound::Inner.index();
    p.seg_corner = 5.0;
    p.notch_end = NotchEnd::Round.index();
    add("ring_wedge_round_inner", p.clone(), 0);
    p.notch_end = NotchEnd::Square.index();
    p.seg_round = SegRound::Both.index();
    p.seg_corner = 0.3;
    add("ring_wedge_round_both", p, 0);

    let mut p = ring();
    p.seg_shape = SegShape::Dot.index();
    p.seg_count = 11;
    p.seg_span = 270.0;
    p.seg_start = -135.0;
    p.hole = HoleType::DFlat.index();
    p.flat_dist = 2.6;
    add("scale_dots_dflat", p, 0);

    let mut p = ring();
    p.seg_shape = SegShape::Slot.index();
    p.hole = HoleType::Notched1.index();
    p.notch_angle = 90.0;
    p.notch_end = NotchEnd::Round.index();
    add("ring_slots_one_notch", p, 0);

    let mut p = WindowParams { screen: Screen::Oled130.index(), ..WindowParams::default() };
    apply_screen(&mut p);
    p.mount_holes = MountHoles::Four.index();
    p.border = 4.0;
    apply_screen(&mut p);
    p.edge = EdgeStyle::Round.index();
    p.edge_size = 0.6;
    p.material = MaterialId::lookup("pc").expect("PC material exists");
    add("display_mount_round_edge", p, 0);

    add("plain_rect_sharp", WindowParams {
        screen: Screen::Custom.index(), outline: Outline::Rect.index(), width: 30.0,
        height: 20.0, corner: 0.0, viewport: Viewport::None.index(), print: false,
        adhesive: false, edge: EdgeStyle::None.index(), ..WindowParams::default()
    }, -1);

    let mut p = WindowParams { dither: true, ..WindowParams::default() };
    add("round_dithered", p.clone(), 0);
    p.screen = Screen::Oled130.index();
    apply_screen(&mut p);
    p.dither_width = 1.5;
    p.dither_pitch = 0.4;
    p.dither_min = 0.1;
    p.dither_overlap = 30.0;
    p.dither_curve = -1.0;
    add("rect_dithered", p, 0);

    let mut p = ring();
    p.seg_count = 60;
    p.hole_d = 21.0;
    p.notch_span = 23.0;
    add("bad_overlaps", p, -2);
    all
}

#[test]
fn bodies_are_closed_and_square_edges_preserve_sheet_volume() {
    for case in cases() {
        let name = &case.name;
        let full = build(&case.p);
        match case.expect_warnings {
            0 => assert!(full.warnings.is_empty(), "{name}: expected no warnings, got {:?}", full.warnings),
            -2 => assert!(full.warnings.len() >= 2, "{name}: expected at least two overlap warnings, got {:?}", full.warnings),
            _ => {}
        }
        drop(full);
        // Keep mesh checks small; dimensions, features and laser resolution stay unchanged.
        let p = WindowParams { segments: 48, ..case.p };
        let b = build(&p);
        for (body, csg, one_piece) in [
            ("window", &b.window, true),
            ("print", &b.print, b.report.dots == 0),
            ("ink", &b.ink, false),
            ("adhesive", &b.adhesive, true),
        ] {
            if csg.steps.is_empty() { continue; }
            let m = crate::lower::mesh(csg).unwrap_or_else(|e| panic!("{name}/{body}: meshing failed: {e}"));
            assert!(!m.triangles.is_empty(), "{name}/{body}: mesh must be nonempty");
            let report = validate_mesh(&m);
            assert!(report.is_closed && report.is_manifold, "{name}/{body}: each edge must have two faces: {report:?}");
            assert!(report.is_consistently_oriented, "{name}/{body}: faces must have consistent winding: {report:?}");
            assert_eq!((report.near_duplicate_pairs, report.coincident_pairs), (0, 0),
                "{name}/{body}: no vertex pairs within the near-duplicate radius: {report:?}");
            if one_piece && case.expect_warnings != -2 {
                assert_eq!(pieces(&m), 1, "{name}/{body}: body must be one piece");
            }
            if body == "window" && p.edge() == EdgeStyle::None {
                let expected = b.report.area * p.thickness;
                let volume = mesh_volume(&m);
                assert!((volume - expected).abs() <= 0.005 * expected,
                    "{name}: square-edge volume {volume} must equal sheet area times thickness {expected} within 0.5 percent");
            }
        }
    }
}

type Attributes<'a> = BTreeMap<&'a str, &'a str>;

struct SvgLayer<'a> {
    id: &'a str,
    paths: Vec<Attributes<'a>>,
}

fn attributes<'a>(mut text: &'a str, name: &str) -> Attributes<'a> {
    let mut out = BTreeMap::new();
    while !text.trim_start().is_empty() {
        text = text.trim_start();
        let (key, rest) = text.split_once('=').unwrap_or_else(|| panic!("{name}: XML attribute needs '='"));
        let key = key.trim_end();
        assert!(!key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b"_:-".contains(&b)), "{name}: invalid XML attribute name {key:?}");
        let rest = rest.trim_start();
        let quote = rest.chars().next().expect("XML attribute has a value");
        assert!(matches!(quote, '\'' | '"'), "{name}: XML attribute must be quoted");
        let (value, rest) = rest[1..].split_once(quote).unwrap_or_else(|| panic!("{name}: unclosed XML attribute"));
        assert!(!value.contains(['<', '&']), "{name}: unexpected markup in XML attribute");
        assert!(out.insert(key, value).is_none(), "{name}: duplicate XML attribute {key}");
        assert!(rest.is_empty() || rest.starts_with(char::is_whitespace), "{name}: XML attributes need whitespace");
        text = rest;
    }
    out
}

// Parse the emitted XML subset, enforcing nesting, quoted attributes and closed tags.
fn parse_svg<'a>(mut text: &'a str, name: &str) -> (Attributes<'a>, Vec<SvgLayer<'a>>) {
    let mut stack = Vec::new();
    let mut root = None;
    let mut declaration = false;
    let mut layers: Vec<SvgLayer<'a>> = Vec::new();
    while !text.trim_start().is_empty() {
        text = text.trim_start();
        if let Some(rest) = text.strip_prefix("<?xml ") {
            assert!(!declaration && root.is_none(), "{name}: XML declaration must occur once before the root");
            let (attrs, rest) = rest.split_once("?>").unwrap_or_else(|| panic!("{name}: unclosed XML declaration"));
            assert_eq!(attributes(attrs, name).get("version"), Some(&"1.0"), "{name}: XML version");
            declaration = true;
            text = rest;
            continue;
        }
        if let Some(rest) = text.strip_prefix("<!--") {
            let (comment, rest) = rest.split_once("-->").unwrap_or_else(|| panic!("{name}: unclosed XML comment"));
            assert!(!comment.contains("--"), "{name}: XML comment contains a double hyphen");
            text = rest;
            continue;
        }
        let rest = text.strip_prefix('<').unwrap_or_else(|| panic!("{name}: unexpected XML text"));
        let (tag, rest) = rest.split_once('>').unwrap_or_else(|| panic!("{name}: unclosed XML tag"));
        text = rest;
        if let Some(close) = tag.strip_prefix('/') {
            assert_eq!(stack.pop(), Some(close.trim()), "{name}: XML closing tag must match its opening tag");
            continue;
        }
        let empty = tag.ends_with('/');
        let tag = tag.strip_suffix('/').unwrap_or(tag).trim_end();
        let (element, attrs) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        let attrs = attributes(attrs, name);
        match element {
            "svg" => {
                assert!(stack.is_empty() && root.is_none() && !empty, "{name}: exactly one SVG root");
                root = Some(attrs);
            }
            "g" => {
                assert!(stack == ["svg"] && !empty, "{name}: layer must be a child of SVG");
                let id = *attrs.get("id").unwrap_or_else(|| panic!("{name}: layer needs an id"));
                assert!(!layers.iter().any(|layer| layer.id == id), "{name}: duplicate layer {id}");
                layers.push(SvgLayer { id, paths: Vec::new() });
            }
            "path" => {
                assert!(stack == ["svg", "g"] && empty, "{name}: path must be closed inside a layer");
                layers.last_mut().expect("path has a layer").paths.push(attrs);
            }
            _ => panic!("{name}: unexpected SVG element {element}"),
        }
        if !empty { stack.push(element); }
    }
    assert!(declaration && stack.is_empty(), "{name}: complete XML document");
    (root.unwrap_or_else(|| panic!("{name}: SVG root is missing")), layers)
}

fn number(text: &str, decimals: usize, name: &str) -> f64 {
    let (_, fraction) = text.split_once('.').unwrap_or_else(|| panic!("{name}: decimal coordinate expected: {text}"));
    assert_eq!(fraction.len(), decimals, "{name}: coordinate precision for {text}");
    let n: f64 = text.parse().unwrap_or_else(|e| panic!("{name}: invalid coordinate {text}: {e}"));
    assert!(n.is_finite(), "{name}: coordinates must be finite");
    n
}

fn svg_polygons(data: &str, name: &str) -> Vec<Vec<[f64; 2]>> {
    let mut out = Vec::new();
    let mut polygon = Vec::new();
    let mut tokens = data.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "Z" {
            assert!(polygon.len() >= 3, "{name}: closed SVG polygon needs at least three vertices");
            out.push(std::mem::take(&mut polygon));
            continue;
        }
        let command = token.as_bytes()[0];
        assert!((command == b'M' && polygon.is_empty()) || (command == b'L' && !polygon.is_empty()),
            "{name}: SVG path must start with M, continue with L and close with Z");
        let y = tokens.next().unwrap_or_else(|| panic!("{name}: SVG vertex needs two coordinates"));
        polygon.push([number(&token[1..], 4, name), number(y, 4, name)]);
    }
    assert!(polygon.is_empty(), "{name}: SVG path must close its last polygon");
    out
}

type Polygon<'a> = (&'a [V2], [f64; 2]);

fn placed(p: &Placed) -> Polygon<'_> { (&p.poly, [p.cx, p.cy]) }
fn centred(p: &Poly) -> Polygon<'_> { (p, [0.0; 2]) }

fn check_polygon(actual: &[[f64; 2]], expected: Polygon<'_>, svg: bool, name: &str) {
    assert_eq!(actual.len(), expected.0.len(), "{name}: polygon vertex count must match layout");
    for (i, (actual, q)) in actual.iter().zip(expected.0).enumerate() {
        let x = q[0] + expected.1[0];
        let y = (q[1] + expected.1[1]) * if svg { -1.0 } else { 1.0 };
        assert!((actual[0] - x).abs() <= 0.000051 && (actual[1] - y).abs() <= 0.000051,
            "{name}: vertex {i} must match placed layout coordinates ({x}, {y}), got {actual:?}");
    }
}

fn check_svg(p: &WindowParams, l: &WindowLayout, name: &str) {
    let text = laser_svg(p);
    let (root, layers) = parse_svg(&text, name);
    assert_eq!(root.get("xmlns"), Some(&"http://www.w3.org/2000/svg"), "{name}: SVG namespace");
    let bounds = l.outline.iter().fold([f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY], |b, q|
        [b[0].min(q[0]), b[1].max(q[0]), b[2].min(q[1]), b[3].max(q[1])]);
    let expected_box = [bounds[0] - 2.0, -bounds[3] - 2.0, bounds[1] - bounds[0] + 4.0, bounds[3] - bounds[2] + 4.0];
    let view_box: Vec<_> = root["viewBox"].split_whitespace().map(|n| number(n, 3, name)).collect();
    assert_eq!(view_box.len(), 4, "{name}: SVG viewBox needs four numbers");
    for (actual, expected) in view_box.iter().zip(expected_box) {
        assert!((actual - expected).abs() <= 0.000501, "{name}: SVG viewBox must surround the outline with a 2 mm margin");
    }
    for (key, expected) in [("width", expected_box[2]), ("height", expected_box[3])] {
        let value = root[key].strip_suffix("mm").unwrap_or_else(|| panic!("{name}: SVG {key} must use millimetres"));
        assert!((number(value, 3, name) - expected).abs() <= 0.000501, "{name}: SVG {key} must match viewBox");
    }
    let mut expected_layers = Vec::new();
    if p.print {
        expected_layers.push("print");
        if p.seg_ink() == SegInk::Diffuser && !l.segs.is_empty() { expected_layers.push("ink"); }
    }
    if p.adhesive { expected_layers.push("adhesive"); }
    expected_layers.push("cut");
    assert_eq!(layers.iter().map(|layer| layer.id).collect::<Vec<_>>(), expected_layers,
        "{name}: SVG layer presence and order");
    for layer in layers {
        let context = format!("{name}/SVG/{}", layer.id);
        let mut paths: Vec<Vec<Polygon<'_>>> = Vec::new();
        match layer.id {
            "print" => {
                let mut mask = vec![centred(&l.print_outer)];
                mask.extend(l.openings.iter().chain(&l.holes).map(placed));
                paths.push(mask);
                if !l.dots.is_empty() { paths.push(l.dots.iter().map(placed).collect()); }
            }
            "ink" => paths.push(l.segs.iter().map(placed).collect()),
            "adhesive" => {
                let mut rings = vec![centred(&l.adhesive_outer), centred(&l.adhesive_inner)];
                rings.extend(l.holes.iter().map(placed));
                paths.push(rings);
            }
            "cut" => {
                let mut cuts = vec![centred(&l.outline)];
                cuts.extend(l.holes.iter().map(placed));
                paths.push(cuts);
            }
            _ => unreachable!(),
        }
        assert_eq!(layer.paths.len(), paths.len(), "{context}: path element count");
        for (attrs, expected) in layer.paths.iter().zip(paths) {
            let actual = svg_polygons(attrs["d"], &context);
            assert_eq!(actual.len(), expected.len(), "{context}: one closed subpath per layout polygon");
            for (actual, expected) in actual.iter().zip(expected) { check_polygon(actual, expected, true, &context); }
        }
        let attrs = &layer.paths[0];
        match layer.id {
            "print" => {
                assert_eq!(attrs.get("fill-rule"), Some(&"evenodd"), "{context}: mask fill rule");
                for path in &layer.paths { assert_eq!(path.get("fill"), Some(&"#000000"), "{context}: black print"); }
            }
            "ink" => {
                assert_eq!(attrs.get("fill"), Some(&"#f4f4f0"), "{context}: diffuser fill");
                assert_eq!(attrs.get("stroke"), Some(&"#999999"), "{context}: diffuser stroke");
                assert_eq!(attrs.get("stroke-width"), Some(&"0.02"), "{context}: diffuser stroke width");
            }
            layer => {
                let (colour, width) = if layer == "cut" { ("#ff0000", "0.01") } else { ("#0060ff", "0.02") };
                assert_eq!(attrs.get("fill"), Some(&"none"), "{context}: hairline has no fill");
                assert_eq!(attrs.get("stroke"), Some(&colour), "{context}: hairline colour");
                assert_eq!(attrs.get("stroke-width"), Some(&width), "{context}: hairline width");
            }
        }
    }
}

fn dxf_record<'a>(input: &mut &'a [(i32, &'a str)], name: &str) -> &'a [(i32, &'a str)] {
    assert_eq!(input.first().map(|p| p.0), Some(0), "{name}: DXF entity starts with group code zero");
    let count = input.iter().skip(1).position(|p| p.0 == 0).map_or(input.len(), |i| i + 1);
    let (record, rest) = input.split_at(count);
    *input = rest;
    record
}

fn dxf_field<'a>(record: &[(i32, &'a str)], code: i32, name: &str) -> &'a str {
    let values: Vec<_> = record.iter().filter(|p| p.0 == code).map(|p| p.1).collect();
    assert_eq!(values.len(), 1, "{name}: DXF entity needs one value for group code {code}");
    values[0]
}

fn check_dxf(p: &WindowParams, l: &WindowLayout, name: &str) {
    let text = laser_dxf(p);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len() % 2, 0, "{name}: DXF must contain complete group-code pairs");
    let pairs: Vec<_> = lines.chunks_exact(2).map(|pair| (
        pair[0].parse::<i32>().unwrap_or_else(|e| panic!("{name}: invalid DXF group code: {e}")), pair[1]
    )).collect();
    assert_eq!(pairs.last(), Some(&(0, "EOF")), "{name}: DXF must end in EOF");
    for (key, value) in [("$ACADVER", (1, "AC1009")), ("$INSUNITS", (70, "4"))] {
        let index = pairs.iter().position(|p| *p == (9, key)).unwrap_or_else(|| panic!("{name}: missing DXF {key}"));
        assert_eq!(pairs.get(index + 1), Some(&value), "{name}: DXF {key}");
    }
    let start = pairs.iter().position(|p| *p == (2, "ENTITIES")).unwrap_or_else(|| panic!("{name}: missing DXF entities section"));
    assert_eq!(pairs.get(start.wrapping_sub(1)), Some(&(0, "SECTION")), "{name}: ENTITIES starts a section");
    let end = start + 1 + pairs[start + 1..].iter().position(|p| *p == (0, "ENDSEC")).unwrap_or_else(|| panic!("{name}: unclosed DXF entities section"));
    assert_eq!(&pairs[end..], &[(0, "ENDSEC"), (0, "EOF")], "{name}: DXF entity section and file terminate");
    let mut input = &pairs[start + 1..end];
    let mut expected = vec![("CUT", centred(&l.outline))];
    expected.extend(l.holes.iter().map(|q| ("CUT", placed(q))));
    if p.print {
        expected.push(("PRINT", centred(&l.print_outer)));
        expected.extend(l.openings.iter().chain(&l.dots).map(|q| ("PRINT", placed(q))));
        if p.seg_ink() == SegInk::Diffuser { expected.extend(l.segs.iter().map(|q| ("INK", placed(q)))); }
    }
    if p.adhesive {
        expected.push(("ADHESIVE", centred(&l.adhesive_outer)));
        expected.push(("ADHESIVE", centred(&l.adhesive_inner)));
    }
    for (layer, polygon) in expected {
        let context = format!("{name}/DXF/{layer}");
        let header = dxf_record(&mut input, &context);
        assert_eq!(header[0].1, "POLYLINE", "{context}: layout polygon must be a POLYLINE");
        let actual_layer = dxf_field(header, 8, &context);
        assert!(["CUT", "PRINT", "INK", "ADHESIVE"].contains(&actual_layer), "{context}: allowed layer name");
        assert_eq!(actual_layer, layer, "{context}: polygon layer and order");
        assert_eq!(dxf_field(header, 66, &context), "1", "{context}: vertices follow");
        assert_eq!(dxf_field(header, 70, &context), "1", "{context}: polyline must be closed");
        let mut vertices = Vec::new();
        loop {
            let record = dxf_record(&mut input, &context);
            assert_eq!(dxf_field(record, 8, &context), layer, "{context}: entity layer must match polyline");
            match record[0].1 {
                "VERTEX" => {
                    assert_eq!(dxf_field(record, 30, &context), "0.0", "{context}: planar vertex z");
                    vertices.push([number(dxf_field(record, 10, &context), 4, &context), number(dxf_field(record, 20, &context), 4, &context)]);
                }
                "SEQEND" => break,
                entity => panic!("{context}: expected VERTEX or SEQEND, got {entity}"),
            }
        }
        check_polygon(&vertices, polygon, false, &context);
    }
    assert!(input.is_empty(), "{name}: DXF must have exactly the layout polylines");
}

#[test]
fn laser_layers_parse_and_match_layout() {
    for case in cases() {
        let l = layout(&case.p);
        check_svg(&case.p, &l, &case.name);
        check_dxf(&case.p, &l, &case.name);
    }
}
