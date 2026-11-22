//! Height bands for flow, nozzle temperature and pressure advance calibration.

/// Calibration values beginning at a layer's print Z; `None` keeps the previous value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    /// The layer's print Z in mm at which this band begins.
    pub z_from: f64,
    /// A multiplier on the job's filament and print flow ratios.
    pub flow: Option<f64>,
    /// Nozzle target in degrees Celsius.
    pub temp: Option<f64>,
    /// Pressure advance K.
    pub pa: Option<f64>,
}

/// Parse `fab_calibration_bands`: `z_from:flow_scale:temp:pa`, separated by
/// semicolons. `-` leaves a field unchanged; empty text means no bands.
/// Equal heights retain their input order, so the last value set wins.
pub fn parse(text: &str) -> Result<Vec<Band>, String> {
    let mut bands = Vec::new();
    if text.trim().is_empty() {
        return Ok(bands);
    }
    for (i, text) in text.split(';').enumerate() {
        let n = i + 1;
        let fields: Vec<&str> = text.split(':').map(str::trim).collect();
        if fields.len() != 4 {
            return Err(format!("fab_calibration_bands band {n}: expected z_from:flow_scale:temp:pa"));
        }
        let z_from = fields[0].parse::<f64>().ok().filter(|v| v.is_finite())
            .ok_or_else(|| format!("fab_calibration_bands band {n}: z_from must be a finite number in mm"))?;
        let mut values = [None; 3];
        for (j, &(name, lo, hi)) in [("flow_scale", 0.5, 1.5), ("temp", 150.0, 350.0), ("pa", 0.0, 0.5)].iter().enumerate() {
            let field = fields[j + 1];
            if field == "-" {
                continue;
            }
            let value = field.parse::<f64>().ok().filter(|v| (lo..=hi).contains(v))
                .ok_or_else(|| format!("fab_calibration_bands band {n}: {name} must be a number in {lo}..={hi} or -"))?;
            values[j] = Some(value);
        }
        bands.push(Band { z_from, flow: values[0], temp: values[1], pa: values[2] });
    }
    bands.sort_by(|a, b| a.z_from.partial_cmp(&b.z_from).unwrap());
    Ok(bands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_are_sorted_and_unchanged_fields_stay_optional() {
        assert!(parse("").unwrap().is_empty(), "empty means no bands");
        assert!(parse(" \t\n ").unwrap().is_empty(), "blank means no bands");
        assert_eq!(parse(" 10 : - : 220 : 0.02 ; 0.2 : 1.1 : - : - ; 5 : - : - : - ").unwrap(), vec![
            Band { z_from: 0.2, flow: Some(1.1), temp: None, pa: None },
            Band { z_from: 5.0, flow: None, temp: None, pa: None },
            Band { z_from: 10.0, flow: None, temp: Some(220.0), pa: Some(0.02) },
        ]);
    }

    #[test]
    fn bounds_are_inclusive_and_equal_heights_keep_their_order() {
        assert_eq!(parse("0:0.5:150:0;0:1.5:350:0.5").unwrap(), vec![
            Band { z_from: 0.0, flow: Some(0.5), temp: Some(150.0), pa: Some(0.0) },
            Band { z_from: 0.0, flow: Some(1.5), temp: Some(350.0), pa: Some(0.5) },
        ]);
    }

    #[test]
    fn malformed_bands_are_errors() {
        for text in [
            "garbage", "0", "0:1:220", "0:1:220:0.02:extra", ":1:220:0.02", "-:1:220:0.02",
            "0::220:0.02", "0:1::0.02", "0:1:220:", "0:no:220:0.02", "0:1:hot:0.02", "0:1:220:no",
            ";0:1:220:0.02", "0:1:220:0.02;", "0:1:220:0.02;;1:-:-:-", "NaN:-:-:-", "inf:-:-:-",
        ] {
            let error = parse(text).unwrap_err();
            assert!(error.starts_with("fab_calibration_bands band "), "{text}: {error}");
        }
    }

    #[test]
    fn out_of_range_and_non_finite_values_are_errors() {
        for text in [
            "0:0.499:-:-", "0:1.501:-:-", "0:-:149.9:-", "0:-:350.1:-", "0:-:-:-0.001", "0:-:-:0.501",
            "0:NaN:-:-", "0:inf:-:-", "0:-:NaN:-", "0:-:inf:-", "0:-:-:NaN", "0:-:-:inf",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }
}
