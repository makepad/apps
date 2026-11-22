//! Stage timers for the slicing pipeline: summed wall time per stage over
//! every thread, read by `fab-slice bench`. Off unless [`enable`] was called;
//! a disabled scope costs one relaxed load.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Contours,
    Paint,
    Walls,
    ArachneSkeleton,
    ArachneBeads,
    ArachneTrace,
    Classes,
    TreeSupport,
    TreeOverhangs,
    TreeVolumes,
    TreeTips,
    TreeDescent,
    TreePlace,
    TreeSmooth,
    TreeDraw,
    TreeInterface,
    CoralSupport,
    NormalSupport,
    Infill,
    InfillBooleans,
    InfillConcentric,
    Overhangs,
    OverhangPrepare,
    OverhangCuts,
    OverhangDegree,
    OverhangRuns,
    Merge,
    CheckLower,
    CheckThin,
    CheckIslands,
    CheckBridges,
    CheckBridgeSpans,
    CheckCantilevers,
    CheckSupportArea,
    CheckMoves,
    SupportRules,
    GcodePlan,
    GcodeLayers,
    GcodeTime,
    GcodeAssemble,
    GcodeTemplates,
    GcodeTravels,
    GcodeExtrusions,
    GcodeReadBack,
    GcodeModelPlan,
    GcodeModelOps,
    Package,
    PackageHashes,
    PackageRenders,
    PackagePngs,
    PackageDeflate,
    LayerTimeline,
}

pub const STAGES: [Stage; 52] = [
    Stage::Contours, Stage::Paint, Stage::Walls, Stage::ArachneSkeleton, Stage::ArachneBeads, Stage::ArachneTrace, Stage::Classes, Stage::TreeSupport,
    Stage::TreeOverhangs, Stage::TreeVolumes, Stage::TreeTips, Stage::TreeDescent, Stage::TreePlace, Stage::TreeSmooth, Stage::TreeDraw, Stage::TreeInterface, Stage::CoralSupport, Stage::NormalSupport,
    Stage::Infill, Stage::InfillBooleans, Stage::InfillConcentric, Stage::Overhangs,
    Stage::OverhangPrepare, Stage::OverhangCuts, Stage::OverhangDegree, Stage::OverhangRuns, Stage::Merge,
    Stage::CheckLower, Stage::CheckThin, Stage::CheckIslands, Stage::CheckBridges, Stage::CheckBridgeSpans, Stage::CheckCantilevers, Stage::CheckSupportArea, Stage::CheckMoves, Stage::SupportRules,
    Stage::GcodePlan, Stage::GcodeLayers, Stage::GcodeTime, Stage::GcodeAssemble,
    Stage::GcodeTemplates, Stage::GcodeTravels, Stage::GcodeExtrusions, Stage::GcodeReadBack, Stage::GcodeModelPlan, Stage::GcodeModelOps, Stage::Package,
    Stage::PackageHashes, Stage::PackageRenders, Stage::PackagePngs, Stage::PackageDeflate, Stage::LayerTimeline,
];

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Contours => "contours",
            Stage::Paint => "paint",
            Stage::Walls => "walls (incl. overhang split)",
            Stage::ArachneSkeleton => "  walls: skeleton (variable width)",
            Stage::ArachneBeads => "  walls: bead layouts",
            Stage::ArachneTrace => "  walls: bead lines",
            Stage::Classes => "surface classes",
            Stage::TreeSupport => "tree support",
            Stage::TreeOverhangs => "  tree: overhangs",
            Stage::TreeVolumes => "  tree: volumes",
            Stage::TreeTips => "  tree: tips and roofs",
            Stage::TreeDescent => "  tree: descent and merges",
            Stage::TreePlace => "  tree: placement",
            Stage::TreeSmooth => "  tree: smoothing",
            Stage::TreeDraw => "  tree: drawing",
            Stage::TreeInterface => "  tree: interfaces and lines",
            Stage::CoralSupport => "coral support",
            Stage::NormalSupport => "normal support",
            Stage::Infill => "infill",
            Stage::InfillBooleans => "  infill: class clips and line regions",
            Stage::InfillConcentric => "  infill: narrow test, concentric loops, bands",
            Stage::Overhangs => "overhang split",
            Stage::OverhangPrepare => "  overhang: layer below grown + grids",
            Stage::OverhangCuts => "  overhang: support crossings",
            Stage::OverhangDegree => "  overhang: degree distances",
            Stage::OverhangRuns => "  overhang: runs simplified + smoothed",
            Stage::Merge => "merge layers",
            Stage::CheckLower => "check: layer below + support window",
            Stage::CheckThin => "check: thin walls",
            Stage::CheckIslands => "check: mid-air + overhang cover",
            Stage::CheckBridges => "check: bridges + cantilevers",
            Stage::CheckBridgeSpans => "  check: bridge spans",
            Stage::CheckCantilevers => "  check: cantilever reach",
            Stage::CheckSupportArea => "check: support area from paths",
            Stage::CheckMoves => "check: moves",
            Stage::SupportRules => "check: support rules (tree report)",
            Stage::GcodePlan => "gcode: plan + fits",
            Stage::GcodeLayers => "gcode: layers written",
            Stage::GcodeTime => "gcode: time model",
            Stage::GcodeAssemble => "gcode: assemble",
            Stage::GcodeTemplates => "  gcode: templates rendered",
            Stage::GcodeTravels => "  gcode: travels",
            Stage::GcodeExtrusions => "  gcode: extrusions",
            Stage::GcodeReadBack => "  gcode: lines read back",
            Stage::GcodeModelPlan => "  gcode: time model motion planning",
            Stage::GcodeModelOps => "  gcode: time model lines interpreted",
            Stage::Package => "package (3MF)",
            Stage::PackageHashes => "  package: G-code MD5 + CRC",
            Stage::PackageRenders => "  package: plate renders",
            Stage::PackagePngs => "  package: PNG encodes",
            Stage::PackageDeflate => "  package: G-code deflate",
            Stage::LayerTimeline => "layer timeline",
        }
    }
}

static ON: AtomicBool = AtomicBool::new(false);
static NS: [AtomicU64; STAGES.len()] = [const { AtomicU64::new(0) }; STAGES.len()];

pub fn enable(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub struct Scope(Option<(Stage, Instant)>);

impl Drop for Scope {
    fn drop(&mut self) {
        if let Some((s, t)) = self.0 {
            NS[s as usize].fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
    }
}

/// Time until the returned guard drops.
pub fn scope(s: Stage) -> Scope {
    Scope(if ON.load(Ordering::Relaxed) { Some((s, Instant::now())) } else { None })
}

/// `f` timed as `s`: its wall time on this thread, plus what helpers spend
/// on the fan-outs it starts (work a call hands to other threads, where no
/// scope of the caller's can reach it).
pub fn scope_fanned<T>(s: Stage, f: impl FnOnce() -> T) -> T {
    let _t = scope(s);
    if ON.load(Ordering::Relaxed) { makepad_fab_slicer::par::timing_helpers(&NS[s as usize], f) } else { f() }
}

/// The summed seconds per stage since the last call, and reset.
pub fn take() -> Vec<(Stage, f64)> {
    STAGES.iter().map(|&s| (s, NS[s as usize].swap(0, Ordering::Relaxed) as f64 * 1e-9)).collect()
}
