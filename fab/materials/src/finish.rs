//! The finish table and the rules read from it.
//!
//! Sources: the SPI rows are the mould polish grades A-1 to D-3 as the SPI (now PLASTICS)
//! finish guide names them, with their polishing media; their Ra ranges are approximate. The
//! VDI 3400 rows are the standard's own Ra values (grade = 20 log10(Ra / 0.1 µm), rounded as
//! the standard's table has them). The machined rows' upper bounds are the ISO 1302 roughness
//! grades N8 (Ra 3.2 µm) and N6 (0.8 µm), their lower bounds approximate. Every other row
//! (etched grain, printed, sanded, coated, as made) is approximate, with no source recorded.

use crate::*;

/// Every finish, in table order. [`FinishId`] indexes it.
pub static FINISHES: &[Finish] = &[
    Finish {
        id: "spi_a1",
        standard: FinishStandard::Spi,
        grade: "A-1",
        description: "Grade #3 diamond buff, mirror / optical",
        ra_um: Range::new(0.012, 0.025),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: true,
        frosted: false,
        draft_deg: [0.5, 0.5, 1.0],
    },
    Finish {
        id: "spi_a2",
        standard: FinishStandard::Spi,
        grade: "A-2",
        description: "Grade #6 diamond buff, high gloss",
        ra_um: Range::new(0.025, 0.05),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: true,
        frosted: false,
        draft_deg: [0.5, 0.5, 1.0],
    },
    Finish {
        id: "spi_a3",
        standard: FinishStandard::Spi,
        grade: "A-3",
        description: "Grade #15 diamond buff, normal gloss",
        ra_um: Range::new(0.05, 0.10),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: true,
        frosted: false,
        draft_deg: [0.5, 0.5, 1.0],
    },
    Finish {
        id: "spi_b1",
        standard: FinishStandard::Spi,
        grade: "B-1",
        description: "600 grit paper, semi-gloss",
        ra_um: Range::new(0.05, 0.10),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: false,
        draft_deg: [0.5, 1.0, 1.0],
    },
    Finish {
        id: "spi_b2",
        standard: FinishStandard::Spi,
        grade: "B-2",
        description: "400 grit paper, semi-gloss",
        ra_um: Range::new(0.10, 0.15),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: false,
        draft_deg: [0.5, 1.0, 1.0],
    },
    Finish {
        id: "spi_b3",
        standard: FinishStandard::Spi,
        grade: "B-3",
        description: "320 grit paper, low gloss",
        ra_um: Range::new(0.28, 0.32),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.5, 1.0, 1.0],
    },
    Finish {
        id: "spi_c1",
        standard: FinishStandard::Spi,
        grade: "C-1",
        description: "600 grit stone, satin",
        ra_um: Range::new(0.35, 0.40),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [1.0, 1.0, 1.5],
    },
    Finish {
        id: "spi_c2",
        standard: FinishStandard::Spi,
        grade: "C-2",
        description: "400 grit stone, matte",
        ra_um: Range::new(0.45, 0.55),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [1.0, 1.5, 1.5],
    },
    Finish {
        id: "spi_c3",
        standard: FinishStandard::Spi,
        grade: "C-3",
        description: "320 grit stone, matte",
        ra_um: Range::new(0.63, 0.70),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [1.0, 1.5, 2.0],
    },
    Finish {
        id: "spi_d1",
        standard: FinishStandard::Spi,
        grade: "D-1",
        description: "Dry blast, glass bead #11, satin texture",
        ra_um: Range::new(0.80, 1.00),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [1.0, 1.5, 2.0],
    },
    Finish {
        id: "spi_d2",
        standard: FinishStandard::Spi,
        grade: "D-2",
        description: "Dry blast, #240 aluminium oxide, dull",
        ra_um: Range::new(1.00, 2.80),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [2.0, 2.5, 3.0],
    },
    Finish {
        id: "spi_d3",
        standard: FinishStandard::Spi,
        grade: "D-3",
        description: "Dry blast, #24 aluminium oxide, coarse",
        ra_um: Range::new(3.20, 18.0),
        rz_um: 0.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [5.0, 6.0, 7.0],
    },
    Finish {
        id: "vdi_12",
        standard: FinishStandard::Vdi3400,
        grade: "12",
        description: "EDM, fine (satin)",
        ra_um: Range::new(0.40, 0.40),
        rz_um: 1.5,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.5, 0.5, 1.0],
    },
    Finish {
        id: "vdi_15",
        standard: FinishStandard::Vdi3400,
        grade: "15",
        description: "EDM, fine",
        ra_um: Range::new(0.56, 0.56),
        rz_um: 2.4,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.5, 0.5, 1.0],
    },
    Finish {
        id: "vdi_18",
        standard: FinishStandard::Vdi3400,
        grade: "18",
        description: "EDM, fine matte",
        ra_um: Range::new(0.80, 0.80),
        rz_um: 3.3,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.5, 0.5, 1.0],
    },
    Finish {
        id: "vdi_21",
        standard: FinishStandard::Vdi3400,
        grade: "21",
        description: "EDM, matte",
        ra_um: Range::new(1.12, 1.12),
        rz_um: 4.7,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.5, 0.5, 1.0],
    },
    Finish {
        id: "vdi_24",
        standard: FinishStandard::Vdi3400,
        grade: "24",
        description: "EDM, matte",
        ra_um: Range::new(1.60, 1.60),
        rz_um: 6.5,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.5, 1.0, 1.5],
    },
    Finish {
        id: "vdi_27",
        standard: FinishStandard::Vdi3400,
        grade: "27",
        description: "EDM, medium (typical housing)",
        ra_um: Range::new(2.24, 2.24),
        rz_um: 10.5,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [1.0, 1.5, 2.0],
    },
    Finish {
        id: "vdi_30",
        standard: FinishStandard::Vdi3400,
        grade: "30",
        description: "EDM, medium",
        ra_um: Range::new(3.15, 3.15),
        rz_um: 12.5,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [1.5, 2.0, 2.0],
    },
    Finish {
        id: "vdi_33",
        standard: FinishStandard::Vdi3400,
        grade: "33",
        description: "EDM, medium coarse",
        ra_um: Range::new(4.50, 4.50),
        rz_um: 17.5,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [2.0, 2.5, 3.0],
    },
    Finish {
        id: "vdi_36",
        standard: FinishStandard::Vdi3400,
        grade: "36",
        description: "EDM, coarse",
        ra_um: Range::new(6.30, 6.30),
        rz_um: 24.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [2.5, 3.0, 4.0],
    },
    Finish {
        id: "vdi_39",
        standard: FinishStandard::Vdi3400,
        grade: "39",
        description: "EDM, coarse",
        ra_um: Range::new(9.00, 9.00),
        rz_um: 34.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [3.5, 4.0, 5.0],
    },
    Finish {
        id: "vdi_42",
        standard: FinishStandard::Vdi3400,
        grade: "42",
        description: "EDM, very coarse",
        ra_um: Range::new(12.5, 12.5),
        rz_um: 48.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [4.5, 5.0, 6.0],
    },
    Finish {
        id: "vdi_45",
        standard: FinishStandard::Vdi3400,
        grade: "45",
        description: "EDM, very coarse",
        ra_um: Range::new(18.0, 18.0),
        rz_um: 69.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [5.5, 6.0, 7.0],
    },
    Finish {
        id: "tex_0_013",
        standard: FinishStandard::Texture,
        grade: "0.013 mm",
        description: "Light grain / matte etch, 0.0005 in deep",
        ra_um: Range::new(2.2, 3.3),
        rz_um: 13.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.5, 1.0, 1.5],
    },
    Finish {
        id: "tex_0_025",
        standard: FinishStandard::Texture,
        grade: "0.025 mm",
        description: "Light grain, 0.001 in deep",
        ra_um: Range::new(4.2, 6.3),
        rz_um: 25.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [1.0, 1.5, 2.0],
    },
    Finish {
        id: "tex_0_050",
        standard: FinishStandard::Texture,
        grade: "0.050 mm",
        description: "Medium grain / leather, 0.002 in deep",
        ra_um: Range::new(8.3, 12.5),
        rz_um: 50.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [2.5, 3.0, 3.5],
    },
    Finish {
        id: "tex_0_075",
        standard: FinishStandard::Texture,
        grade: "0.075 mm",
        description: "Medium-heavy grain, 0.003 in deep",
        ra_um: Range::new(12.5, 19.0),
        rz_um: 75.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [4.0, 4.5, 5.0],
    },
    Finish {
        id: "tex_0_100",
        standard: FinishStandard::Texture,
        grade: "0.100 mm",
        description: "Heavy grain, 0.004 in deep",
        ra_um: Range::new(17.0, 25.0),
        rz_um: 100.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [5.5, 6.0, 6.5],
    },
    Finish {
        id: "tex_0_150",
        standard: FinishStandard::Texture,
        grade: "0.150 mm",
        description: "Very heavy grain / stipple, 0.006 in deep",
        ra_um: Range::new(25.0, 38.0),
        rz_um: 150.0,
        processes: Processes::INJECTION,
        clear_ok: false,
        frosted: true,
        draft_deg: [8.5, 9.0, 9.5],
    },
    Finish {
        id: "sla",
        standard: FinishStandard::Print,
        grade: "SLA",
        description: "SLA / DLP as printed, 25-100 um layers",
        ra_um: Range::new(0.8, 2.5),
        rz_um: 0.0,
        processes: Processes::SLA,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "fdm_0_1",
        standard: FinishStandard::Print,
        grade: "FDM 0.1",
        description: "FDM as printed, 0.1 mm layers (side walls)",
        ra_um: Range::new(5.0, 10.0),
        rz_um: 0.0,
        processes: Processes::FDM,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "fdm_0_2",
        standard: FinishStandard::Print,
        grade: "FDM 0.2",
        description: "FDM as printed, 0.2 mm layers (side walls)",
        ra_um: Range::new(9.0, 18.0),
        rz_um: 0.0,
        processes: Processes::FDM,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "fdm_0_3",
        standard: FinishStandard::Print,
        grade: "FDM 0.3",
        description: "FDM as printed, 0.3 mm layers (side walls)",
        ra_um: Range::new(15.0, 30.0),
        rz_um: 0.0,
        processes: Processes::FDM,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "sls",
        standard: FinishStandard::Print,
        grade: "SLS",
        description: "Powder bed as printed (bead blasted)",
        ra_um: Range::new(6.0, 12.0),
        rz_um: 0.0,
        processes: Processes::SLS,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "sanded",
        standard: FinishStandard::Print,
        grade: "Sanded",
        description: "Sanded 400-600 grit / primed",
        ra_um: Range::new(0.4, 1.0),
        rz_um: 0.0,
        processes: Processes::FDM.union(Processes::SLA).union(Processes::SLS).union(Processes::CNC),
        clear_ok: false,
        frosted: true,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "vapour",
        standard: FinishStandard::Print,
        grade: "Vapour",
        description: "Vapour smoothed (acetone: ABS/ASA/HIPS; solvent: PA12/TPU)",
        ra_um: Range::new(0.3, 1.0),
        rz_um: 0.0,
        processes: Processes::FDM.union(Processes::SLS),
        clear_ok: false,
        frosted: false,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "polished_clear",
        standard: FinishStandard::Print,
        grade: "Polished",
        description: "Wet sanded to 2000+ grit and polished / clear coated",
        ra_um: Range::new(0.03, 0.10),
        rz_um: 0.0,
        processes: Processes::SLA.union(Processes::CNC),
        clear_ok: true,
        frosted: false,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "cnc_3_2",
        standard: FinishStandard::Machined,
        grade: "Ra 3.2",
        description: "CNC as machined (standard)",
        ra_um: Range::new(1.6, 3.2),
        rz_um: 0.0,
        processes: Processes::CNC,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "cnc_0_8",
        standard: FinishStandard::Machined,
        grade: "Ra 0.8",
        description: "CNC fine finish pass",
        ra_um: Range::new(0.4, 0.8),
        rz_um: 0.0,
        processes: Processes::CNC,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "cnc_polish",
        standard: FinishStandard::Machined,
        grade: "Polished",
        description: "Machined then flame / vapour / buff polished (acrylic, PC)",
        ra_um: Range::new(0.03, 0.10),
        rz_um: 0.0,
        processes: Processes::CNC,
        clear_ok: true,
        frosted: false,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "mask_gloss",
        standard: FinishStandard::Coating,
        grade: "Gloss",
        description: "Glossy solder mask",
        ra_um: Range::new(0.11, 0.147),
        rz_um: 0.0,
        processes: Processes::NONE,
        clear_ok: false,
        frosted: false,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "mask_matte",
        standard: FinishStandard::Coating,
        grade: "Matte",
        description: "Matte solder mask",
        ra_um: Range::new(0.8, 1.25),
        rz_um: 0.0,
        processes: Processes::NONE,
        clear_ok: false,
        frosted: false,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "as_made",
        standard: FinishStandard::AsMade,
        grade: "",
        description: "The material's own surface as made, plated or fired",
        ra_um: Range::new(0.0, 0.0),
        rz_um: 0.0,
        processes: Processes::NONE,
        clear_ok: false,
        frosted: false,
        draft_deg: [0.0, 0.0, 0.0],
    },
    // Metal surfaces: Ra is set so the render roughness is 0.04 polished and 0.28 turned.
    Finish {
        id: "metal_polish",
        standard: FinishStandard::Machined,
        grade: "Polished metal",
        description: "Machined then buff polished metal",
        ra_um: Range::new(0.012, 0.012),
        rz_um: 0.0,
        processes: Processes::CNC,
        clear_ok: true,
        frosted: false,
        draft_deg: [0.0, 0.0, 0.0],
    },
    Finish {
        id: "turned",
        standard: FinishStandard::Machined,
        grade: "Turned",
        description: "Satin turned metal",
        ra_um: Range::new(0.28466266, 0.28466266),
        rz_um: 0.0,
        processes: Processes::CNC,
        clear_ok: false,
        frosted: true,
        draft_deg: [0.0, 0.0, 0.0],
    },
];
impl FinishStandard {
    pub fn name(self) -> &'static str {
        match self {
            Self::Spi => "SPI",
            Self::Vdi3400 => "VDI 3400",
            Self::Texture => "Texture",
            Self::Print => "Print",
            Self::Machined => "Machined",
            Self::Coating => "Coating",
            Self::AsMade => "As made",
        }
    }
}

