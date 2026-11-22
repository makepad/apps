//! The cloud layers' light (design §2.3; the lifted layers' light, design §1.2).
//!
//! - **Families.** A modern layer is lit by its family's law ([`radiance`]). A heap (Stratocumulus, the Cumulus
//!   sheet) is lit by the marched volume's own law (`volumetric::sun_light`), so a sheet and its shell share one
//!   light. Ice (Cirrus) has two phase lobes and is fully lit. A slab (Stratus, Nimbostratus) has a forward lobe
//!   through its depth, the share of the sun it passes diffusely, and the dome through it. A lifted (classic) layer
//!   keeps today's light, worked out here and drawn by today's code from its parts.
//! - **The light pass** ([`light_pass`]) runs once per map. It gives each entry its sun at its own height (one call
//!   per entry), its share of the dome through its stand-in over the dome's 256 looks (`dome_look`, never
//!   marched), and its ambient: the dome greyed by the share of it that the entries at and above it hide (G_i; a
//!   volume's by those above it alone, design §12.9 item 5), dimmed by what the entries above it pass diffusely (the
//!   cascade).
//! - **Shadows between layers** ([`t_above`]). A higher flat entry takes Shade below times its cover off a lower
//!   entry's sun, where the sun's ray from the lower hit crosses its plane.
//!
//! Directions are in the map's own frame; ground metres run x east and z south. Nothing here allocates per texel:
//! `radiance` and `t_above` are arithmetic and lookups, and `light_pass` runs once per map.

use makepad_draw::*;
use crate::clouds::hg_phase;
use crate::sky::luminance;
use crate::volumetric;
use crate::{dome_look, CloudGenus, DOME_COLUMNS, DOME_ROWS};

/// T_d(tau) = 1 / (1 + TD_K tau) (design §2.3): a slab of optical depth 50 passes 0.154 of the light diffusely.
pub(crate) const TD_K: f32 = 0.11;
/// The ice family's lobes (design §2.3): ICE_GAIN x ((1 - lining) HG(ICE_G_BROAD) + lining HG(ICE_G_FORWARD)), the
/// Silver lining 0.5 by default.
const ICE_G_BROAD: f32 = 0.3;
const ICE_G_FORWARD: f32 = 0.85;
const ICE_GAIN: f32 = 1.5;
/// A slab's forward lobe (design §2.3): lining x SLAB_GAIN x HG(SLAB_G) through half its depth.
const SLAB_G: f32 = 0.6;
const SLAB_GAIN: f32 = 1.5;
/// A sheet's cover stands for the optical depth -ln(1 - cover) in its diffuse transmittance. The cover is held below
/// this, where its opacity rounds to 1.
const SHEET_COVER_MAX: f32 = 0.999;
/// The shadows between layers fade in over the sun's first two degrees (the sine of its elevation). A sun on the
/// horizon throws a higher layer's shadow far off, and as it sets the shadows do not snap off.
const SHADOW_FADE_SIN: f32 = 0.035;
/// Octaves of a higher entry's cover in its shadow (design §2.3): released, held.
const SHADOW_OCTAVES: u32 = 3;
const HELD_SHADOW_OCTAVES: u32 = 2;

/// How a layer is lit (design §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightFamily {
    /// Stratocumulus and the Cumulus sheet (later Altocumulus, Mammatus, the placed and lens types): the volume's
    /// light, a self-shadow toward the sun, the ground's bounce on the underside.
    Heap,
    /// Cirrus (later Cirrocumulus): two lobes, fully lit.
    Ice,
    /// Stratus and Nimbostratus (later Cirrostratus, Altostratus): a forward lobe through the slab, its diffuse share
    /// of the sun, the dome through it.
    Slab,
    /// A lifted layer, lit as today (design §1.2) and drawn by today's code.
    Classic,
}

/// The family a modern layer of `genus` is lit by. A lifted layer is Classic whatever its type.
pub fn family_of(genus: CloudGenus) -> LightFamily {
    match genus {
        CloudGenus::Cirrus | CloudGenus::Cirrocumulus => LightFamily::Ice,
        CloudGenus::Cirrostratus | CloudGenus::Altostratus | CloudGenus::Nimbostratus | CloudGenus::Stratus => LightFamily::Slab,
        CloudGenus::Altocumulus | CloudGenus::Stratocumulus | CloudGenus::Cumulus | CloudGenus::Cumulonimbus | CloudGenus::Lenticular | CloudGenus::Mammatus => LightFamily::Heap,
    }
}

/// A layer's light rows, its type's defaults filled in (design §4.1).
/// - Brightness scales its light.
/// - Sky light scales its ambient and its bounce.
/// - Silver lining is its forward lobe's share.
/// - Shade below is how much it takes off a lower layer's sun.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightRows {
    pub brightness: f32,
    pub sky_light: f32,
    pub lining: f32,
    pub shade_below: f32,
}

/// An entry along a view ray, measured by its class before any light (design §2.2). The light law reads it, so it
/// lives here, and `cloud_stack` re-exports it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Shape {
    /// Unfaded opacity: what hides the discs (Tu).
    pub cover: f32,
    /// cover x smoothstep(0, 0.12, d.y): what hides the sky (T).
    pub alpha: f32,
    /// A slab's optical depth tau x n at the hit (alpha = 1 - e^(-tau / mu)); 0 for sheets.
    pub tau: f32,
    /// A heap's self-shadow depth toward the sun (4 steps released, 2 held, times Shading).
    pub tau_sun: f32,
    /// Where the ray met the entry's plane: metres east (+x) and south (+z) of the viewer.
    pub hit_m: Vec2f,
    /// That plane's height, metres.
    pub height_m: f32,
}

/// An entry's cover along a look, without marching (design §2.1): its unfaded opacity, and for a slab its optical
/// depth tau x n there. A sheet's tau is not read: the light pass uses the depth its cover stands for.
#[derive(Clone, Copy, Debug, Default)]
pub struct CoverSample {
    pub cover: f32,
    pub tau: f32,
}

/// What the light pass, the shadows and the facts read of an entry. `cloud_stack::CloudEntry` implements it.
pub trait LitEntry {
    /// Its index in `CloudStackParams::layers`.
    fn layer(&self) -> usize;
    fn genus(&self) -> CloudGenus;
    /// A lifted layer, drawn by today's code (`classic` set).
    fn is_classic(&self) -> bool;
    /// Marched. For now only the one classic volume is: the layers do not march jointly yet (design §2.2).
    fn marched(&self) -> bool;
    /// Where it stands: the order's key.
    fn height_m(&self) -> f32;
    /// Its layer's base.
    fn base_m(&self) -> f32;
    /// Where the sun lights it: a classic entry's `classic.light_m` (design §1.2), a modern one's height.
    fn light_height_m(&self) -> f32;
    fn rows(&self) -> LightRows;
    /// The Clouds Cover times its eye.
    fn presence(&self) -> f32;
    /// A classic deck or volume: presence x classic.coverage. None for a classic cirrus and for a modern entry.
    fn classic_amount(&self) -> Option<f32>;
    /// Its exact unfaded cover along `d`. A classic volume marches its faded alpha, as today.
    fn cover_toward(&self, d: Vec3f) -> f32;
    /// Its cover along `d`, never marching: the light pass's and the facts' look.
    fn stand_in(&self, d: Vec3f) -> CoverSample;
    /// A flat entry's cover at a ground point of its plane (metres east, south), at `octaves`. A modern cloud volume's is
    /// its shadow at that point of its base (what it takes of the sun there), the classic volume's 0.
    fn plane_cover(&self, xz_m: Vec2f, octaves: u32) -> f32;
    /// A marched cloud volume's shell (base, top), metres: the light pass gives it the sun at its base and at its top
    /// (design §2.3's two sun colours). None for every other entry, lit at its light height alone.
    fn volume_shell_m(&self) -> Option<(f32, f32)> {
        None
    }
    /// A marched Nimbostratus' share of the sun on the ground under it (its mean sun transmittance), which the sunlit
    /// ground's bounce takes in place of its dome's pass. None for every other entry (lane ns).
    fn sun_share(&self) -> Option<f32> {
        None
    }
}

