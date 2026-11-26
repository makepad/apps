//! Design rules: the board's rule statements (`Rules`, plain data, A6) and the resolved
//! `RuleMatrix` the engines read (an engine cache, never a field).
//!
//! Custom rules carry the identity of their Splash `when` fn (`RuleCode`, A6), never its text;
//! `resolve_rules` asks a `RuleSource` (the language's evaluator) once per pair of rule keys, so
//! no predicate ever runs in an inner loop. A rule key is a net class and an item kind.
//!
//! Cost: `resolve_rules` is O(custom rules x keys^2) with keys = classes x 5, run only when the
//! board's rules, nets, classes, pairs, stackup or factory profile change (`BoardDelta::global`);
//! lookups are O(1) (plus one check per layered custom rule).
//! Pairs resolve once per rules pass in O(pairs), and `partner` is an O(1) lookup.

use crate::Board;
use fab_elec_circuit::{ClassRules, ViaSize};
use fab_elec_core::{canon_struct, canon_tags, ClassId, ContentHash, ItemId, LayerId, LayerSet, NetId, Nm, PadIx, Severity};

/// The board's rules as written.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Rules {
    pub base: RuleSet,
    pub custom: Vec<CustomRule>,
    pub checks: Vec<CustomCheck>,
    pub waivers: Vec<Waiver>,
}
canon_struct!(Rules { base, custom, checks, waivers });

impl Rules {
    /// A fresh board inherits the four house-sensitive base minima. Only a value
    /// explicitly set on the board can produce a finer-than-factory warning.
    pub fn standard() -> Rules {
        let base = RuleSet { track: None, clearance: None, drill: None, annular: None, ..RuleSet::standard() };
        Rules { base, ..Rules::default() }
    }
}

/// What a factory can make: the rule preset bound to its profile (ECAM gate 3: rule minima are a
/// board concept; `fab-elec-cam` re-exports it).
#[derive(Clone, PartialEq, Debug)]
pub struct Capabilities {
    pub min_track: Nm,
    pub min_clearance: Nm,
    pub min_drill: Nm,
    pub min_annular: Nm,
    /// The factory fills and caps vias in pads.
    pub via_in_pad: bool,
    /// Copper layer counts offered.
    pub layers: Vec<u8>,
    pub board_min: (Nm, Nm),
    pub board_max: (Nm, Nm),
    pub thickness: Vec<Nm>,
    /// How far a board's thickness may be from an offered one, in parts per million (5% = 50_000).
    pub thickness_tol_ppm: u32,
}

/// Each factory profile's capabilities by profile name, resolved by the host once per change of
/// the profile document and handed to the engines.
pub type Factories = std::collections::BTreeMap<String, std::sync::Arc<Capabilities>>;

/// The board's rules with the factory's minimum track width, clearance, drill and annular
/// ring applied. Each minimum is the greater of the design's value and the factory's; a
/// smaller design minimum is replaced, a stricter one is kept. Unspecified base values inherit
/// the standard rules, as `resolve_rules` does; explicit custom minima are raised too and
/// unspecified custom values keep inheriting. Pure; the result is derived, never stored.
pub fn apply_capabilities(rules: &Rules, caps: &Capabilities) -> Rules {
    let mut out = rules.clone();
    let standard = RuleSet::standard();
    let raise = |value: Option<Nm>, fallback: Option<Nm>, limit: Nm| Some(value.or(fallback).unwrap_or(0).max(limit));
    out.base.track = raise(out.base.track, standard.track, caps.min_track);
    out.base.clearance = raise(out.base.clearance, standard.clearance, caps.min_clearance);
    out.base.drill = raise(out.base.drill, standard.drill, caps.min_drill);
    out.base.annular = raise(out.base.annular, standard.annular, caps.min_annular);
    for rule in &mut out.custom {
        rule.set.track = rule.set.track.map(|v| v.max(caps.min_track));
        rule.set.clearance = rule.set.clearance.map(|v| v.max(caps.min_clearance));
        rule.set.drill = rule.set.drill.map(|v| v.max(caps.min_drill));
        rule.set.annular = rule.set.annular.map(|v| v.max(caps.min_annular));
    }
    out
}