fn normalise(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = String::new();
    for (i, &ch) in bytes.iter().enumerate() {
        if ch.is_ascii_alphanumeric() || ch == b'.' {
            out.push(ch.to_ascii_lowercase() as char);
        } else if ch == b'_' && i > 0 && bytes[i - 1].is_ascii_digit() && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            // Decimal ids such as fdm_0_2 also accept the spelling fdm-0.2.
            out.push('.');
        }
    }
    out
}

/// By id or label; see [`FinishId::lookup`].
pub(crate) fn lookup(name: &str) -> Option<FinishId> {
    let mut name = normalise(name);
    if let Some(tail) = name.strip_prefix("vdi3400") {
        name = format!("vdi{tail}");
    }
    if let Some(tail) = name.strip_prefix("vdiref") {
        name = format!("vdi{tail}");
    }
    let name = match name.as_str() {
        "mirror" => "spia1",
        "polished" => "spia2",
        "gloss" => "spia3",
        "frostedfine" => "vdi12",
        "frosted" => "vdi24",
        "textured" => "vdi27",
        "asprinted" => "fdm0.2",
        name => name,
    };
    FINISHES
        .iter()
        .position(|f| {
            name == normalise(f.id)
                || (f.standard == FinishStandard::Spi && (name == normalise(f.grade) || name == format!("spi{}", normalise(f.grade))))
        })
        .map(|index| FinishId(index as u16))
}