/// What the light pass reads of the map.
pub struct LightInputs<'a> {
    /// Toward the sun, map frame.
    pub sun_dir: Vec3f,
    /// The dome above every layer: atmo.ambient() + night.ambient(), the older layers' `cloud_ambient`.
    pub a_top: Vec3f,
    /// The ground under the clouds that lights their bases (`Env::cloud_floor` of the ground colour).
    pub ground_albedo: Vec3f,
    /// The sun a layer at a height is lit by (`Atmosphere::sun_irradiance_at`), called once per entry, so a lifted
    /// layer keeps today's sun exactly; a stack with a modern entry also reads it at 0, the sun on the ground.
    pub sun_at: &'a dyn Fn(f32) -> Vec3f,
    /// The clear sky's radiance along a direction (map frame, the discs apart), which colours the open sky beside a
    /// heap's face (`EntryLight::side`); None leaves the side the dome's colour.
    pub sky_at: Option<&'a dyn Fn(Vec3f) -> Vec3f>,
}

/// The open sky beside a heap's face is kept for this many azimuths of the face (`EntryLight::side`).
pub const SIDE_AZIMUTHS: usize = 16;
/// The sky the side ring reads: SIDE_RINGS rings of SIDE_LOOKS looks over the upper hemisphere.
const SIDE_RINGS: usize = 8;
const SIDE_LOOKS: usize = 32;

/// The open sky beside a vertical face against the whole dome, per channel, for SIDE_AZIMUTHS azimuths of the face's
/// normal (k x 360 / SIDE_AZIMUTHS degrees, as `dir_from_az_el`'s): the clear sky over the face's upper quarter
/// sphere weighted by the cosine to its normal, over the sky over the upper hemisphere weighted by the cosine to the
/// zenith, so a face sees the sky's colour on its own side (design §12.8 item 7: a shaded side looking away from the
/// sun sees the blue sky away from it, not the dome's mean). 1 everywhere without a sky.
fn side_ring(sky_at: Option<&dyn Fn(Vec3f) -> Vec3f>) -> [Vec3f; SIDE_AZIMUTHS] {
    let Some(sky_at) = sky_at else {
        return [Vec3f::all(1.0); SIDE_AZIMUTHS];
    };
    let mut looks = Vec::with_capacity(SIDE_RINGS * SIDE_LOOKS);
    for i in 0..SIDE_RINGS {
        // Rings equal in solid angle, at their middles.
        let y = (i as f32 + 0.5) / SIDE_RINGS as f32;
        let r = (1.0 - y * y).max(0.0).sqrt();
        for j in 0..SIDE_LOOKS {
            let a = std::f32::consts::TAU * (j as f32 + 0.5 * (i % 2) as f32) / SIDE_LOOKS as f32;
            let d = vec3f(r * a.sin(), y, -r * a.cos());
            looks.push((d, sky_at(d)));
        }
    }
    let (dome, dome_w) = looks.iter().fold((Vec3f::default(), 0.0f32), |(sum, w), (d, l)| (sum + *l * d.y, w + d.y));
    let dome = dome / dome_w.max(1.0e-9);
    std::array::from_fn(|k| {
        let a = std::f32::consts::TAU * k as f32 / SIDE_AZIMUTHS as f32;
        let n = vec3f(a.sin(), 0.0, -a.cos());
        let (side, side_w) = looks.iter().fold((Vec3f::default(), 0.0f32), |(sum, w), (d, l)| {
            let c = d.dot(n).max(0.0);
            (sum + *l * c, w + c)
        });
        let side = side / side_w.max(1.0e-9);
        let ratio = |s: f32, d: f32| if d > 0.0 && s.is_finite() { s / d } else { 1.0 };
        vec3f(ratio(side.x, dome.x), ratio(side.y, dome.y), ratio(side.z, dome.z))
    })
}

/// One entry's light, from the light pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntryLight {
    pub family: LightFamily,
    /// The sun at the entry's light height; a cloud volume's at its base.
    pub sun: Vec3f,
    /// A cloud volume's sun at its top (design §2.3: a high top stays pink after the base goes dark); `sun` for every
    /// other entry.
    pub sun_top: Vec3f,
    /// Modern: A_i, the dome greyed by G_i (a volume's once, by the entries above it), times the cascade. Classic: the
    /// dome greyed as today's layer greys it (`CloudLayer::shade`'s sky, `VolumetricClouds::sky_light`), times the
    /// cascade of the modern entries above it.
    /// Either way it is ready to use and is never greyed again.
    pub ambient: Vec3f,
    /// The dome's light from below on its base, which Sky light scales with the dome (design §12.8 item 6).
    /// - Modern: the ground's albedo times the dome as it reaches the ground under every entry (a heap's bounce).
    /// - Classic: the ground's albedo times `ambient` (the older volume's ground: `cloud_floor` times its sky light).
    pub ground: Vec3f,
    /// The sun's light from below on its base, which follows the sun, not Sky light (design §12.8 item 6): modern, the
    /// ground's albedo times sun.y / pi of the sun at 0 m times what every entry passes of it; classic, none.
    pub ground_sun: Vec3f,
    /// The dome's light on a heap's face from the open sky beside it, per azimuth of the face's normal (SIDE_AZIMUTHS,
    /// [`EntryLight::side_seen_along`]): `ambient` coloured as the sky on that side (design §12.8 item 7).
    pub side: [Vec3f; SIDE_AZIMUTHS],
    pub rows: LightRows,
    /// m_i: its mean stand-in cover over the 256 dome looks.
    pub dome_share: f32,
    /// tau_i where it covers, weighted by its cover; the depth T_d reads.
    pub mean_tau: f32,
}

impl EntryLight {
    /// The open sky's light on the face a view along `d` sees (its normal -d, level): `side` at the azimuth of -d,
    /// linear between its azimuths; `ambient` for a view straight up or down.
    pub fn side_seen_along(&self, d: Vec3f) -> Vec3f {
        let (x, z) = (-d.x, -d.z);
        if !(x * x + z * z > 1.0e-12) {
            return self.ambient;
        }
        let k = x.atan2(-z).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * SIDE_AZIMUTHS as f32;
        let i = (k as usize).min(SIDE_AZIMUTHS - 1);
        let t = k - i as f32;
        self.side[i] + (self.side[(i + 1) % SIDE_AZIMUTHS] - self.side[i]) * t
    }
}

/// T_d(tau) = 1 / (1 + 0.11 tau): the share of the light a slab of optical depth `tau` passes diffusely (design §2.3).
pub fn diffuse_transmittance(tau: f32) -> f32 {
    1.0 / (1.0 + TD_K * tau.max(0.0))
}

/// The heap family's sunlight per unit of sun irradiance, behind optical depth `tau_sun` toward the sun.
/// It is `volumetric::sun_light` with march_inner's powder weight (volumetric.rs:545) and `lining` as the forward
/// lobe's share. At the volume's own share (0.75) it is the volume's, bit for bit.
pub fn heap_sun(tau_sun: f32, cos_sun: f32, lining: f32) -> f32 {
    let powder_weight = volumetric::POWDER * 0.5 * (1.0 - cos_sun);
    volumetric::sun_light(tau_sun, &heap_phases(cos_sun, lining), powder_weight)
}

/// `volumetric::phase_octaves` with `lining` in place of FORWARD_SHARE: every octave halves the anisotropy.
fn heap_phases(cos_sun: f32, lining: f32) -> [f32; volumetric::OCTAVES] {
    let mut phases = [0.0; volumetric::OCTAVES];
    let mut round = 1.0f32;
    for phase in phases.iter_mut() {
        *phase = lining * hg_phase(cos_sun, volumetric::G_FORWARD * round) + (1.0 - lining) * hg_phase(cos_sun, volumetric::G_BACK * round);
        round *= 0.5;
    }
    phases
}

/// The ice family's phase (design §2.3), per unit of sun irradiance.
pub(crate) fn ice_phase(cos_sun: f32, lining: f32) -> f32 {
    ICE_GAIN * ((1.0 - lining) * hg_phase(cos_sun, ICE_G_BROAD) + lining * hg_phase(cos_sun, ICE_G_FORWARD))
}

