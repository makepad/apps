use crate::error::CamError;
use crate::ipc356::{read_ipc356, write_ipc356, IpcFile, IpcOpts};
use fab_elec_board::{Board, BoardNet, Flash, Origin, Placed, StackKind, Stackup, Tent, Via, ViaKind};
use fab_elec_circuit::Circuit;
use fab_elec_core::{mm, Angle, BoardId, ClassId, CompId, Contour, LayerId, LayerSet, NetId, Nm, PadIx, Side, P};
use fab_elec_parts::{Drill, Footprint, Pad, PadKind, PadProp, PadShape, PadStack};
use std::sync::Arc;

fn board(names: &[&str]) -> Board {
    let mut board = Board::new(BoardId(0), "oracle");
    board.stackup = Stackup::standard_2_layer();
    board.nets = names.iter().map(|name| BoardNet { name: (*name).into(), class: ClassId(0) }).collect();
    board
}

fn four_layers(board: &mut Board) {
    let mut inner = board.stackup.layers.iter().find(|l| matches!(l.kind, StackKind::Copper(_))).unwrap().clone();
    let back = board.stackup.layers.iter().position(|l| l.kind == StackKind::Copper(LayerId::B_CU)).unwrap();
    inner.kind = StackKind::Copper(LayerId::inner(1));
    board.stackup.layers.insert(back, inner.clone());
    inner.kind = StackKind::Copper(LayerId::inner(2));
    board.stackup.layers.insert(back + 1, inner);
}

fn opts() -> IpcOpts { IpcOpts { job: "oracle".into(), ..IpcOpts::default() } }

fn pad(no: &str, kind: PadKind, stack: PadStack, drill: Option<P>) -> Pad {
    Pad {
        no: no.into(), kind, stack, at: P::ZERO, rot: Angle::ZERO,
        drill: drill.map(|size| Drill { size, offset: P::ZERO }), mask_margin: None,
        paste: Default::default(), thermal: None, zone_connect: None, die_len: 0, prop: PadProp::None,
    }
}

fn smd() -> Pad {
    pad("1", PadKind::Smd, PadStack::smd(PadShape::Rect { size: P::mm(1.016, 0.508) }), None)
}

fn place<'a>(board: &'a mut Board, net: Option<usize>, refdes: &str, at: P, pad: Pad) -> &'a mut Placed {
    let id = board.alloc_id();
    board.placed.push(Placed {
        id, comp: CompId(0), path: refdes.into(), refdes: refdes.into(),
        footprint: Arc::new(Footprint { pads: vec![pad], ..Footprint::default() }),
        pad_nets: vec![net.map(|n| NetId(n as u32))], at, rot: Angle::ZERO, side: Side::Top,
        locked: false, origin: Origin::Authored, body_z: None, ref_label: None, value_label: None, legend: None, value: None,
    });
    board.placed.last_mut().unwrap()
}

fn via(board: &mut Board, net: usize, at: P) -> &mut Via {
    let id = board.alloc_id();
    board.vias.push(Via {
        id, net: NetId(net as u32), at, drill: mm(0.3), pad: mm(0.6),
        span: (LayerId::F_CU, LayerId::B_CU), kind: ViaKind::Through, tent: Tent::Both,
        backdrill: None, locked: false, origin: Origin::Authored,
        flash: Flash::All,
    });
    board.vias.last_mut().unwrap()
}

fn export(board: &Board, opts: &IpcOpts) -> (Vec<String>, IpcFile) {
    let bytes = write_ipc356(board, &Circuit::default(), opts);
    let text = String::from_utf8(bytes.clone()).expect("S46.R4/Q1: ASCII output");
    assert!(text.ends_with('\n') && !text.contains('\r'), "S46.R3: LF on every platform");
    let lines: Vec<_> = text.lines().map(str::to_owned).collect();
    assert!(lines.iter().all(|l| l.len() == 80), "S46.R4: every record has 80 columns");
    let file = read_ipc356(&bytes).expect("S46.R23-R27: reader accepts writer output");
    (lines, file)
}

