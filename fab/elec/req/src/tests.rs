use crate::*;
use fab_elec_core::{DocId, Sel, Severity};

const LEDCTL: &str = include_str!("../../fixtures/ledctl.req.splash");

/// The first error's message and the text it points at.
fn first_error(text: &str) -> (String, String) {
    let (_, errors) = read(DocId(3), text);
    let e = errors.first().unwrap_or_else(|| panic!("{text:?} should not read"));
    let Some(Sel::Source { doc, start, end }) = e.sel.first() else { panic!("{text:?}: the error is not located: {e:?}") };
    assert_eq!(*doc, DocId(3), "{text:?}: located in its own document");
    (e.message.clone(), text[*start as usize..*end as usize].to_owned())
}

#[test]
fn reads_every_statement_form_and_locates_each_mistake() {
    let (doc, errors) = read(DocId(7), LEDCTL);
    assert!(errors.is_empty(), "the fixture reads cleanly: {errors:?}");
    assert_eq!((doc.requirements.len(), doc.waivers.len()), (11, 1), "every require and waive statement");
    let r = |name: &str| doc.requirements.iter().find(|r| r.name == name).unwrap_or_else(|| panic!("{name} read"));
    assert_eq!(r("fits a 60 x 40 case").measure, Measure::BoardSize("ledctl".into()));
    assert_eq!(r("fits a 60 x 40 case").bound, Bound::Range { min: None, max: Some(Value::Size([0.06, 0.04])) }, "a size bound in metres");
    assert_eq!(r("DRC clean").measure, Measure::Checks { family: Family::Drc, unit: Some("ledctl".into()), count: CountOf::Errors });
    assert_eq!(r("sits in its case").bound, Bound::Holds, "a pass measure takes no bound");
    assert_eq!(r("3V3 holds").measure, Measure::BenchExpect { bench: "bringup".into(), expect: "3V3 up".into() });
    assert_eq!(r("prints in 3 hours").bound, Bound::Range { min: None, max: Some(Value::Scalar(3.0 * 3600.0)) }, "hours read as seconds");
    assert_eq!(r("under 40 g of filament").bound, Bound::Range { min: None, max: Some(Value::Scalar(40.0)) }, "grams");
    let first = &doc.requirements[0];
    assert!(LEDCTL[first.span.start as usize..first.span.end as usize].starts_with("require(\"fits a 60 x 40 case\""), "spans cover the statement");
    assert_eq!(first.measure_text, "board(\"ledctl\").size");
    assert_eq!(first.bound_text, "{max: [60mm 40mm]}");
    assert_eq!(doc.waivers[0].reason, "first prototype: time is no concern");

    // Each mistake is an error at what is wrong, saying how to write it.
    let cases: &[(&str, &str, &str)] = &[
        ("require(\"a\", board(\"x\").colour, {max: 1})", "not colour", "board(\"x\").colour"),
        ("require(\"a\", wattage, {max: 1})", "not a measure", "wattage"),
        ("require(\"a\", print.time, {max: 3mm})", "a time", "3mm"),
        ("require(\"a\", board(\"x\").layers, {max: 2mm})", "whole number", "2mm"),
        ("require(\"a\", board(\"x\").size, {max: 60mm})", "[w h]", "60mm"),
        ("require(\"a\", case(\"x\").fits, {max: 0})", "takes no bound", "{max: 0}"),
        ("require(\"a\", board(\"x\").layers)", "give a bound", "require(\"a\", board(\"x\").layers)"),
        ("require(\"a\", board(\"x\").layers, {is: 2 max: 4})", "stands alone", "{is: 2 max: 4}"),
        ("require(\"a\", board(\"x\").layers, {min: 4 max: 2})", "min is above max", "{min: 4 max: 2}"),
        ("require(\"a\", board(\"x\").layers, {max: 2 tol: 1})", "tol goes with is", "{max: 2 tol: 1}"),
        ("require(\"a\", board(\"x\").layers, {most: 2})", "not most", "{most: 2}"),
        ("require(\"a\", checks(@everything).errors, {max: 0})", "takes a family", "checks(@everything).errors"),
        ("require(\"\", print.time, {max: 1h})", "empty", "\"\""),
        ("require(\"a\", print.time, {max: 1h})\nrequire(\"a\", print.time, {max: 2h})", "two requirements", "\"a\""),
        ("waive(\"a\", \" \")", "empty", "\" \""),
        ("waive(\"a\", \"x\")\nwaive(\"a\", \"y\")", "waived twice", "\"a\""),
        ("let x = 3", "holds require", "let x = 3"),
    ];
    for (text, says, at) in cases {
        let (message, located) = first_error(text);
        assert!(message.contains(says), "{text:?}: the error says {says:?}, not {message:?}");
        assert_eq!(located, *at, "{text:?}: the error points at {at:?}");
    }

    // A line that does not parse is an error there, and the lines around it still read.
    let (doc, errors) = read(DocId(1), "require(\"a\", print.time, {max: 1h})\nrequire(\"b\", (\nrequire(\"c\", print.filament, {max: 5g})\n");
    assert_eq!(errors.len(), 1, "one syntax error: {errors:?}");
    assert!(errors[0].message.starts_with("syntax:"), "{errors:?}");
    assert_eq!(doc.requirements.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["a", "c"], "the other lines read");
}