/// Rules derived for one rules pass (the factory's capabilities applied to the board's own),
/// with the name of what raised them; never stored on the board.
#[derive(Clone, PartialEq, Debug)]
pub struct Effective {
    pub rules: Rules,
    pub origin: String,
    /// The factory fills and caps vias in pads (its capability).
    pub via_in_pad: bool,
}

/// A set of limits; `None` leaves the value to the next rule down.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct RuleSet {
    pub clearance: Option<Nm>,
    pub track: Option<Nm>,
    pub via: Option<ViaSize>,
    pub drill: Option<Nm>,
    pub annular: Option<Nm>,
    pub edge: Option<Nm>,
    pub hole_to_hole: Option<Nm>,
    pub hole_clearance: Option<Nm>,
    /// Solder mask expansion of a pad that names none.
    pub mask_margin: Option<Nm>,
    /// Paste opening margin (signed) of a pad that names none.
    pub paste_margin: Option<Nm>,
    /// Paste ratio (signed, parts per million) of a pad that names none.
    pub paste_ratio_ppm: Option<i32>,
    /// Silk to mask opening clearance; touching or overlapping silk always warns, even at 0.
    pub silk_clearance: Option<Nm>,
    /// Edge-to-edge gap between the two tracks of a differential pair; a pair's own gap
    /// (`Intent::Pair::gap`) and its class's (`PairRule::gap`) take precedence.
    /// The clearance rule still applies between the pair's nets.
    pub pair_gap: Option<Nm>,
}
canon_struct!(RuleSet {
    clearance, track, via, drill, annular, edge, hole_to_hole, hole_clearance, mask_margin,
    paste_margin, paste_ratio_ppm, silk_clearance, pair_gap
});

impl RuleSet {
    /// 0.15 mm track and space, 0.3 mm drill, 0.45 mm via pad, 0.20 mm pair gap (decision 8).
    pub fn standard() -> RuleSet {
        RuleSet {
            clearance: Some(150_000),
            track: Some(150_000),
            via: Some(ViaSize { drill: 300_000, pad: 450_000 }),
            drill: Some(300_000),
            annular: Some(75_000),
            edge: Some(300_000),
            hole_to_hole: Some(250_000),
            hole_clearance: Some(250_000),
            mask_margin: Some(50_000),
            paste_margin: Some(0),
            paste_ratio_ppm: Some(0),
            silk_clearance: Some(0),
            pair_gap: Some(200_000),
        }
    }
}

/// The identity of a Splash `when` fn (E4 A6): its registry index in the language's evaluator,
/// the hash of its source, and where it is (`(document, byte offset)`; not content).
#[derive(Clone, PartialEq, Debug)]
pub struct RuleCode {
    pub id: u32,
    pub hash: ContentHash,
    pub at: (u32, u32),
}

impl fab_elec_core::Canon for RuleCode {
    /// Content only: the source hash (the registry index and position are not content).
    fn canon(&self, w: &mut fab_elec_core::Writer) {
        self.hash.canon(w)
    }
}

/// `rule("name", {when: fn(a, b) {..} layers: .. set: {..}})`: later rules override earlier ones.
#[derive(Clone, PartialEq, Debug)]
pub struct CustomRule {
    pub name: String,
    pub when: RuleCode,
    pub layers: Option<LayerSet>,
    pub set: RuleSet,
    pub severity: Severity,
}
canon_struct!(CustomRule { name, when, layers, set, severity });

/// A user `check` fn run over the board (through the language).
#[derive(Clone, PartialEq, Debug)]
pub struct CustomCheck {
    pub name: String,
    pub code: RuleCode,
    pub severity: Severity,
}
canon_struct!(CustomCheck { name, code, severity });

