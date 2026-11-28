//! Charts (lane DV1): every documented example loads, draws the same frame
//! twice and passes the label checks; layout, races, voice-synced landing,
//! number formats, and the checks finding what they should.

use makepad_motion::plain::PrimitiveFrame;
use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}\n{source}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn wrap(layer: &str) -> String {
    format!("use mod.motion.*; Motion{{size: vec2(1920, 1080) duration: 8.0 background: #0b0d12 layers: [\n{layer}\n]}}")
}

/// The ```splash blocks of the stage-motion skill's charts section.
fn skill_examples() -> Vec<String> {
    let skill = include_str!("../guide/SKILL.md");
    let start = skill.find("## 2c.").expect("the skill has a charts section");
    let end = skill[start..].find("\n## 2d.").map_or(skill.len(), |e| start + e);
    skill[start..end].split("```splash").skip(1).map(|block| block.split("```").next().unwrap().trim().to_string()).collect()
}

fn texts(frame: &MotionFrame) -> Vec<String> {
    frame.walk().into_iter().filter_map(|(_, item)| match &item.kind {
        ItemKind::Primitive(PrimitiveFrame::Text { text, .. }) if item.opacity > 0.5 => Some(text.clone()),
        _ => None,
    }).collect()
}

fn samples(doc: &MotionDoc) -> Vec<f64> {
    (0..(doc.duration * 10.0) as usize).map(|k| k as f64 / 10.0).collect()
}

#[test]
fn every_documented_chart_loads_is_deterministic_and_passes_its_checks() {
    let examples = skill_examples();
    assert!(examples.len() >= 27, "one example per kind, found {}", examples.len());
    let mut kinds = std::collections::BTreeSet::new();
    for example in &examples {
        let kind = example.split("type: @").nth(1).and_then(|r| r.split(|c: char| !c.is_alphanumeric()).next()).unwrap_or("").to_string();
        kinds.insert(kind.clone());
        let source = wrap(example);
        let a = load(&source);
        let b = load(&source);
        for &t in &[0.0, 0.7, 1.5, 3.0, 5.0, 7.9] {
            let (fa, fb) = (a.eval(t), b.eval(t));
            assert!(fa.errors.is_empty(), "@{kind} at {t}: {:?}", fa.errors);
            assert_eq!(fa.hash(), fb.hash(), "@{kind} at {t}: two loads draw different frames");
            assert_eq!(fa.hash(), a.eval(t).hash(), "@{kind} at {t}: evaluating twice differs");
        }
        let issues = chart_checks(&a, &samples(&a));
        assert!(issues.is_empty(), "@{kind}:\n{}", issues.iter().map(|i| format!("{} {} ({:.1}–{:.1} s)", i.code, i.message, i.from, i.to)).collect::<Vec<_>>().join("\n"));
    }
    for kind in ["bar", "line", "area", "donut", "bubble", "network", "treemap", "map", "kpi", "counter", "timeline", "scoreboard", "versus", "sticker", "meter", "axis", "gantt", "probs"] {
        assert!(kinds.contains(kind), "no example for @{kind}");
    }
}

#[test]
fn bars_stay_inside_their_box_and_reach_their_values() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [
        Chart{type: @bar at: vec2(200, 150) size: vec2(1200, 700) data: [["a", 10], ["b", 30], ["c", 20]] title: "T"}
    ]}"#);
    let frame = doc.eval(3.9);
    let bars: Vec<(f64, f64, f64)> = frame.walk().into_iter().filter_map(|(_, item)| match &item.kind {
        ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) if size[0] > 100.0 && size[1] > 5.0 => {
            let o = item.world.apply([0.0, 0.0]);
            assert!(o[0] >= 200.0 && o[0] + size[0] <= 1400.0 + 1e-6 && o[1] >= 150.0 && o[1] + size[1] <= 850.0 + 1e-6, "{o:?} {size:?}");
            Some((o[0], size[1], o[1] + size[1]))
        }
        _ => None,
    }).collect();
    assert_eq!(bars.len(), 3, "{bars:?}");
    // Proportional heights on a shared baseline.
    assert!((bars[1].1 / bars[0].1 - 3.0).abs() < 0.01 && (bars[2].1 / bars[0].1 - 2.0).abs() < 0.01, "{bars:?}");
    assert!(bars.iter().all(|b| (b.2 - bars[0].2).abs() < 1e-6));
    // Nice ticks (0, 5, …, 30) and the values as labels.
    let t = texts(&frame);
    for want in ["0", "10", "20", "30", "a", "b", "c", "T"] {
        assert!(t.iter().any(|x| x == want), "{want} in {t:?}");
    }
}

