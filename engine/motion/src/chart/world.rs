//! Continent outlines for `@map`: equirectangular, longitude -180..180
//! across 0..1000 and latitude 90..-90 down 0..500 (from Makepad's
//! storybook world map, `apps/storybook/resources/world_map.svg`).

pub(crate) const WIDTH: f64 = 1000.0;
pub(crate) const HEIGHT: f64 = 500.0;

/// `(region id, path data)`; only `M`, `L` and `Z` with absolute points.
pub(crate) const REGIONS: &[(&str, &str)] = &[
    ("north-america", "M33.3 69.4 L55.6 52.8 L138.9 55.6 L222.2 55.6 L272.2 47.2 L305.6 77.8 L322.2 83.3 L347.2 105.6 L316.7 125.0 L291.7 147.2 L275.0 180.6 L266.7 166.7 L238.9 169.4 L230.6 177.8 L236.1 200.0 L255.6 205.6 L269.4 225.0 L280.6 225.0 L269.4 227.8 L258.3 213.9 L236.1 205.6 L208.3 194.4 L194.4 183.3 L183.3 161.1 L163.9 150.0 L155.6 127.8 L155.6 116.7 L130.6 97.2 L83.3 86.1 L41.7 83.3 Z"),
    ("greenland", "M375.0 83.3 L416.7 72.2 L444.4 55.6 L438.9 38.9 L416.7 25.0 L375.0 19.4 L333.3 22.2 L311.1 33.3 L338.9 61.1 L355.6 75.0 Z"),
    ("south-america", "M286.1 227.8 L300.0 216.7 L327.8 222.2 L355.6 236.1 L361.1 250.0 L394.4 263.9 L402.8 272.2 L391.7 294.4 L366.7 319.4 L338.9 344.4 L327.8 361.1 L319.4 380.6 L308.3 402.8 L291.7 388.9 L297.2 366.7 L302.8 341.7 L305.6 313.9 L291.7 288.9 L275.0 266.7 L277.8 250.0 L283.3 238.9 Z"),
    ("europe", "M483.3 150.0 L475.0 144.4 L475.0 130.6 L497.2 122.2 L488.9 116.7 L505.6 108.3 L511.1 102.8 L522.2 100.0 L527.8 88.9 L513.9 83.3 L530.6 72.2 L547.2 58.3 L583.3 55.6 L611.1 66.7 L666.7 61.1 L666.7 97.2 L644.4 111.1 L633.3 122.2 L611.1 125.0 L602.8 125.0 L577.8 125.0 L577.8 136.1 L566.7 138.9 L563.9 150.0 L552.8 138.9 L544.4 133.3 L550.0 138.9 L541.7 144.4 L533.3 136.1 L527.8 127.8 L508.3 130.6 L500.0 138.9 Z"),
    ("africa", "M483.3 150.0 L527.8 147.2 L555.6 161.1 L588.9 163.9 L597.2 172.2 L619.4 216.7 L641.7 216.7 L625.0 244.4 L613.9 255.6 L611.1 277.8 L597.2 305.6 L588.9 330.6 L572.2 344.4 L550.0 344.4 L533.3 297.2 L525.0 252.8 L525.0 238.9 L508.3 233.3 L491.7 236.1 L480.6 236.1 L463.9 225.0 L452.8 208.3 L455.6 191.7 L463.9 172.2 L475.0 161.1 Z M636.1 286.1 L638.9 300.0 L627.8 319.4 L622.2 311.1 L625.0 297.2 L633.3 283.3 Z"),
    ("asia", "M666.7 61.1 L708.3 47.2 L777.8 38.9 L861.1 47.2 L944.4 55.6 L972.2 66.7 L944.4 88.9 L888.9 105.6 L861.1 130.6 L838.9 141.7 L836.1 161.1 L811.1 188.9 L791.7 222.2 L786.1 247.2 L775.0 227.8 L772.2 205.6 L761.1 205.6 L750.0 188.9 L736.1 194.4 L722.2 208.3 L713.9 227.8 L700.0 194.4 L688.9 183.3 L669.4 180.6 L658.3 180.6 L658.3 188.9 L644.4 202.8 L625.0 213.9 L619.4 208.3 L608.3 191.7 L597.2 172.2 L597.2 150.0 L583.3 150.0 L572.2 138.9 L580.6 136.1 L597.2 133.3 L613.9 130.6 L633.3 122.2 L644.4 111.1 L666.7 97.2 Z"),
    ("antarctica", "M0.0 450.0 L83.3 458.3 L166.7 452.8 L250.0 444.4 L333.3 425.0 L416.7 444.4 L500.0 444.4 L583.3 438.9 L666.7 436.1 L750.0 433.3 L833.3 433.3 L916.7 450.0 L1000.0 450.0 L1000.0 486.1 L0.0 486.1 Z"),
    ("oceania", "M813.9 311.1 L816.7 322.2 L819.4 338.9 L827.8 347.2 L850.0 338.9 L872.2 338.9 L880.6 347.2 L888.9 355.6 L905.6 358.3 L916.7 352.8 L925.0 327.8 L902.8 291.7 L894.4 280.6 L877.8 283.3 L861.1 280.6 L852.8 288.9 L838.9 297.2 Z M980.6 347.2 L994.4 355.6 L991.7 361.1 L977.8 363.9 L972.2 372.2 L963.9 377.8 L961.1 375.0 L975.0 366.7 Z"),
];

/// The region a name means (`"North America"`, `north_america`, `NA`…).
pub(crate) fn region(name: &str) -> Option<usize> {
    let key: String = name.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    let alias = match key.as_str() {
        "na" | "northamerica" | "namerica" | "usa" | "us" | "unitedstates" | "canada" | "mexico" => "north-america",
        "sa" | "southamerica" | "samerica" | "latam" | "latinamerica" | "brazil" => "south-america",
        "eu" | "europe" | "emea" => "europe",
        "af" | "africa" => "africa",
        "as" | "asia" | "apac" | "china" | "india" | "japan" => "asia",
        "oc" | "oceania" | "australia" | "anz" => "oceania",
        "an" | "antarctica" => "antarctica",
        "greenland" => "greenland",
        _ => return None,
    };
    REGIONS.iter().position(|(id, _)| *id == alias)
}

/// Path data mapped from the map's 1000x500 box into `[x, y, w, h]`.
pub(crate) fn fit(data: &str, rect: [f64; 4]) -> String {
    let (sx, sy) = (rect[2] / WIDTH, rect[3] / HEIGHT);
    let mut out = String::with_capacity(data.len());
    let mut coord = 0;
    for token in data.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        match token {
            "M" | "L" | "Z" => {
                out.push_str(token);
                coord = 0;
            }
            _ if token.starts_with('M') || token.starts_with('L') => {
                out.push_str(&token[..1]);
                out.push(' ');
                let v: f64 = token[1..].parse().unwrap_or(0.0);
                out.push_str(&format!("{:.1}", rect[0] + v * sx));
                coord = 1;
            }
            _ => {
                let v: f64 = token.parse().unwrap_or(0.0);
                let mapped = if coord % 2 == 0 { rect[0] + v * sx } else { rect[1] + v * sy };
                out.push_str(&format!("{mapped:.1}"));
                coord += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_resolve_and_fit_their_box() {
        assert_eq!(REGIONS[region("North America").unwrap()].0, "north-america");
        assert_eq!(region("APAC"), region("asia"));
        let d = fit("M100.0 50.0 L200.0 100.0 Z", [10.0, 20.0, 500.0, 250.0]);
        assert_eq!(d, "M 60.0 45.0 L 110.0 70.0 Z");
    }
}