/// GGX perceptual roughness from Ra in micrometres, with logarithmic interpolation.
pub fn roughness_from_ra(ra_um: f32) -> f32 {
    const ANCHORS: [(f32, f32); 6] = [(0.012, 0.04), (0.1, 0.15), (0.5, 0.35), (2.0, 0.55), (10.0, 0.80), (30.0, 0.95)];
    if ra_um <= ANCHORS[0].0 {
        return ANCHORS[0].1;
    }
    for pair in ANCHORS.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        if ra_um <= x1 {
            let t = ((ra_um / x0).ln() / (x1 / x0).ln()).clamp(0.0, 1.0);
            return y0 + t * (y1 - y0);
        }
    }
    ANCHORS[ANCHORS.len() - 1].1
}

impl Finish {
    /// The finish note for a drawing.
    pub fn label(&self) -> String {
        match self.standard {
            FinishStandard::Spi => format!("SPI {}", self.grade),
            FinishStandard::Vdi3400 => format!("VDI 3400 Ref {}", self.grade),
            FinishStandard::Texture => format!("Texture {}", self.grade),
            _ => self.description.to_owned(),
        }
    }

    pub fn render_roughness(&self) -> f32 {
        roughness_from_ra((self.ra_um.lo * self.ra_um.hi).sqrt())
    }

