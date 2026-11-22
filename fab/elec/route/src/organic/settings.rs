// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.h (OrganicSettings, ForceTerm).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Settings and force terms of the guarded energy-descent relaxer.

use super::model::Trace;

#[derive(Clone, Debug, PartialEq)]
pub struct OrganicSettings {
    /// Spring stiffness share; its ratio to repulsion controls soft-margin penetration.
    pub tension: f32,
    /// Contraction contribution to spring stiffness, weighted by 0.5.
    pub contraction_tension: f32,
    /// Straightening contribution to spring stiffness.
    pub straightening_force: f32,
    /// Soft-clearance stiffness against pads, vias and the board edge.
    pub pad_repulsion: f32,
    /// Soft-clearance stiffness against foreign traces.
    pub trace_repulsion: f32,
    /// Crossing-angle and non-pair bending stiffness; 0 = off.
    pub layer_crossing_perpendicularity: f32,
    /// Extra via repulsion, widening the foreign-via margin by 0.1 mm per unit; 0 = off.
    pub via_via_repulsion: f32,
    /// Same-layer crossing drift (negative = normal, positive = opposite); 0 = off.
    pub crossing_disentanglement: f32,
    /// Extra width-weighted spring stiffness for wide non-pair tracks; 0 = off.
    pub wide_track_straightening: f32,
    /// Wide-track threshold as a multiple of the default trace width.
    pub wide_track_ratio: f32,
    /// Wide-track snapping tolerance around 0/45/90 degrees; 0 = off.
    pub wide_track_snap_deg: f32,
    /// Wide-track pull towards simplified straight stretches; 0 = off.
    pub wide_track_line_pull: f32,
    /// Douglas-Peucker tolerance of the line pull, in mm.
    pub wide_track_deviation: f32,
    /// Pad-exit half-plane stiffness and own-pad depth guard; 0 = off.
    pub pad_exit_half_plane: f32,
    /// Maximum turn after a fixed pad-exit stub, in degrees.
    pub pad_exit_max_turn_deg: f32,
    /// Upper bound on the preserved via-to-via run length, in mm; 0 = off.
    pub hop_keep_dist: f32,
    /// Via-hop length threshold, in mm.
    pub hop_limit: f32,
    /// Spring multiplier resisting via-hop contraction below the preserved length.
    pub hop_keep_weight: f32,
    /// Bus-centering spring multiplier for movable thin non-pair tracks; 0 = off.
    pub wire_spread: f32,
    /// Desired copper gap on each side of a bus, in mm.
    pub wire_spread_reach: f32,
    /// Maximum bus gap growth in a channel, in mm; 0 = fixed pitch.
    pub wire_spread_pitch: f32,
    /// Desired copper gap for a lone track, in mm.
    pub wire_spread_lone_reach: f32,
    /// Largest gap linking parallel tracks into a bus, in mm (at least pitch + 0.5).
    pub wire_spread_link: f32,
    /// Along-track low-pass Gaussian sigma, in mm.
    pub wire_spread_smooth: f32,
    /// Maximum absolute shift derivative along a field update.
    pub wire_spread_slope: f32,
    /// Maximum shift per field update, in mm.
    pub wire_spread_max_shift: f32,
    /// Fade distance towards anchors, in mm.
    pub wire_spread_taper: f32,
    /// Steps between wire-spread field updates.
    pub wire_spread_every: i32,
    /// Steps before the first wire-spread field update.
    pub wire_spread_start: i32,
    /// Field updates per run before targets freeze; an edit restarts the count.
    pub wire_spread_updates: i32,
    /// Soft-margin relief beside remembered straight runs at pads and vias; 0 = off.
    pub straight_pass_relief: f32,
    /// Spring multiplier preserving Route bus pitch and relieving its soft margin; 0 = off.
    pub bundle_keep: f32,
    /// Extra non-pair bending stiffness as a spring multiplier; 0 = off.
    pub straight_bend: f32,
    /// SOR factor for nodes carrying straight bending, except junction hosts.
    pub bend_over_relaxation: f32,
    /// Settle wide bending nodes by the residual force for a plain node's 1 um move.
    pub wide_bend_settle: bool,
    /// Straight-run relief taper distance from the ends, in mm.
    pub straight_taper: f32,
    /// Minimum remembered straight-run length, in mm.
    pub straight_min_len: f32,
    /// Soft buffer beyond the minimum clearance, in mm.
    pub clearance_margin: f32,
    /// Used only to report velocity as displacement / dt.
    pub dt: f32,
    /// Gauss-Seidel sweeps per step.
    pub substeps_per_frame: i32,
    /// Bind differential pairs by nearest-point distance.
    pub enable_diff_pair_coupling: bool,
    /// Fallback pair centre-to-centre spacing when no gap is specified, in mm.
    pub diff_pair_distance: f32,
    /// Pair spring stiffness multiplier (16 times this times trace spring stiffness).
    pub diff_pair_coupling_strength: f32,
    /// Keep routed geometry on coupled stretches of a pair.
    pub diff_pair_rigid: bool,
    /// Tune intra-pair skew with local crinkles on the shorter member.
    pub enable_diff_pair_skew_tuning: bool,
    /// Intra-pair skew tolerance, in mm (also subject to the 1% criterion).
    pub diff_pair_skew_tolerance: f32,
    /// Maximum skew-crinkle amplitude as a multiple of the pair gap.
    pub diff_pair_skew_amplitude: f32,
    /// Share of the soft margin kept from partner pads/vias, 0..1; 0 = hard clearance only.
    pub diff_pair_partner_margin: f32,
    /// Allow skew crinkles without a coupling-loss budget.
    pub diff_pair_crinkle_coupled: bool,
    /// Allowed crinkle coupling loss, in percentage points of initial coupled length.
    pub diff_pair_crinkle_max_loss: f32,
    /// Apply the coupling-loss guard per member trace as well as to the whole pair.
    pub diff_pair_crinkle_segment_guard: bool,
    /// Pin routed pad approaches up to the first coupled node within 1.5 mm of arc.
    pub diff_pair_pin_approach: bool,
    /// Pinned arc past the first coupled node, in mm; 0 = only 1.2 mm from the pad.
    pub diff_pair_pin_past: f32,
    /// Pin whole pad-to-pad pair traces no longer than this, in mm; 0 = off.
    pub diff_pair_pin_short: f32,
    /// Enable organic meanders and crinkles.
    pub enable_length_tuning: bool,
    /// Organic meander frequency.
    pub meander_frequency: f32,
    /// Organic meander amplitude.
    pub meander_amplitude: f32,
    /// Spline interpolation segments between nodes.
    pub curve_subdivisions: i32,
    /// Move vias with traces, tethering escape vias to their pads.
    pub enable_dynamic_vias: bool,
    /// Worker threads; 0 = process default, 1 = serial.
    pub threads: i32,
    /// Defer nonessential spline updates above this awake-node count; -1 = off.
    pub defer_splines_awake: i32,
    /// Generate continuous circular-arc traces.
    pub enable_biarcs: bool,
    /// Narrow exported trace width where clearance is tight.
    pub enable_dynamic_neckdown: bool,
    /// Minimum neckdown width, in mm, never below the board's minimum trace width.
    pub min_neckdown_width: f32,
    /// Nominal-to-neck width transition length, in mm.
    pub neckdown_taper_length: f32,
    /// Export terminal-pad neckdowns as constant-width stubs instead of tapers.
    pub neckdown_pad_stub: bool,
    /// Flare copper into pad, via and T-junction connections.
    pub enable_teardrops: bool,
    /// Skip teardrops narrower than this multiple of the trace width.
    pub teardrop_flare_factor: f32,
    /// Teardrop width divided by junction cross size.
    pub teardrop_width_ratio: f32,
    /// Length beyond the pad edge divided by junction cross size.
    pub teardrop_length_ratio: f32,
    /// Maximum teardrop width, in mm.
    pub teardrop_max_width: f32,
    /// Maximum teardrop length, in mm.
    pub teardrop_max_length: f32,
    /// Flank curvature: 0 = full smoothstep flank, 1 = concave fillet.
    pub teardrop_curvature: f32,
    /// Generate teardrops where a branch joins another trace.
    pub teardrop_t_junctions: bool,
    /// Teardrop style: 0 = classic band, 1 = fillet, 2 = distance field.
    pub teardrop_style: i32,
    /// Tune accordions/meanders to a positive net target length.
    pub enable_target_length_matching: bool,
    /// Relative target-length tolerance (0.02 = 2%).
    pub length_matching_tolerance: f32,
    /// Widen the soft margin to centre traces in corridors.
    pub enable_medial_axis_centering: bool,
    /// Strength of medial-axis corridor centering.
    pub medial_axis_centering_strength: f32,
    /// Simplify self-intersecting net loops into Steiner trees.
    pub enable_loop_simplification: bool,
    /// Reject moves that worsen clearance or topology; keep enabled.
    pub enable_drc_guard: bool,
    /// Maximum node displacement per substep, in mm.
    pub max_node_step: f32,
    /// Unconstrained-node SOR factor; 1 = Gauss-Seidel.
    pub over_relaxation: f32,
    /// Maximum adaptive segment length, in mm; merge below 0.45 times this.
    pub node_spacing: f32,
    /// Float-safety margin added to the guard's minimum clearance, in mm.
    pub guard_clearance_slack: f32,
    /// Keep vias outside footprint courtyards without a pad of their net.
    pub via_courtyard_guard: bool,
    /// Limit spline overshoot to preserve smoothed-path clearance.
    pub guard_splines: bool,
    /// Adopt and relax imported unlocked routing; otherwise it stays immutable.
    pub edit_imported: bool,
}

