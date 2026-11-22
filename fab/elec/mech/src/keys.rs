//! Host keys for board features that case solids derive from.

use fab_elec_core::{BoardId, CompId, CompRef, FeatureRef, ItemId, PadIx};

/// `elec:port:<board>:<comp>:<index>`, `elec:hole:<board>:<item>`,
/// `elec:pad:<board>:<comp>:<pad>` (the ids' numbers).
/// None for the other kinds: a case derives from none of them yet.
pub fn feature_key(f: &FeatureRef) -> Option<String> {
    match f {
        FeatureRef::Port(c, index) => Some(format!("elec:port:{}:{}:{index}", c.board.0, c.comp.0)),
        FeatureRef::Hole(board, item) => Some(format!("elec:hole:{}:{}", board.0, item.0)),
        FeatureRef::Pad(c, pad) => Some(format!("elec:pad:{}:{}:{}", c.board.0, c.comp.0, pad.0)),
        _ => None,
    }
}

/// The inverse of [`feature_key`]; None for a hand-written, foreign or malformed key.
pub fn feature_of_key(key: &str) -> Option<FeatureRef> {
    let mut fields = key.strip_prefix("elec:")?.split(':');
    let kind = fields.next()?;
    let board = BoardId(fields.next()?.parse().ok()?);
    let feature = match kind {
        "port" => FeatureRef::Port(
            CompRef { board, comp: CompId(fields.next()?.parse().ok()?) },
            fields.next()?.parse().ok()?,
        ),
        "hole" => FeatureRef::Hole(board, ItemId(fields.next()?.parse().ok()?)),
        "pad" => FeatureRef::Pad(
            CompRef { board, comp: CompId(fields.next()?.parse().ok()?) },
            PadIx(fields.next()?.parse().ok()?),
        ),
        _ => return None,
    };
    fields.next().is_none().then_some(feature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_feature_keys_round_trip_and_reject_other_forms() {
        let comp = CompRef { board: BoardId(1), comp: CompId(2) };
        for (feature, key) in [
            (FeatureRef::Port(comp, 3), "elec:port:1:2:3"),
            (FeatureRef::Hole(comp.board, ItemId(4)), "elec:hole:1:4"),
            (FeatureRef::Pad(comp, PadIx(5)), "elec:pad:1:2:5"),
        ] {
            assert_eq!(feature_key(&feature).as_deref(), Some(key), "a case feature's key carries its kind and numeric ids");
            assert_eq!(feature_of_key(key), Some(feature), "a case feature round-trips through its host key");
        }
        assert_eq!(feature_key(&FeatureRef::Body(comp)), None, "a feature the case does not derive from has no key");
        for key in ["elec:port:1:2", "other:port:1:2:3", "elec:port:a:2:3", "elec:hole:1:2:3", "elec:pad:1:2:65536"] {
            assert_eq!(feature_of_key(key), None, "foreign and malformed keys do not resolve: {key}");
        }
    }
}
