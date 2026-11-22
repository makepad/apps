//! Extrusion roles.

/// Extrusion roles, with the names the G-code `; FEATURE:` tags use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Role {
    #[default]
    None = 0,
    OuterWall,
    InnerWall,
    OverhangWall,
    SparseInfill,
    InternalSolidInfill,
    TopSurface,
    BottomSurface,
    Bridge,
    InternalBridge,
    GapFill,
    Ironing,
    Skirt,
    Brim,
    Support,
    SupportInterface,
    SupportTransition,
    PrimeTower,
    FloatingVerticalShell,
    Flush,
    Custom,
}

impl Role {
    pub const ALL: [Role; 21] = [
        Role::None,
        Role::OuterWall,
        Role::InnerWall,
        Role::OverhangWall,
        Role::SparseInfill,
        Role::InternalSolidInfill,
        Role::TopSurface,
        Role::BottomSurface,
        Role::Bridge,
        Role::InternalBridge,
        Role::GapFill,
        Role::Ironing,
        Role::Skirt,
        Role::Brim,
        Role::Support,
        Role::SupportInterface,
        Role::SupportTransition,
        Role::PrimeTower,
        Role::FloatingVerticalShell,
        Role::Flush,
        Role::Custom,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Role::None => "Undefined",
            Role::OuterWall => "Outer wall",
            Role::InnerWall => "Inner wall",
            Role::OverhangWall => "Overhang wall",
            Role::SparseInfill => "Sparse infill",
            Role::InternalSolidInfill => "Internal solid infill",
            Role::TopSurface => "Top surface",
            Role::BottomSurface => "Bottom surface",
            Role::Bridge => "Bridge",
            Role::InternalBridge => "Internal Bridge",
            Role::GapFill => "Gap infill",
            Role::Ironing => "Ironing",
            Role::Skirt => "Skirt",
            Role::Brim => "Brim",
            Role::Support => "Support",
            Role::SupportInterface => "Support interface",
            Role::SupportTransition => "Support transition",
            Role::PrimeTower => "Prime tower",
            Role::FloatingVerticalShell => "Floating vertical shell",
            Role::Flush => "Flush",
            Role::Custom => "Custom",
        }
    }

    /// The role with discriminant `v` (`Role::None` when out of range).
    pub fn from_u8(v: u8) -> Role {
        Role::ALL.get(v as usize).copied().unwrap_or(Role::None)
    }

    pub fn from_name(s: &str) -> Role {
        Role::ALL.iter().copied().find(|r| r.name() == s).unwrap_or(Role::None)
    }

    pub fn is_wall(self) -> bool {
        matches!(self, Role::OuterWall | Role::InnerWall | Role::OverhangWall)
    }

    /// Whether a path of this role fills the part's inside, between its walls and its skins (the
    /// floating vertical shell's strips among them): what a part closed in on every side hides
    /// (`ExtrusionPath::core`).
    pub fn fills_inside(self) -> bool {
        matches!(self, Role::SparseInfill | Role::InternalSolidInfill | Role::InternalBridge | Role::FloatingVerticalShell)
    }

    /// Whether a path of this role is on the printed part's surface, where its colour shows.
    pub fn shows(self) -> bool {
        matches!(self, Role::OuterWall | Role::OverhangWall | Role::FloatingVerticalShell | Role::TopSurface | Role::BottomSurface | Role::Bridge | Role::GapFill | Role::Ironing)
    }
}