fn expect_records(board: &Board, opts: &IpcOpts, expected: &[&str], requirement: &str) -> IpcFile {
    let (lines, file) = export(board, opts);
    assert!(expected.iter().all(|l| l.len() == 80), "S46.R4: full 80-column oracle expectations; {requirement}");
    assert_eq!(&lines[6..lines.len() - 1], expected, "{requirement}");
    file
}

#[test]
fn ipc356_oracle_point_records() {
    let origin = P::mm(20.0, 30.0);
    let opts = IpcOpts { origin, ..opts() };
    // O4: through pad; a deliberately unrelated outline must not become the origin.
    {
        let mut b = board(&["GND"]);
        b.outline.push(Contour::rect(P::mm(-100.0, -100.0), P::ZERO, 0));
        let p = pad("1", PadKind::Thru, PadStack::thru(PadShape::Circle { d: mm(1.7) }), Some(P::mm(1.0, 1.0)));
        place(&mut b, Some(0), "J1", origin.add(P::mm(10.16, -5.08)), p);
        let file = expect_records(&b, &opts, &[
            "317GND              J1    -1    D0394PA00X+004000Y+002000X0669Y0000R000 S0      ",
        ], "O4 S46.R7/R11/R14-R16/R19: round plated pad and auxiliary origin");
        assert_eq!(file.points[0].at.board_at(origin), Some(origin.add(P::mm(10.16, -5.08))), "S46.R27: restore board axes and origin");
    }
    // O5 and O16: dimensions stay in the pad's own frame; rotation truncates.
    {
        let mut b = board(&["/SDA"]);
        let mut p = pad("12", PadKind::Smd, PadStack::smd(PadShape::Rect { size: P::mm(1.5, 0.6) }), None);
        p.rot = Angle::QUARTER;
        place(&mut b, Some(0), "U1", origin.add(P::mm(2.54, 1.27)), p);
        expect_records(&b, &opts, &[
            "327/SDA             U1    -12         A01X+001000Y-000500X0591Y0236R270 S2      ",
        ], "O5 S46.R12/R14/R15/R17/R18: front rectangle, rotation and mask");
        Arc::make_mut(&mut b.placed[0].footprint).pads[0].rot = Angle::deg(359.7);
        expect_records(&b, &opts, &[
            "327/SDA             U1    -12         A01X+001000Y-000500X0591Y0236R001 S2      ",
        ], "O16 S46.R17: 359.7 degrees writes R001");
    }
    // O6: every via is a midpoint, even when connected.
    {
        let mut b = board(&["VCC"]);
        via(&mut b, 0, origin);
        expect_records(&b, &opts, &[
            "317VCC              VIA        MD0118PA00X+000000Y+000000X0236Y0000R000 S3      ",
        ], "O6 S46.R13/R15/R18/R19: through via, zero Y size, both sides tented");
    }
    // O7: surface continuation has no midpoint or drill, and does not add a point.
    {
        let mut b = board(&["SIG1"]);
        four_layers(&mut b);
        let v = via(&mut b, 0, origin.add(P::mm(5.0, -7.0)));
        v.span = (LayerId::F_CU, LayerId::inner(2));
        v.kind = ViaKind::Blind;
        v.pad = 317_500;
        v.drill = 127_000;
        v.tent = Tent::None;
        let file = expect_records(&b, &opts, &[
            "307SIG1             VIA        MD0050PA01X+001969Y+002756               S0L01L03",
            "027SIG1             VIA               A01X+001969Y+002756X0125Y0000R000 S0      ",
        ], "O7 S46.R13/R18/R20: blind via and surface continuation");
        assert_eq!(file.points.len(), 1, "S46.R25: continuation belongs to the previous via");
        assert_eq!((file.points[0].size, file.points[0].layer_span), ((317_500, 0), Some((1, 3))), "S46.R25: continuation restores surface size");
    }
    // O8: NPTH pad with copper layer membership remains an unconnected midpoint.
    {
        let mut b = board(&[]);
        let p = pad("", PadKind::Npth, PadStack::thru(PadShape::Circle { d: mm(3.2) }), Some(P::mm(3.2, 3.2)));
        place(&mut b, None, "H1", origin.add(P::mm(-3.0, -3.0)), p);
        let file = expect_records(&b, &opts, &[
            "367N/C              H1         MD1260UA00X-001181Y+001181X1260Y0000R000 S0      ",
        ], "O8 S46.R8/R11/R19: unplated mounting hole, no net or pin");
        assert!(!file.points[0].plated && file.points[0].net.is_empty(), "S46.R25/R26: unplated and unconnected survive reading");
    }
}

