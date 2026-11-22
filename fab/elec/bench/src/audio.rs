//! A board's audio outputs, found by their data alone: a part whose definition says
//! `fields.audio == "out"` is an output jack. Its `audio_left` pin (and `audio_right`, on a stereo
//! jack) is a channel heard against its `audio_ref` pin; the field values name pins. No part, board
//! or project is named anywhere: any board with such a part has an audio output.
//!
//! Cost: O(components x pins of a jack) for all of them, O(pins) for one.

use fab_elec_circuit::Circuit;
use fab_elec_core::ids::{CompId, NetId};

/// One output jack: its channels' nets and their reference's.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioJack {
    pub comp: CompId,
    /// Its reference designator (`J1`), for people.
    pub refdes: String,
    pub left: NetId,
    /// None on a mono jack, or a stereo one whose right pin is not connected.
    pub right: Option<NetId>,
    pub reference: NetId,
}

impl AudioJack {
    pub fn stereo(&self) -> bool {
        self.right.is_some()
    }

    /// Its channels, left first.
    pub fn channels(&self) -> impl Iterator<Item = NetId> + '_ {
        std::iter::once(self.left).chain(self.right)
    }
}

/// The circuit's output jacks in component order. A jack left unpopulated, or whose left or
/// reference pin is not connected, cannot be heard and is left out.
pub fn output_jacks(circuit: &Circuit) -> Vec<AudioJack> {
    circuit.comps.iter().filter_map(|comp| output_jack(circuit, comp.id)).collect()
}

/// `comp` as an output jack, when it is one that can be heard. O(its pins).
pub fn output_jack(circuit: &Circuit, comp: CompId) -> Option<AudioJack> {
    let comp = circuit.comps.get(comp.ix()).filter(|c| c.id == comp)?;
    let fields = &comp.part.fields;
    if comp.dnp || fields.get("audio") != Some("out") {
        return None;
    }
    let net = |field: &str| fields.get(field).and_then(|pin| comp.part.pin(pin)).and_then(|pin| circuit.net_of(comp.id, pin));
    Some(AudioJack { comp: comp.id, refdes: comp.refdes.clone(), left: net("audio_left")?, right: net("audio_right"), reference: net("audio_ref")? })
}

/// What is plugged into an output jack. A connected load goes from each channel to the jack's
/// reference.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum AudioLoad {
    /// Nothing plugged in.
    Open,
    Line,
    Phones,
    /// A chosen resistance in ohms, positive and finite.
    Ohms(f64),
}

impl AudioLoad {
    /// The resistance across each channel in ohms, or None for an open jack.
    pub fn ohms(self) -> Option<f64> {
        match self {
            AudioLoad::Open => None,
            AudioLoad::Line => Some(10_000.0),
            AudioLoad::Phones => Some(32.0),
            AudioLoad::Ohms(r) => Some(r),
        }
    }

    /// The load as the sim statement writes it, for example `phones` or `600ohm`.
    pub fn literal(self) -> String {
        match self {
            AudioLoad::Open => "open".into(),
            AudioLoad::Line => "line".into(),
            AudioLoad::Phones => "phones".into(),
            AudioLoad::Ohms(r) => format!("{r}ohm"),
        }
    }
}
