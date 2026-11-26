//! The infill patterns a profile can select, parsed once from the job.
//!
//! Every name a process profile may carry for `sparse_infill_pattern`,
//! `top_surface_pattern`, `bottom_surface_pattern`,
//! `internal_solid_infill_pattern` and the locked-zag skin/skeleton
//! patterns maps to its own generator. A name outside these lists is an
//! error ([`check`]); it never falls back to another pattern.

use makepad_fab_settings::machine::Job;

/// Sparse infill patterns (`sparse_infill_pattern`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sparse {
    /// "zig-zag": one line family per layer, the direction turning 90°
    /// every layer, lines joined along the walls within the anchor reach.
    Rectilinear,
    /// "alignedrectilinear": as rectilinear, the same direction every layer.
    AlignedRectilinear,
    /// "line": one family per layer turning 90°, lines not joined.
    Line,
    /// "zigzag": rectilinear lines joined into continuous zig-zags.
    ZigZag,
    /// "crosszag": zig-zags whose lines shift sideways a step per layer
    /// (`infill_shift_step`), reversing at half a spacing, so neighbouring
    /// layers cross.
    CrossZag,
    /// "lockedzag": a skin band along the walls and a skeleton inside, each
    /// with its own pattern, density and width, overlapping by the lock depth.
    LockedZag,
    /// "grid": two families per layer.
    Grid,
    /// "crosshatch": one family per layer that switches direction every
    /// block of layers through a short zig-zag transition.
    CrossHatch,
    /// "2dlattice": two families with the same direction whose offsets
    /// move with z at the two lattice angles (an X lattice in side view).
    Lattice2d,
    /// "triangles": three families meeting at common points.
    Triangles,
    /// "tri-hexagon": three families, one shifted half a spacing (stars).
    TriHexagon,
    /// "cubic": three families whose offsets move with z (cubes standing
    /// on a corner).
    Cubic,
    /// "adaptivecubic": cubic, its cells subdivided towards horizontal
    /// surfaces above and below and coarser away from them.
    AdaptiveCubic,
    /// "supportcubic": cubic, subdivided only under top surfaces.
    SupportCubic,
    /// "gyroid": sections of the gyroid surface.
    Gyroid,
    /// "tpmsd": sections of the Schwarz diamond surface.
    TpmsD,
    /// "tpmsfk": sections of the Fischer-Koch S surface.
    TpmsFk,
    /// "honeycomb": hexagon prisms.
    Honeycomb,
    /// "3dhoneycomb": stacked truncated-octahedron-like cells: trapezoid
    /// waves whose flats change with z, the direction switching per block.
    Honeycomb3d,
    /// "lightning": trees that only hold up the surfaces above them.
    Lightning,
    /// "hilbertcurve": a Hilbert curve.
    Hilbert,
    /// "archimedeanchords": an Archimedean spiral of chords.
    ArchimedeanChords,
    /// "octagramspiral": a spiral of eight-pointed stars.
    OctagramSpiral,
    /// "concentric": loops parallel to the walls.
    Concentric,
}

impl Sparse {
    pub const ALL: [(&'static str, Sparse); 24] = [
        ("zig-zag", Sparse::Rectilinear),
        ("alignedrectilinear", Sparse::AlignedRectilinear),
        ("line", Sparse::Line),
        ("zigzag", Sparse::ZigZag),
        ("crosszag", Sparse::CrossZag),
        ("lockedzag", Sparse::LockedZag),
        ("grid", Sparse::Grid),
        ("crosshatch", Sparse::CrossHatch),
        ("2dlattice", Sparse::Lattice2d),
        ("triangles", Sparse::Triangles),
        ("tri-hexagon", Sparse::TriHexagon),
        ("cubic", Sparse::Cubic),
        ("adaptivecubic", Sparse::AdaptiveCubic),
        ("supportcubic", Sparse::SupportCubic),
        ("gyroid", Sparse::Gyroid),
        ("tpmsd", Sparse::TpmsD),
        ("tpmsfk", Sparse::TpmsFk),
        ("honeycomb", Sparse::Honeycomb),
        ("3dhoneycomb", Sparse::Honeycomb3d),
        ("lightning", Sparse::Lightning),
        ("hilbertcurve", Sparse::Hilbert),
        ("archimedeanchords", Sparse::ArchimedeanChords),
        ("octagramspiral", Sparse::OctagramSpiral),
        ("concentric", Sparse::Concentric),
    ];

    pub fn parse(name: &str) -> Option<Sparse> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
    }

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(_, p)| *p == self).map(|(n, _)| *n).unwrap()
    }

    /// Patterns whose layers depend on other layers (a pass over the whole
    /// object before the per-layer fill).
    pub fn cross_layer(self) -> bool {
        matches!(self, Sparse::Lightning | Sparse::AdaptiveCubic | Sparse::SupportCubic)
    }

    /// Patterns allowed as the locked-zag skin or skeleton.
    pub fn lockable(self) -> bool {
        !matches!(self, Sparse::LockedZag | Sparse::Lightning | Sparse::AdaptiveCubic | Sparse::SupportCubic | Sparse::Lattice2d)
    }
}