#[test]
fn ipc356_oracle_net_names() {
    // O9: NNAME has no separator between keyword and alias.
    {
        let mut b = board(&["/inout_user/CAN_H"]);
        place(&mut b, Some(0), "J1", P::ZERO, smd());
        let file = expect_records(&b, &opts(), &[
            "P  NNAMEM0000 /inout_user/CAN_H                                                 ",
            "327M0000            J1    -1          A01X+000000Y+000000X0400Y0200R000 S2      ",
        ], "O9 S46.R9/R10: first hierarchical alias");
        assert!(file.points[0].matches_net("/inout_user/CAN_H"), "S46.R26: alias matches source name");
    }
    // O10: both canonical-name collisions are aliased in source-name order.
    {
        let mut b = board(&["Gnd", "GND"]);
        place(&mut b, Some(0), "J1", P::ZERO, smd());
        place(&mut b, Some(1), "J2", P::ZERO, smd());
        expect_records(&b, &opts(), &[
            "P  NNAMEM0000 GND                                                               ",
            "P  NNAMEM0001 Gnd                                                               ",
            "327M0000            J2    -1          A01X+000000Y+000000X0400Y0200R000 S2      ",
            "327M0001            J1    -1          A01X+000000Y+000000X0400Y0200R000 S2      ",
        ], "O10 S46.R8/R9/R21: canonical collisions and written-name order");
    }
    // O11: a literal N/C net differs from no net.
    {
        let mut b = board(&["N/C"]);
        place(&mut b, None, "J1", P::ZERO, smd());
        place(&mut b, Some(0), "J2", P::ZERO, smd());
        let file = expect_records(&b, &opts(), &[
            "P  NNAMEM0000 N/C                                                               ",
            "327M0000            J2    -1          A01X+000000Y+000000X0400Y0200R000 S2      ",
            "327N/C              J1    -1          A01X+000000Y+000000X0400Y0200R000 S2      ",
        ], "O11 S46.R8/R9: N/C source net needs an alias");
        assert!(file.points[0].matches_net("N/C") && !file.points[0].matches_net(""), "S46.R26: aliased N/C is connected");
        assert!(file.points[1].matches_net("") && !file.points[1].matches_net("N/C"), "S46.R26: unconnected N/C is empty");
    }
    // O12: spaces are substituted before ASCII upper-casing.
    {
        let mut b = board(&["a b"]);
        place(&mut b, Some(0), "J1", P::ZERO, smd());
        let file = expect_records(&b, &opts(), &[
            "327A?B              J1    -1          A01X+000000Y+000000X0400Y0200R000 S2      ",
        ], "O12 S46.R8: canonical substitution");
        assert!(file.points[0].matches_net("a b"), "S46.R26: canonical net matches source");
    }
    // O13: a 15-character name cannot be used directly.
    {
        let mut b = board(&["abcdefghijklmno"]);
        place(&mut b, Some(0), "J1", P::ZERO, smd());
        expect_records(&b, &opts(), &[
            "P  NNAMEM0000 abcdefghijklmno                                                   ",
            "327M0000            J1    -1          A01X+000000Y+000000X0400Y0200R000 S2      ",
        ], "O13 S46.R8-R10: alias boundary at 15 characters");
    }
}

