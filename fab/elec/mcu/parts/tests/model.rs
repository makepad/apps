use fab_elec_mcu_parts::{model, PartKind};

#[test]
fn schematic_keys_require_exact_valid_parameters() {
    let params = |pairs: &[(&str, f64)]| pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect::<Vec<_>>();
    assert_eq!(model("co5300", &params(&[("glass_x", 8.0), ("glass_y", 10.0),
        ("glass_w", 400.0), ("glass_h", 420.0)])),
        Ok(PartKind::Co5300 { glass: [8, 10, 400, 420] }), "panel key must preserve the supplied glass window");
    for (key, addr, expected) in [
        ("tca9554", 0x20, PartKind::Tca9554 { addr: 0x20 }),
        ("Qmi8658C", 0x6b, PartKind::Qmi8658c { addr: 0x6b }),
        ("CST820", 0x15, PartKind::Cst820 { addr: 0x15 }),
    ] {
        assert_eq!(model(key, &params(&[("addr", f64::from(addr))])), Ok(expected),
            "{key} must match without case and retain its address");
        let error = model(key, &[]).expect_err("missing addr must fail");
        assert!(error.contains(key) && error.contains("addr"), "missing-parameter error must name {key} and addr");
        let error = model(key, &params(&[("addr", f64::from(addr)), ("adress", 21.0)]))
            .expect_err("extra parameter must fail");
        assert!(error.contains(key) && error.contains("adress"), "unused-parameter error must name the typo and {key}");
        for invalid in [f64::NAN, f64::INFINITY, -1.0, 21.5, 128.0] {
            let error = model(key, &params(&[("addr", invalid)])).expect_err("invalid address must fail");
            assert!(error.contains(key) && error.contains("addr"), "invalid address error must name {key} and addr");
        }
    }
    for key in ["mystery", "Key", "Not"] {
        assert!(model(key, &[]).expect_err("non-bus key must fail").contains(key),
            "unknown-key error must name {key}");
    }
    let error = model("CST820", &params(&[("addr", 21.0), ("addr", 22.0)])).expect_err("duplicate must fail");
    assert!(error.contains("CST820") && error.contains("addr"), "duplicate error must identify key and parameter");
    assert!(model("TCA9554", &params(&[("addr", 0x28 as f64)])).is_err(), "expander must reject an address outside its straps");
    assert!(model("QMI8658C", &params(&[("addr", 0x69 as f64)])).is_err(), "IMU must reject an address outside its straps");
    let mut glass = params(&[("glass_x", 0.0), ("glass_y", 0.0), ("glass_w", 480.0), ("glass_h", 480.0)]);
    for index in 0..4 {
        let mut missing = glass.clone();
        let (name, _) = missing.remove(index);
        let error = model("CO5300", &missing).expect_err("every glass parameter must be explicit");
        assert!(error.contains("CO5300") && error.contains(&name), "missing glass error must identify {name}");
        let mut invalid = glass.clone();
        invalid[index].1 = if index < 2 { 480.0 } else { 0.0 };
        let error = model("CO5300", &invalid).expect_err("invalid glass must fail");
        assert!(error.contains("CO5300") && error.contains(&name), "invalid glass error must identify {name}");
    }
    glass[0].1 = 1.0;
    let error = model("CO5300", &glass).expect_err("glass must fit after its offset");
    assert!(error.contains("CO5300") && error.contains("glass_w"), "cross-parameter glass error must identify width");
}
