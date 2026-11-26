//! A candle light's settings (`ModifierParams::candle`, modifier kind "candle"): the candles, their flames, what
//! holds them and where that stands (owner 2026-11-18: candle light is a lamp type usable everywhere the studio
//! lights are, on the floor, a table, a wall, a ceiling or a pedestal). The model is light_model_candle.rs.
//!
//! The light's `SpaceParams::position_m` is the mount's point: on the floor under a floor stand, a table or a
//! pedestal; on the wall behind a sconce; on the ceiling over a chandelier. The mount's own sizes reach from there to
//! the candles (a table's height, a pedestal's, a chandelier's drop). Its yaw turns the whole of it round the
//! vertical; a candle stands upright whatever the face's pitch and roll were (clamp() holds them at 0).
use super::*;

/// `CandleParams::candle`: a pillar (a thick block), a taper (a long thin dinner candle) or a tea light (a short
/// disc of wax in its aluminium cup).
pub const CANDLE_KIND_NAMES: &[&str] = &["pillar", "taper", "tea_light"];
/// `CandleParams::holder`.
pub const CANDLE_HOLDER_NAMES: &[&str] = &["none", "votive", "candlestick", "candelabra", "chandelier", "sconce"];
/// `CandleParams::mount`. "floor" is the floor or any surface the scene already has (a yacht's table, an altar):
/// the candles stand at the light's point on it and bring no furniture; "table" and "pedestal" bring their own.
pub const CANDLE_MOUNT_NAMES: &[&str] = &["floor", "table", "wall", "ceiling", "pedestal"];
/// `CandleParams::layout`: how a group stands: scattered round the point, in a row across the view, or on a stepped
/// votive rack (a church's, its tiers drawn in the holder's metal).
pub const CANDLE_LAYOUT_NAMES: &[&str] = &["scatter", "row", "rack"];
/// The ready arrangements (owner 2026-11-18: a candle arrangement is a preset applied to a scene, not part of a
/// scene's model): `candle_arrangement` makes each.
pub const CANDLE_ARRANGEMENT_NAMES: &[&str] = &["Tea lights", "Votive rack", "Altar candles", "Pillar trio", "Candelabra", "Chandelier", "Wall sconce", "Table with tea lights"];
/// `CandleParams::metal`: the holder's finish.
pub const CANDLE_METAL_NAMES: &[&str] = &["brass", "iron", "silver"];

/// The candle light's defaults: a tea light's flame at its colour (Kelvin) and a flame's mean radiance (EV over the
/// scene's unit of 6690 cd/m2: a candle flame is about 8000 cd/m2, atmosphere.rs's units).
pub const CANDLE_KELVIN: f32 = 1850.0;
pub const CANDLE_INTENSITY_EV: f32 = 0.6;