/// A waived violation, by its stable id.
#[derive(Clone, PartialEq, Debug)]
pub struct Waiver {
    pub violation: u64,
    pub reason: String,
}
canon_struct!(Waiver { violation, reason });

/// The copper item kinds rules distinguish.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum RuleItem {
    Pad,
    Track,
    Via,
    Zone,
    Hole,
}
canon_tags!(RuleItem);

impl RuleItem {
    pub const ALL: [RuleItem; 5] = [RuleItem::Pad, RuleItem::Track, RuleItem::Via, RuleItem::Zone, RuleItem::Hole];
}

/// A rule key: what the matrix is indexed by.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct RuleKey {
    pub class: ClassId,
    pub item: RuleItem,
}

/// What a `when` fn sees of one side.
#[derive(Clone, PartialEq, Debug)]
pub struct RuleProps {
    pub class: ClassId,
    pub class_name: String,
    pub item: RuleItem,
}

/// Which rule set a limit: an index into the matrix's rule names (`RuleMatrix::rule_name`).
/// The built-in rules have fixed refs; custom rules follow in statement order, then factory rows.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct RuleRef(pub u16);

impl RuleRef {
    pub const CLEARANCE: RuleRef = RuleRef(0);
    pub const EDGE: RuleRef = RuleRef(1);
    pub const HOLE_CLEARANCE: RuleRef = RuleRef(2);
    pub const HOLE_TO_HOLE: RuleRef = RuleRef(3);
    pub const TRACK: RuleRef = RuleRef(4);
    pub const DRILL: RuleRef = RuleRef(5);
    pub const ANNULAR: RuleRef = RuleRef(6);
    pub const DANGLING: RuleRef = RuleRef(7);
    pub const KEEPOUT: RuleRef = RuleRef(8);
    /// The silk to mask opening clearance rule.
    pub const SILK: RuleRef = RuleRef(9);
    /// The differential pair gap rule.
    pub const PAIR_GAP: RuleRef = RuleRef(10);
    /// A via drill's gap to a pad (`RuleMatrix::via_drill_to_pad`).
    pub const VIA_IN_PAD: RuleRef = RuleRef(11);
    const BUILTIN: [&'static str; 12] = [
        "clearance", "edge", "hole clearance", "hole to hole", "track", "drill", "annular", "dangling",
        "keepout", "silk clearance", "pair gap", "via in pad",
    ];
}

/// Evaluates custom rules' `when` fns (implemented by the language, C5).
pub trait RuleSource {
    fn when(&self, code: &RuleCode, a: &RuleProps, b: &RuleProps) -> bool;
}

/// The resolved rules: every limit for every key pair, ready for O(1) lookup.
#[derive(Clone, PartialEq, Debug)]
pub struct RuleMatrix {
    classes: usize,
    clearance: Vec<Nm>,
    clearance_rule: Vec<u16>,
    layered: Vec<(LayerSet, Vec<Option<Nm>>, u16)>,
    track: Vec<Nm>,
    track_rule: Vec<u16>,
    via: Vec<ViaSize>,
    /// The resolved gap for each class.
    pair_gap: Vec<Nm>,
    /// One partner per board net; the earlier valid pair wins when pairs share a net.
    partner: Vec<Option<(NetId, Nm)>>,
    names: Vec<String>,
    origins: Vec<Option<String>>,
    class_rules: Vec<ClassRules>,
    default_class: ClassRules,
    factory_via_in_pad: bool,
    mask_margin: Nm,
    edge: Nm,
    edge_rule: u16,
    silk: Nm,
    silk_rule: u16,
    drill: Nm,
    drill_rule: u16,
    annular: Nm,
    annular_rule: u16,
    hole_to_hole: Nm,
    hole_to_hole_rule: u16,
    hole_clearance: Nm,
    hole_clearance_rule: u16,
    max_clearance: Nm,
    /// The largest resolved gap among the board's pairs.
    max_pair_gap: Nm,
}