/// The optical depth a look's cover stands for, for T_d: a slab's own (its stand-in's tau x n), a sheet's the depth
/// whose Beer-Lambert opacity its cover is.
fn look_tau(family: LightFamily, cover: f32, tau: f32) -> f32 {
    match family {
        LightFamily::Slab => tau.max(0.0),
        LightFamily::Heap | LightFamily::Ice | LightFamily::Classic => -(1.0 - cover.min(SHEET_COVER_MAX)).ln(),
    }
}

/// The light pass (design §2.1 steps 3-5, §2.3; the lifted layers, design §1.2), once per map, over the built entries
/// in ascending height.
/// - Each entry gets its sun at its light height.
/// - Each entry gets its share of the dome and its depth there, through its stand-in.
/// - A modern entry's ambient is the dome greyed by G_i = 1 - prod over j >= i of (1 - m_j), times the cascade over
///   the flat entries and the modern cloud volumes above it, prod (1 - m_j (1 - T_d,j)). A volume's is the dome
///   greyed once, by j > i alone and without the classic half (design §12.9 item 5): its own shadow on its dome is its
///   tau_up's T_d.
/// - A classic entry keeps today's greying, and only modern entries above it dim it.
/// - On a classic-only stack every cascade is exactly 1.
pub fn light_pass<E: LitEntry>(entries: &[E], inputs: &LightInputs) -> Vec<EntryLight> {
    let looks = (DOME_ROWS * DOME_COLUMNS) as f32;
    // Each entry's sun at its height, its share of the dome and its depth where it covers, through its stand-in.
    let mut lights: Vec<EntryLight> = entries
        .iter()
        .map(|entry| {
            let family = if entry.is_classic() { LightFamily::Classic } else { family_of(entry.genus()) };
            let (mut covered, mut depth) = (0.0f32, 0.0f32);
            for j in 0..DOME_ROWS {
                for k in 0..DOME_COLUMNS {
                    let s = entry.stand_in(dome_look(j, k).0);
                    let cover = if s.cover > 0.0 { s.cover.min(1.0) } else { 0.0 };
                    covered += cover;
                    depth += cover * look_tau(family, cover, s.tau);
                }
            }
            // A cloud volume is lit at its base and at its top (two calls), every other entry at its light height (one).
            let (sun, sun_top) = match entry.volume_shell_m() {
                Some((base_m, top_m)) => ((inputs.sun_at)(base_m), (inputs.sun_at)(top_m)),
                None => {
                    let sun = (inputs.sun_at)(entry.light_height_m());
                    (sun, sun)
                }
            };
            EntryLight {
                family,
                sun,
                sun_top,
                ambient: Vec3f::default(),
                ground: Vec3f::default(),
                ground_sun: Vec3f::default(),
                side: [Vec3f::default(); SIDE_AZIMUTHS],
                rows: entry.rows(),
                dome_share: covered / looks,
                mean_tau: if covered > 0.0 { depth / covered } else { 0.0 },
            }
        })
        .collect();
    // What each entry passes of the dome's light below it (its factor in the cascade), from its share and its depth.
    let passes: Vec<f32> = lights.iter().map(|light| 1.0 - light.dome_share * (1.0 - diffuse_transmittance(light.mean_tau))).collect();
    // Today's greying of the lifted layers (design §1.2): 1 - prod (1 - amount) over the built classic decks and
    // volumes. A classic cirrus is greyed over the decks alone: under a volume the older cirrus layer is built at
    // coverage 0 (the volumetric mode of `Env::new_for_width_par`), so it was greyed by half.
    let (mut classic_open, mut deck_open) = (1.0f32, 1.0f32);
    for entry in entries.iter().filter(|entry| entry.is_classic()) {
        if let Some(amount) = entry.classic_amount() {
            classic_open *= 1.0 - amount;
            if !entry.marched() {
                deck_open *= 1.0 - amount;
            }
        }
    }
    let grey = luminance(inputs.a_top);
    let greyed = |g: f32| inputs.a_top.mix(vec3f(grey, grey, grey), 0.5 + 0.5 * g);
    // Under every entry: the dome as it reaches the ground, greyed by all of them and passed by every unmarched one
    // (a marched entry dims nothing yet). This is what a heap's underside sees.
    let open = lights.iter().fold(1.0f32, |open, light| open * (1.0 - light.dome_share));
    let through = entries.iter().zip(&passes).filter(|(entry, _)| !entry.marched()).fold(1.0f32, |t, (_, &pass)| t * pass);
    // And the sun on that ground (design §4.3, Task 6a L2 of §12.7 item 5): a Lambertian ground under the sun is albedo x E_sun x sun.y /
    // pi where the sun reaches it, the share every entry passes (its clear share and what diffuses through its cover,
    // as the dome's cascade takes it, marched entries included: they shade the ground). Only a stack with a modern
    // entry reads it, so a classic-only stack calls `sun_at` exactly as before.
    let sun_ground = if entries.iter().any(|entry| !entry.is_classic()) {
        // (a marched cloud volume passes the sun to the ground by its own sunlit share, not its dome's pass: under a
        // closed deck no sunlit ground lights the base)
        let lit_share: f32 = entries.iter().zip(&passes).map(|(entry, &pass)| entry.sun_share().unwrap_or(pass)).product();
        (inputs.sun_at)(0.0) * (inputs.sun_dir.y.max(0.0) / std::f32::consts::PI * lit_share)
    } else {
        Vec3f::default()
    };
    let bounce = inputs.ground_albedo * (greyed(1.0 - open) * through);
    let sun_bounce = inputs.ground_albedo * sun_ground;
    let ring = side_ring(inputs.sky_at);
    for (i, entry) in entries.iter().enumerate() {
        let classic = entry.is_classic();
        // The entries above that dim its ambient. A marched entry is dimmed by none yet; a modern cloud volume dims a
        // flat entry under it as a flat entry does (a Stratus under a closed Nimbostratus keeps what the deck passes),
        // the classic volume none. Two classic entries never pair: today's layers do not dim each other.
        let mut cascade = 1.0f32;
        if !entry.marched() {
            for (above, &pass) in entries[i + 1..].iter().zip(&passes[i + 1..]) {
                if !(above.is_classic() && (above.marched() || classic)) {
                    cascade *= pass;
                }
            }
        }
        let (ambient, ground, ground_sun) = if classic {
            let g = if entry.classic_amount().is_some() { 1.0 - classic_open } else { 1.0 - deck_open };
            let ambient = greyed(g) * cascade;
            (ambient, inputs.ground_albedo * ambient, Vec3f::default())
        } else if entry.marched() {
            // A volume's dome is greyed once (design §12.9 item 5): what its own cover hides of the dome is in its
            // tau_up's T_d already, so it greys only by the entries above it, without the classic half. Greyed by its
            // own share too, a Cumulus' dome went from B/R 1.97 to 1.17 and its shaded sides to 1.04 against
            // kloofendal_48d's 1.39.
            let open_above = lights[i + 1..].iter().fold(1.0f32, |open, light| open * (1.0 - light.dome_share));
            (inputs.a_top.mix(vec3f(grey, grey, grey), 1.0 - open_above) * cascade, bounce, sun_bounce)
        } else {
            let open_above = lights[i..].iter().fold(1.0f32, |open, light| open * (1.0 - light.dome_share));
            (greyed(1.0 - open_above) * cascade, bounce, sun_bounce)
        };
        lights[i].ambient = ambient;
        lights[i].ground = ground;
        lights[i].ground_sun = ground_sun;
        lights[i].side = ring.map(|r| ambient * r);
    }
    lights
}