/// Facts in which the ledctl fixture's rows land in every status.
fn facts() -> Facts {
    let finding = |id, unit: Option<&str>, family, severity, unconnected| FindingFact { id, unit: unit.map(Into::into), family, severity, unconnected };
    Facts {
        units: vec![UnitFacts { unit: "ledctl".into(), board: Some(BoardMeasures { size: Some([0.05, 0.03]), layers: 2, parts: 31, unplaced: 1 }) }],
        checks: Some(CheckFacts {
            current: true,
            pending: vec![],
            findings: vec![
                finding(11, Some("ledctl"), Family::Drc, Severity::Error, true),
                finding(12, Some("ledctl"), Family::Drc, Severity::Warning, false),
                finding(13, Some("ledctl"), Family::Erc, Severity::Warning, false),
            ],
        }),
        cases: vec![CaseFacts { unit: "ledctl".into(), has_case: true, current: false, errors: vec![], failed: None }],
        benches: vec![BenchFacts {
            bench: "bringup".into(),
            run: Some(BenchRun {
                current: true,
                at: 5.0,
                outcomes: vec![
                    Outcome { name: "LED0".into(), verdict: Verdict::Pass, value: Some("4.98 mA".into()) },
                    Outcome { name: "3V3 up".into(), verdict: Verdict::Pass, value: Some("3.3 V".into()) },
                ],
            }),
        }],
        print: Some(PrintFacts { current: true, at: 9.0, seconds: 4.0 * 3600.0 + 12.0 * 60.0, grams: 38.5 }),
        producers: EVERY_PRODUCER,
    }
}

const EVERY_PRODUCER: Producers = Producers { cases: true, benches: true, print: true };