const ITEMS: usize = 5;

/// The narrowest strip of solder mask (nm) kept between a pad's mask opening and a via drill
/// that may not sit in the pad. A thinner strip does not hold and solder wicks into the hole.
/// 0.1 mm is a common minimum mask web.
/// TODO: its home is the factory profile (the factory's minimum mask web); a constant until the profile carries one.
pub const MIN_MASK_WEB: Nm = 100_000;

impl RuleMatrix {
    fn k(&self, key: RuleKey) -> usize {
        key.class.ix().min(self.classes.saturating_sub(1)) * ITEMS + key.item as usize
    }
    /// Required copper clearance between two items on `layer`.
    pub fn clearance(&self, a: RuleKey, b: RuleKey, layer: LayerId) -> Nm {
        self.clearance_and_rule(a, b, layer).0
    }
    /// The clearance and the name of the rule that set it.
    pub fn clearance_and_rule(&self, a: RuleKey, b: RuleKey, layer: LayerId) -> (Nm, &str) {
        let (c, r) = self.clearance_ref(a, b, layer);
        (c, self.rule_name(r))
    }
    /// The clearance and the rule that set it.
    pub fn clearance_ref(&self, a: RuleKey, b: RuleKey, layer: LayerId) -> (Nm, RuleRef) {
        let n = self.classes * ITEMS;
        let i = self.k(a) * n + self.k(b);
        let mut v = (self.clearance[i], self.clearance_rule[i]);
        for (layers, m, rule) in &self.layered {
            if layers.has(layer) {
                if let Some(c) = m[i] {
                    v = (c, *rule);
                }
            }
        }
        (v.0, RuleRef(v.1))
    }
    pub fn rule_name(&self, r: RuleRef) -> &str {
        self.names.get(r.0 as usize).map_or("?", |s| s.as_str())
    }
    /// What raised the rule above the design's own: the factory profile's name;
    /// `None` for the design's own rules.
    pub fn origin(&self, r: RuleRef) -> Option<&str> {
        self.origins.get(r.0 as usize).and_then(|s| s.as_deref())
    }
    /// A class's own facts (impedance, neck-down, stubs, via in pad, return vias); the default
    /// for an unknown class.
    pub fn class(&self, c: ClassId) -> &ClassRules {
        self.class_rules.get(c.ix()).unwrap_or(&self.default_class)
    }
    /// Vias of this class may be drilled in pads: the class allows it (`ClassRules::via_in_pad`)
    /// and the bound factory, if any, fills and caps vias in pads.
    pub fn via_in_pad(&self, class: ClassId) -> bool {
        self.class(class).via_in_pad == Some(true) && self.factory_via_in_pad
    }
    /// Minimum gap (nm) from the via drill wall to pad copper when the via's class may not sit in
    /// pads (`via_in_pad` is false). The pad's mask expansion plus a mask web keeps solder out of
    /// the hole. Even with a negative expansion the drill stays at least 1 nm off the copper.
    /// Where via-in-pad is allowed, a netted pad's own net may drill in it. With standard rules and
    /// mask expansion, other nets already need the larger copper clearance. DRC's via-in-pad check,
    /// the grid's via planes and the organic relaxer read this gap. Callers pass the pad's own
    /// `mask_margin` live from the board: editing a pad changes the board without re-resolving the matrix.
    pub fn via_drill_to_pad(&self, pad_mask_margin: Option<Nm>) -> Nm {
        pad_mask_margin.unwrap_or(self.mask_margin).saturating_add(MIN_MASK_WEB).max(1)
    }
    /// Drill-wall clearance for this placed pad, using the board-wide expansion if the pad is absent.
    pub fn via_drill_to_board_pad(&self, board: &Board, placed: ItemId, pad: PadIx) -> Nm {
        let margin = board.placed_item(placed)
            .and_then(|p| p.footprint.pads.get(pad.ix()))
            .and_then(|p| p.mask_margin);
        self.via_drill_to_pad(margin)
    }
    /// Broad-phase bound for DRC and the grid, including the board-wide need and every pad's own. O(pads).
    pub fn max_via_drill_to_pad(&self, board: &Board) -> Nm {
        board.placed.iter().flat_map(|p| &p.footprint.pads)
            .map(|pad| self.via_drill_to_pad(pad.mask_margin))
            .fold(self.via_drill_to_pad(None), Nm::max)
    }
    pub fn track_min(&self, class: ClassId) -> Nm {
        self.track[class.ix().min(self.classes.saturating_sub(1))]
    }
    pub fn track_ref(&self, class: ClassId) -> RuleRef {
        RuleRef(self.track_rule[class.ix().min(self.classes.saturating_sub(1))])
    }
    pub fn via(&self, class: ClassId) -> ViaSize {
        self.via[class.ix().min(self.classes.saturating_sub(1))]
    }
    /// A class's pair gap, clamped to the last resolved class for an unknown class.
    pub fn pair_gap(&self, class: ClassId) -> Nm {
        self.pair_gap[class.ix().min(self.classes.saturating_sub(1))]
    }
    /// The net's pair partner and gap; `None` for an unpaired or out-of-range net.
    /// The earlier valid pair wins when pairs share a net.
    pub fn partner(&self, net: NetId) -> Option<(NetId, Nm)> {
        self.partner.get(net.ix()).copied().flatten()
    }
    /// The largest resolved pair gap (0 with no pairs), for broad-phase inflation.
    pub fn max_pair_gap(&self) -> Nm {
        self.max_pair_gap
    }
    /// The differential pair gap rule (`RuleRef::PAIR_GAP`).
    pub fn pair_gap_ref(&self) -> RuleRef {
        RuleRef::PAIR_GAP
    }
    pub fn edge(&self) -> Nm {
        self.edge
    }
    pub fn edge_ref(&self) -> RuleRef {
        RuleRef(self.edge_rule)
    }
    /// Required silk clearance from mask openings on the same side.
    pub fn silk_clearance(&self) -> Nm {
        self.silk
    }
    /// The rule that sets the silk to mask opening clearance.
    pub fn silk_clearance_ref(&self) -> RuleRef {
        RuleRef(self.silk_rule)
    }
    pub fn drill_min(&self) -> Nm {
        self.drill
    }
    pub fn drill_ref(&self) -> RuleRef {
        RuleRef(self.drill_rule)
    }
    pub fn annular_min(&self) -> Nm {
        self.annular
    }
    pub fn annular_ref(&self) -> RuleRef {
        RuleRef(self.annular_rule)
    }
    pub fn hole_to_hole(&self) -> Nm {
        self.hole_to_hole
    }
    pub fn hole_to_hole_ref(&self) -> RuleRef {
        RuleRef(self.hole_to_hole_rule)
    }
    pub fn hole_clearance(&self) -> Nm {
        self.hole_clearance
    }
    pub fn hole_clearance_ref(&self) -> RuleRef {
        RuleRef(self.hole_clearance_rule)
    }
    /// The largest clearance any pair can require (for broad-phase inflation).
    pub fn max_clearance(&self) -> Nm {
        self.max_clearance
    }
}