#[test]
fn ipc356_oracle_options_and_header() {
    let mut b = board(&[""]);
    place(&mut b, None, "J1", P::ZERO, smd());
    via(&mut b, 0, P::ZERO);
    // O14: the GUI option skips pads, but never vias.
    expect_records(&b, &IpcOpts { skip_unconnected: true, ..opts() }, &[
        "317N/C              VIA        MD0118PA00X+000000Y+000000X0236Y0000R000 S3      ",
    ], "O14 S46.R1/R13: keep unconnected via while skipping unconnected pad");
    // O15: the job/default export writes both, with the via first.
    expect_records(&b, &opts(), &[
        "317N/C              VIA        MD0118PA00X+000000Y+000000X0236Y0000R000 S3      ",
        "327N/C              J1    -1          A01X+000000Y+000000X0400Y0200R000 S2      ",
    ], "O15 S46.R2/R21: job defaults include every pad");
    // O17: all six headers and the end record, including case and column 10/15 values.
    let (lines, _) = export(&board(&[]), &opts());
    assert_eq!(lines, [
        "P  JOB   oracle                                                                 ",
        "P  CODE  00                                                                     ",
        "P  UNITS CUST 0                                                                 ",
        "P  arrayDim   N                                                                 ",
        "P  VER   IPC-D-356A                                                             ",
        "P  IMAGE PRIMARY                                                                ",
        "999                                                                             ",
    ], "O17 S46.R4-R6: exact header and terminator");
}

#[test]
fn ipc356_alias_reservations_tail_and_stable_order() {
    // Reserve a real M0000, then cross the base-36 Z boundary.
    let mut b = board(&["M0000"]);
    place(&mut b, Some(0), "J1", P::ZERO, smd());
    for i in 0..36 {
        b.nets.push(BoardNet { name: format!("long_signal_{i:04}"), class: ClassId(0) });
        place(&mut b, Some(i + 1), "J1", P::ZERO, smd());
    }
    let (lines, file) = export(&b, &opts());
    assert_eq!(lines[6], "P  NNAMEM0001 long_signal_0000                                                  ", "S46.R9: skip reserved canonical alias");
    assert_eq!(lines[40], "P  NNAMEM000Z long_signal_0034                                                  ", "S46.R9: base-36 digit Z");
    assert_eq!(lines[41], "P  NNAMEM0010 long_signal_0035                                                  ", "S46.R9: carry after Z");
    assert_eq!(file.points[0].net, "M0000", "S46.R26: real alias-shaped net remains distinct");

    let tail = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ012345";
    let source = format!("discarded/é/{tail}");
    let mut b = board(&[&source]);
    place(&mut b, Some(0), "J1", P::ZERO, smd());
    let (lines, file) = export(&b, &opts());
    assert_eq!(lines[6], "P  NNAMEM0000 abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ012345        ", "S46.R10: keep the last 58 sanitized characters");
    assert_eq!(file.points[0].net, tail, "S46.R26: read the NNAME tail");
    assert!(file.points[0].matches_net(&source), "S46.R26: match source by sanitized tail");

    let mut b = board(&["gnd"]);
    place(&mut b, Some(0), "R10", P::ZERO, smd());
    let placed = place(&mut b, Some(0), "R2", P::ZERO, smd());
    let mut second = smd();
    second.no = "2".into();
    Arc::make_mut(&mut placed.footprint).pads.insert(0, second);
    placed.pad_nets.push(Some(NetId(0)));
    via(&mut b, 0, P::mm(1.0, 0.0));
    via(&mut b, 0, P::mm(2.0, 0.0));
    let (_, file) = export(&b, &opts());
    let names: Vec<_> = file.points.iter().map(|p| (p.refdes.as_str(), p.pin.as_str())).collect();
    assert_eq!(names, [("VIA", ""), ("VIA", ""), ("R10", "1"), ("R2", "2"), ("R2", "1")], "S46.R21: stable via, footprint and pad order within a net");
    assert!(file.points[0].at.x < file.points[1].at.x, "S46.R21: via board order is retained");
    assert!(file.points.iter().all(|p| p.net == "GND" && p.matches_net("gnd")), "S46.R8/R26: unaliased names lose case");
}

