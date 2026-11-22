use fab_elec_circuit::erc;
use fab_elec_lang::{evaluate, std_libs, Sources};
use fab_elec_parts::{PartKind, PinKind};

#[test]
fn power_flag_supplies_an_external_rail() {
    let mut sources = Sources::new();
    sources.add("external.ckt.splash", r#"let e = mod.elec
let va = e.net({name: "VA" kind: @power})
let gnd = e.net({name: "GND" kind: @ground})
let flag = e.part(mod.std.power_flag(), {pins: [va]})
let ground_flag = e.part(mod.std.power_flag(), {pins: [gnd]})
let reg = e.part(mod.std.ldo_3v3(), {pins: {VIN: va EN: va GND: gnd} nc: [@VOUT @NC]})
"#).expect("the external supply circuit source has a valid name");
    let evaluated = evaluate(&sources, &std_libs()).expect("the flagged circuit evaluates");
    let circuit = &evaluated.design.units[0].circuit;
    let flag = circuit.comp("flag").expect("the circuit contains the VA flag");
    assert_eq!(flag.refdes, "#FLG1", "flags use their own reference prefix");
    assert_eq!(flag.part.name, "PWR_FLAG", "the standard flag has its schematic part name");
    assert_eq!(flag.part.kind, PartKind::PowerFlag, "the Splash tag resolves to a power flag");
    assert!(flag.part.footprint.is_none(), "a schematic flag has no board footprint");
    let [pin] = flag.part.pins.as_slice() else { panic!("the flag has exactly one pin") };
    assert_eq!((pin.name.as_str(), pin.kind), ("1", PinKind::PowerOut), "pin 1 supplies the flagged net");
    let findings = erc(circuit);
    assert!(findings.is_empty(), "the flagged regulator inputs pass ERC without a footprint on either flag: {findings:?}");
}