/// Solid surface patterns (top, bottom, internal solid).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// "zig-zag": lines joined along the boundary, turning 90° per layer.
    Rectilinear,
    /// "alignedrectilinear": as rectilinear, the same direction every layer.
    AlignedRectilinear,
    /// "monotonic": lines laid in one sweep order, joined at the ends.
    Monotonic,
    /// "monotonicline": monotonic lines, each its own stroke.
    MonotonicLine,
    /// "globalmonotonicline" (top only): monotonic lines over the layer.
    GlobalMonotonicLine,
    Concentric,
    Hilbert,
    ArchimedeanChords,
    OctagramSpiral,
}

impl Surface {
    pub const ALL: [(&'static str, Surface); 9] = [
        ("zig-zag", Surface::Rectilinear),
        ("alignedrectilinear", Surface::AlignedRectilinear),
        ("monotonic", Surface::Monotonic),
        ("monotonicline", Surface::MonotonicLine),
        ("globalmonotonicline", Surface::GlobalMonotonicLine),
        ("concentric", Surface::Concentric),
        ("hilbertcurve", Surface::Hilbert),
        ("archimedeanchords", Surface::ArchimedeanChords),
        ("octagramspiral", Surface::OctagramSpiral),
    ];

    pub fn parse(name: &str) -> Option<Surface> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
    }

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(_, p)| *p == self).map(|(n, _)| *n).unwrap()
    }

    pub fn monotonic(self) -> bool {
        matches!(self, Surface::Monotonic | Surface::MonotonicLine | Surface::GlobalMonotonicLine)
    }
}

/// The pattern keys the toolpath stage reads, and which list each takes.
const SURFACE_KEYS: [&str; 3] = ["top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"];

/// Every pattern setting of the job names a pattern Fab prints. Callers
/// run this before planning: [`crate::Params::from_job`] treats an
/// unchecked unknown name as a programming error.
pub fn check(job: &Job) -> Result<(), String> {
    let mut bad = Vec::new();
    let sparse = job.s("sparse_infill_pattern");
    match Sparse::parse(sparse) {
        None => bad.push(format!("sparse_infill_pattern \"{sparse}\"")),
        Some(Sparse::LockedZag) => {
            for key in ["locked_skin_infill_pattern", "locked_skeleton_infill_pattern"] {
                let v = job.s(key);
                if !Sparse::parse(v).is_some_and(|p| p.lockable()) {
                    bad.push(format!("{key} \"{v}\""));
                }
            }
        }
        Some(_) => {}
    }
    for key in SURFACE_KEYS {
        let v = job.s(key);
        match Surface::parse(v) {
            Some(Surface::GlobalMonotonicLine) if key != "top_surface_pattern" => bad.push(format!("{key} \"{v}\" (top surfaces only)")),
            Some(_) => {}
            None => bad.push(format!("{key} \"{v}\"")),
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!("unknown infill pattern: {} (Fab prints sparse: {}; surfaces: {})", bad.join(", "), Sparse::ALL.map(|p| p.0).join(", "), Surface::ALL.map(|p| p.0).join(", ")))
    }
}

/// The parsed pattern of `key`; [`check`] has accepted the job.
pub(crate) fn sparse_of(job: &Job, key: &str) -> Sparse {
    let v = job.s(key);
    Sparse::parse(v).unwrap_or_else(|| panic!("{key} \"{v}\" is not a sparse pattern; toolpath::check the job before planning"))
}

pub(crate) fn surface_of(job: &Job, key: &str) -> Surface {
    let v = job.s(key);
    Surface::parse(v).unwrap_or_else(|| panic!("{key} \"{v}\" is not a surface pattern; toolpath::check the job before planning"))
}