/// The typed views of the names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandleKind {
    Pillar,
    Taper,
    TeaLight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandleHolder {
    None,
    Votive,
    Candlestick,
    Candelabra,
    Chandelier,
    Sconce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandleMount {
    Floor,
    Table,
    Wall,
    Ceiling,
    Pedestal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandleLayout {
    Scatter,
    Row,
    Rack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandleMetal {
    Brass,
    Iron,
    Silver,
}

/// A candle light (`ModifierParams::candle`). None reads as `CandleParams::default()`, a tea light in a clear votive
/// on a table.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct CandleParams {
    /// `CANDLE_KIND_NAMES`.
    pub candle: String,
    /// The wax's diameter, metres, 0.01..=0.25.
    pub diameter_m: f32,
    /// A new candle's height, metres, 0.005..=0.6 (a tea light's wax 0.015, a taper 0.25).
    pub height_m: f32,
    /// 0..=0.95: how far it has burnt down, a share of its height (a pillar's wall stands higher round its well,
    /// a taper drips, a tea light's pool sinks into its cup).
    pub burn: f32,
    /// The wax's colour, linear 0..=1.
    pub wax: [f32; 3],
    /// 0.3..=2: the flame's size against a candle's own (a pillar's wider wick burns a larger flame).
    pub flame: f32,
    /// 0..=1: how much each flame differs from the next and leans as if in a breath of air (a still map shows the
    /// flicker frozen: some flames taller, some bent, a little brighter or dimmer).
    pub flicker: f32,
    /// The pattern of the flicker and of a group's spacing.
    pub seed: u32,
    /// `CANDLE_HOLDER_NAMES`.
    pub holder: String,
    /// A votive's glass tint, linear 0..=1 (None: clear glass).
    pub glass: Option<[f32; 3]>,
    /// 0..=1: how much a votive's glass scatters the flame's light (0 clear, 1 frosted): it glows and lights what
    /// stands round it, as a lampshade does.
    pub frost: f32,
    /// `CANDLE_METAL_NAMES`: the candlestick's, candelabra's, chandelier's or sconce's finish.
    pub metal: String,
    /// 2..=12: a candelabra's or chandelier's arms (each a candle; a candelabra adds one in the middle).
    pub arms: u32,
    /// 1..=24: candles (each in its votive or on its own) set down together on a floor, table or pedestal; a
    /// candlestick holds one each.
    pub count: u32,
    /// How far apart a group stands, metres, 0..=2: the radius of its scatter, half a row's length, half a rack's
    /// width.
    pub spread_m: f32,
    /// `CANDLE_LAYOUT_NAMES`: how a group stands (scatter, row, rack).
    pub layout: String,
    /// `CANDLE_MOUNT_NAMES`.
    pub mount: String,
    /// A table's or a pedestal's height, a chandelier's drop from the ceiling; metres, 0..=4 (a sconce's plate is
    /// sized to its candle).
    pub mount_height_m: f32,
    /// A table's top's diameter (round) or a pedestal's width, metres, 0.1..=4.
    pub mount_size_m: f32,
    /// A table's or pedestal's top colour, linear 0..=1.
    pub surface: [f32; 3],
    /// 0..=1: how glossy that top is (a lacquered yacht table 0.9, stone 0.2).
    pub gloss: f32,
}

impl Default for CandleParams {
    fn default() -> Self {
        CandleParams {
            candle: "tea_light".to_string(),
            diameter_m: 0.038,
            height_m: 0.016,
            burn: 0.3,
            wax: [0.86, 0.82, 0.74],
            flame: 1.0,
            flicker: 0.3,
            seed: 1,
            holder: "votive".to_string(),
            glass: None,
            frost: 0.35,
            metal: "brass".to_string(),
            arms: 5,
            count: 6,
            spread_m: 0.25,
            layout: "scatter".to_string(),
            mount: "floor".to_string(),
            mount_height_m: 0.74,
            mount_size_m: 0.9,
            surface: [0.78, 0.78, 0.76],
            gloss: 0.7,
        }
    }
}

impl CandleParams {
    pub(super) fn clamp_fields(&mut self, prefix: &str, f: &mut Fixes) {
        let d = CandleParams::default();
        f.choice(prefix, "candle", &mut self.candle, CANDLE_KIND_NAMES);
        f.range(prefix, "diameter_m", &mut self.diameter_m, 0.01, 0.25, d.diameter_m);
        f.range(prefix, "height_m", &mut self.height_m, 0.005, 0.6, d.height_m);
        f.range(prefix, "burn", &mut self.burn, 0.0, 0.95, d.burn);
        f.color(prefix, "wax", &mut self.wax, 1.0, d.wax);
        f.range(prefix, "flame", &mut self.flame, 0.3, 2.0, d.flame);
        f.range(prefix, "flicker", &mut self.flicker, 0.0, 1.0, d.flicker);
        f.choice(prefix, "holder", &mut self.holder, CANDLE_HOLDER_NAMES);
        if let Some(glass) = self.glass.as_mut() {
            f.color(prefix, "glass", glass, 1.0, [1.0; 3]);
        }
        f.range(prefix, "frost", &mut self.frost, 0.0, 1.0, d.frost);
        f.choice(prefix, "metal", &mut self.metal, CANDLE_METAL_NAMES);
        f.int(prefix, "arms", &mut self.arms, 2, 12);
        f.int(prefix, "count", &mut self.count, 1, 24);
        f.range(prefix, "spread_m", &mut self.spread_m, 0.0, 2.0, d.spread_m);
        f.choice(prefix, "layout", &mut self.layout, CANDLE_LAYOUT_NAMES);
        f.choice(prefix, "mount", &mut self.mount, CANDLE_MOUNT_NAMES);
        f.range(prefix, "mount_height_m", &mut self.mount_height_m, 0.0, 4.0, d.mount_height_m);
        f.range(prefix, "mount_size_m", &mut self.mount_size_m, 0.1, 4.0, d.mount_size_m);
        f.color(prefix, "surface", &mut self.surface, 1.0, d.surface);
        f.range(prefix, "gloss", &mut self.gloss, 0.0, 1.0, d.gloss);
    }

    pub fn kind(&self) -> CandleKind {
        match self.candle.trim().to_ascii_lowercase().as_str() {
            "taper" => CandleKind::Taper,
            "tea_light" => CandleKind::TeaLight,
            _ => CandleKind::Pillar,
        }
    }

    pub fn holder(&self) -> CandleHolder {
        match self.holder.trim().to_ascii_lowercase().as_str() {
            "votive" => CandleHolder::Votive,
            "candlestick" => CandleHolder::Candlestick,
            "candelabra" => CandleHolder::Candelabra,
            "chandelier" => CandleHolder::Chandelier,
            "sconce" => CandleHolder::Sconce,
            _ => CandleHolder::None,
        }
    }

    pub fn mount(&self) -> CandleMount {
        match self.mount.trim().to_ascii_lowercase().as_str() {
            "table" => CandleMount::Table,
            "wall" => CandleMount::Wall,
            "ceiling" => CandleMount::Ceiling,
            "pedestal" => CandleMount::Pedestal,
            _ => CandleMount::Floor,
        }
    }

    pub fn layout(&self) -> CandleLayout {
        match self.layout.trim().to_ascii_lowercase().as_str() {
            "row" => CandleLayout::Row,
            "rack" => CandleLayout::Rack,
            _ => CandleLayout::Scatter,
        }
    }

    /// A ready arrangement by its name (`CANDLE_ARRANGEMENT_NAMES`), to put on any scene's surface, wall or ceiling;
    /// None for an unknown name. "Table with tea lights" brings its own table, for a scene without one.
    pub fn arrangement(name: &str) -> Option<CandleParams> {
        let d = CandleParams::default();
        let tea = |count: u32, layout: &str, spread: f32| CandleParams { count, layout: layout.to_string(), spread_m: spread, ..d.clone() };
        let taper = |holder: &str, mount: &str| CandleParams {
            candle: "taper".to_string(),
            diameter_m: 0.022,
            height_m: 0.25,
            burn: 0.15,
            wax: [0.9, 0.87, 0.8],
            holder: holder.to_string(),
            mount: mount.to_string(),
            ..d.clone()
        };
        Some(match name.trim() {
            "Tea lights" => tea(6, "scatter", 0.25),
            "Votive rack" => CandleParams { metal: "iron".to_string(), frost: 0.15, glass: Some([0.85, 0.35, 0.25]), ..tea(18, "rack", 0.4) },
            "Altar candles" => CandleParams { count: 4, layout: "row".to_string(), spread_m: 0.5, metal: "brass".to_string(), ..taper("candlestick", "floor") },
            "Pillar trio" => CandleParams {
                candle: "pillar".to_string(),
                diameter_m: 0.075,
                height_m: 0.15,
                burn: 0.3,
                holder: "none".to_string(),
                count: 3,
                spread_m: 0.09,
                ..d.clone()
            },
            "Candelabra" => CandleParams { arms: 4, count: 1, metal: "brass".to_string(), ..taper("candelabra", "floor") },
            "Chandelier" => CandleParams { arms: 6, count: 1, metal: "iron".to_string(), height_m: 0.18, mount_height_m: 0.9, ..taper("chandelier", "ceiling") },
            "Wall sconce" => CandleParams {
                candle: "pillar".to_string(),
                diameter_m: 0.05,
                height_m: 0.1,
                burn: 0.4,
                holder: "sconce".to_string(),
                mount: "wall".to_string(),
                count: 2,
                ..d.clone()
            },
            "Table with tea lights" => CandleParams { mount: "table".to_string(), mount_height_m: 0.74, mount_size_m: 1.1, gloss: 0.8, ..tea(6, "scatter", 0.3) },
            _ => return None,
        })
    }

    pub fn metal(&self) -> CandleMetal {
        match self.metal.trim().to_ascii_lowercase().as_str() {
            "iron" => CandleMetal::Iron,
            "silver" => CandleMetal::Silver,
            _ => CandleMetal::Brass,
        }
    }

    /// A candle of `kind` as it comes new: its diameter and height, metres.
    pub fn kind_size(kind: CandleKind) -> (f32, f32) {
        match kind {
            CandleKind::Pillar => (0.075, 0.12),
            CandleKind::Taper => (0.022, 0.25),
            CandleKind::TeaLight => (0.038, 0.016),
        }
    }

    /// The flame's height and width (metres) for this candle: a tea light's small wick about 2.4 cm tall, a taper's
    /// 3.2 cm, a pillar's 3.6 cm, times `flame`.
    pub fn flame_size(&self) -> (f32, f32) {
        let tall = match self.kind() {
            CandleKind::TeaLight => 0.024,
            CandleKind::Taper => 0.032,
            CandleKind::Pillar => 0.036,
        } * self.flame;
        (tall, tall * 0.36)
    }
}

clamped_copy!(CandleParams);

impl ModifierParams {
    /// The candle block as clamp() holds it, or its defaults when there is none.
    pub fn candle_params(&self) -> CandleParams {
        self.candle.as_ref().map(CandleParams::clamped).unwrap_or_default()
    }
}

/// clamp()'s rule for a candle light (a light whose modifier is "candle"): it is its own content, always in 3D,
/// upright: a disc face the size of its flame, never the key, added, without a picture, gobo, aim or wear. Its
/// face is turned only about the vertical (`SpaceParams::yaw_deg` turns the holder and the mount); pitch and roll
/// are held at 0. Reports what it changed under `prefix`.
pub(super) fn hold_candle(light: &mut LightParams, prefix: &str, f: &mut Fixes) {
    let is_candle = light.kind() == LightKind::Light && light.modifier.as_ref().is_some_and(|m| m.kind() == ModifierKind::Candle);
    if !is_candle {
        return;
    }
    for (field, cleared) in [
        ("image", light.image.take().is_some()),
        ("gobo", light.gobo.take().is_some()),
        ("aim", light.aim.take().is_some()),
        ("imperfections", light.imperfections.take().is_some()),
    ] {
        if cleared {
            f.push(prefix, field);
        }
    }
    if light.shape != "disc" {
        light.shape = "disc".to_string();
        f.push(prefix, "shape");
    }
    if light.key {
        light.key = false;
        f.push(prefix, "key");
    }
    if light.blend != "add" {
        light.blend = "add".to_string();
        f.push(prefix, "blend");
    }
    let candle = light.modifier.as_ref().map(ModifierParams::candle_params).unwrap_or_default();
    let (tall, wide) = candle.flame_size();
    let had_space = light.space.is_some();
    crate::space::to_3d(light, None);
    if let Some(space) = light.space.as_mut() {
        let mut changed = false;
        for (value, want) in [(&mut space.pitch_deg, 0.0), (&mut space.roll_deg, 0.0), (&mut space.width_m, wide), (&mut space.height_m, tall), (&mut space.beam, 0.0)] {
            if *value != want {
                *value = want;
                changed = true;
            }
        }
        if changed && had_space {
            f.push(prefix, "space");
        }
    }
}
