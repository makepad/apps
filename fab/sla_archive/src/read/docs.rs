use super::{near, Fault, Job, Rule, Screen, Software, TableRow, Totals};
use crate::format;
use makepad_strict_json::Value;

const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
const IHDR: [u8; 4] = *b"IHDR";

pub(super) struct Config {
    pub screen: Screen,
    pub job: Job,
    pub resin: String,
    pub price: f64,
    pub bottle: f64,
    pub capacity: u32,
    pub previews: [[u32; 2]; 2],
}

pub(super) fn parse(bytes: &[u8], name: &str) -> Result<Value, Fault> {
    makepad_strict_json::parse_depth(bytes, 16)
        .map_err(|e| Fault::new(Rule::Json, format!("{name}: {e:?}")))
}
fn member<'a>(value: &'a Value, key: &str) -> Result<&'a Value, Fault> {
    value.get(key).ok_or_else(|| Fault::new(Rule::Json, format!("missing member {key}")))
}
fn number(value: &Value, key: &str) -> Result<f64, Fault> {
    numeric(member(value, key)?).filter(|n| n.is_finite())
        .ok_or_else(|| Fault::new(Rule::Json, format!("{key}: expected a finite number")))
}
fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::F64(number) => Some(*number),
        Value::Int(number) => Some(*number as f64),
        _ => None,
    }
}
fn integer_value(value: &Value, key: &str) -> Result<u64, Fault> {
    value.as_u64().ok_or_else(|| Fault::new(Rule::Json, format!("{key}: expected a nonnegative integer")))
}
fn integer(value: &Value, key: &str) -> Result<u64, Fault> { integer_value(member(value, key)?, key) }
fn uint(value: &Value, key: &str) -> Result<u32, Fault> {
    u32::try_from(integer(value, key)?).map_err(|_| Fault::new(Rule::Json, format!("{key}: integer exceeds u32")))
}
fn string(value: &Value, key: &str) -> Result<String, Fault> {
    member(value, key)?.as_str().map(str::to_owned)
        .ok_or_else(|| Fault::new(Rule::Json, format!("{key}: expected a string")))
}
fn list<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], Fault> {
    member(value, key)?.as_arr()
        .ok_or_else(|| Fault::new(Rule::Json, format!("{key}: expected an array")))
}
fn dimensions(value: &Value, key: &str) -> Result<[u32; 2], Fault> {
    let array = list(value, key)?;
    if array.len() != 2 { return Err(Fault::new(Rule::Json, format!("{key}: {} values, expected two", array.len()))); }
    let mut out = [0; 2];
    for (i, v) in array.iter().enumerate() {
        out[i] = u32::try_from(integer_value(v, key)?).map_err(|_| Fault::new(Rule::Json, format!("{key}: dimension exceeds u32")))?;
    }
    Ok(out)
}

pub(super) fn settings(value: &Value) -> Result<Config, Fault> {
    use format::settings as key;
    let machine = member(value, key::MACHINE)?;
    let screen = Screen {
        pixels: [uint(machine, key::RES_X)?, uint(machine, key::RES_Y)?],
        pitch: [number(machine, key::PITCH_X)? / 1000.0, number(machine, key::PITCH_Y)? / 1000.0],
        size: [number(machine, key::SIZE_X)?, number(machine, key::SIZE_Y)?, number(machine, key::SIZE_Z)?],
    };
    let previews = [dimensions(machine, key::PREV_IMAGE_SIZE)?, dimensions(machine, key::PREV2_IMAGE_SIZE)?];
    let capacity = uint(machine, key::SEGMENTS_CAPACITY)?;
    let external = member(value, key::EXTERN)?;
    let active = list(external, key::ACTIVE_RESINS)?;
    let resin = active.first().and_then(Value::as_str)
        .ok_or_else(|| Fault::new(Rule::Resin, format!("{}: expected a first resin name", key::ACTIVE_RESINS)))?.to_owned();
    let resins = list(external, key::USER_RESINS)?;
    let selected = resins.iter().find(|r| r.get(key::PROPERTY).and_then(|p| p.get(key::NAME)).and_then(Value::as_str) == Some(resin.as_str()))
        .ok_or_else(|| Fault::new(Rule::Resin, format!("active resin {resin:?} is absent from {}", key::USER_RESINS)))?;
    let property = member(selected, key::PROPERTY)?;
    let price = number(property, key::PRICE)?;
    let bottle = number(property, key::VOLUME)?;
    let slice = member(selected, key::SLICEPARA)?;
    let job = Job {
        layer_height: number(slice, key::ZTHICK)?, exposure: number(slice, key::EXPOSURE_TIME)?,
        bottom_exposure: number(slice, key::BOTT_TIME)?, bottom_layers: uint(slice, key::BOTT_LAYERS)?,
        light_off: number(slice, key::OFF_TIME)?, lift_height: number(slice, key::ZUP_HEIGHT)?,
        lift_speed: number(slice, key::ZUP_SPEED)?, retract_speed: number(slice, key::ZDOWN_SPEED)?,
        anti_alias: uint(slice, key::ANTI_COUNT)?,
    };
    Ok(Config { screen, job, resin, price, bottle, capacity, previews })
}

pub(super) fn screen_size(screen: &Screen) -> Result<(), Fault> {
    for axis in 0..2 {
        let expected = screen.pixels[axis] as f64 * screen.pitch[axis];
        if screen.pixels[axis] == 0 || screen.pitch[axis] <= 0.0 || !near(screen.size[axis], expected, screen.pitch[axis]) {
            return Err(Fault::new(Rule::ScreenSize, format!("axis {axis}: size {}, expected {expected} within pitch {} mm", screen.size[axis], screen.pitch[axis])));
        }
    }
    screen.pixels_len()?;
    Ok(())
}