#[test]
fn ipc356_layers_masks_slots_and_rounding() {
    let mut b = board(&["SIG"]);
    four_layers(&mut b);
    let mut p = pad("1", PadKind::Thru, PadStack::smd(PadShape::Rect { size: P::mm(1.016, 0.508) }), Some(P::mm(0.3, 0.9)));
    p.rot = Angle::deg(90.5);
    p.stack.back = Some(PadShape::Circle { d: mm(2.0) });
    // A collapsed opening must still count as a mask layer, per S46.R18.
    p.mask_margin = Some(-mm(10.0));
    p.drill.as_mut().unwrap().offset = P::mm(1.0, 2.0);
    place(&mut b, Some(0), "J1", P::ZERO, p).side = Side::Bottom;
    let mut inner = smd();
    inner.stack.copper = LayerSet::one(LayerId::inner(2));
    place(&mut b, Some(0), "J2", P::ZERO, inner);
    let mut no_copper = smd();
    no_copper.stack.copper = LayerSet::EMPTY;
    place(&mut b, Some(0), "SKIP", P::ZERO, no_copper);
    let v = via(&mut b, 0, P::ZERO);
    v.span = (LayerId::inner(1), LayerId::B_CU);
    v.tent = Tent::Back;
    let v = via(&mut b, 0, P::ZERO);
    v.span = (LayerId::inner(1), LayerId::inner(2));
    v.tent = Tent::None;
    let (lines, file) = export(&b, &opts());
    assert_eq!(file.points.len(), 4, "S46.R12/R20: skip no-copper pad and merge surface continuation");
    assert_eq!((file.points[0].access, file.points[0].layer_span), (4, Some((2, 4))), "S46.R13: back blind via uses layer count");
    assert_eq!((file.points[1].access, file.points[1].layer_span, file.points[1].size), (2, Some((2, 3)), (0, 0)), "S46.R13/R20/R25: buried via, no size or continuation");
    assert_eq!(&lines[6..9], &[
        "307SIG              VIA        MD0118PA04X+000000Y+000000               S2L02L04",
        "027SIG              VIA               A04X+000000Y+000000X0236Y0000R000 S2      ",
        "307SIG              VIA        MD0118PA02X+000000Y+000000               S0L02L03",
    ], "S46.R13/R18/R20: full back-blind and buried records");
    let p = &file.points[2];
    assert_eq!((p.access, p.size, p.rot_deg, p.drill), (4, (1_016_000, 508_000), 91, 299_720), "S46.R12/R15/R17/R19: flipped component-side size, mirrored angle and slot width");
    assert_eq!(p.at.board_at(P::ZERO), Some(P::ZERO), "S46.R14: use pad centre, not drill offset");
    assert!(p.mask_top && !p.mask_bottom, "S46.R18: flipped mask membership, independent of opening geometry");
    assert_eq!(file.points[3].access, 3, "S46.R12: inner-only pad uses physical layer number");

    let mut b = board(&["SIG"]);
    let mut p = smd();
    p.stack.front = PadShape::Rect { size: P::new(1270, 3810) };
    p.rot = Angle::deg(90.5);
    place(&mut b, Some(0), "J1", P::new(-1270, 1270), p);
    expect_records(&b, &opts(), &[
        "327SIG              J1    -1          A01X-000001Y-000001X0001Y0002R270 S2      ",
    ], "S46.R16/R17: signed half-decimil ties away from zero, fractional angle truncation");
    let p = &mut Arc::make_mut(&mut b.placed[0].footprint).pads[0];
    p.rot = Angle::deg(-90.0);
    let (_, file) = export(&b, &opts());
    assert_eq!(file.points[0].rot_deg, 90, "S46.R17: negative quarter turn");

    let mut b = board(&["SIG"]);
    let p = pad("1", PadKind::Thru, PadStack::thru(PadShape::Circle { d: Nm::MAX }), Some(P::new(Nm::MAX, Nm::MAX)));
    place(&mut b, Some(0), "J1", P::new(Nm::MAX, Nm::MIN), p);
    let opts = IpcOpts { origin: P::new(Nm::MIN, Nm::MAX), ..opts() };
    let file = expect_records(&b, &opts, &[
        "317SIG              J1    -1    D9999PA00X+999999Y+999999X9999Y0000R000 S0      ",
    ], "S46.R14-R16/R19: clamp coordinates, holes and sizes without overflow");
    assert_eq!((file.points[0].at.x, file.points[0].at.y), (2_539_997_460, -2_539_997_460), "S46.R25/R27: full IPC range remains readable");
    b.placed[0].at = P::new(Nm::MIN, Nm::MAX);
    let opts = IpcOpts { origin: P::new(Nm::MAX, Nm::MIN), ..opts };
    expect_records(&b, &opts, &[
        "317SIG              J1    -1    D9999PA00X-999999Y-999999X9999Y0000R000 S0      ",
    ], "S46.R14/R16: negative overflow clamps without losing its sign");
}

