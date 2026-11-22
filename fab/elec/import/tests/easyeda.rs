//! The supplier adapter on component responses written here: which model a footprint places.

use fab_elec_import::easyeda::Lcsc;

fn component(shapes: &[&str]) -> Vec<u8> {
    let shapes: Vec<String> = shapes.iter().map(|s| format!("{s:?}")).collect();
    format!(r#"{{"success":true,"code":0,"result":{{"packageDetail":{{"dataStr":{{"shape":[{}]}}}}}}}}"#, shapes.join(","))
        .into_bytes()
}

#[test]
fn the_footprint_names_its_model() {
    let outline = r#"SVGNODE~{"gId":"g1","attrs":{"c_etype":"outline3D","uuid":"00ff00ff00ff00ff00ff00ff00ff00ff","z":"0"},"childNodes":[]}"#;
    let other = r#"SVGNODE~{"gId":"g0","attrs":{"c_etype":"other","uuid":"aa"}}"#;
    let pad = "PAD~RECT~0~0~1~1~1~~1~0~0 0 1 0 1 1 0 1~0~g2~0~~Y~0~0~0~0,0";
    let uuid = Lcsc::model_uuid(&component(&[pad, other, outline])).expect("a component");
    assert_eq!(uuid.as_deref(), Some("00ff00ff00ff00ff00ff00ff00ff00ff"), "the outline3D node's uuid");
    assert_eq!(Lcsc::model_uuid(&component(&[pad])).expect("a component"), None, "no model placed");

    let bad = r#"SVGNODE~{"attrs":{"c_etype":"outline3D","uuid":"../x"}}"#;
    assert_eq!(Lcsc::model_uuid(&component(&[bad])).expect_err("not hex").field, "SVGNODE", "a uuid goes into a URL: hex only");
    let refused = br#"{"success":false,"code":404,"message":"no such part"}"#;
    assert!(Lcsc::model_uuid(refused).expect_err("no component").message.contains("no such part"));
    assert_eq!(Lcsc::model_uuid(b"<html>").expect_err("not json").field, "json");
}