/// Resolve the board's rules. Custom rules need `src` to evaluate their `when` fns; without one
/// they are left out (the base rules and net classes still resolve).
pub fn resolve_rules(board: &Board, src: Option<&dyn RuleSource>) -> RuleMatrix {
    resolve_rules_with(board, None, src)
}

/// Resolve effective rules for this pass, naming the factory only for limits it raised.
pub fn resolve_rules_with(board: &Board, effective: Option<&Effective>, src: Option<&dyn RuleSource>) -> RuleMatrix {
    let own = resolve_set(board, &board.rules, src);
    let Some(e) = effective else { return own };
    let mut m = resolve_set(board, &e.rules, src);
    m.factory_via_in_pad = e.via_in_pad;
    let mut factory_rule = |base: RuleRef| {
        let r = m.names.len() as u16;
        m.names.push(format!("{}: {} minimum", RuleRef::BUILTIN[base.0 as usize], e.origin));
        m.origins.push(Some(e.origin.clone()));
        r
    };

    // Capabilities preserve custom predicates and layers; a layered rule sets one clearance
    // for every pair it matches, so its factory row is shared by all those entries too.
    let raised_layered: Vec<bool> = m.layered.iter().zip(&own.layered).map(|((_, values, _), (_, before, _))| {
        values.iter().zip(before).any(|(value, old)| matches!((value, old), (Some(value), Some(old)) if value > old))
    }).collect();
    if m.clearance.iter().zip(&own.clearance).any(|(value, old)| value > old) || raised_layered.iter().any(|raised| *raised) {
        let rule = factory_rule(RuleRef::CLEARANCE);
        for ((value, old), r) in m.clearance.iter().zip(&own.clearance).zip(&mut m.clearance_rule) {
            if value > old {
                *r = rule;
            }
        }
        for ((_, _, r), raised) in m.layered.iter_mut().zip(raised_layered) {
            if raised {
                *r = rule;
            }
        }
    }
    if m.track.iter().zip(&own.track).any(|(value, old)| value > old) {
        let rule = factory_rule(RuleRef::TRACK);
        for ((value, old), r) in m.track.iter().zip(&own.track).zip(&mut m.track_rule) {
            if value > old {
                *r = rule;
            }
        }
    }
    for (value, old, r, base) in [
        (m.drill, own.drill, &mut m.drill_rule, RuleRef::DRILL),
        (m.annular, own.annular, &mut m.annular_rule, RuleRef::ANNULAR),
        (m.edge, own.edge, &mut m.edge_rule, RuleRef::EDGE),
        (m.hole_to_hole, own.hole_to_hole, &mut m.hole_to_hole_rule, RuleRef::HOLE_TO_HOLE),
        (m.hole_clearance, own.hole_clearance, &mut m.hole_clearance_rule, RuleRef::HOLE_CLEARANCE),
    ] {
        if value > old {
            *r = factory_rule(base);
        }
    }
    m
}