/// A modern sheet's or slab's radiance along `d`, premultiplied by `shape.alpha`, by its family's law (design §2.3).
/// `t_above` is what the entries above leave of its sun ([`t_above`]). A classic entry is shaded by its older layer
/// (cloud_classic.rs) with this light's sun and ambient, never here. It never allocates.
pub fn radiance(light: &EntryLight, shape: &Shape, d: Vec3f, sun_dir: Vec3f, t_above: f32) -> Vec3f {
    debug_assert!(light.family != LightFamily::Classic, "a classic entry is shaded by its older layer (cloud_classic.rs), not here");
    if !(shape.alpha > 0.0) {
        return Vec3f::default();
    }
    let rows = &light.rows;
    let cos_sun = d.dot(sun_dir);
    let sun = light.sun * t_above;
    let lit = match light.family {
        LightFamily::Heap => sun * heap_sun(shape.tau_sun, cos_sun, rows.lining) + (light.ambient + light.ground * volumetric::BOUNCE_SHARE) * rows.sky_light + light.ground_sun * volumetric::BOUNCE_SHARE,
        LightFamily::Ice => sun * ice_phase(cos_sun, rows.lining) + light.ambient * rows.sky_light,
        LightFamily::Slab => {
            let tau = shape.tau.max(0.0);
            let through = diffuse_transmittance(tau);
            let forward = rows.lining * SLAB_GAIN * hg_phase(cos_sun, SLAB_G) * (-0.5 * tau).exp();
            let diffuse = sun_dir.y.max(0.0) * through / std::f32::consts::PI;
            sun * (forward + diffuse) + light.ambient * (rows.sky_light * (0.5 + 0.5 * through))
        }
        LightFamily::Classic => return Vec3f::default(),
    };
    lit * (rows.brightness * shape.alpha)
}

/// T_above for entry `i` at its hit `shape` (design §2.3). It is the product, over each higher flat entry j, of
/// 1 - shade_below_j x j's plane cover where the sun's ray from the hit crosses j's plane. It uses 3 octaves released
/// and 2 held.
/// - A modern cloud volume above casts too, where the ray crosses its base: its plane cover there is what it takes of
///   the sun (`CloudVolume::shadow_cover`). The classic volume casts nothing, as today.
/// - It is 1 with the sun at or below the horizon, where the hit shows no cloud, and for a marched entry (marched
///   entries receive none of these shadows yet).
/// - Two classic entries never pair: classic layers never shade each other, as today.
/// - It fades in over the sun's first two degrees.
/// - It never allocates.
pub fn t_above<E: LitEntry>(entries: &[E], i: usize, shape: &Shape, sun_dir: Vec3f, held: bool) -> f32 {
    let Some(entry) = entries.get(i) else {
        return 1.0;
    };
    if entry.marched() || !(shape.alpha > 0.0) || !(sun_dir.y > 0.0) {
        return 1.0;
    }
    passed_from(&entries[i + 1..], entry.is_classic(), false, shape.hit_m, shape.height_m, sun_dir, held, false)
}

/// T_above at a point of the marched cloud volume `i` (design §2.3: a higher flat entry shades a lower volume): the
/// point is `hit_m` (metres east and south) at `height_m`, where the light of one record of its march came from (its
/// mean depth), and every flat entry standing higher than the point takes Shade below times its plane cover where
/// the sun's ray from the point crosses its plane, as [`t_above`] for a flat hit. Marched entries cast nothing yet.
/// 1 with the sun at or below the horizon; it fades in over the sun's first two degrees. It never allocates.
pub fn t_above_volume<E: LitEntry>(entries: &[E], i: usize, hit_m: Vec2f, height_m: f32, sun_dir: Vec3f, held: bool) -> f32 {
    if i >= entries.len() || !(sun_dir.y > 0.0) {
        return 1.0;
    }
    let classic = entries[i].is_classic();
    let (below, above) = entries.split_at(i);
    let passed = passed_from(below, classic, true, hit_m, height_m, sun_dir, held, true);
    passed * passed_from(&above[1..], classic, true, hit_m, height_m, sun_dir, held, true)
}

/// What the entries of `above` leave of the sun at a hit `hit_m` at `height_m` (the loop of [`t_above`]), faded in over
/// the sun's first degrees: every flat entry, and every modern cloud volume where the hit is no marched one's (`marched`:
/// two marched entries never pair yet). `over_only` skips an entry standing no higher than the hit (a volume's point
/// can stand above a flat entry inside its shell).
fn passed_from<E: LitEntry>(above: &[E], classic: bool, marched: bool, hit_m: Vec2f, height_m: f32, sun_dir: Vec3f, held: bool, over_only: bool) -> f32 {
    let octaves = if held { HELD_SHADOW_OCTAVES } else { SHADOW_OCTAVES };
    // Along the sun's ray from the hit: metres east and south per metre of height.
    let slope = vec2f(sun_dir.x, sun_dir.z) * (1.0 / sun_dir.y);
    let mut passed = 1.0f32;
    for above in above {
        if (above.marched() && (marched || above.is_classic())) || (classic && above.is_classic()) {
            continue;
        }
        if over_only && !(above.height_m() > height_m) {
            continue;
        }
        let shade = above.rows().shade_below;
        if !(shade > 0.0) {
            continue;
        }
        // a flat entry's plane; a cloud volume's base, where the ray enters it
        let plane = above.volume_shell_m().map_or(above.height_m(), |(base, _)| base);
        let rise = (plane - height_m).max(0.0);
        let cover = above.plane_cover(hit_m + slope * rise, octaves);
        let cover = if cover > 0.0 { cover.min(1.0) } else { 0.0 };
        passed *= 1.0 - shade.min(1.0) * cover;
    }
    let fade = smooth(0.0, SHADOW_FADE_SIN, sun_dir.y);
    if fade >= 1.0 {
        passed
    } else {
        1.0 - fade * (1.0 - passed)
    }
}