    /// The visible bump pattern, depth and spacing in millimetres.
    pub fn texture(&self) -> SurfaceTexture {
        let ra_mid = (self.ra_um.lo * self.ra_um.hi).sqrt();
        let rz = if self.rz_um > 0.0 { self.rz_um } else { 5.0 * ra_mid };
        match self.standard {
            FinishStandard::Spi if self.grade.starts_with('D') => SurfaceTexture {
                kind: TextureKind::Blast,
                depth_mm: rz * 1e-3,
                size_mm: (8.0 * rz * 1e-3).clamp(0.03, 0.35),
            },
            FinishStandard::Vdi3400 => SurfaceTexture {
                kind: TextureKind::Edm,
                depth_mm: rz * 1e-3,
                size_mm: (8.0 * rz * 1e-3).clamp(0.01, 0.6),
            },
            FinishStandard::Texture => SurfaceTexture {
                kind: TextureKind::Grain,
                depth_mm: self.rz_um * 1e-3,
                size_mm: (12.0 * self.rz_um * 1e-3).clamp(0.15, 2.0),
            },
            FinishStandard::Print => match self.id {
                "fdm_0_1" | "fdm_0_2" | "fdm_0_3" => SurfaceTexture {
                    kind: TextureKind::FdmLayers,
                    depth_mm: 3.0 * ra_mid * 1e-3,
                    size_mm: match self.id {
                        "fdm_0_1" => 0.1,
                        "fdm_0_2" => 0.2,
                        "fdm_0_3" => 0.3,
                        _ => unreachable!(),
                    },
                },
                "sla" => SurfaceTexture { kind: TextureKind::SlaLayers, depth_mm: 2.0 * ra_mid * 1e-3, size_mm: 0.05 },
                "sls" => SurfaceTexture { kind: TextureKind::Blast, depth_mm: rz * 1e-3, size_mm: 0.08 },
                _ => SurfaceTexture::default(),
            },
            FinishStandard::Machined => match self.id {
                "turned" => SurfaceTexture { kind: TextureKind::Machined, depth_mm: 0.002, size_mm: 0.05 },
                "cnc_3_2" => SurfaceTexture { kind: TextureKind::Machined, depth_mm: rz * 1e-3, size_mm: 0.1 },
                "cnc_0_8" => SurfaceTexture { kind: TextureKind::Machined, depth_mm: rz * 1e-3, size_mm: 0.05 },
                _ => SurfaceTexture::default(),
            },
            _ => SurfaceTexture::default(),
        }
    }