impl Default for OrganicSettings {
    fn default() -> Self {
        Self {
            tension: 1.2,
            contraction_tension: 1.0,
            straightening_force: 1.5,
            pad_repulsion: 3.5,
            trace_repulsion: 2.0,
            layer_crossing_perpendicularity: 1.0,
            via_via_repulsion: 1.0,
            crossing_disentanglement: 0.0,
            wide_track_straightening: 4.0,
            wide_track_ratio: 1.5,
            wide_track_snap_deg: 0.0,
            wide_track_line_pull: 0.0,
            wide_track_deviation: 0.5,
            pad_exit_half_plane: 8.0,
            pad_exit_max_turn_deg: 60.0,
            hop_keep_dist: 4.0,
            hop_limit: 3.0,
            hop_keep_weight: 8.0,
            wire_spread: 1.0,
            wire_spread_reach: 1.2,
            wire_spread_pitch: 1.4,
            wire_spread_lone_reach: 0.9,
            wire_spread_link: 1.25,
            wire_spread_smooth: 2.5,
            wire_spread_slope: 0.15,
            wire_spread_max_shift: 0.25,
            wire_spread_taper: 2.0,
            wire_spread_every: 5,
            wire_spread_start: 30,
            wire_spread_updates: 12,
            straight_pass_relief: 0.0,
            bundle_keep: 4.0,
            straight_bend: 4.0,
            bend_over_relaxation: 1.9,
            wide_bend_settle: false,
            straight_taper: 1.5,
            straight_min_len: 2.5,
            clearance_margin: 0.35,
            dt: 0.04,
            substeps_per_frame: 6,
            enable_diff_pair_coupling: true,
            diff_pair_distance: 0.5,
            diff_pair_coupling_strength: 1.5,
            diff_pair_rigid: false,
            enable_diff_pair_skew_tuning: true,
            diff_pair_skew_tolerance: 0.1,
            diff_pair_skew_amplitude: 3.0,
            diff_pair_partner_margin: 0.0,
            diff_pair_crinkle_coupled: false,
            diff_pair_crinkle_max_loss: 2.0,
            diff_pair_crinkle_segment_guard: true,
            diff_pair_pin_approach: true,
            diff_pair_pin_past: 1.2,
            diff_pair_pin_short: 8.0,
            enable_length_tuning: false,
            meander_frequency: 0.25,
            meander_amplitude: 0.8,
            curve_subdivisions: 6,
            enable_dynamic_vias: true,
            threads: 0,
            defer_splines_awake: -1,
            enable_biarcs: false,
            enable_dynamic_neckdown: true,
            min_neckdown_width: 0.15,
            neckdown_taper_length: 1.0,
            neckdown_pad_stub: false,
            enable_teardrops: true,
            teardrop_flare_factor: 1.25,
            teardrop_width_ratio: 0.9,
            teardrop_length_ratio: 0.6,
            teardrop_max_width: 2.0,
            teardrop_max_length: 1.2,
            teardrop_curvature: 0.6,
            teardrop_t_junctions: true,
            teardrop_style: 2,
            enable_target_length_matching: true,
            length_matching_tolerance: 0.02,
            enable_medial_axis_centering: true,
            medial_axis_centering_strength: 0.5,
            enable_loop_simplification: true,
            enable_drc_guard: true,
            max_node_step: 0.1,
            over_relaxation: 1.6,
            node_spacing: 1.2,
            guard_clearance_slack: 0.002,
            via_courtyard_guard: false,
            guard_splines: true,
            edit_imported: false,
        }
    }
}

