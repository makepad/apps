use crate::{Co5300, PartKind};

/// Resolve a schematic model key and its explicit numeric parameters.
/// Keys are ASCII case-insensitive; parameter names are exact, as documented on
/// `PartKind`. Numbers must be finite integers in the part's documented range.
/// Missing, duplicate, unused and invalid parameters are errors, never defaults.
pub fn model(key: &str, params: &[(String, f64)]) -> Result<PartKind, String> {
    let names: &[&str] = if key.eq_ignore_ascii_case("CO5300") {
        &["glass_x", "glass_y", "glass_w", "glass_h"]
    } else if ["TCA9554", "QMI8658C", "CST820", "ES8311"].iter().any(|name| key.eq_ignore_ascii_case(name)) {
        &["addr"]
    } else if key.eq_ignore_ascii_case("AMP") {
        &["enable", "gain_db", "start_ms"]
    } else {
        return Err(format!("{key}: unknown part model key"));
    };
    for (index, (name, _)) in params.iter().enumerate() {
        if !names.contains(&name.as_str()) { return Err(format!("{key}: unused parameter {name}")); }
        if params[..index].iter().any(|(other, _)| other == name) {
            return Err(format!("{key}: duplicate parameter {name}"));
        }
    }
    let signed = |name: &str, min: i32, max: i32| -> Result<i32, String> {
        let value = params.iter().find(|(given, _)| given == name)
            .map(|(_, value)| *value).ok_or_else(|| format!("{key}: missing parameter {name}"))?;
        if !value.is_finite() || value.fract() != 0.0 || value < f64::from(min) || value > f64::from(max) {
            return Err(format!("{key}: parameter {name} must be an integer in {min}..={max}"));
        }
        Ok(value as i32)
    };
    let number = |name: &str, min: u16, max: u16| signed(name, i32::from(min), i32::from(max)).map(|n| n as u16);
    if key.eq_ignore_ascii_case("CO5300") {
        let glass = [number("glass_x", 0, 479)?, number("glass_y", 0, 479)?,
            number("glass_w", 1, 480)?, number("glass_h", 1, 480)?];
        Co5300::validate_glass(glass).map_err(|why| format!("{key}: {why}"))?;
        Ok(PartKind::Co5300 { glass })
    } else if key.eq_ignore_ascii_case("TCA9554") {
        Ok(PartKind::Tca9554 { addr: number("addr", 0x20, 0x27)? as u8 })
    } else if key.eq_ignore_ascii_case("QMI8658C") {
        Ok(PartKind::Qmi8658c { addr: number("addr", 0x6a, 0x6b)? as u8 })
    } else if key.eq_ignore_ascii_case("ES8311") {
        // es8311-datasheet.txt:303-307; es8311-user-guide.txt:969-972.
        Ok(PartKind::Es8311 { addr: number("addr", 0x18, 0x19)? as u8 })
    } else if key.eq_ignore_ascii_case("AMP") {
        Ok(PartKind::Amp {
            enable_high: number("enable", 0, 1)? == 1, gain_db: signed("gain_db", -40, 40)? as i8,
            start_ms: number("start_ms", 0, 1000)?,
        })
    } else {
        Ok(PartKind::Cst820 { addr: number("addr", 0, 0x7f)? as u8 })
    }
}