#[test]
fn judges_pass_fail_unknown_waived_and_stale() {
    let (doc, _) = read(DocId(7), LEDCTL);
    let docs = [doc];
    let report = judge(&docs, &facts());
    let row = |name: &str| report.row(name).unwrap_or_else(|| panic!("a row for {name}"));
    let status = |name: &str| row(name).status.clone();

    assert_eq!(status("fits a 60 x 40 case"), Status::Pass, "50 x 30 mm is within 60 x 40 mm");
    assert_eq!(row("fits a 60 x 40 case").value.as_deref(), Some("50 x 30 mm"), "the value in mm");
    assert_eq!(row("fits a 60 x 40 case").bound, "max 60 x 40 mm", "the bound in words");
    assert_eq!(status("two layers"), Status::Pass);
    assert_eq!(status("every part placed"), Status::Fail, "one part is unplaced");
    assert_eq!(status("fully routed"), Status::Fail, "an Unconnected finding is unrouted");
    assert_eq!(row("fully routed").findings, vec![11], "its evidence is the finding");
    assert_eq!(status("ERC clean"), Status::Pass, "an ERC warning is not an error");
    assert_eq!(status("DRC clean"), Status::Fail, "the DRC error counts");
    assert!(matches!(status("sits in its case"), Status::Unknown(why) if why.starts_with("stale:")), "a case check not run on the newest edit is unknown");
    assert!(row("sits in its case").stale && row("sits in its case").value.as_deref() == Some("no case errors"), "its last value still shows");
    assert_eq!(status("LEDs light"), Status::Pass, "every expectation passed");
    assert_eq!(row("LEDs light").measured_at, Some(5.0), "when the bench was checked");
    assert_eq!(status("3V3 holds"), Status::Pass);
    assert_eq!(row("3V3 holds").value.as_deref(), Some("3.3 V"), "the expectation's value");
    assert_eq!(row("prints in 3 hours").value.as_deref(), Some("4 h 12 min"));
    assert_eq!(status("prints in 3 hours"), Status::Waived { reason: "first prototype: time is no concern".into(), was: Box::new(Status::Fail) }, "waived over a fail");
    assert_eq!(status("under 40 g of filament"), Status::Pass);
    assert_eq!((report.pass, report.fail, report.unknown, report.waived), (6, 3, 1, 1), "totals");
    assert!(!report.met(), "a fail or an unknown is not met");
    assert_eq!(report.met_count(), (7, 11), "pass and waived count as met");

    // Nothing measured yet: every row that needs a producer is unknown, with why; never a pass.
    let empty = judge(&docs, &Facts::default());
    for row in &empty.rows {
        assert!(matches!(row.status, Status::Unknown(_) | Status::Waived { .. }), "{}: unknown without facts, not {:?}", row.name, row.status);
    }
    assert!(matches!(&empty.row("fits a 60 x 40 case").unwrap().status, Status::Unknown(why) if why.contains("no unit named \"ledctl\"")), "says the unit is missing");
    assert!(matches!(&empty.row("LEDs light").unwrap().status, Status::Unknown(why) if why.contains("no bench named")), "says the bench is missing");
    assert!(matches!(&empty.row("under 40 g of filament").unwrap().status, Status::Unknown(why) if why == "the slice estimate is not read here"),
        "a host without the slicer says so, never \"not sliced\"");
    let unread = Facts { producers: Producers::default(), ..facts() };
    for (name, why) in [("sits in its case", "the case checks are not read here"), ("LEDs light", "Check bench results are not read here")] {
        assert!(matches!(&judge(&docs, &unread).row(name).unwrap().status, Status::Unknown(w) if w == why), "{name}: a producer this host does not read answers {why:?}");
    }
    let case_why = |cases: Vec<CaseFacts>| match &judge(&docs, &Facts { cases, ..facts() }).row("sits in its case").unwrap().status {
        Status::Unknown(why) => why.clone(),
        other => panic!("a board without case results is unknown, not {other:?}"),
    };
    assert_eq!(case_why(vec![]), "the case checks have not run on ledctl yet", "a board the case checks have not reached");
    assert_eq!(case_why(vec![CaseFacts { unit: "ledctl".into(), has_case: false, current: true, errors: vec![], failed: None }]), "ledctl has no case on this plate",
        "a board the checks reached that has no case");
    let unsliced = judge(&docs, &Facts { producers: EVERY_PRODUCER, ..Facts::default() });
    assert!(matches!(&unsliced.row("under 40 g of filament").unwrap().status, Status::Unknown(why) if why.contains("not sliced yet")), "with the slicer and no slice, says to slice");
    assert!(!empty.met());

    // Checks still running: their rows are unknown and stale; a fixed DRC then passes.
    let mut running = facts();
    let checks = running.checks.as_mut().unwrap();
    checks.current = false;
    checks.pending = vec!["DRC".into()];
    let report = judge(&docs, &running);
    assert!(matches!(&report.row("DRC clean").unwrap().status, Status::Unknown(why) if why.contains("still running: DRC")), "pending checks are unknown");
    let mut fixed = facts();
    fixed.checks.as_mut().unwrap().findings.retain(|f| f.id != 11);
    let report = judge(&docs, &fixed);
    assert_eq!(report.row("DRC clean").unwrap().status, Status::Pass, "no DRC error left");
    assert_eq!(report.row("fully routed").unwrap().status, Status::Pass, "nothing unconnected");

    // A bench expectation not judged leaves the bench unknown unless another failed.
    let mut unjudged = facts();
    unjudged.benches[0].run.as_mut().unwrap().outcomes[0].verdict = Verdict::NotJudged;
    let report = judge(&docs, &unjudged);
    assert!(matches!(report.row("LEDs light").unwrap().status, Status::Unknown(_)), "not judged is not a pass");

    // Across documents: a name used twice and a waiver of nothing are located notes.
    let (other, errors) = read(DocId(8), "require(\"two layers\", board(\"ledctl\").layers, {is: 2})\nwaive(\"no such\", \"why\")\n");
    assert!(errors.is_empty(), "{errors:?}");
    let report = judge(&[docs[0].clone(), other], &facts());
    assert_eq!(report.rows.len(), 11, "the second \"two layers\" is not a row");
    assert!(report.notes.iter().any(|n| n.severity == Severity::Error && n.message.contains("two requirements are named \"two layers\"")), "{:?}", report.notes);
    assert!(report.notes.iter().any(|n| n.severity == Severity::Warning && n.message.contains("no requirement")), "{:?}", report.notes);

    // `is` with its default tolerances.
    let (exact, _) = read(DocId(9), "require(\"size\", board(\"ledctl\").size, {is: [50.04mm 30mm]})\nrequire(\"far\", board(\"ledctl\").size, {is: [50.1mm 30mm]})\nrequire(\"time\", print.time, {is: 4.2h})\nrequire(\"tight\", print.time, {is: 4h tol: 1min})\n");
    let report = judge(&[exact], &facts());
    let s = |n: &str| report.row(n).unwrap().status.clone();
    assert_eq!((s("size"), s("far"), s("time"), s("tight")), (Status::Pass, Status::Fail, Status::Pass, Status::Fail), "0.05 mm for lengths, 1% for times, tol when given");
    assert_eq!(report.row("tight").unwrap().bound, "4 h ±1 min");
}