impl OrganicSettings {
    /// The first landing ports the relaxer's base path; each optional term is added with
    /// a test that 0 is identical. Starting from `Default`, disables
    /// `layer_crossing_perpendicularity`, `via_via_repulsion`, `crossing_disentanglement`,
    /// `wide_track_straightening`, `wide_track_snap_deg`, `wide_track_line_pull`,
    /// `pad_exit_half_plane`, `hop_keep_dist`, `wire_spread`, `straight_pass_relief`,
    /// `bundle_keep`, `straight_bend`, `wide_bend_settle`, `enable_diff_pair_coupling`,
    /// `enable_diff_pair_skew_tuning`, `enable_length_tuning`, `enable_target_length_matching`,
    /// `enable_dynamic_vias`, `enable_loop_simplification`, `enable_biarcs`,
    /// `enable_dynamic_neckdown` and `enable_teardrops` (0.0 for strengths, false for flags).
    pub fn base() -> OrganicSettings {
        Self {
            layer_crossing_perpendicularity: 0.0,
            via_via_repulsion: 0.0,
            crossing_disentanglement: 0.0,
            wide_track_straightening: 0.0,
            wide_track_snap_deg: 0.0,
            wide_track_line_pull: 0.0,
            pad_exit_half_plane: 0.0,
            hop_keep_dist: 0.0,
            wire_spread: 0.0,
            straight_pass_relief: 0.0,
            bundle_keep: 0.0,
            straight_bend: 0.0,
            wide_bend_settle: false,
            enable_diff_pair_coupling: false,
            enable_diff_pair_skew_tuning: false,
            enable_length_tuning: false,
            enable_target_length_matching: false,
            enable_dynamic_vias: false,
            enable_loop_simplification: false,
            enable_biarcs: false,
            enable_dynamic_neckdown: false,
            enable_teardrops: false,
            ..Self::default()
        }
    }
}