#[test]
fn a_bar_race_reorders_as_its_datasets_arrive() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 8.0 layers: [
        Chart{type: @bar sort: true split: "year" every: 2.0 csv: "year,name,value\n1,A,9\n1,B,5\n1,C,1\n2,A,4\n2,B,6\n2,C,12"}
    ]}"#);
    let order = |t: f64| {
        let frame = doc.eval(t);
        let mut bars: Vec<(f64, String)> = frame.walk().into_iter().filter_map(|(_, item)| match &item.kind {
            ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) if size[1] > 20.0 && size[0] > 3.0 => Some((item.world.apply([0.0, 0.0])[1], item.path.clone())),
            _ => None,
        }).collect();
        bars.sort_by(|a, b| a.0.total_cmp(&b.0));
        bars.into_iter().map(|b| b.1).collect::<Vec<_>>()
    };
    let (first, last) = (order(1.9), order(7.9));
    assert_eq!(first.len(), 3);
    // A led, C was last; C leads at the end, A is last.
    assert_eq!(first[0], last[2]);
    assert_eq!(first[2], last[0]);
    // The period label crossfades from "1" to "2".
    assert!(texts(&doc.eval(1.9)).contains(&"1".to_string()) && texts(&doc.eval(7.9)).contains(&"2".to_string()));
}

#[test]
fn a_line_reaches_each_point_as_its_label_is_spoken() {
    // Words at 1.0 + i/2: "revenue" 1.0, "jan" 1.5, "feb" 2.0, "mar" 2.5.
    let features = AudioFeatures::synthetic("revenue jan feb mar", 1.0, 2.0, 120.0, 10.0);
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 6.0 layers: [
        Chart{type: @line size: vec2(1400, 700) grow: 0.8 data: [["Jan", 1], ["Feb", 2], ["Mar", 3]] land: {voice: @vo said: @label}}
    ]}"#);
    let (doc, warnings) = doc.with_context(MotionContext::analyzed(features, None, 0.0)).unwrap();
    assert!(warnings.iter().all(|w| !w.message.contains("word")), "{warnings:?}");
    let trim = |t: f64| doc.eval(t).walk().into_iter().find_map(|(_, item)| match &item.kind {
        ItemKind::Primitive(PrimitiveFrame::Path { trim, stroke: Some(_), .. }) if item.path.ends_with(']') && trim[1] < 1.0 + 1e-9 => Some(trim[1]),
        _ => None,
    }).unwrap();
    // Lands a beat of `grow` after the word starts (0.35 × grow = 0.28 s).
    assert!(trim(1.5 + 0.28) < 0.01, "{}", trim(1.78));
    assert!((trim(2.0 + 0.28) - 0.5).abs() < 0.02, "{}", trim(2.28));
    assert!((trim(2.5 + 0.28) - 1.0).abs() < 1e-6, "{}", trim(2.78));
}