pub(super) fn totals(value: &Value) -> Result<Totals, Fault> {
    use format::print_info as key;
    Ok(Totals { cost: number(value, key::COST)?, currency: string(value, key::CURRENCY)?,
        print_seconds: integer(value, key::PRINT_TIME)?, volume_ml: number(value, key::VOLUME)? })
}
pub(super) fn software(value: &Value) -> Result<Software, Fault> {
    use format::software as key;
    Ok(Software { mark: string(value, key::MARK)?, opengl: string(value, key::OPENGL)?,
        os: string(value, key::OS)?, version: string(value, key::VERSION)? })
}
pub(super) fn table_rows(value: &Value) -> Result<Vec<TableRow>, Fault> {
    use format::layer_table as key;
    integer(value, key::COUNT)?;
    list(value, key::PARAS)?.iter().enumerate().map(|(k, row)| {
        let row = (|| {
            integer(row, key::LAYER_INDEX)?;
            Ok(TableRow { exposure: number(row, key::EXPOSURE_TIME)?, min_height: number(row, key::LAYER_MINHEIGHT)?,
                thickness: number(row, key::LAYER_THICKNESS)?, lift_height: number(row, key::ZUP_HEIGHT)?, lift_speed: number(row, key::ZUP_SPEED)? })
        })();
        row.map_err(|e: Fault| e.at(&format!("layer {k}")))
    }).collect()
}
pub(super) fn check_table(value: &Value, rows: &[TableRow], layers: u32, job: &Job) -> Result<(), Fault> {
    use format::layer_table as key;
    let count = integer(value, key::COUNT)?;
    if count != layers as u64 || rows.len() != layers as usize {
        return Err(Fault::new(Rule::TableCount, format!("count {count}, rows {}, expected {layers}", rows.len())));
    }
    for (k, row) in list(value, key::PARAS)?.iter().enumerate() {
        let index = integer(row, key::LAYER_INDEX)?;
        if index != k as u64 { return Err(Fault::new(Rule::TableIndex, format!("row {k}: index {index}, expected {k}"))); }
    }
    for (k, row) in rows.iter().enumerate() {
        let expected = k as f64 * job.layer_height;
        if !near(row.min_height, expected, 1e-4) || !near(row.thickness, job.layer_height, 1e-4) {
            return Err(Fault::new(Rule::TableHeights, format!("layer {k}: bottom {}, thickness {}; expected {expected}, {} mm", row.min_height, row.thickness, job.layer_height)));
        }
    }
    for (k, row) in rows.iter().enumerate() {
        let expected = if k < job.bottom_layers as usize { job.bottom_exposure } else { job.exposure };
        if row.exposure as f32 != expected as f32 {
            return Err(Fault::new(Rule::TableExposure, format!("layer {k}: exposure {}, expected {expected} s", row.exposure)));
        }
    }
    for (k, row) in rows.iter().enumerate() {
        if row.lift_height as f32 != job.lift_height as f32 || row.lift_speed as f32 != job.lift_speed as f32 {
            return Err(Fault::new(Rule::TableLift, format!("layer {k}: lift {} at {}, expected {} mm at {} mm/s", row.lift_height, row.lift_speed, job.lift_height, job.lift_speed)));
        }
    }
    Ok(())
}

pub(super) fn preview(bytes: &[u8], size: [u32; 2]) -> Result<(), Fault> {
    if bytes.len() < 33 || bytes[..8] != PNG_SIGNATURE || bytes[8..12] != 13u32.to_be_bytes() || bytes[12..16] != IHDR {
        return Err(Fault::new(Rule::Preview, "expected PNG signature and 13-byte first IHDR chunk"));
    }
    let actual = [u32::from_be_bytes(bytes[16..20].try_into().unwrap()), u32::from_be_bytes(bytes[20..24].try_into().unwrap())];
    if actual != size || bytes[24..29] != [8, 6, 0, 0, 0] {
        return Err(Fault::new(Rule::Preview, format!("dimensions {actual:?}, encoding {:?}; expected {size:?}, 8-bit RGBA, standard compression/filter, no interlace", &bytes[24..29])));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_number_kinds_and_preview_header() {
        let name = format::SETTINGS;
        for text in [b"25".as_slice(), b"25.0", b"2.5e1"] {
            let value = parse(text, name).unwrap();
            assert_eq!(numeric(&value), Some(25.0), "float members accept integer or float text");
        }
        assert_eq!(integer_value(&parse(b"25", name).unwrap(), format::settings::RES_X).unwrap(), 25);
        assert!(integer_value(&parse(b"25.0", name).unwrap(), format::settings::RES_X).is_err(), "integer members reject float text");
        let mut bytes = vec![0; 33];
        bytes[..8].copy_from_slice(&PNG_SIGNATURE);
        bytes[8..12].copy_from_slice(&13u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&IHDR);
        bytes[16..20].copy_from_slice(&224u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&168u32.to_be_bytes());
        bytes[24] = 8;
        bytes[25] = 6;
        preview(&bytes, [224, 168]).unwrap();
        assert_eq!(preview(&bytes, [336, 252]).unwrap_err().rule, Rule::Preview, "preview dimensions");
        for (offset, value) in [(0, 0), (12, 0), (24, 16), (25, 2), (28, 1)] {
            let mut bad = bytes.clone();
            bad[offset] = value;
            assert_eq!(preview(&bad, [224, 168]).unwrap_err().rule, Rule::Preview, "preview offset {offset}");
        }
    }
}