    pub fn min_draft_deg(&self, m: &Material) -> f32 {
        self.draft_deg[m.draft_class.index()]
    }

    /// Warnings about process, draft, polish and clarity; `None` skips draft.
    pub fn check(&self, m: &Material, draft_deg: Option<f32>, want_clear: bool) -> Vec<String> {
        let mut warnings = Vec::new();
        let label = self.label();
        if self.processes != Processes::NONE && !self.processes.intersects(m.processes) {
            warnings.push(format!("{label} is a {} finish; {} is not made that way ({})", self.processes.names(), m.short_name, m.processes.names()));
        }
        let need = self.min_draft_deg(m);
        if let Some(draft) = draft_deg {
            if need > 0.0 && draft + 1e-4 < need {
                warnings.push(format!("{label} on {} needs >= {need:.1} deg draft per side (have {draft:.1}): drag marks / sticking", m.short_name));
            }
        }
        if self.standard == FinishStandard::Spi && self.ra_um.hi <= 0.05 && !m.holds_high_gloss {
            warnings.push(format!("{} won't hold a mirror {} finish (semi-crystalline / filled / soft): use B-1 or C-1", m.short_name, self.grade));
        }
        if want_clear {
            if m.optics.clarity == Clarity::Opaque || !m.translucency_allowed() {
                warnings.push(format!("{} has no clear grade", m.short_name));
            } else if !self.clear_ok {
                warnings.push(format!("{label} diffuses light: a clear {} part will look frosted", m.short_name));
            }
        }
        warnings
    }