#[test]
fn numbers_count_up_to_their_formatted_values() {
    let doc = load(&wrap(r#"Chart{type: @kpi data: [{label: "Revenue" value: 12400000 format: @currency delta: 0.18}, {label: "Users" value: 48210}, {label: "Rate" value: 42 format: @percent}]}"#));
    let end = texts(&doc.eval(7.9));
    for want in ["$12.4M", "48,210", "42%", "+18%", "Revenue"] {
        assert!(end.iter().any(|t| t == want), "{want} in {end:?}");
    }
    // Part way: counting, not there yet.
    let early = texts(&doc.eval(0.5));
    assert!(!early.iter().any(|t| t == "48,210"), "{early:?}");
}

#[test]
fn the_label_checks_find_overlaps_and_labels_off_the_frame() {
    let crowded = load(&wrap(r#"Chart{type: @scatter labels: true size: vec2(800, 500) data: [{label: "Alpha" x: 1 y: 1}, {label: "Bravo" x: 1.02 y: 1.01}, {label: "Charlie" x: 5 y: 3}]}"#));
    let issues = chart_checks(&crowded, &samples(&crowded));
    assert!(issues.iter().any(|i| i.code == "chart.label_overlap" && i.message.contains("Alpha") && i.message.contains("Bravo")), "{issues:?}");
    let off = load(&wrap(r#"Chart{type: @bar at: vec2(1500, 100) size: vec2(900, 500) data: [["North", 3], ["South", 5], ["East", 2]]}"#));
    let issues = chart_checks(&off, &samples(&off));
    assert!(issues.iter().any(|i| i.code == "chart.label_off_frame"), "{issues:?}");
    let clean = load(&wrap(r#"Chart{type: @bar data: [["North", 3], ["South", 5], ["East", 2]]}"#));
    assert!(chart_checks(&clean, &samples(&clean)).is_empty());
}

#[test]
fn chart_mistakes_teach() {
    let error = |layer: &str| MotionDoc::load(&wrap(layer)).err().map(|d| d.iter().map(|d| format!("{} {}", d.message, d.hint.clone().unwrap_or_default())).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    assert!(error(r#"Chart{type: @donught data: [1, 2]}"#).contains("@donut"));
    assert!(error(r#"Chart{type: @bar}"#).contains("csv"));
    assert!(error(r#"Chart{type: @bar data: [1, 2] format: @money}"#).contains("@currency"));
    assert!(error(r#"Chart{type: @bar data: [1, 2] next: [[3, 4], [5, 6]] switch: [2.0]}"#).contains("switch"));
    assert!(error(r#"Chart{type: @line data: [1, 2, 3] next: [[1, 2]]}"#).contains("same count"));
    assert!(error(r#"Chart{type: @bar csv: "a,b\nx,1" split: "year"}"#).contains("year"));
}

#[test]
fn led_cells_are_dark_before_they_spin_and_land_on_their_text() {
    let doc = load(&wrap(r#"Chart{type: @scoreboard theme: @benchmark layout: @line size: vec2(1300, 360) start: 1.0
        data: [["BRUTE", 57.9], ["SPARK", 10.9]] next: [[{label: "SPARK" value: 66.6 text: "LEAD"}]] switch: [4.0]}"#));
    let lit = |t: f64| -> usize {
        doc.eval(t).walk().into_iter().filter(|(_, item)| item.path.len() > 0).filter_map(|(_, item)| match &item.kind {
            // A lit dot is a circle: its first curve ends one radius below the start.
            ItemKind::Primitive(PrimitiveFrame::Path { path, fill: Some(_), .. }) if item.path.contains("layers") => Some(path.cmds.windows(2).filter(|pair| {
                matches!((&pair[0], &pair[1]), (geometry::PathCmd::MoveTo(_, y), geometry::PathCmd::BezierTo(_, _, _, _, _, end_y)) if end_y - y > 0.01)
            }).count()),
            _ => None,
        }).sum()
    };
    let before = lit(0.5);
    let after = lit(3.5);
    println!("before {before} after {after}");
    assert!(after > before, "{before} {after}");
}

#[test]
fn a_head_to_head_card_rolls_its_digits_onto_their_values() {
    let doc = load(&wrap(r#"Chart{type: @versus title: "SWE bench" size: vec2(1500, 760) data: [["Brute", 57.9], ["Spark", 10.9]] land: [2.0, 3.2]}"#));
    let digits = |t: f64| -> Vec<String> {
        let frame = doc.eval(t);
        let mut out: Vec<(f64, String)> = frame.walk().into_iter().filter_map(|(_, item)| match &item.kind {
            ItemKind::Primitive(PrimitiveFrame::Text { text, .. }) if item.opacity > 0.5 && item.path.contains("layers[0].layers") && text.len() == 1 => Some((item.world.apply([0.0, 0.0])[0], text.clone())),
            _ => None,
        }).collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.into_iter().map(|(_, t)| t).collect()
    };
    let end = digits(7.9).join("");
    assert!(end.contains("57.9") && end.contains("10.9"), "{end}");
    // Spark's wheel is still turning just before its word.
    let mid = digits(3.0).join("");
    assert!(!mid.contains("10.9"), "{mid}");
}

#[test]
fn a_scoreboard_moves_rows_to_their_new_ranks() {
    let doc = load(&wrap(r#"Chart{type: @scoreboard size: vec2(1300, 700) data: [["Brute", 57.9], ["Spark", 10.9]] next: [[["Brute", 57.9], ["Spark", 66.6]]] switch: [3.0]}"#));
    let y_of = |t: f64, name: &str| doc.eval(t).walk().into_iter().find_map(|(_, item)| match &item.kind {
        ItemKind::Primitive(PrimitiveFrame::Text { text, .. }) if text == name => Some(item.world.apply([0.0, 0.0])[1]),
        _ => None,
    }).unwrap();
    assert!(y_of(2.0, "BRUTE") < y_of(2.0, "SPARK"));
    assert!(y_of(7.0, "SPARK") < y_of(7.0, "BRUTE"));
}

#[test]
fn counters_count_time_in_motions_time_formats() {
    let doc = load(&wrap(r#"Chart{type: @counter data: [["race time", 2710]] format: @mmss}"#));
    let end = texts(&doc.eval(7.9));
    assert!(end.iter().any(|t| t == "45:10"), "{end:?}");
}

/// The fill's height of a thermometer meter at `t`.
fn fill_height(doc: &MotionDoc, t: f64) -> f64 {
    doc.eval(t).walk().into_iter().find_map(|(_, item)| match &item.kind {
        // The fill: narrower than the tube, taller than a tick.
        ItemKind::Primitive(PrimitiveFrame::Rect { size, radius, .. }) if *radius > 1.0 && size[1] > 3.0 && size[0] < 70.0 => Some(size[1]),
        _ => None,
    }).unwrap_or(0.0)
}

#[test]
fn a_meter_keyed_in_edit_time_reads_the_same_in_every_shot() {
    let source = wrap(r#"Chart{type: @meter title: "P(DOOM)" format: @percent size: vec2(420, 900) data: [9, 35, 99] land: [edit(2.0), edit(5.0), edit(8.0)]}"#);
    // The same document in two shots: one from edit 0, one from edit 4.
    let first = MotionDoc::load(&source).unwrap();
    let (second, _) = first.with_context(MotionContext { offset: 4.0, ..MotionContext::default() }).unwrap();
    for edit in [4.5, 5.0, 6.0, 7.9] {
        let (a, b) = (fill_height(&first, edit), fill_height(&second, edit - 4.0));
        assert!((a - b).abs() < 1e-6 && a > 0.0, "edit {edit}: {a} vs {b}");
    }
    // It holds between beats and rises at them.
    assert!((fill_height(&first, 3.0) - fill_height(&first, 4.0)).abs() < 1e-6);
    assert!(fill_height(&first, 7.9) > fill_height(&first, 4.0));
}

#[test]
fn a_clocked_counter_reads_its_clip_as_x_of_total_seconds() {
    let doc = load(&wrap(r#"Rect{name: "reel" span: [1.0, 15.0] size: vec2(4, 4) fill: #00000000}
        Chart{type: @counter clock: playhead("reel") format: @seconds total: 15}"#));
    let t = texts(&doc.eval(4.5));
    assert!(t.iter().any(|x| x == "3.5 / 15.0 s"), "{t:?}");
}

#[test]
fn a_rider_stays_on_its_line() {
    let doc = load(&wrap(r#"Chart{type: @line size: vec2(1400, 700) data: [["a", 1], ["b", 4], ["c", 2], ["d", 6]]
        rider: Rect{name: "rider" size: vec2(4, 4) fill: #ff0000} rider_rotate: true}"#));
    for t in [0.8, 1.5, 2.2, 3.5] {
        let frame = doc.eval(t);
        let (mut line, mut rider) = (None, None);
        for (_, item) in frame.walk() {
            match &item.kind {
                ItemKind::Primitive(PrimitiveFrame::Path { path, trim, stroke: Some(_), .. }) => line = Some((path.clone(), trim[1], item.world.apply([0.0, 0.0]))),
                ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) if *size == [4.0, 4.0] => rider = Some(item.world.apply([0.0, 0.0])),
                _ => {}
            }
        }
        let (path, trim, origin) = line.unwrap();
        let head = Polyline::from_path(&path).point(trim);
        let rider = rider.unwrap();
        assert!((head[0] + origin[0] - rider[0]).abs() < 1.0 && (head[1] + origin[1] - rider[1]).abs() < 1.0, "t {t}: head {head:?} rider {rider:?}");
    }
}

#[test]
fn a_log_axis_marker_climbs_with_its_labels() {
    let doc = load(&wrap(r#"Chart{type: @axis title: "P(DOOM)" size: vec2(900, 900) window: 4
        data: [["basically zero", 1e-9], ["hmm", 1e-5], ["uh oh", 0.01], ["doom", 0.99]] land: [1.0, 2.6, 4.2, 5.8]}"#));
    let at = |t: f64| texts(&doc.eval(t));
    println!("{:?}", at(1.6));
    assert!(at(1.6).iter().any(|x| x == "1e-9") && at(1.6).iter().any(|x| x == "basically zero"), "{:?}", at(1.6));
    assert!(at(7.5).iter().any(|x| x == "0.99") && at(7.5).iter().any(|x| x == "doom"), "{:?}", at(7.5));
    // The window scrolled: the top decade shown at the end is 1.
    assert!(at(7.5).iter().any(|x| x == "1") && !at(7.5).iter().any(|x| x == "1e-9"));
}

#[test]
fn a_gantt_review_colours_each_task_as_today_passes_it() {
    let doc = load(&wrap(r#"Chart{type: @gantt csv: "task,start,end,status\nA,0,3,done\nB,2,8,late" review: {from: 0 to: 10 at: 3.0 length: 4.0}}"#));
    let fill = |t: f64, name: &str| doc.eval(t).walk().into_iter().find_map(|(_, item)| match &item.kind {
        ItemKind::Primitive(PrimitiveFrame::Rect { fill: Some(f), .. }) if doc.all_layers().iter().any(|l| l.path == item.path && l.name.as_deref() == Some(name)) => Some(*f),
        _ => None,
    }).unwrap();
    // Before the line reaches A's end (week 3 = 4.2 s) A is neutral; after, green.
    let (early, late) = (fill(3.5, "task A"), fill(5.0, "task A"));
    assert!(early != late && late[1] > late[0], "{early:?} {late:?}");
    // B (ends week 8 = 6.2 s) is still neutral at 5.0 and red at 7.5.
    let b = fill(7.5, "task B");
    assert!(fill(5.0, "task B") != b && b[0] > b[1], "{b:?}");
    let t = texts(&doc.eval(7.5));
    assert!(t.iter().any(|x| x == "DONE") && t.iter().any(|x| x == "LATE") && t.iter().any(|x| x == "TODAY"), "{t:?}");
}

#[test]
fn an_activation_wave_reaches_nodes_hop_by_hop() {
    let doc = load(&wrap(r#"Chart{type: @network layout: @layers data: [{label: "a" group: "in"}, {label: "b" group: "mid"}, {label: "c" group: "out"}]
        links: [["a", "b"], ["b", "c"]] activate: [{from: "a" at: 2.0 hop: 1.0}]}"#));
    let lit = |t: f64| doc.eval(t).walk().into_iter().filter(|(_, item)| matches!(item.kind, ItemKind::Primitive(PrimitiveFrame::Ellipse { fill: Some(_), .. })) && item.opacity > 0.3
        && doc.all_layers().iter().any(|l| l.path == item.path && l.name.as_deref() == Some("node lit"))).count();
    assert_eq!((lit(1.9), lit(2.1), lit(3.1), lit(4.1)), (0, 1, 2, 3));
}

#[test]
fn a_probability_table_lights_the_sampled_token_per_step() {
    let doc = load(&wrap(r#"Chart{type: @probs size: vec2(1000, 520) data: [["up", 0.61], ["down", 0.12]]
        next: [[{label: "my" value: 0.3} {label: "the" value: 0.5}, {label: "your" value: 0.2 pick: true}]] switch: [3.0]}"#));
    let lit = |t: f64| doc.eval(t).walk().into_iter().find_map(|(_, item)| match &item.kind {
        ItemKind::Primitive(PrimitiveFrame::Text { text, units, .. }) if item.opacity > 0.5 && text.starts_with('"') && units.first().is_some_and(|u| u.color[0] > 0.9 && u.color[2] < 0.4) => Some(text.clone()),
        _ => None,
    });
    assert_eq!(lit(2.0).as_deref(), Some("\"up\""));
    assert_eq!(lit(5.0).as_deref(), Some("\"your\""));
}

#[test]
fn a_drum_lands_on_every_digit_even_for_huge_counts() {
    for (value, want) in [(1284000.0, "1284000"), (1e30, "1000000000000000000000000000000")] {
        let doc = load(&wrap(&format!(r#"Chart{{type: @counter face: @drum data: [["n", {value}]] land: [3.0] grow: 2.0}}"#)));
        let frame = doc.eval(7.9);
        let mut digits: Vec<(f64, String)> = frame.walk().into_iter().filter_map(|(_, item)| match &item.kind {
            ItemKind::Primitive(PrimitiveFrame::Text { text, .. }) if item.opacity > 0.5 && text.len() == 1 && text.chars().all(|c| c.is_ascii_digit())
                && doc.all_layers().iter().any(|l| l.path == item.path && l.name.as_deref().is_some_and(|n| n.starts_with("value ·"))) => Some((item.world.apply([0.0, 0.0])[0], text.clone())),
            _ => None,
        }).collect();
        digits.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(digits.into_iter().map(|d| d.1).collect::<String>(), want);
    }
}

#[test]
fn an_escalating_counter_reads_as_words_then_science() {
    for (value, want) in [(12400000.0, "12.4 million"), (1e42, "1.0e42")] {
        let doc = load(&wrap(&format!(r#"Chart{{type: @counter format: @escalate data: [["n", {value}]]}}"#)));
        let end = texts(&doc.eval(7.9));
        assert!(end.iter().any(|t| t == want), "{end:?}");
    }
}