#[test]
fn ipc356_reader_and_text_substitution() {
    let mut b = board(&["é a\n"]);
    let mut p = smd();
    p.no = "é12345".into();
    place(&mut b, Some(0), "Ré123456", P::ZERO, p);
    let (lines, _) = export(&b, &IpcOpts { job: "jobé".into(), ..opts() });
    assert_eq!(lines[0], "P  JOB   job?                                                                   ", "S46.R6/Q1: job substitutes non-ASCII");
    assert_eq!(lines[6], "327??A?             R?1234-?123       A01X+000000Y+000000X0400Y0200R000 S2      ", "S46.R8/R22/Q1: substitute by character before reference/pin truncation");
    let (long_job, _) = export(&board(&[]), &IpcOpts { job: "a".repeat(80), ..opts() });
    assert_eq!(long_job[0], "P  JOB   aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa        ", "S46.R6: header ends by column 72");

    let parent = "307SIG1             VIA        MD0050PA01X+001969Y+002756               S0L01L03";
    let continuation = "027SIG1             VIA               A01X+001969Y+002756X0125Y0000R000 S0      ";
    let input = format!("Comment\r\nP  job   example\r\nP  arrayDim   N\r\n{parent}\r\n{}\r\n999\r\ninvalid\n", continuation.trim_end());
    let file = read_ipc356(input.as_bytes()).expect("S46.R23/R24: CRLF, first-column comments, short lines and default units");
    assert_eq!(file.job, "example", "S46.R24: case-insensitive parameter keyword");
    assert_eq!(file.points.len(), 1, "S46.R25: continuation enriches one point");
    assert_eq!((file.points[0].at.x, file.points[0].at.y, file.points[0].size), (5_001_260, -7_000_240, (317_500, 0)), "S46.R25/R27: signed decimil conversion and continuation size");
    let mut extended = format!("{parent}\n{continuation}").into_bytes();
    extended.extend_from_slice(b"ignored past column 80\xff\n999\n\xff\n");
    assert_eq!(read_ipc356(&extended).unwrap().points, file.points, "S46.R23: ignore bytes after column 80 and after 999");
    let mut aliased = format!("{}\nP  NNAMEM0000 mixedCase\n999\n", lines[6].replace("??A?          ", "M0000         "));
    let file = read_ipc356(aliased.as_bytes()).expect("S46.R24/R26: resolve NNAME after data");
    assert_eq!(file.points[0].net, "mixedCase", "S46.R26: aliased source case retained");
    aliased.insert_str(0, "P  units CUST 0\n");
    assert_eq!(read_ipc356(aliased.as_bytes()).unwrap(), file, "S46.R24: explicit and default decimils agree");
    for units in ["CUST 1", "CUST 2", "INCH", ""] {
        let input = format!("C\nP  UNITS {units}\n999\n");
        assert!(matches!(read_ipc356(input.as_bytes()), Err(CamError::Unsupported { at: 2, msg }) if msg.contains("UNITS")), "S46.R24/Q2: unsupported units name line 2: {units}");
    }
    for code in ["418", "X", "", "399", "099"] {
        let input = format!("C\n{code}\n999\n");
        assert!(matches!(read_ipc356(input.as_bytes()), Err(CamError::Unsupported { at: 2, .. })), "S46.R23: unsupported record names line 2: {code:?}");
    }
    assert!(matches!(read_ipc356(format!("{continuation}\n999\n").as_bytes()), Err(CamError::Parse { at: 1, .. })), "S46.R25: orphan continuation rejected");
    assert!(matches!(read_ipc356(format!("{parent}\n").as_bytes()), Err(CamError::Parse { .. })), "S46.R23: missing terminator rejected");
    for (offset, value) in [(41, b'Q'), (42, b'0'), (43, b'!'), (73, b'4'), (68, b'9')] {
        let mut bad = format!("C\n{}\n999\n", lines[6]).into_bytes();
        bad[2 + offset] = value;
        assert!(matches!(read_ipc356(&bad), Err(CamError::Parse { at: 2, .. })), "S46.R25: malformed column {} names line 2", offset + 1);
    }
}