/// Why: `elec.req.add`, `.remove`, `.waive` and `.unwaive` write one statement each, leave the rest
/// of the document as it was, and refuse what would not read as asked.
#[test]
fn writes_each_statement_as_one_edit() {
    let doc = |text: &str| {
        let (doc, errors) = read(DocId(3), text);
        assert!(errors.is_empty(), "{text}\nreads without mistakes: {errors:?}");
        doc
    };
    let ledctl = doc(LEDCTL);
    let names = |d: &ReqDoc| d.requirements.iter().map(|r| r.name.clone()).collect::<Vec<_>>();

    let added = write::add(LEDCTL, &ledctl, "under 25 g", "board(\"ledctl\").layers", Some("{max: 4}")).unwrap();
    let after = doc(&added);
    assert_eq!(names(&after).last().map(String::as_str), Some("under 25 g"), "the new requirement follows the last one");
    assert!(added.find("under 25 g").unwrap() < added.find("waive(").unwrap(), "it goes before the waivers, after the requirements");
    assert_eq!(added.replace("require(\"under 25 g\", board(\"ledctl\").layers, {max: 4})\n", ""), LEDCTL, "nothing else moves");
    assert!(write::add(LEDCTL, &ledctl, "two layers", "board(\"ledctl\").layers", None).unwrap_err().contains("there already"), "a name is unique");
    for measure in ["board(\"ledctl\").layers, {max: 2})\nrequire(\"y\", board(\"ledctl\").layers, {max: 2}", "board(\"ledctl\").layers, {max: 2})\nrequire(\"y\", board(\"ledctl\").layers"] {
        let smuggled = write::add(LEDCTL, &ledctl, "x", measure, None);
        assert!(smuggled.unwrap_err().contains("does not read as one requirement"), "a measure that opens a second statement, read or not, is refused: {measure}");
    }
    assert_eq!(write::add("", &doc(""), "two layers", "board(\"b\").layers", Some("{max: 2}")).unwrap(), "require(\"two layers\", board(\"b\").layers, {max: 2})\n", "the first statement of an empty document");

    let removed = doc(&write::remove(LEDCTL, &ledctl, "prints in 3 hours").unwrap());
    assert!(!names(&removed).iter().any(|n| n == "prints in 3 hours") && removed.waivers.is_empty(), "remove takes its waiver with it");
    assert_eq!(removed.requirements.len(), ledctl.requirements.len() - 1, "one requirement goes");
    assert!(write::remove(LEDCTL, &ledctl, "fits a 6 x 4 case").unwrap_err().contains("\"fits a 60 x 40 case\""), "an unknown name lists the names there are");

    let waived = doc(&write::waive(LEDCTL, &ledctl, "LEDs light", "the bench is not wired yet", false).unwrap());
    assert_eq!(waived.waivers.iter().map(|w| (w.name.as_str(), w.reason.as_str())).collect::<Vec<_>>(),
        [("prints in 3 hours", "first prototype: time is no concern"), ("LEDs light", "the bench is not wired yet")], "a new waiver goes at the end");
    let rewaived = doc(&write::waive(LEDCTL, &ledctl, "prints in 3 hours", "a slow printer is fine", false).unwrap());
    assert_eq!(rewaived.waivers.len(), 1, "a second waiver of the same requirement replaces its reason");
    assert_eq!(rewaived.waivers[0].reason, "a slow printer is fine");
    assert!(write::waive(LEDCTL, &ledctl, "LEDs light", "  ", false).unwrap_err().starts_with("reason:"), "a waiver needs a reason");

    let unwaived = write::unwaive(LEDCTL, &ledctl, "prints in 3 hours").unwrap();
    assert!(doc(&unwaived).waivers.is_empty() && !unwaived.contains("waive("), "unwaive removes the statement and its line");
    assert!(write::unwaive(LEDCTL, &ledctl, "two layers").unwrap_err().contains("not waived"), "only a waived requirement unwaives");

    // The AI proposes a waiver; only the person's own waive waives.
    let proposed_text = write::waive(LEDCTL, &ledctl, "DRC clean", "routing comes next", true).unwrap();
    assert!(proposed_text.ends_with("propose_waiver(\"DRC clean\", \"routing comes next\")\n"), "the AI's waiver is a proposal: {proposed_text}");
    let proposed = doc(&proposed_text);
    let judged = judge(std::slice::from_ref(&proposed), &facts());
    let row = judged.row("DRC clean").unwrap();
    assert!(!matches!(row.status, Status::Waived { .. }) && row.proposed.as_deref() == Some("routing comes next") && judged.proposed == 1,
        "a proposal waives nothing; the row carries it: {row:?}");
    let accepted = doc(&write::waive(&proposed_text, &proposed, "DRC clean", "routing comes next", false).unwrap());
    assert_eq!(accepted.waivers.iter().filter(|w| w.name == "DRC clean").map(|w| w.proposed).collect::<Vec<_>>(), [false], "the person's waive takes the proposal's place");
    assert_eq!(write::unwaive(&proposed_text, &proposed, "DRC clean").unwrap(), LEDCTL, "declining takes the proposal out");
    assert!(write::waive(LEDCTL, &ledctl, "prints in 3 hours", "faster", true).unwrap_err().contains("the person waived"), "the AI proposes nothing over the person's waiver");
}