pub fn relax_immutable(t: &Trace, s: &OrganicSettings) -> bool {
    t.locked || (t.imported && !s.edit_imported)
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum ForceTerm {
    Tension = 0,
    Contraction,
    Straightening,
    PadRepulsion,
    TraceRepulsion,
    ViaRepulsion,
    Perpendicularity,
    Disentanglement,
    Coupling,
    Crinkle,
    Meander,
    Tether,
    PadExit,
}

pub const FORCE_TERM_COUNT: usize = 13;

impl ForceTerm {
    pub const ALL: [Self; FORCE_TERM_COUNT] = [
        Self::Tension,
        Self::Contraction,
        Self::Straightening,
        Self::PadRepulsion,
        Self::TraceRepulsion,
        Self::ViaRepulsion,
        Self::Perpendicularity,
        Self::Disentanglement,
        Self::Coupling,
        Self::Crinkle,
        Self::Meander,
        Self::Tether,
        Self::PadExit,
    ];
}

const K_FORCE_KEYS: [&str; FORCE_TERM_COUNT] = [
    "tension",
    "contraction",
    "straightening",
    "pad_repulsion",
    "trace_repulsion",
    "via_repulsion",
    "perpendicularity",
    "disentanglement",
    "coupling",
    "crinkle",
    "meander",
    "tether",
    "pad_exit",
];

const K_FORCE_LABELS: [&str; FORCE_TERM_COUNT] = [
    "Tension",
    "Length contraction",
    "Straightening",
    "Pad repulsion",
    "Trace repulsion",
    "Via-to-via repulsion",
    "Layer-crossing perpendicularity",
    "Disentanglement drift",
    "Pair coupling",
    "Skew crinkle",
    "Meander",
    "Via tether",
    "Pad-exit half plane",
];

pub fn force_term_key(t: ForceTerm) -> &'static str {
    K_FORCE_KEYS[t as usize]
}

pub fn force_term_label(t: ForceTerm) -> &'static str {
    K_FORCE_LABELS[t as usize]
}

pub fn force_term_from_key(key: &str) -> Option<ForceTerm> {
    for i in 0..FORCE_TERM_COUNT {
        if key == K_FORCE_KEYS[i] {
            return Some(ForceTerm::ALL[i]);
        }
    }
    None
}