/// smoothstep under its own name, so no glob can make it ambiguous (volumetric.rs `smooth`).
fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dir_from_az_el;

    /// The dome over the test layers and the ground under them.
    const A_TOP: Vec3f = Vec3f { x: 0.1, y: 0.2, z: 0.4 };
    const GROUND: Vec3f = Vec3f { x: 0.2, y: 0.25, z: 0.15 };

    /// An entry of one cover over every look above the sine of elevation `above` (none below it), for the light law
    /// alone. Its plane holds that cover everywhere, or with `east_of` only east of a line.
    #[derive(Clone, Copy)]
    pub(super) struct Mock {
        genus: CloudGenus,
        classic: bool,
        marched: bool,
        height_m: f32,
        light_height_m: f32,
        rows: LightRows,
        amount: Option<f32>,
        cover: f32,
        tau: f32,
        above: f32,
        /// Where its plane's cover begins: metres east of the viewer, in `Shape::hit_m`'s frame. None covers it all.
        east_of_m: Option<f32>,
    }

    impl Mock {
        /// A modern layer of `genus`, its type's light rows.
        pub(super) fn modern(genus: CloudGenus, height_m: f32, cover: f32, tau: f32) -> Mock {
            let rows = LightRows { brightness: 1.0, sky_light: 1.0, lining: genus.default_lining(), shade_below: genus.default_shade_below() };
            Mock { genus, classic: false, marched: false, height_m, light_height_m: height_m, rows, amount: None, cover, tau, above: 0.0, east_of_m: None }
        }

        /// The same entry, its plane covered only east of `line_m` metres east of the viewer, clear west of it.
        pub(super) fn east_of(self, line_m: f32) -> Mock {
            Mock { east_of_m: Some(line_m), ..self }
        }

        /// A lifted layer lit at `light_height_m`: a deck or a volume carries its amount, a cirrus none.
        fn classic(genus: CloudGenus, height_m: f32, light_height_m: f32, amount: Option<f32>, cover: f32) -> Mock {
            Mock { classic: true, light_height_m, amount, ..Mock::modern(genus, height_m, cover, 0.0) }
        }
    }

    impl LitEntry for Mock {
        fn layer(&self) -> usize {
            0
        }
        fn genus(&self) -> CloudGenus {
            self.genus
        }
        fn is_classic(&self) -> bool {
            self.classic
        }
        fn marched(&self) -> bool {
            self.marched
        }
        fn height_m(&self) -> f32 {
            self.height_m
        }
        fn base_m(&self) -> f32 {
            self.height_m
        }
        fn light_height_m(&self) -> f32 {
            self.light_height_m
        }
        fn rows(&self) -> LightRows {
            self.rows
        }
        fn presence(&self) -> f32 {
            1.0
        }
        fn classic_amount(&self) -> Option<f32> {
            self.amount
        }
        fn cover_toward(&self, d: Vec3f) -> f32 {
            if d.y > self.above { self.cover } else { 0.0 }
        }
        fn stand_in(&self, d: Vec3f) -> CoverSample {
            CoverSample { cover: self.cover_toward(d), tau: self.tau }
        }
        fn plane_cover(&self, xz_m: Vec2f, _octaves: u32) -> f32 {
            let west_of_the_line = self.east_of_m.is_some_and(|line_m| !(xz_m.x > line_m));
            if self.marched || west_of_the_line { 0.0 } else { self.cover }
        }
    }

    fn inputs(sun_at: &dyn Fn(f32) -> Vec3f) -> LightInputs<'_> {
        LightInputs { sun_dir: dir_from_az_el(180.0, 40.0), a_top: A_TOP, ground_albedo: GROUND, sun_at, sky_at: None }
    }

    /// The dome greyed toward its own luminance by `g`, as clouds.rs:212-214 greys it by its coverage.
    fn greyed(g: f32) -> Vec3f {
        let grey = luminance(A_TOP);
        A_TOP.mix(vec3f(grey, grey, grey), 0.5 + 0.5 * g)
    }

    fn close(a: Vec3f, b: Vec3f, relative: f32) -> bool {
        (a - b).length() <= relative * b.length().max(1.0e-12)
    }

    /// Design §2.3: a heap is lit by the marched volume's own law, so a sheet and its shell share one light. At the
    /// volume's forward share (0.75, the heaps' default Silver lining) the heap's sun term is `volumetric::sun_light`
    /// with march_inner's octave phases and powder (volumetric.rs:543-545), bit for bit.
    #[test]
    fn the_heap_law_is_the_volumes() {
        assert_eq!(volumetric::FORWARD_SHARE, CloudGenus::Stratocumulus.default_lining());
        assert_eq!(volumetric::FORWARD_SHARE, CloudGenus::Cumulus.default_lining());
        for cos_sun in [-1.0f32, -0.4, 0.0, 0.3, 0.8, 0.99, 1.0] {
            for tau in [0.0f32, 0.05, 0.4, 1.0, 3.0, 12.0] {
                let volume = volumetric::sun_light(tau, &volumetric::phase_octaves(cos_sun), volumetric::POWDER * 0.5 * (1.0 - cos_sun));
                assert_eq!(heap_sun(tau, cos_sun, volumetric::FORWARD_SHARE).to_bits(), volume.to_bits(), "cos {cos_sun}, tau {tau}");
            }
        }
        // Its radiance: that law on the sun at its height through T_above, its ambient and the dome's bounce by Sky
        // light, the sun's bounce as it is (design §12.8 item 6), all times Brightness and alpha.
        let rows = LightRows { brightness: 0.8, sky_light: 1.3, lining: 0.75, shade_below: 1.0 };
        let light = EntryLight { family: LightFamily::Heap, sun: vec3f(3.0, 2.5, 2.0), sun_top: vec3f(3.0, 2.5, 2.0), ambient: vec3f(0.1, 0.2, 0.4), ground: vec3f(0.02, 0.03, 0.01), ground_sun: vec3f(0.05, 0.04, 0.03), side: [Vec3f::default(); SIDE_AZIMUTHS], rows, dome_share: 0.4, mean_tau: 2.0 };
        let (d, sun_dir) = (dir_from_az_el(170.0, 35.0), dir_from_az_el(180.0, 40.0));
        let shape = Shape { cover: 1.0, alpha: 0.7, tau: 0.0, tau_sun: 1.3, hit_m: Vec2f::default(), height_m: 1500.0 };
        let want = (light.sun * 0.6 * heap_sun(1.3, d.dot(sun_dir), 0.75) + (light.ambient + light.ground * volumetric::BOUNCE_SHARE) * 1.3 + light.ground_sun * volumetric::BOUNCE_SHARE) * (0.8 * 0.7);
        let got = radiance(&light, &shape, d, sun_dir, 0.6);
        assert!(close(got, want, 1.0e-6), "{got:?} vs {want:?}");
        // Less Silver lining is less light toward the sun, and the lining is brightest toward it.
        assert!(heap_sun(0.5, 0.98, 0.75) > heap_sun(0.5, 0.98, 0.3));
        assert!(heap_sun(0.5, 0.98, 0.75) > 3.0 * heap_sun(0.5, -0.5, 0.75));
        // Nothing seen, nothing drawn.
        assert_eq!(radiance(&light, &Shape { alpha: 0.0, ..shape }, d, sun_dir, 1.0), Vec3f::default());
    }

    /// Design §2.3: ice is two lobes, 1.5 x (0.5 HG(0.3) + 0.5 HG(0.85)) at its default Silver lining, fully lit: no
    /// self-shadow reaches it.
    #[test]
    fn ice_is_fully_lit() {
        let lining = CloudGenus::Cirrus.default_lining();
        assert_eq!(lining, 0.5);
        let rows = LightRows { brightness: 1.0, sky_light: 1.0, lining, shade_below: CloudGenus::Cirrus.default_shade_below() };
        let light = EntryLight { family: LightFamily::Ice, sun: vec3f(4.0, 3.5, 3.0), sun_top: vec3f(4.0, 3.5, 3.0), ambient: Vec3f::default(), ground: Vec3f::default(), ground_sun: Vec3f::default(), side: [Vec3f::default(); SIDE_AZIMUTHS], rows, dome_share: 0.3, mean_tau: 0.4 };
        let sun_dir = dir_from_az_el(200.0, 25.0);
        let open = Shape { cover: 0.4, alpha: 0.4, tau: 0.0, tau_sun: 0.0, hit_m: Vec2f::default(), height_m: 9000.0 };
        let shaded = Shape { tau_sun: 8.0, ..open };
        for (az, el) in [(200.0f32, 30.0f32), (150.0, 60.0), (20.0, 40.0)] {
            let d = dir_from_az_el(az, el);
            let lit = radiance(&light, &open, d, sun_dir, 1.0);
            assert_eq!(lit, radiance(&light, &shaded, d, sun_dir, 1.0), "az {az} el {el}: a self-shadow does not reach ice");
            let cos = d.dot(sun_dir);
            let want = light.sun * (1.5 * (0.5 * hg_phase(cos, 0.3) + 0.5 * hg_phase(cos, 0.85))) * 0.4;
            assert!(close(lit, want, 1.0e-6), "az {az} el {el}: {lit:?} vs {want:?}");
        }
        let toward = radiance(&light, &open, dir_from_az_el(200.0, 27.0), sun_dir, 1.0);
        let away = radiance(&light, &open, dir_from_az_el(20.0, 40.0), sun_dir, 1.0);
        assert!(toward.y > 5.0 * away.y, "its silver lining is toward the sun: {toward:?} vs {away:?}");
    }

    /// Design §2.3: a slab passes T_d(tau) = 1 / (1 + 0.11 tau) of the light diffusely (Nimbostratus at tau 50:
    /// 0.154), glows forward round the sun when thin, and under a closed one a lower layer's ambient is the grey it
    /// passes.
    #[test]
    fn a_closed_slab_passes_its_diffuse_share() {
        assert!((diffuse_transmittance(50.0) - 0.154).abs() < 1.0e-3, "{}", diffuse_transmittance(50.0));
        assert_eq!(diffuse_transmittance(0.0), 1.0);
        let sun_dir = dir_from_az_el(180.0, 40.0);
        let rows = LightRows { brightness: 1.0, sky_light: 1.0, lining: CloudGenus::Nimbostratus.default_lining(), shade_below: 1.0 };
        let light = EntryLight { family: LightFamily::Slab, sun: vec3f(5.0, 4.5, 4.0), sun_top: vec3f(5.0, 4.5, 4.0), ambient: vec3f(0.1, 0.12, 0.15), ground: Vec3f::default(), ground_sun: Vec3f::default(), side: [Vec3f::default(); SIDE_AZIMUTHS], rows, dome_share: 1.0, mean_tau: 50.0 };
        let closed = Shape { cover: 1.0, alpha: 1.0, tau: 50.0, tau_sun: 0.0, hit_m: Vec2f::default(), height_m: 1000.0 };
        // Closed: no forward lobe left (e^-25), its diffuse share of the sun, the dome greyed through it.
        let through = diffuse_transmittance(50.0);
        let want = light.sun * (sun_dir.y * through / std::f32::consts::PI) + light.ambient * (0.5 + 0.5 * through);
        let got = radiance(&light, &closed, sun_dir, sun_dir, 1.0);
        assert!(close(got, want, 1.0e-5), "{got:?} vs {want:?}");
        // Thin: it glows forward round the sun.
        let thin = Shape { tau: 1.0, ..closed };
        let (toward, away) = (radiance(&light, &thin, sun_dir, sun_dir, 1.0), radiance(&light, &thin, dir_from_az_el(0.0, 40.0), sun_dir, 1.0));
        assert!(toward.y > 3.0 * away.y, "{toward:?} vs {away:?}");
        // The cascade: a Stratus under a closed Nimbostratus of tau 50 sees the dome's grey through T_d of it; the
        // slab greys its own dome by its share and keeps its luminance.
        let entries = [Mock::modern(CloudGenus::Stratus, 400.0, 0.5, 2.0), Mock::modern(CloudGenus::Nimbostratus, 1000.0, 1.0, 50.0)];
        let sun_at = |_: f32| Vec3f::all(1.0);
        let lights = light_pass(&entries, &inputs(&sun_at));
        assert_eq!((lights[1].dome_share, lights[1].mean_tau), (1.0, 50.0));
        let grey = luminance(A_TOP);
        let under = vec3f(grey, grey, grey) * through;
        assert!(close(lights[0].ambient, under, 1.0e-5), "{:?} vs {under:?}", lights[0].ambient);
        assert!(close(lights[1].ambient, vec3f(grey, grey, grey), 1.0e-5), "{:?}", lights[1].ambient);
        assert!((luminance(lights[1].ambient) / grey - 1.0).abs() < 1.0e-6);
        // A heap under the closed slab is lit from below by the ground under both: the dome greyed by every entry's
        // share (closed, so its grey) and the sun at 0 m on it, sun.y / pi of it (the sun here 40 degrees up, white),
        // each times what every entry passes diffusely (the heap's sheet 1 - 0.4 (1 - T_d(-ln 0.6)), the slab T_d(50)),
        // times the ground's albedo.
        let heap_under = [Mock::modern(CloudGenus::Stratocumulus, 1200.0, 0.4, 0.0), Mock::modern(CloudGenus::Nimbostratus, 2000.0, 1.0, 50.0)];
        let lights = light_pass(&heap_under, &inputs(&sun_at));
        let sheet_passes = 1.0 - 0.4 * (1.0 - diffuse_transmittance(-(0.6f32).ln()));
        let sun_on_ground = 40f32.to_radians().sin() / std::f32::consts::PI;
        let (bounce, sun_bounce) = (GROUND * vec3f(grey, grey, grey) * (sheet_passes * through), GROUND * Vec3f::all(sun_on_ground * sheet_passes * through));
        assert!(close(lights[0].ground, bounce, 1.0e-5), "the heap's ground {:?} vs {bounce:?}", lights[0].ground);
        assert!(close(lights[0].ground_sun, sun_bounce, 1.0e-5), "the heap's sunlit ground {:?} vs {sun_bounce:?}", lights[0].ground_sun);
        assert_eq!(lights[1].ground_sun, lights[0].ground_sun, "every modern entry sees the one sunlit ground");
        // The heap alone under an open sky: the dome greyed by its share times what it passes (a marched entry dims no
        // dome light yet), and the sun at 0 m (here twice the sun at its height) times what it passes, marched or not
        // (a marched entry shades the ground); the sun is read at its height, then once at 0 m.
        let heights = std::cell::RefCell::new(Vec::new());
        let sun_at = |h: f32| {
            heights.borrow_mut().push(h);
            if h == 0.0 { Vec3f::all(2.0) } else { Vec3f::all(1.0) }
        };
        for marched in [false, true] {
            heights.borrow_mut().clear();
            let lights = light_pass(&[Mock { marched, ..heap_under[0] }], &inputs(&sun_at));
            assert_eq!(*heights.borrow(), vec![1200.0, 0.0], "marched {marched}: its sun at its height, then the ground's at 0 m, once");
            let dome = if marched { 1.0 } else { sheet_passes };
            let (bounce, sun_bounce) = (GROUND * greyed(0.4) * dome, GROUND * Vec3f::all(2.0 * sun_on_ground * sheet_passes));
            assert!(close(lights[0].ground, bounce, 1.0e-5), "marched {marched}: the heap's ground {:?} vs {bounce:?}", lights[0].ground);
            assert!(close(lights[0].ground_sun, sun_bounce, 1.0e-5), "marched {marched}: the heap's sunlit ground {:?} vs {sun_bounce:?}", lights[0].ground_sun);
        }
        // The open sky beside a heap's face (design §12.8 item 7): under a sky alike everywhere it is the dome's light;
        // under a sky blue to the south (azimuth 180) and white to the north, a face looking south (seen by a view along
        // the north) takes more blue than red, more than the dome, and one looking north the reverse; with no sky given,
        // the dome's.
        let plain = light_pass(&[heap_under[0]], &inputs(&sun_at))[0];
        assert!(plain.side.iter().all(|&s| s == plain.ambient), "no sky: the side is the dome's light");
        let even = |_: Vec3f| vec3f(0.3, 0.4, 0.6);
        let lit = light_pass(&[heap_under[0]], &LightInputs { sky_at: Some(&even), ..inputs(&sun_at) })[0];
        assert!(lit.side.iter().all(|&s| close(s, lit.ambient, 1.0e-5)), "an even sky: the side is the dome's light");
        let split = |d: Vec3f| if d.z > 0.0 { vec3f(0.2, 0.3, 0.8) } else { vec3f(0.6, 0.6, 0.6) };
        let lit = light_pass(&[heap_under[0]], &LightInputs { sky_at: Some(&split), ..inputs(&sun_at) })[0];
        let (south, north) = (lit.side_seen_along(dir_from_az_el(0.0, 10.0)), lit.side_seen_along(dir_from_az_el(180.0, 10.0)));
        let blue = |c: Vec3f, a: Vec3f| c.z / a.z / (c.x / a.x);
        assert!(blue(south, lit.ambient) > 1.5 && blue(north, lit.ambient) < 1.0 / 1.5, "the face looking south {south:?}, north {north:?}, the dome {:?}", lit.ambient);
        assert!(close(lit.side_seen_along(vec3f(0.0, 1.0, 0.0)), lit.ambient, 0.0), "a view straight up sees no side");
    }

    /// Design §2.3: for one layer the cascade is today's rule (clouds.rs:212-214, the dome greyed by 0.5 + 0.5 x
    /// coverage) with its measured share of the dome in place of its coverage number.
    #[test]
    fn one_layer_greys_by_its_share() {
        let sun_at = |_: f32| Vec3f::default();
        for share in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let lights = light_pass(&[Mock::modern(CloudGenus::Stratocumulus, 1200.0, share, 0.0)], &inputs(&sun_at));
            assert_eq!(lights[0].dome_share, share);
            assert!(close(lights[0].ambient, greyed(share), 1.0e-6), "share {share}: {:?} vs {:?}", lights[0].ambient, greyed(share));
        }
        // Measured, not its Cover number: a layer whole above 30 degrees and clear below hides half the dome's looks.
        let upper = Mock { above: 0.5, ..Mock::modern(CloudGenus::Stratocumulus, 1200.0, 1.0, 0.0) };
        let lights = light_pass(&[upper], &inputs(&sun_at));
        assert_eq!(lights[0].dome_share, 0.5);
        assert!(close(lights[0].ambient, greyed(0.5), 1.0e-6), "{:?}", lights[0].ambient);
        // A volume is greyed once (design §12.9 item 5): its own share is its tau_up's, so alone it takes the dome as it
        // is whatever its share, and under a layer it greys by that layer's share alone, without the classic half.
        let grey = luminance(A_TOP);
        let once = |g: f32| A_TOP.mix(vec3f(grey, grey, grey), g);
        for share in [0.0f32, 0.5, 1.0] {
            let volume = Mock { marched: true, ..Mock::modern(CloudGenus::Cumulus, 1200.0, share, 0.0) };
            let lights = light_pass(&[volume], &inputs(&sun_at));
            assert_eq!(lights[0].dome_share, share);
            assert!(close(lights[0].ambient, A_TOP, 1.0e-6), "a volume at share {share}: {:?}", lights[0].ambient);
            let lights = light_pass(&[volume, Mock::modern(CloudGenus::Stratocumulus, 3000.0, 0.25, 0.0)], &inputs(&sun_at));
            assert!(close(lights[0].ambient, once(0.25), 1.0e-6), "a volume at share {share} under 0.25: {:?}", lights[0].ambient);
        }
    }

    /// Design §1.2: a lifted layer is lit at its own light height (one call per entry) and greyed as today: a deck or
    /// a volume by 0.5 + 0.5 g, g = 1 - prod (1 - amount) over the built classic decks and volumes; a cirrus by the
    /// decks alone (the volumetric mode of `Env::new_for_width_par` builds the older cirrus at coverage 0, so it was
    /// greyed by half). Nothing dims a classic-only stack; a modern layer above a classic one dims it, and a classic
    /// one above a modern one dims and greys that.
    #[test]
    fn a_classic_entry_is_greyed_as_today() {
        let heights = std::cell::RefCell::new(Vec::new());
        let sun_at = |h: f32| {
            heights.borrow_mut().push(h);
            vec3f(h, 1.0, 1.0)
        };
        let deck = Mock::classic(CloudGenus::Stratocumulus, 2000.0, 2000.0, Some(0.6), 0.6);
        let cirrus = Mock::classic(CloudGenus::Cirrus, 8000.0, 2000.0, None, 0.2);
        // Deck mode: one shade() call greyed both by the deck's coverage, lit at the deck's altitude.
        let lights = light_pass(&[deck, cirrus], &inputs(&sun_at));
        assert_eq!(*heights.borrow(), vec![2000.0, 2000.0], "each entry's sun at its light height, once");
        for light in &lights {
            assert_eq!(light.family, LightFamily::Classic);
            assert_eq!(light.sun, vec3f(2000.0, 1.0, 1.0));
            assert!(close(light.ambient, greyed(0.6), 1.0e-6), "{:?} vs {:?}", light.ambient, greyed(0.6));
        }
        // Volume mode: the volume greyed by its amount and lit at its middle, its ground the floor under that dome;
        // the cirrus greyed by half.
        heights.borrow_mut().clear();
        let volume = Mock { marched: true, ..Mock::classic(CloudGenus::Cumulus, 2250.0, 2750.0, Some(0.5), 0.5) };
        let lights = light_pass(&[volume, cirrus], &inputs(&sun_at));
        assert_eq!(*heights.borrow(), vec![2750.0, 2000.0]);
        assert!(close(lights[0].ambient, greyed(0.5), 1.0e-6));
        assert!(close(lights[0].ground, GROUND * greyed(0.5), 1.0e-6));
        assert!(close(lights[1].ambient, greyed(0.0), 1.0e-6));
        // Two decks grey each other by the union of their amounts.
        let second = Mock::classic(CloudGenus::Nimbostratus, 3000.0, 3000.0, Some(0.5), 0.5);
        for light in light_pass(&[deck, second], &inputs(&sun_at)) {
            assert!(close(light.ambient, greyed(1.0 - 0.4 * 0.5), 1.0e-6), "{:?}", light.ambient);
        }
        // A modern Cirrus above the deck dims its ambient by what the Cirrus passes; the deck above a modern Stratus
        // dims the Stratus and greys it with its share.
        let pass = |cover: f32| 1.0 - cover * (1.0 - diffuse_transmittance(-(1.0 - cover).ln()));
        let lights = light_pass(&[deck, Mock::modern(CloudGenus::Cirrus, 9000.0, 0.5, 0.0)], &inputs(&sun_at));
        assert!(close(lights[0].ambient, greyed(0.6) * pass(0.5), 1.0e-5), "{:?}", lights[0].ambient);
        assert!(close(lights[1].ambient, greyed(0.5), 1.0e-6));
        let lights = light_pass(&[Mock::modern(CloudGenus::Stratus, 400.0, 0.5, 2.0), deck], &inputs(&sun_at));
        assert!(close(lights[0].ambient, greyed(1.0 - 0.5 * 0.4) * pass(0.6), 1.0e-5), "{:?}", lights[0].ambient);
    }

    /// Design §2.3: T_above is 1 with the sun at or below the horizon, where the hit shows no cloud, for and from a
    /// marched entry (marched entries neither cast nor receive these shadows yet), between two classic entries and
    /// for the highest entry; a closed layer above with Shade below 1 takes all of the sun, at 0.25 a quarter; it
    /// fades in over the sun's first degrees.
    #[test]
    fn nothing_shades_with_the_sun_down() {
        let low = Mock::modern(CloudGenus::Stratus, 400.0, 1.0, 5.0);
        let high = Mock::modern(CloudGenus::Nimbostratus, 1000.0, 1.0, 50.0);
        let entries = [low, high];
        let hit = Shape { cover: 1.0, alpha: 1.0, tau: 5.0, tau_sun: 0.0, hit_m: vec2f(120.0, -340.0), height_m: 400.0 };
        for el in [0.0f32, -0.5, -10.0, -60.0] {
            let sun = dir_from_az_el(180.0, el);
            for held in [false, true] {
                assert_eq!(t_above(&entries, 0, &hit, sun, held), 1.0, "el {el}, held {held}");
            }
        }
        let up = dir_from_az_el(180.0, 40.0);
        assert_eq!(t_above(&entries, 0, &hit, up, false), 0.0, "a closed layer above, Shade below 1");
        assert_eq!(t_above(&entries, 0, &hit, up, true), 0.0, "held too");
        let quarter = [low, Mock { rows: LightRows { shade_below: 0.25, ..high.rows }, ..high }];
        assert!((t_above(&quarter, 0, &hit, up, false) - 0.75).abs() < 1.0e-6);
        assert_eq!(t_above(&entries, 1, &Shape { height_m: 1000.0, ..hit }, up, false), 1.0, "nothing above the highest");
        assert_eq!(t_above(&entries, 0, &Shape { cover: 0.0, alpha: 0.0, ..hit }, up, false), 1.0, "no cloud at the hit");
        assert_eq!(t_above(&entries, 5, &hit, up, false), 1.0, "no such entry");
        assert_eq!(t_above(&[Mock { marched: true, ..low }, high], 0, &hit, up, false), 1.0, "a marched entry receives none");
        assert_eq!(t_above(&[low, Mock { marched: true, ..high }], 0, &hit, up, false), 1.0, "nor casts");
        let classic = |m: Mock| Mock { classic: true, amount: Some(1.0), ..m };
        assert_eq!(t_above(&[classic(low), classic(high)], 0, &hit, up, false), 1.0, "two classic entries never pair");
        assert_eq!(t_above(&[classic(low), high], 0, &hit, up, false), 0.0, "a modern layer above a classic one shades it");
        // Half a degree up, the shadow has begun to fall: between none and all of it.
        let t = t_above(&entries, 0, &hit, dir_from_az_el(180.0, 0.5), false);
        assert!(t > 0.0 && t < 1.0, "{t}");
    }
}