fn resolve_set(board: &Board, rules: &Rules, src: Option<&dyn RuleSource>) -> RuleMatrix {
    let base = &rules.base;
    let std = RuleSet::standard();
    let pick = |o: Option<Nm>, d: Option<Nm>| o.or(d).unwrap_or(0);
    let classes = board.classes.len().max(1);
    let class_rule = |c: usize| board.classes.get(c).map(|k| k.rules.clone()).unwrap_or_default();
    let base_clear = pick(base.clearance, std.clearance);
    let mut names: Vec<String> = RuleRef::BUILTIN.iter().map(|s| s.to_string()).collect();
    let n = classes * ITEMS;
    let mut clearance = vec![0; n * n];
    let mut clearance_rule = vec![0u16; n * n];
    for a in 0..n {
        for b in 0..n {
            let ca = class_rule(a / ITEMS).clearance.unwrap_or(base_clear);
            let cb = class_rule(b / ITEMS).clearance.unwrap_or(base_clear);
            clearance[a * n + b] = ca.max(cb);
        }
    }
    let base_via = base.via.or(std.via).unwrap();
    let mut track: Vec<Nm> = (0..classes).map(|c| class_rule(c).track.unwrap_or(pick(base.track, std.track))).collect();
    let mut via: Vec<ViaSize> = (0..classes).map(|c| class_rule(c).via.unwrap_or(base_via)).collect();
    let mut pair_gap: Vec<Nm> = (0..classes)
        .map(|c| class_rule(c).pair.map(|p| p.gap).unwrap_or(pick(base.pair_gap, std.pair_gap)))
        .collect();
    let mut layered = Vec::new();
    if let Some(src) = src {
        let props = |k: usize| RuleProps {
            class: ClassId((k / ITEMS) as u16),
            class_name: board.classes.get(k / ITEMS).map(|c| c.name.clone()).unwrap_or_default(),
            item: RuleItem::ALL[k % ITEMS],
        };
        for rule in &rules.custom {
            names.push(rule.name.clone());
            let name_ix = (names.len() - 1) as u16;
            let mut m: Vec<Option<Nm>> = vec![None; n * n];
            for a in 0..n {
                for b in 0..n {
                    if rule.set.clearance.is_some() && src.when(&rule.when, &props(a), &props(b)) {
                        m[a * n + b] = rule.set.clearance;
                    }
                }
            }
            for c in 0..classes {
                let k = c * ITEMS + RuleItem::Track as usize;
                if (rule.set.track.is_some() || rule.set.via.is_some() || rule.set.pair_gap.is_some())
                    && src.when(&rule.when, &props(k), &props(k))
                {
                    if let Some(t) = rule.set.track {
                        track[c] = t;
                    }
                    if let Some(v) = rule.set.via {
                        via[c] = v;
                    }
                    if let Some(g) = rule.set.pair_gap {
                        pair_gap[c] = g;
                    }
                }
            }
            match rule.layers {
                Some(l) => layered.push((l, m, name_ix)),
                None => {
                    for (i, v) in m.into_iter().enumerate() {
                        if let Some(v) = v {
                            clearance[i] = v;
                            clearance_rule[i] = name_ix;
                        }
                    }
                }
            }
        }
    }
    // The language refuses a pair whose halves are one net, so every pair names two distinct nets.
    let mut partner = vec![None; board.nets.len()];
    let mut max_pair_gap = 0;
    for pair in &board.pairs {
        let (p, n) = (pair.p.ix(), pair.n.ix());
        if p >= partner.len() || n >= partner.len() || partner[p].is_some() || partner[n].is_some() {
            continue;
        }
        let class_gap = |i: usize| pair_gap[board.nets[i].class.ix().min(classes.saturating_sub(1))];
        let g = pair.gap.unwrap_or_else(|| class_gap(p).max(class_gap(n)));
        partner[p] = Some((pair.n, g));
        partner[n] = Some((pair.p, g));
        max_pair_gap = max_pair_gap.max(g);
    }
    let max_layered = layered.iter().flat_map(|(_, m, _)| m.iter().flatten().copied()).max().unwrap_or(0);
    let edge = pick(base.edge, std.edge);
    let hole_clearance = pick(base.hole_clearance, std.hole_clearance);
    let max_clearance = clearance.iter().copied().max().unwrap_or(0).max(max_layered).max(edge).max(hole_clearance);
    RuleMatrix {
        classes,
        clearance,
        clearance_rule,
        layered,
        track,
        track_rule: vec![RuleRef::TRACK.0; classes],
        via,
        pair_gap,
        partner,
        origins: vec![None; names.len()],
        names,
        class_rules: board.classes.iter().map(|c| c.rules.clone()).collect(),
        default_class: ClassRules::default(),
        factory_via_in_pad: true,
        mask_margin: pick(base.mask_margin, std.mask_margin),
        edge,
        edge_rule: RuleRef::EDGE.0,
        silk: pick(base.silk_clearance, std.silk_clearance),
        silk_rule: RuleRef::SILK.0,
        drill: pick(base.drill, std.drill),
        drill_rule: RuleRef::DRILL.0,
        annular: pick(base.annular, std.annular),
        annular_rule: RuleRef::ANNULAR.0,
        hole_to_hole: pick(base.hole_to_hole, std.hole_to_hole),
        hole_to_hole_rule: RuleRef::HOLE_TO_HOLE.0,
        hole_clearance,
        hole_clearance_rule: RuleRef::HOLE_CLEARANCE.0,
        max_clearance,
        max_pair_gap,
    }
}