    /// A short tooltip with roughness, draft and clarity.
    pub fn summary(&self) -> String {
        use crate::material::three_significant;
        let rz = if self.rz_um > 0.0 { format!(", Rz {} um", three_significant(self.rz_um)) } else { String::new() };
        let ra = if self.ra_um.lo == self.ra_um.hi {
            three_significant(self.ra_um.lo)
        } else {
            format!("{}-{}", three_significant(self.ra_um.lo), three_significant(self.ra_um.hi))
        };
        if self.standard == FinishStandard::AsMade {
            return format!("{} - {}", self.label(), self.description);
        }
        let mut out = format!("{} - {}\nRa {ra} um{rz}, render roughness {:.2}\n", self.label(), self.description, self.render_roughness());
        if self.draft_deg[1] > 0.0 {
            out.push_str(&format!("Min draft {:.1} / {:.1} / {:.1} deg (POM-PA-PP / ABS / PC)\n", self.draft_deg[0], self.draft_deg[1], self.draft_deg[2]));
        }
        out.push_str(if self.clear_ok { "Keeps clear parts clear" } else if self.frosted { "Clear parts look frosted" } else { "Glossy, not optical" });
        out
    }
}

impl TextureKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Edm => "EDM pits",
            Self::Blast => "bead / grit blast",
            Self::Grain => "grain",
            Self::FdmLayers => "FDM layer lines",
            Self::SlaLayers => "SLA layers",
            Self::Machined => "tool marks",
        }
    }
}