/// The light law on built stacks.
#[cfg(test)]
mod stack_tests {
    use super::tests::Mock;
    use super::*;
    use crate::{dir_from_az_el, CloudLayerParams, CloudStackParams, Env, HdriParams};

    /// A map of `layers` (type, Cover) as "+ Cloud" makes them, the sun 40 degrees up in the south.
    fn stacked(layers: &[(CloudGenus, f32)]) -> HdriParams {
        let mut p = HdriParams::default();
        p.sky.sun.mode = "manual".to_string();
        p.sky.sun.elevation_deg = 40.0;
        p.sky.sun.azimuth_deg = 180.0;
        let mut list: Vec<CloudLayerParams> = Vec::new();
        for &(genus, cover) in layers {
            let mut layer = CloudLayerParams::new(genus, &list, p.seed);
            layer.cover = Some(cover);
            list.push(layer);
        }
        p.sky.cloud_stack = Some(CloudStackParams { layers: list, ..CloudStackParams::default() });
        p
    }

    /// The built entry of `genus`.
    fn index_of(env: &Env, genus: CloudGenus) -> usize {
        env.cloud_stack.as_ref().expect("built").entries().iter().position(|entry| entry.genus() == genus).expect("listed and built")
    }

    /// Design §2.3: a higher layer takes Shade below times its cover off a lower flat layer's sun, where the sun's ray
    /// from the lower hit crosses it. It never does so the other way, and casts nothing at Shade below 0. In the map, a
    /// Stratus under a shading deck has lost its sun term. The Stratus and the Nimbostratus are marched volumes (the
    /// volumes design §2.2, lane ns), and two marched entries never pair yet: here the Stratus is a classic deck at its
    /// base, and the Nimbostratus over it casts its volume's shadow (`CloudVolume::shadow_cover`) as a flat layer does.
    /// (A marched entry's sun is taken at a point of it as the compositor takes a record's, `t_above_volume`.)
    #[test]
    fn a_higher_layer_shades_a_lower_one() {
        let sun = dir_from_az_el(180.0, 40.0);
        // Hits on an entry's plane, 2 km apart over 40 km round the viewer, cloud at each.
        let mean_t = |env: &Env, i: usize, held: bool| {
            let stack = env.cloud_stack.as_ref().unwrap();
            let height_m = stack.entries()[i].height_m();
            let mut sum = 0.0f32;
            for x in -10..=10 {
                for z in -10..=10 {
                    let hit_m = vec2f(x as f32 * 2000.0, z as f32 * 2000.0);
                    let hit = Shape { cover: 1.0, alpha: 1.0, tau: 4.0, tau_sun: 0.0, hit_m, height_m };
                    sum += if stack.entries()[i].marched() { t_above_volume(stack.entries(), i, hit_m, height_m, sun, held) } else { t_above(stack.entries(), i, &hit, sun, held) };
                }
            }
            sum / 441.0
        };
        // The Stratus under it: a classic Stratus deck (the modern one is a marched volume, and the Nimbostratus over it
        // is one too, lane ns: two marched entries never pair yet), closed, at the modern Stratus' default base.
        let decked = |cover: f32| {
            let mut p = stacked(&[(CloudGenus::Stratus, 1.0), (CloudGenus::Nimbostratus, cover)]);
            let deck = &mut p.sky.cloud_stack.as_mut().unwrap().layers[0];
            deck.classic = Some(crate::ClassicLook { coverage: 1.0, sharpness: 1.0, light_m: deck.base_m() });
            p
        };
        let closed = Env::new(&decked(1.0));
        let (st, ns) = (index_of(&closed, CloudGenus::Stratus), index_of(&closed, CloudGenus::Nimbostratus));
        assert!(st < ns, "the lower layer is in front");
        let under = mean_t(&closed, st, false);
        assert!(under < 0.05, "under a closed Nimbostratus a Stratus keeps {under} of its sun");
        assert_eq!(mean_t(&closed, ns, false), 1.0, "nothing above the highest");
        let half = Env::new(&decked(0.5));
        let (released, held) = (mean_t(&half, st, false), mean_t(&half, st, true));
        assert!((0.3..0.7).contains(&released) && (released - held).abs() < 0.1, "under a half cover: {released} released, {held} held");
        let mut p = decked(1.0);
        p.sky.cloud_stack.as_mut().unwrap().layers[1].shade_below = Some(0.0);
        let unshaded = Env::new(&p);
        assert_eq!(mean_t(&unshaded, st, false), 1.0, "Shade below 0 casts nothing");
        // Where the sun's ray meets the layer above: a deck 600 m over the hit, covered only east of the line 0 m
        // east of the viewer, and the hit 100 m west of that line. With the sun 30 degrees up the ray climbs 600 m over
        // about 1040 m, so it meets the covered side only when the sun is in the east.
        let edge = [Mock::modern(CloudGenus::Stratus, 400.0, 1.0, 5.0), Mock::modern(CloudGenus::Nimbostratus, 1000.0, 1.0, 50.0).east_of(0.0)];
        let west_of_edge = Shape { cover: 1.0, alpha: 1.0, tau: 5.0, tau_sun: 0.0, hit_m: vec2f(-100.0, 0.0), height_m: 400.0 };
        let shade = CloudGenus::Nimbostratus.default_shade_below();
        for held in [false, true] {
            let east = t_above(&edge, 0, &west_of_edge, dir_from_az_el(90.0, 30.0), held);
            assert!(east < 1.0 && (east - (1.0 - shade)).abs() < 1.0e-6, "held {held}: the sun in the east, its ray under the covered side, keeps {east}");
            let west = t_above(&edge, 0, &west_of_edge, dir_from_az_el(270.0, 30.0), held);
            assert_eq!(west, 1.0, "held {held}: the sun in the west, its ray under the clear side");
        }
        // The upper sky above 45 degrees: the closed Stratus in front, its sun term gone under the shading deck.
        let upper = |env: &Env| {
            let map = env.bake(128);
            map.data[..128 * 16].iter().map(|t| luminance(vec3f(t[0], t[1], t[2]))).sum::<f32>()
        };
        let (shaded, open) = (upper(&closed), upper(&unshaded));
        assert!(shaded < 0.8 * open, "a Stratus under a shading deck {shaded} against one under a deck that casts nothing {open}");
    }