#[test]
fn ipc356_ledctl_roundtrip() {
    let design = fab_elec_fixtures::ledctl();
    let unit = &design.units[0];
    let board = unit.board.as_ref().unwrap();
    let origin = P::mm(3.0, 7.0);
    let opts = IpcOpts { job: board.name.clone(), origin, skip_unconnected: false };
    let bytes = write_ipc356(board, &unit.circuit, &opts);
    assert!(bytes.chunks_exact(81).all(|line| line[80] == b'\n') && bytes.len() % 81 == 0, "S46.R4: fixture has 80 columns on every line");
    let file = read_ipc356(&bytes).expect("S46.R23-R27: LED-controller fixture reads back");
    let mut expected = Vec::new();
    let name = |net: Option<NetId>| net.and_then(|n| board.nets.get(n.ix())).map_or("", |n| n.name.as_str());
    for via in &board.vias { expected.push((name(Some(via.net)), "VIA", "", via.at)); }
    for placed in &board.placed {
        for (i, pad) in placed.footprint.pads.iter().enumerate() {
            let ix = PadIx(i as u16);
            if placed.pad_layers(ix, board.copper_set(), board.copper_count()).is_empty() { continue; }
            expected.push((name(placed.pad_nets.get(i).copied().flatten()), placed.refdes.as_str(), pad.no.as_str(), placed.pad_at(ix).unwrap()));
        }
    }
    for hole in &board.holes { expected.push((name(hole.net), hole.name.as_str(), "", hole.at)); }
    assert_eq!(file.points.len(), expected.len(), "S46.R12/R13: fixture point count includes unconnected pads, vias and holes");
    let mut remaining = file.points.clone();
    for (net, refdes, pin, at) in expected {
        let index = remaining.iter().position(|p| {
            let Some(restored) = p.at.board_at(origin) else { return false };
            p.matches_net(net) && p.refdes == refdes && p.pin == pin
                && (restored.x as i64 - at.x as i64).abs() <= 1270
                && (restored.y as i64 - at.y as i64).abs() <= 1270
        }).unwrap_or_else(|| panic!("S46.R26/R27: missing fixture point {net}/{refdes}/{pin} at {at:?}"));
        remaining.swap_remove(index);
    }
    assert!(remaining.is_empty(), "S46.R26/R27: no duplicated or extra fixture points");
}
