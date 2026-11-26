//! Seek syntax coverage, derived only from training queries. The player accepts
//! seconds and percent shares (not the `@frame` syntax of its capture option).

use crate::Driver;

/// Reverse the training targets and express each in both accepted classes.
/// The largest training time (at least one second) is the conversion scale;
/// percent-only input uses 100 seconds. This is a coverage variation, not a
/// claim to know the document duration or to preserve the original positions.
/// Query values are deliberately not URL-decoded: the player's query parser
/// consumes them verbatim too. Other query fields and repeated seek lists stay
/// in place. Invalid training input remains an error, never silently discarded.
pub fn query(query: &str) -> Result<Option<String>, String> {
    let mut has_seek = false;
    let mut scale = 1.0_f64;
    let mut has_seconds = false;
    for pair in query.trim_start_matches('?').split('&') {
        if let Some(value) = pair.strip_prefix("seek=") {
            for target in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let (number, share) = target.strip_suffix('%').map_or((target, false), |s| (s, true));
                let n = number.parse::<f64>().map_err(|_| format!("seek variation: invalid target `{target}`"))?;
                if !n.is_finite() {
                    return Err(format!("seek variation: non-finite target `{target}`"));
                }
                has_seek = true;
                if !share { has_seconds = true; scale = scale.max(n); }
            }
        }
    }
    if !has_seek { return Ok(None); }
    if !has_seconds { scale = 100.0; }
    let mut pairs = Vec::new();
    for pair in query.trim_start_matches('?').split('&') {
        if let Some(value) = pair.strip_prefix("seek=") {
            let mut targets = Vec::new();
            for target in value.split(',').rev().map(str::trim).filter(|s| !s.is_empty()) {
                // Validated above, including finiteness.
                let (seconds, percent) = if let Some(share) = target.strip_suffix('%') {
                    let percent = share.parse::<f64>().unwrap().clamp(0.0, 100.0);
                    (percent / 100.0 * scale, percent)
                } else {
                    let seconds = target.parse::<f64>().unwrap().max(0.0);
                    (seconds, seconds / scale * 100.0)
                };
                targets.push(format!("{percent}%"));
                targets.push(seconds.to_string());
            }
            pairs.push(format!("seek={}", targets.join(",")));
        } else {
            pairs.push(pair.to_string());
        }
    }
    Ok(Some(format!("{}{}", if query.starts_with('?') { "?" } else { "" }, pairs.join("&"))))
}

/// One additional driver per distinct seek query. Repeated queries at other
/// geometries need no extra whole-document run to cover the syntax classes.
/// Only caller-supplied training drivers enter this function, never replays or
/// the environment expansion. The first driver supplies all non-query inputs.
pub fn drivers(training: &[Driver]) -> Result<Vec<Driver>, String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut derived = Vec::new();
    for base in training {
        if !seen.insert(base.query.trim_start_matches('?')) { continue; }
        if let Some(query) = query(&base.query)? {
            derived.push(Driver { name: format!("seek-input-class-{}", base.name), query, ..base.clone() });
        }
    }
    Ok(derived)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_shares_and_mixed_queries() {
        assert_eq!(query("?doc=film&seek=0,25,100&seek-play=0.25").unwrap().unwrap(),
            "?doc=film&seek=100%,100,25%,25,0%,0&seek-play=0.25");
        assert_eq!(query("seek=20%,80%").unwrap().unwrap(), "seek=80%,80,20%,20");
        assert_eq!(query("seek=10,50%&seek=, -2,200%,").unwrap().unwrap(),
            "seek=50%,5,100%,10&seek=100%,10,0%,0");
        assert_eq!(query("seek=0").unwrap().unwrap(), "seek=0%,0");
        assert_eq!(query("seek=1e1,2.5").unwrap().unwrap(), "seek=25%,2.5,100%,10");
    }

    #[test]
    fn no_seek_and_invalid_input() {
        for input in ["", "?doc=calculator", "?seek=, ,&seek-play=1", "?other=seek=10"] {
            assert_eq!(query(input).unwrap(), None);
        }
        for input in ["seek=@10", "seek=bad%", "seek=NaN", "seek=inf%"] {
            assert!(query(input).is_err(), "{input}");
        }
    }

    #[test]
    fn one_driver_per_training_query_preserves_context() {
        let base = Driver::parse("name=a1;window=400x640@2;ms=4000;stress;query=?seek=5,10;gpu=mali").unwrap();
        let second = Driver { name: "a2".into(), window: "800x600@1".into(), ..base.clone() };
        let derived = drivers(&[base.clone(), second]).unwrap();
        assert_eq!(derived, vec![Driver { name: "seek-input-class-a1".into(), query: "?seek=100%,10,50%,5".into(), ..base }]);
        assert!(drivers(&[Driver::default()]).unwrap().is_empty());
    }
}