impl Material {
    /// A suitable finish for the material's process and optical grade.
    pub fn default_finish(&self) -> FinishId {
        if !self.family.is_moulding() {
            return lookup(if self.id == "lpi_mask" { "mask_gloss" } else { "as_made" }).expect("default finish is in the table");
        }
        let name = match self.id {
            "pc" | "pmma" | "ps" | "pet" => "spi_a2",
            "pla" | "petg" => "fdm_0_2",
            "sla_std" => "sla",
            "pbt" => "vdi_27",
            "pom" | "peek" | "pps" => "spi_c1",
            "pa66_gf30" => "vdi_30",
            "lsr" => "vdi_24",
            "glass" | "lead_crystal" | "flint" | "diamond" => "polished_clear",
            _ if !self.processes.contains(Processes::INJECTION) => {
                if self.processes.contains(Processes::SLA) { "sla" }
                else if self.processes.contains(Processes::FDM) { "fdm_0_2" }
                else if self.processes.contains(Processes::SLS) { "sls" }
                else { "cnc_3_2" }
            }
            _ if self.draft_class == DraftClass::Easy && self.family != Family::Elastomer => "vdi_24",
            _ => "vdi_27",
        };
        lookup(name).expect("default finish is in the table")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn finish_table_and_roughness() {
        assert_eq!(FINISHES.len(), 46, "41 moulding and print finishes, 2 mask grades, as made, 2 metal finishes");
        for (id, roughness) in [("metal_polish", 0.04), ("turned", 0.28), ("cnc_0_8", 0.36), ("cnc_3_2", 0.55)] {
            let f = FinishId::lookup(id).expect("metal surface resolves by id").get();
            assert_eq!(f.standard, FinishStandard::Machined, "metal surface standard: {id}");
            assert_eq!(f.processes, Processes::CNC, "metal surface process: {id}");
            let tolerance = if matches!(id, "metal_polish" | "turned") { 1e-6 } else { 0.03 };
            assert!((f.render_roughness() - roughness).abs() <= tolerance, "metal surface roughness: {id}");
        }
        for standard in [FinishStandard::Spi, FinishStandard::Vdi3400, FinishStandard::Texture] {
            let finishes: Vec<_> = FINISHES.iter().filter(|f| f.standard == standard).collect();
            if standard != FinishStandard::Texture {
                assert_eq!(finishes.len(), 12, "12 grades in {}", standard.name());
            }
            for pair in finishes.windows(2) {
                let (prev, f) = (pair[0], pair[1]);
                assert!(f.ra_um.mid() >= prev.ra_um.mid() && f.ra_um.lo >= prev.ra_um.lo, "Ra non-decreasing: {}", f.id);
                if standard == FinishStandard::Vdi3400 {
                    assert!(f.ra_um.lo > prev.ra_um.lo && f.rz_um > prev.rz_um, "VDI strictly increasing: {}", f.id);
                }
                for k in 0..3 {
                    assert!(f.draft_deg[k] >= prev.draft_deg[k], "draft grows with texture: {} class {k}", f.id);
                }
            }
        }
        let mut ids = HashSet::new();
        for (index, f) in FINISHES.iter().enumerate() {
            let id = f.id;
            assert!(ids.insert(id), "unique finish: {id}");
            assert_eq!(lookup(id), Some(FinishId(index as u16)), "finish lookup: {id}");
            assert!((f.ra_um.lo > 0.0 || f.standard == FinishStandard::AsMade) && f.ra_um.lo <= f.ra_um.hi, "Ra range: {id}");
            assert!(!(f.clear_ok && f.frosted), "clear xor frosted: {id}");
            assert!(f.draft_deg[0] <= f.draft_deg[1] && f.draft_deg[1] <= f.draft_deg[2], "draft by class: {id}");
            if f.processes.contains(Processes::INJECTION) {
                assert!(f.draft_deg[1] > 0.0, "moulds need draft: {id}");
            } else {
                assert_eq!(f.draft_deg, [0.0; 3], "no draft for prints / machining: {id}");
            }
            assert!(f.render_roughness() > 0.0 && f.render_roughness() < 1.0, "render roughness: {id}");
            if f.standard == FinishStandard::Vdi3400 {
                let grade: f32 = f.grade.parse().unwrap();
                assert!((f.ra_um.lo - 0.1 * 10.0_f32.powf(grade / 20.0)).abs() < 0.03 * f.ra_um.lo, "VDI Ra formula: {id}");
            }
        }
        let mut sorted: Vec<_> = FINISHES.iter().collect();
        sorted.sort_by(|a, b| (a.ra_um.lo * a.ra_um.hi).total_cmp(&(b.ra_um.lo * b.ra_um.hi)));
        for pair in sorted.windows(2) {
            assert!(pair[1].render_roughness() >= pair[0].render_roughness(), "roughness monotonic: {}", pair[1].id);
        }
        let mut ra = 0.005;
        while ra < 50.0 {
            assert!(roughness_from_ra(ra * 1.1) >= roughness_from_ra(ra), "roughness mapping monotonic at {ra}");
            ra *= 1.1;
        }
        assert!(lookup("SPI A-1").unwrap().get().render_roughness() < 0.08 && lookup("spi d3").unwrap().get().render_roughness() > 0.7, "roughness anchors");
        for m in crate::material::MATERIALS {
            let f = m.default_finish().get();
            let draft = m.recommended_draft(f.standard != FinishStandard::Spi || f.ra_um.lo > 0.3);
            let warnings = f.check(m, Some(draft), false);
            assert!(warnings.is_empty(), "default finish {} on {}: {warnings:?}", f.id, m.id);
        }
    }

    #[test]
    fn finish_names_warnings_and_textures() {
        for (names, expected) in [
            (&["spi-a2", "SPI A-2", "a2", "A-2", "polished"][..], "spi_a2"),
            (&["VDI 27", "vdi27", "VDI 3400 Ref 27", "textured"], "vdi_27"),
            (&["fdm-0.2", "fdm_0_2", "as_printed"], "fdm_0_2"),
            (&["mirror"], "spi_a1"),
            (&["gloss"], "spi_a3"),
            (&["frosted_fine"], "vdi_12"),
            (&["frosted"], "vdi_24"),
        ] {
            for name in names {
                assert_eq!(lookup(name).unwrap().name(), expected, "finish spelling: {name}");
            }
        }
        assert_eq!(lookup("vdi-28"), None, "unknown VDI grade");
        assert_eq!(lookup("E-1"), None, "unknown SPI grade");
        let warns = |f: &str, m: &str, draft: Option<f32>, clear| {
            lookup(f).unwrap().get().check(MaterialId::lookup(m).unwrap().get(), draft, clear)
        };
        assert!(!warns("spi_a1", "pom", None, false).is_empty(), "mirror on POM warns");
        assert!(warns("spi_a1", "pc", Some(1.0), true).is_empty(), "mirror on clear PC allowed");
        assert!(!warns("vdi_36", "abs", Some(1.0), false).is_empty() && warns("vdi_36", "abs", Some(3.0), false).is_empty(), "VDI 36 on ABS needs 3 degrees");
        assert_eq!(lookup("vdi_36").unwrap().get().min_draft_deg(MaterialId::lookup("pc").unwrap().get()), 4.0, "VDI 36 on PC needs 4 degrees");
        assert!(!warns("vdi_36", "pc", Some(3.0), false).is_empty() && warns("vdi_36", "pc", Some(4.0), false).is_empty(), "PC draft threshold");
        assert!(warns("vdi_36", "pc", None, false).is_empty(), "None skips draft check");
        assert!(warns("vdi_36", "pc", Some(4.0 - 0.00005), false).is_empty(), "draft tolerance is 1e-4");
        assert!(warns("vdi_27", "pc", None, true).iter().any(|s| s.contains("will look frosted")), "textured clear PC frosts");
        assert!(warns("spi_a2", "abs_cnt", None, true).iter().any(|s| s.contains("has no clear grade")), "conductive ABS has no clear grade");
        assert!(warns("fdm_0_2", "pom", None, false).iter().any(|s| s.contains("not made that way")), "FDM on non-FDM material warns");
        assert_eq!(lookup("a2").unwrap().get().label(), "SPI A-2", "SPI drawing label");
        assert_eq!(lookup("vdi27").unwrap().get().label(), "VDI 3400 Ref 27", "VDI drawing label");
        assert_eq!(lookup("tex_0_050").unwrap().get().label(), "Texture 0.050 mm", "texture drawing label");
        for (id, layer) in [("fdm_0_1", 0.1), ("fdm_0_2", 0.2), ("fdm_0_3", 0.3)] {
            let t = lookup(id).unwrap().get().texture();
            assert_eq!(t.kind, TextureKind::FdmLayers, "FDM texture: {id}");
            assert_eq!(t.size_mm, layer, "FDM layer height: {id}");
            assert!(t.depth_mm > 0.0 && t.depth_mm < layer, "FDM depth below layer height: {id}");
        }
        // Feature spacings the texture model aims at: VDI 27 about 0.08 mm, the coarsest blast about 0.3 mm,
        // a 0.05 mm grain about 0.6 mm cells.
        for (id, kind, size) in [("vdi_27", TextureKind::Edm, 0.08), ("spi_d3", TextureKind::Blast, 0.3), ("tex_0_050", TextureKind::Grain, 0.6)] {
            let t = lookup(id).unwrap().get().texture();
            assert_eq!(t.kind, kind, "texture kind: {id}");
            assert!((t.size_mm - size).abs() < 0.1 * size, "texture spacing near {size} mm: {id} has {}", t.size_mm);
        }
        for id in ["spi_a1", "spi_b2", "spi_c1", "sanded", "vapour", "polished_clear", "cnc_polish"] {
            assert_eq!(lookup(id).unwrap().get().texture(), SurfaceTexture::default(), "no visible bump: {id}");
        }
    }
}