    /// Design §2.3: a layer's ambient is the dome greyed by the share of it the layers at and above it hide, dimmed
    /// by what the layers above pass diffusely. The lower layer is flat, a Cirrostratus sheet at 5000 m under a
    /// Nimbostratus (a marched volume, lane ns) from 5200 to 8200 m or a Cirrus at 10000 m: the Stratus and the Stratocumulus are marched volumes now
    /// (the volumes design §2.2, lane sc), and a marched entry is dimmed by none yet.
    /// - Under a closed Nimbostratus the sheet is lit by the grey the slab passes, 1 - m (1 - T_d).
    /// - Under a Cirrus it keeps most of the dome, a little greyer.
    /// - The highest layer greys only its own dome and keeps its luminance.
    #[test]
    fn the_ambient_follows_the_layers_above() {
        let low = |layers: &[(CloudGenus, f32)]| {
            let mut p = stacked(layers);
            for layer in p.sky.cloud_stack.as_mut().unwrap().layers.iter_mut() {
                match layer.genus() {
                    Some(CloudGenus::Cirrostratus) => layer.cirrostratus.get_or_insert_with(Default::default).base_m = 5000.0,
                    Some(CloudGenus::Nimbostratus) => {
                        let block = layer.nimbostratus.get_or_insert_with(Default::default);
                        (block.base_m, block.tops_m) = (5200.0, 8200.0);
                    }
                    Some(CloudGenus::Cirrus) => layer.cirrus.get_or_insert_with(Default::default).base_m = 10_000.0,
                    _ => {}
                }
            }
            Env::new(&p)
        };
        let alone = low(&[(CloudGenus::Cirrostratus, 0.6)]);
        let under_ns = low(&[(CloudGenus::Cirrostratus, 0.6), (CloudGenus::Nimbostratus, 1.0)]);
        let under_ci = low(&[(CloudGenus::Cirrostratus, 0.6), (CloudGenus::Cirrus, 0.35)]);
        let light = |env: &Env, genus: CloudGenus| env.cloud_stack.as_ref().unwrap().lights()[index_of(env, genus)];
        let a = light(&alone, CloudGenus::Cirrostratus).ambient;
        let n = light(&under_ns, CloudGenus::Cirrostratus).ambient;
        let c = light(&under_ci, CloudGenus::Cirrostratus).ambient;
        let slab = light(&under_ns, CloudGenus::Nimbostratus);
        let td = diffuse_transmittance(slab.mean_tau);
        assert!(slab.dome_share > 0.95 && td < 0.2, "the premise, a closed dark slab: share {}, T_d {td}", slab.dome_share);
        let passes = 1.0 - slab.dome_share * (1.0 - td);
        assert!((luminance(n) / luminance(a) - passes).abs() < 1.0e-4, "under the slab: {} against {passes}", luminance(n) / luminance(a));
        assert!((n.z / n.x - 1.0).abs() < 0.02, "and grey: {n:?}");
        assert!(luminance(c) > 0.75 * luminance(a), "under a Cirrus most of the dome: {c:?} against {a:?}");
        assert!(c.z / c.x < a.z / a.x, "and a little greyer: {c:?} against {a:?}");
        let atmo = under_ns.atmo.as_ref().unwrap();
        let dome = atmo.ambient() + under_ns.night.as_ref().map_or(Vec3f::default(), |night| night.ambient());
        assert!((luminance(slab.ambient) / luminance(dome) - 1.0).abs() < 1.0e-4, "the highest keeps the dome's luminance");
    }
}
