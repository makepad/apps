//! The cloud volumes' light (design §2.3, revised by §12.1 items 6, 7 and 9): one law for every marched cloud type.
//!
//! - **Per ray** ([`RayLight`]). With one sun the angle between the view and the sun is fixed along a ray, so the
//!   dual Henyey-Greenstein lobes of the three multiple-scattering octaves and the powder's weight are worked out once.
//! - **Per step** ([`step`]). An energy-conserving step (Hillaire 2015) adds what the step scatters toward the eye,
//!   seen through what is in front of it, into a [`VolumeMarch`]: the sun's octaves integrated across the step, the powder, the
//!   slab family's diffuse share of the sun, the dome's light by height and depth, the ground's bounce, and the depth
//!   the light came from.
//! - **Per texel** ([`shade`]). The record keeps the forward and back lobes apart, each for the sun at the layer's base
//!   and at its top, so Silver lining, the two sun colours, Brightness, Sky light, the dome, the ground and the haze
//!   are applied afterwards in a few nanoseconds: changing them needs no march.
//!
//! The record holds scalars only (Copy, no heap), so the colours stay outside, as `CloudMarch` keeps them.
//!
//! **Segments** (design §12.1 item 9). A flat entry inside the shell cuts a ray's march into segments, and each
//! segment is its own record, begun at [`VolumeMarch::CLEAR`]: its sums are seen through its own transmittance only,
//! never through the segments in front, so [`shade`]'s opacity and haze are the segment's own. The march carries the
//! ray's transmittance in front of the segment beside it (`front`, 1 for a ray's first or only segment):
//! - it stops where [`VolumeMarch::stops`]`(front)` says so, and [`VolumeMarch::close`]s the segment it stopped in
//!   (only that one: the segments in front keep their own transmittance);
//! - the compositor adds `front x shade(segment)` and carries `front x segment.transmittance` to the next one.

use makepad_draw::*;
use crate::cloud_light::{diffuse_transmittance, ice_phase, LightFamily};
use crate::clouds::hg_phase;
use crate::volumetric::{BOUNCE_SHARE, G_BACK, G_FORWARD, MIN_TRANSMITTANCE, OCTAVES, POWDER, SUN_GAIN};

/// Each octave carries this share of the last one's energy and sees this share of its optical depth (a = b = 0.5,
/// design §2.3). The per-step exps below rest on b being one half.
const OCTAVE_ENERGY: f32 = 0.5;
// The octave terms below are written out for three octaves.
const _: () = assert!(OCTAVES == 3);
/// The powder reads the optical depth over this much cloud round a point (design §2.3), metres; the caller measures it.
pub const POWDER_REACH_M: f32 = 60.0;
/// The haze starts this far away (design §2.3): nearer clouds keep their own colour.
const HAZE_FROM_M: f32 = 8000.0;
/// The heap family's diffusion (design §2.3's multiple scattering inside a heap, Task 6a L1 of §12.7 item 5): the
/// octaves fall as exp(-tau_sun / 4) at best, so past a few mean free paths toward the sun a heap had no sun left,
/// where real cloud passes the slab law's T_d(tau) = 1 / (1 + 0.11 tau). A step adds K_MS x 4 x the third octave's
/// phase x T_d(tau_sun) x (1 - exp(-tau_sun / 4)) per unit of sun irradiance to each lobe. Tuned on the first look's
/// setup 1 (sun 48, one Cumulus at its defaults, 8K) against kloofendal_48d's cloud-to-sky ratios, with the open
/// side (OPEN_SIDE): at 0.25 its bases, shaded sides, bodies toward the sun, base-to-top and sunlit-to-shaded are
/// 0.68-1.24 of the reference's, the sunlit tops 1.47 (0.15 left the base-to-top at 0.64, 0.4 put them 1.14-1.61x
/// over). A heap deck of optical depth 50 seen from below then passes 0.29-0.37 of the sun the flat slab law's diffuse
/// share passes (`a_thick_deck_passes_the_diffuse_share` prints it): never more.
const K_MS: f32 = 0.25;
/// The heap family's open side (Task 6a L3 of design §12.7 item 5): a point on a heap's side or base faces open sky
/// along the way the eye looks in, which the column above (T_d(tau_up)) does not see. Its face turned toward the eye
/// (normal -d) sees the sky over (1 - d.y) / 2 of its hemisphere and the ground over the rest, whose bounce reaches it
/// by height already. So that share of the dome the column hides reaches it: OPEN_SIDE x (1 - d.y)
/// ([`RayLight::seen_from`]) x (1 - T_d(tau_up)) x T_d(tau_in), tau_in the view ray's own depth into the volume. A
/// level look's face takes half of it (the brief's "half the dome"), a base seen straight up none, a top seen straight
/// down all; a sunlit top (T_d(tau_up) about 1) keeps its light. Not tuned: the sky's view factor of a level face.
const OPEN_SIDE: f32 = 0.5;
/// The heap family's octaves take this gain, not the classic's SUN_GAIN (2): SUN_GAIN lifts the classic volume's octaves
/// to stand in for the many scatterings they leave out, which a heap carries as its own diffusion (K_MS), so a heap lit
/// with both took that light twice (task 6a tune round 2). Sunlit tops then read 1.53x kloofendal_48d's against the sky
/// at a sky matched to it (sun-to-sky 2.85 against 2.86), the other five light targets 1.06-1.23x and 0.79x; with gain
/// 1 every target is within 0.98-1.17x of the reference (setup 1, 8K). Its diffusion is the same at either gain.
const HEAP_SUN_GAIN: f32 = 1.0;

/// What stays the same along one view ray (design §12.1 item 6): the octaves' phases per lobe and the powder's weight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayLight {
    /// Octave k's forward lobe, per unit of sun irradiance (1/sr), with the octave's energy a^k and the family's gain
    /// folded in. The ice family's is its sharp forward lobe.
    pub forward: [f32; OCTAVES],
    /// Octave k's back lobe, the same way. The ice family's is its broad lobe.
    pub back: [f32; OCTAVES],
    /// The forward lobe's share that [`RayLight::sun_light`] mixes with. The march itself never reads it: the record
    /// keeps the lobes apart, and [`shade`] mixes them with the Silver lining it is given.
    pub lining: f32,
    /// How much the powder can darken: POWDER x 0.5 x (1 - cos), none toward the sun.
    pub powder_weight: f32,
    /// The slab family's diffuse share of the sun, per unit of T_d (design §2.3, the slab law's sun.y x T_d / pi):
    /// sun.y / pi for a slab, 0 for the others.
    pub diffuse: f32,
    /// The sun's height, sin(elevation): a point's depth toward the sun times it is the depth above it on the sun's
    /// path, which the diffuse share's T_d reads.
    pub sun_up: f32,
    /// The heap family's diffusion per lobe (forward, back), per unit of sun irradiance, T_d(tau_sun) and light built
    /// up past the skin: K_MS x 4 x the third octave's phase (K_MS). 0 for the others. [`RayLight::sun_light`] leaves
    /// it out, so it stays the classic's octaves.
    pub diffusion: [f32; 2],
    /// The heap family's open side: the share of the dome the column above hides that reaches the face the eye sees,
    /// a level look's (OPEN_SIDE) until [`RayLight::seen_from`] sets the view; 0 for the others.
    pub open_side: f32,
    /// The slab family's diffuse share through the column over the point (T_d of the dome's depth, `step`'s tau_up)
    /// instead of along the sun's path (T_d of tau_sun x sun_up): a broken deck's (a Stratocumulus'), whose sun path
    /// crosses cloud far from the point while its diffuse light comes down through the cloud over it, so its thin
    /// rims are bright and its thick middles grey. The same in a uniform slab. False from [`RayLight::new`].
    pub diffuse_through_column: bool,
}

impl RayLight {
    /// The constants of a ray whose direction meets the sun's at `cos_view_sun`, under a sun `sun_up` (sin(elevation))
    /// high.
    /// - The heap and slab families take the volume's dual lobe (G_FORWARD, G_BACK), each octave halving the
    ///   anisotropy, as `volumetric::sun_light` lights the classic volume: the slab family times SUN_GAIN, the heap
    ///   family times HEAP_SUN_GAIN (its diffusion carries the light SUN_GAIN stands in for).
    /// - The ice family takes the ice phase (`cloud_light::ice_phase`, its two lobes apart) at every octave. Its
    ///   energies are normalised to one, so an unshadowed ice point is lit as the ice sheet's law lights it (before the
    ///   powder).
    /// - The slab family also takes the slab law's diffuse share of the sun, sun.y x T_d / pi, so a deck's base is lit
    ///   by the sun that diffuses through it as a flat deck's is, where the octaves have died out (design §2.3: a
    ///   Nimbostratus base is grey, not black). It goes to both lobes alike, so Silver lining leaves it as it is.
    pub fn new(cos_view_sun: f32, sun_up: f32, silver_lining: f32, family: LightFamily) -> RayLight {
        debug_assert!(family != LightFamily::Classic, "a classic entry is marched by its older layer (volumetric.rs), not here");
        let (mut forward, mut back) = ([0.0f32; OCTAVES], [0.0f32; OCTAVES]);
        let mut energy = 1.0f32;
        match family {
            LightFamily::Ice => {
                let total: f32 = (0..OCTAVES).map(|k| OCTAVE_ENERGY.powi(k as i32)).sum();
                let (sharp, broad) = (ice_phase(cos_view_sun, 1.0), ice_phase(cos_view_sun, 0.0));
                for k in 0..OCTAVES {
                    forward[k] = energy / total * sharp;
                    back[k] = energy / total * broad;
                    energy *= OCTAVE_ENERGY;
                }
            }
            LightFamily::Heap | LightFamily::Slab | LightFamily::Classic => {
                let gain = if family == LightFamily::Heap { HEAP_SUN_GAIN } else { SUN_GAIN };
                let mut round = 1.0f32;
                for k in 0..OCTAVES {
                    forward[k] = gain * energy * hg_phase(cos_view_sun, G_FORWARD * round);
                    back[k] = gain * energy * hg_phase(cos_view_sun, G_BACK * round);
                    energy *= OCTAVE_ENERGY;
                    round *= 0.5;
                }
            }
        }
        let diffuse = if family == LightFamily::Slab { sun_up.max(0.0) / std::f32::consts::PI } else { 0.0 };
        let (diffusion, open_side) = if family == LightFamily::Heap {
            // The third octave's lobes without its energy and gain: 4 x its phase, about 1 / pi, tilted forward.
            let third = 4.0 * K_MS / (HEAP_SUN_GAIN * OCTAVE_ENERGY * OCTAVE_ENERGY);
            ([third * forward[OCTAVES - 1], third * back[OCTAVES - 1]], OPEN_SIDE)
        } else {
            ([0.0; 2], 0.0)
        };
        RayLight { forward, back, lining: silver_lining, powder_weight: POWDER * 0.5 * (1.0 - cos_view_sun), diffuse, sun_up, diffusion, open_side, diffuse_through_column: false }
    }

    /// The same ray looked along a view `view_up` (its direction's y) high: a heap's open side is the sky its face
    /// toward the eye sees, (1 - view_up) of a level look's (OPEN_SIDE), so a base seen straight up takes none of it
    /// (that side is the ground, whose bounce reaches it by height) and a top seen straight down all the dome hidden.
    pub fn seen_from(self, view_up: f32) -> RayLight {
        RayLight { open_side: self.open_side * (1.0 - view_up).clamp(0.0, 2.0), ..self }
    }

    /// The sun's light at a point behind `tau_sun` toward the sun, per unit of sun irradiance, with the lobes mixed by
    /// this ray's Silver lining: what one step of [`step`] adds per unit of scattered light, before the colours.
    pub fn sun_light(&self, tau_sun: f32, tau_local: f32) -> f32 {
        let (e4, e2, e) = octave_terms(tau_sun);
        let forward = self.forward[0] * e4 + self.forward[1] * e2 + self.forward[2] * e;
        let back = self.back[0] * e4 + self.back[1] * e2 + self.back[2] * e;
        (self.lining * forward + (1.0 - self.lining) * back) * self.powder(tau_local) + self.diffuse_share(tau_sun)
    }

    /// The slab family's diffuse share of the sun at a point behind `tau_sun` toward the sun: sun.y x T_d / pi of the
    /// depth above it on the sun's path (0 for the other families). It takes no powder.
    #[inline]
    fn diffuse_share(&self, tau_sun: f32) -> f32 {
        if self.diffuse > 0.0 {
            self.diffuse * diffuse_transmittance(tau_sun * self.sun_up)
        } else {
            0.0
        }
    }

    /// The powder (design §2.3): mix(1, 1 - exp(-2 tau_local), weight). Light that has entered little cloud has not
    /// yet scattered toward the eye, so thin rims and crevices seen from the sun's side are darker.
    #[inline]
    fn powder(&self, tau_local: f32) -> f32 {
        1.0 - self.powder_weight * (-2.0 * tau_local).exp()
    }
}

/// The three octaves' Beer-Lambert terms behind `tau` from one exp (design §12.1 item 6). The sun term of design
/// §2.3 is sum over k = 0..2 of a^k phase_k exp(-b^k tau); with b = 0.5 the exponents are tau, tau / 2 and tau / 4,
/// so with e = exp(-tau / 4): exp(-tau) = e^4, exp(-tau / 2) = e^2 and exp(-tau / 4) = e. The a^k sit in the phases.
#[inline]
fn octave_terms(tau: f32) -> (f32, f32, f32) {
    let e = (-0.25 * tau).exp();
    let e2 = e * e;
    (e2 * e2, e2, e)
}

/// What one octave sends toward the eye over a step, per unit of the transmittance in front of it (design §2.3, the
/// step's integral taken with the sun's depth varying across it). With x the share of the step walked, the step's
/// optical depth `a` along the view and the octave's sun depth growing by `q` across it (its share of the change in
/// tau_sun), the step scatters the integral over x of a e^(-a x) e^(-q x), times the octave's term at the step's start:
/// `start` (1 - e^(-(a + q))) a / (a + q). Since `start` e^(-q) is the octave's term at the step's end (`end`) and
/// e^(-a) the step's own transmittance (`view`), that is (`start` - `view` `end`) a / (a + q). With q = 0 it is
/// `start` (1 - e^(-a)), a constant source.
#[inline]
fn octave_step(a: f32, q: f32, start: f32, end: f32, view: f32) -> f32 {
    let u = a + q;
    if u.abs() < 1.0e-3 {
        // Where the sun's depth falls as fast as the view's rises (a backlit step), or a step with next to no
        // cloud: (1 - e^(-u)) / u by its series.
        a * start * (1.0 - u * (0.5 - u / 6.0))
    } else {
        a * (start - view * end) / u
    }
}

/// The light law's numbers the GPU export takes (design §12.3): `k_light` (G_FORWARD, G_BACK, HEAP_SUN_GAIN, K_MS) and
/// `k_light2` (OPEN_SIDE, POWDER, POWDER_REACH_M, TD_K); the shader's ray constants mirror `RayLight::new` for the heap
/// family (OCTAVE_ENERGY one half, OCTAVES three) and its step mirrors [`step`].
pub(crate) fn gpu_layout(out: &mut crate::cloud_volume_gpu::UploadBuilder) {
    const _: () = assert!(OCTAVE_ENERGY == 0.5, "the shader's octaves halve their energy");
    out.uniform("k_light", [G_FORWARD, G_BACK, HEAP_SUN_GAIN, K_MS]);
    out.uniform("k_light2", [OPEN_SIDE, crate::volumetric::POWDER, POWDER_REACH_M, crate::cloud_light::TD_K]);
}

/// `k_light` for a type whose forward lobe is its own (written after [`gpu_layout`], it replaces the law's for that
/// volume alone): the law's numbers with `g_forward` in place of G_FORWARD.
pub(crate) fn gpu_forward_lobe(out: &mut crate::cloud_volume_gpu::UploadBuilder, g_forward: f32) {
    out.uniform("k_light", [g_forward, G_BACK, HEAP_SUN_GAIN, K_MS]);
}

/// What a march gathered along a ray or a segment of it (design §12.1 item 9), in scalars, so the colours and the
/// light rows apply afterwards in [`shade`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeMarch {
    /// What still passes: 1 before any cloud.
    pub transmittance: f32,
    /// The forward lobe's octave sums seen by the eye, weighted toward the base's sun (1 - h01) and the top's (h01).
    pub forward_low: f32,
    pub forward_high: f32,
    /// The back lobe's, the same way.
    pub back_low: f32,
    pub back_high: f32,
    /// The dome's light by height and through the cloud above, per unit of the dome.
    pub ambient: f32,
    /// A heap's open side (OPEN_SIDE), per unit of the dome; [`shade`] colours it as the sky beside the face.
    pub open: f32,
    /// The ground's bounce by height, per unit of the ground's light (BOUNCE_SHARE is applied in [`shade`]).
    pub bounce: f32,
    /// Sum over the steps of each step's opacity seen through what is in front (w) times its distance, and of w: the
    /// depth the light came from, for the haze.
    pub depth_weighted: f32,
    pub weight: f32,
}

impl VolumeMarch {
    /// Before any cloud.
    pub const CLEAR: VolumeMarch = VolumeMarch { transmittance: 1.0, forward_low: 0.0, forward_high: 0.0, back_low: 0.0, back_high: 0.0, ambient: 0.0, open: 0.0, bounce: 0.0, depth_weighted: 0.0, weight: 0.0 };

    /// The march stops here (design §12.1 item 7): the ray's transmittance, `front` (what the segments in front of
    /// this one leave; 1 for the first) times this segment's, passes too little to see.
    pub fn stops(&self, front: f32) -> bool {
        front * self.transmittance <= MIN_TRANSMITTANCE
    }

    /// The march stopped in this segment: the little left counts as opaque, so a thick cloud has no faint see-through
    /// (as the classic volume). Only the segment the ray stopped in is closed.
    pub fn close(&mut self) {
        self.transmittance = 0.0;
    }

    /// Its opacity, from its own transmittance (0 once closed).
    pub fn alpha(&self) -> f32 {
        1.0 - self.transmittance
    }

    /// The mean distance its light came from, metres; 0 when nothing was met.
    pub fn mean_depth_m(&self) -> f32 {
        if self.weight > 0.0 {
            self.depth_weighted / self.weight
        } else {
            0.0
        }
    }
}

/// One energy-conserving step of the light law (design §2.3, §12.1 item 6) into `m`.
/// - `sigma_t` the extinction (1/m) at the step's point and `ds` its length (m). Cloud scatters what it takes out
///   (albedo 1, sigma_s = sigma_t), so the light the step sends toward the eye is S x (1 - exp(-sigma_t ds)) / sigma_t
///   times sigma_s, that is S x (1 - exp(-sigma_t ds)), seen through the transmittance in front of it: thick steps
///   neither gain nor lose energy.
/// - `tau_sun` the optical depth toward the sun at the step's point and `dtau_sun` its rate along the view (1/m: the
///   change from the last sample over the distance between them; 0 treats it as the same across the step).
///   `tau_local` the depth over POWDER_REACH_M round the point (the powder); `tau_up` the depth straight up (the
///   dome's light); `h01` the height through the layer (0 base, 1 top); `depth_m` the distance from the eye; `front`
///   the volume's transmittance in front of the segment (1 for a ray's first or only one), which the open side reads.
/// - The sun is integrated across the step with tau_sun linear through the point ([`octave_step`]), never below 0 at
///   either end. On a sunlit face under a low sun the sun's term falls up to 1 / sin(elevation) times faster than the
///   view's transmittance, so one sample a step at a mean free path would read the sunlit skin a third too dark at
///   9 degrees; integrated, a uniform slab is exact at any step.
/// - The sun's three octaves cost one exp at each end of the step: the octave terms are e^4, e^2 and e of
///   e = exp(-tau_sun / 4).
/// - The slab family adds its diffuse share of the sun ([`RayLight::new`]) at the point, to both lobes alike.
/// - The heap family adds its diffusion (K_MS) to each lobe: its lobe's weight x T_d(tau_sun) across the step x the
///   step's light past the skin, the step's opacity less the third octave's integral across it (`w` x (1 - e)
///   integrated, e = exp(-tau_sun / 4)), so the skin takes none and the third octave's own cost (no exp) pays for it.
///   T_d is the mean of its values at the step's start, middle and end, weighted as that light falls across the step
///   (Simpson's weights times e^(-a x) (1 - e)): taken at the point alone, a step a mean free path long through a slab
///   under a 9 degree sun was off by up to 4 % against two half steps (now 1.1 %); one sqrt and two divisions a step.
/// - The dome reaches the point by the slab law's diffuse transmittance T_d(tau_up) = 1 / (1 + 0.11 tau_up)
///   (`cloud_light::diffuse_transmittance`), so a thick deck's base passes T_d of the dome, grey and not black, as
///   design §2.3 asks of a deck of optical depth 50 (the exp(-0.25 tau_up) written there passes 5e-6 of it at that
///   depth, not 0.15). AMBIENT_FLOOR's mix by height, the classic's stand-in for the cloud above a low point, is not
///   taken as well: T_d(tau_up) is that cloud (design §12.6 item 1, revised by Task 6a L3). A heap adds its open side
///   (OPEN_SIDE), at the cost of one ln a step. The ground's bounce reaches it by 1 - h01.
#[inline]
pub fn step(m: &mut VolumeMarch, light: &RayLight, sigma_t: f32, ds: f32, tau_sun: f32, dtau_sun: f32, tau_up: f32, tau_local: f32, h01: f32, depth_m: f32, front: f32) {
    // A depth toward the sun is never negative: below 0 the octave terms would read exp(+tau).
    let tau_sun = tau_sun.max(0.0);
    let a = sigma_t * ds;
    let step_transmittance = (-a).exp();
    let w = m.transmittance * (1.0 - step_transmittance);
    // tau_sun across the step, linear through the point and never below 0 at either end.
    let reach = 2.0 * tau_sun;
    let change = (dtau_sun * ds).clamp(-reach, reach);
    let (s4, s2, s1) = octave_terms(tau_sun - 0.5 * change);
    let (e4, e2, e1) = octave_terms(tau_sun + 0.5 * change);
    let octaves = [
        octave_step(a, change, s4, e4, step_transmittance),
        octave_step(a, 0.5 * change, s2, e2, step_transmittance),
        octave_step(a, 0.25 * change, s1, e1, step_transmittance),
    ];
    let lit = m.transmittance * light.powder(tau_local);
    let forward = (light.forward[0] * octaves[0] + light.forward[1] * octaves[1] + light.forward[2] * octaves[2]) * lit;
    let back = (light.back[0] * octaves[0] + light.back[1] * octaves[1] + light.back[2] * octaves[2]) * lit;
    let diffuse = w * if light.diffuse_through_column { light.diffuse * diffuse_transmittance(tau_up) } else { light.diffuse_share(tau_sun) };
    // The heap's diffusion: the step's opacity seen through what is in front, times 1 - e of the third octave
    // integrated across the step (none at the skin), times T_d(tau_sun) across the step, weighted as that light falls
    // across it (its integrand e^(-a x) (1 - e) at the step's start, middle and end, by Simpson's weights).
    let half = step_transmittance.sqrt();
    let weights = [1.0 - s1, 4.0 * half * (1.0 - (s1 * e1).sqrt()), step_transmittance * (1.0 - e1)];
    let total = weights[0] + weights[1] + weights[2];
    let across = if total > 0.0 {
        (weights[0] * diffuse_transmittance(tau_sun - 0.5 * change) + weights[1] * diffuse_transmittance(tau_sun) + weights[2] * diffuse_transmittance(tau_sun + 0.5 * change)) / total
    } else {
        diffuse_transmittance(tau_sun)
    };
    let built = (m.transmittance * ((1.0 - step_transmittance) - octaves[2])).max(0.0) * across;
    let (forward, back) = (forward + diffuse + light.diffusion[0] * built, back + diffuse + light.diffusion[1] * built);
    let (low, high) = (1.0 - h01, h01);
    m.forward_low += forward * low;
    m.forward_high += forward * high;
    m.back_low += back * low;
    m.back_high += back * high;
    let above = diffuse_transmittance(tau_up);
    let open = if light.open_side > 0.0 {
        // The view ray's own depth into the volume at the step's middle, through the segments in front too, so a march
        // cut into segments takes the uncut one's light.
        light.open_side * (1.0 - above) * diffuse_transmittance(0.5 * a - (front * m.transmittance).ln())
    } else {
        0.0
    };
    m.ambient += w * above;
    m.open += w * open;
    m.bounce += w * low;
    m.depth_weighted += w * depth_m;
    m.weight += w;
    m.transmittance *= step_transmittance;
}

/// What [`shade`] lights a record with: the layer's rows and the light pass's colours for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadeInputs {
    /// Silver lining: the forward lobe's share.
    pub lining: f32,
    /// The sun at the layer's base and at its top (the light pass's sun table), already dimmed by what is above.
    pub sun_base: Vec3f,
    pub sun_top: Vec3f,
    pub brightness: f32,
    pub sky_light: f32,
    /// The dome as it reaches the layer (`EntryLight::ambient`), the dome's light on the ground under it
    /// (`EntryLight::ground`) and the sun's (`EntryLight::ground_sun`); `side` is the dome as the open sky beside the
    /// face seen shows it (`EntryLight::side_seen_along`), which a heap's open side takes.
    pub ambient: Vec3f,
    pub side: Vec3f,
    pub ground: Vec3f,
    pub ground_sun: Vec3f,
    /// The clear sky behind the cloud along the ray, which the haze mixes toward.
    pub sky: Vec3f,
    /// The stack's visibility, metres (60 km / Haze).
    pub visibility_m: f32,
}

/// A record's radiance (design §2.3, §12.1 item 9), premultiplied by its opacity: add it to `behind x (1 - alpha)`.
/// - The lobes are mixed by Silver lining here, and each sun colour takes its own sums.
/// - The dome and its bounce are scaled by Sky light, everything by Brightness; the sun's bounce follows the sun
///   (design §12.8 item 6).
/// - One haze for the record, at its mean depth: past HAZE_FROM_M it mixes toward the sky behind by
///   1 - exp(-(t - HAZE_FROM_M) / V), so distant clouds turn blue-grey (it starts at nothing, so there is no seam).
pub fn shade(m: &VolumeMarch, s: &ShadeInputs) -> Vec3f {
    let alpha = m.alpha();
    if !(alpha > 0.0) {
        return Vec3f::default();
    }
    let low = s.lining * m.forward_low + (1.0 - s.lining) * m.back_low;
    let high = s.lining * m.forward_high + (1.0 - s.lining) * m.back_high;
    let lit = (s.sun_base * low + s.sun_top * high + (s.ambient * m.ambient + s.side * m.open + s.ground * (BOUNCE_SHARE * m.bounce)) * s.sky_light + s.ground_sun * (BOUNCE_SHARE * m.bounce)) * s.brightness;
    let haze = haze_share(m.mean_depth_m(), s.visibility_m);
    lit * (1.0 - haze) + s.sky * (alpha * haze)
}

/// The share of a cloud's light at `depth_m` that the air in front turns to the sky behind (design §2.3); a flat entry
/// at the same distance takes the same share, so the two meet without a seam.
pub(crate) fn haze_share(depth_m: f32, visibility_m: f32) -> f32 {
    let beyond = depth_m - HAZE_FROM_M;
    if !(beyond > 0.0) || !(visibility_m > 0.0) {
        return 0.0;
    }
    1.0 - (-beyond / visibility_m).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_light::{radiance, EntryLight, LightRows, Shape, SIDE_AZIMUTHS};
    use crate::volumetric::FORWARD_SHARE;

    /// A uniform horizontal slab, BASE_M up and SLAB_M deep, marched straight through.
    const BASE_M: f32 = 1000.0;
    const SLAB_M: f32 = 1000.0;

    /// The view through the slab: from below looking up (the sun beyond the slab: backlit) or from above looking
    /// down onto its sunlit top (the sun behind the eye: sunlit).
    #[derive(Clone, Copy, Debug)]
    enum View {
        Backlit,
        Sunlit,
    }

    /// One march of a slab of optical depth `tau` with the sun `elevation_deg` up, in `n` equal steps sampled at their
    /// middles, stopping as the march stops, with the sun's octaves at Silver lining `lining`. `far_m` moves the slab
    /// away (the haze), and `nothing_above` lights it as if no cloud stood over any point (tau_up 0). `close_at`, when
    /// given, replaces the stop at T 0.01: the march ends once the transmittance falls under it, with a closing step
    /// that sends the rest at that step's light (an infinite slab behind); at 0 it marches to the end.
    fn slab(tau: f32, n: usize, view: View, elevation_deg: f32, lining: f32, far_m: f32, nothing_above: bool, close_at: Option<f32>) -> VolumeMarch {
        march(&Slab::new(LightFamily::Heap, tau, n, view, elevation_deg, lining, far_m, nothing_above), n, close_at)
    }

    /// The march of [`slab`] over `slab`'s `n` steps.
    fn march(slab: &Slab, n: usize, close_at: Option<f32>) -> VolumeMarch {
        let mut m = VolumeMarch::CLEAR;
        for i in 0..n {
            let stopped = match close_at {
                None => m.stops(1.0),
                Some(c) => m.transmittance < c,
            };
            if stopped {
                break;
            }
            slab.step(&mut m, i, slab.ds, 1.0);
            if let Some(c) = close_at {
                if c > 0.0 && m.transmittance < c {
                    // The closing term: one step that takes out all that is left at this step's light.
                    slab.step(&mut m, i, f32::MAX, 1.0);
                }
            }
        }
        if close_at.is_none() && m.stops(1.0) {
            m.close();
        }
        m
    }

    /// The steps of a [`slab`] march.
    struct Slab {
        sigma: f32,
        sin_e: f32,
        light: RayLight,
        ds: f32,
        /// tau_sun's rate along the view: the sun depth grows going down into a sunlit top and falls going up toward a
        /// backlit one.
        rate: f32,
        view: View,
        far_m: f32,
        nothing_above: bool,
    }

    impl Slab {
        fn new(family: LightFamily, tau: f32, n: usize, view: View, elevation_deg: f32, lining: f32, far_m: f32, nothing_above: bool) -> Slab {
            let sin_e = elevation_deg.to_radians().sin();
            let cos = match view {
                View::Backlit => sin_e,
                View::Sunlit => -sin_e,
            };
            let sigma = tau / SLAB_M;
            let rate = match view {
                View::Backlit => -sigma / sin_e,
                View::Sunlit => sigma / sin_e,
            };
            // Seen straight up from below (backlit) or straight down onto its top (sunlit).
            let view_up = match view {
                View::Backlit => 1.0,
                View::Sunlit => -1.0,
            };
            Slab { sigma, sin_e, light: RayLight::new(cos, sin_e, lining, family).seen_from(view_up), ds: SLAB_M / n as f32, rate, view, far_m, nothing_above }
        }

        /// Step `i` of the march, sampled at its middle, `ds` long (the closing step takes all that is left at the
        /// point's sun depth).
        fn step(&self, m: &mut VolumeMarch, i: usize, ds: f32, front: f32) {
            let s = (i as f32 + 0.5) * self.ds;
            let height = match self.view {
                View::Backlit => s,
                View::Sunlit => SLAB_M - s,
            };
            let sigma = self.sigma;
            let tau_up = if self.nothing_above { 0.0 } else { sigma * (SLAB_M - height) };
            let tau_sun = sigma * (SLAB_M - height) / self.sin_e;
            let rate = if ds < f32::MAX { self.rate } else { 0.0 };
            step(m, &self.light, sigma, ds, tau_sun, rate, tau_up, sigma * POWDER_REACH_M, height / SLAB_M, self.far_m + BASE_M + s, front);
        }
    }

    /// The sun that one step of `light` sends toward the eye at Silver lining `lining`, per unit of the transmittance
    /// in front and before the powder, worked out apart from [`step`]: each octave's integral over a step of optical
    /// depth `a` with its sun depth linear from tau_mid - change / 2 to tau_mid + change / 2, by exp_m1.
    fn exact_sun_step(light: &RayLight, lining: f32, a: f32, tau_mid: f32, change: f32) -> f32 {
        let (mut sum, mut c) = (0.0f32, 1.0f32);
        for k in 0..OCTAVES {
            let u = a + c * change;
            let share = if u.abs() > 1.0e-6 { -(-u).exp_m1() / u } else { 1.0 };
            sum += (lining * light.forward[k] + (1.0 - lining) * light.back[k]) * (-c * (tau_mid - 0.5 * change)).exp() * a * share;
            c *= 0.5;
        }
        sum
    }

    /// Its light with white suns, a white dome and a white ground at Silver lining `lining`: (sun, dome, bounce).
    fn channels(m: &VolumeMarch, lining: f32) -> (f32, f32, f32) {
        (lining * (m.forward_low + m.forward_high) + (1.0 - lining) * (m.back_low + m.back_high), m.ambient, m.bounce)
    }

    /// Design §2.3: energy-conserving steps, so a slab of optical depth 1, 5 and 20 marched in N and 2N steps keeps
    /// its light within 3 % and its opacity within 1e-3, sunlit and backlit, the sun 9, 30 and 60 degrees up. Grown
    /// with: the lobes kept apart (design §12.1 item 9), so a record shaded at another Silver lining equals a march
    /// lit at that Silver lining from the start, within 1e-5; a march cut into two segments composites to the uncut
    /// one within 1e-5; and the closing term at T 0.05 (design §12.1 item 7),
    /// measured against a march to the end, beside the stop at T 0.01, and printed.
    ///
    /// Grown with known answers, so a law that only agrees with itself fails:
    /// - the opacity of the slab of optical depth 1 (no stop) is 1 - e^-1 within 1e-5;
    /// - a step handed a sun depth below 0 is the step at 0, bit for bit;
    /// - one heap step's dome (T_d(tau_up) and the open side) and diffusion, and a slab step's, by hand within 1e-5;
    /// - the N-step march's sun (the heap's diffusion by Simpson's rule), dome and bounce equal the slab's closed form
    ///   (the integrals to where the march stops) within 0.1 %, 0.1 % and 1 % (a step that is not energy-conserving
    ///   is off by half a step's optical depth, 0.4 % at the thinnest);
    /// - a ray's octaves at the default Silver lining, the powder off, equal the classic volume's `sun_light` at the
    ///   heap's gain (HEAP_SUN_GAIN / SUN_GAIN of it) within 1e-6 at optical depth 0, 1 and 5, toward the sun and away
    ///   from it; and the powder darkens by its weight.
    #[test]
    fn volume_steps_conserve_energy() {
        // The steps' count: each step through the thickest slab is 0.16 of its optical depth along the view.
        const N: usize = 128;
        for tau in [1.0f32, 5.0, 20.0] {
            for view in [View::Backlit, View::Sunlit] {
                for elevation in [9.0f32, 30.0, 60.0] {
                    let coarse = slab(tau, N, view, elevation, FORWARD_SHARE, 0.0, false, None);
                    let fine = slab(tau, 2 * N, view, elevation, FORWARD_SHARE, 0.0, false, None);
                    assert!((coarse.alpha() - fine.alpha()).abs() <= 1.0e-3, "tau {tau}, {view:?}, sun {elevation}: alpha {} against {}", coarse.alpha(), fine.alpha());
                    let (a, b) = (channels(&coarse, FORWARD_SHARE), channels(&fine, FORWARD_SHARE));
                    for (name, x, y) in [("sun", a.0, b.0), ("dome", a.1, b.1), ("bounce", a.2, b.2)] {
                        // Light under 1e-4 of the sun's or the dome's is not seen (a backlit thick slab's sun).
                        assert!((x - y).abs() <= 0.03 * y.abs() + 1.0e-4, "tau {tau}, {view:?}, sun {elevation}: {name} {x} in {N} steps against {y} in {}", 2 * N);
                    }
                    // The march's own steps in cloud are about a mean free path (design §12.1 item 3): the same
                    // comparison at tau steps against 2 tau. The sun, integrated across each step, holds 3 % there
                    // too, a low sun over a sunlit top included (its term falls 6.4 times faster than the view's
                    // transmittance at 9 degrees); the dome and the bounce are printed.
                    let n = tau.ceil() as usize;
                    let (c, f) = (channels(&slab(tau, n, view, elevation, FORWARD_SHARE, 0.0, false, None), FORWARD_SHARE), channels(&slab(tau, 2 * n, view, elevation, FORWARD_SHARE, 0.0, false, None), FORWARD_SHARE));
                    assert!((c.0 - f.0).abs() <= 0.03 * f.0.abs() + 1.0e-4, "tau {tau}, {view:?}, sun {elevation}: sun {} in {n} steps against {} in {}", c.0, f.0, 2 * n);
                    let off = |x: f32, y: f32| if y.abs() > 1.0e-4 { format!("{:+.1} %", 100.0 * (x - y) / y) } else { format!("{x:.1e} against {y:.1e}") };
                    println!(
                        "tau {tau}, {view:?}, sun {elevation}: {N} against {} steps: sun {}, dome {}, bounce {}; {n} against {} steps (a mean free path): sun {}, dome {}, bounce {}",
                        2 * N,
                        off(a.0, b.0),
                        off(a.1, b.1),
                        off(a.2, b.2),
                        2 * n,
                        off(c.0, f.0),
                        off(c.1, f.1),
                        off(c.2, f.2)
                    );
                }
            }
        }
        // Known answers. The opacity of the thinnest slab, which the march never stops in.
        for view in [View::Backlit, View::Sunlit] {
            let thin = slab(1.0, N, view, 30.0, FORWARD_SHARE, 0.0, false, None);
            assert!((thin.alpha() - (1.0 - (-1.0f32).exp())).abs() <= 1.0e-5, "{view:?}: alpha {} against 1 - e^-1", thin.alpha());
        }
        // A depth toward the sun handed in below 0 (an extrapolated rate's) lights the step as a depth of 0 does, bit
        // for bit: never by exp(+tau).
        let light = RayLight::new(0.5, 0.15, FORWARD_SHARE, LightFamily::Heap);
        for rate in [-0.2f32, 0.0, 0.05] {
            let (mut below, mut zero) = (VolumeMarch::CLEAR, VolumeMarch::CLEAR);
            step(&mut below, &light, 0.06, 16.0, -6.4, rate, 1.0, 1.0, 0.5, 2000.0, 1.0);
            step(&mut zero, &light, 0.06, 16.0, 0.0, rate, 1.0, 1.0, 0.5, 2000.0, 1.0);
            assert_eq!(below, zero, "a step at sun depth -6.4 (rate {rate}) against one at 0");
        }
        // One step's dome and diffusion by hand: a heap's step 0.5 deep at T 0.4 in front, 30 under the cloud above and
        // 12 from the sun, takes the dome T_d(30) plus OPEN_SIDE of the 1 - T_d(30) it hides through T_d of the view's
        // own depth at the step's middle (0.25 - ln 0.4), and to each lobe K_MS x 4 x the third octave's phase x T_d(12)
        // x (1 - e^-3) of its light; a slab's step takes T_d(30) and no diffusion. The open side is the sky's view factor
        // of the face the eye sees, (1 - view y) / 2 of what the column hides: none looking straight up, half level.
        let td = |tau: f32| 1.0 / (1.0 + 0.11 * tau);
        for family in [LightFamily::Heap, LightFamily::Slab] {
            let light = RayLight::new(0.6, 0.7, FORWARD_SHARE, family);
            let open = if family == LightFamily::Heap { OPEN_SIDE } else { 0.0 };
            assert_eq!(light.open_side, open);
            for (view_up, sky) in [(1.0f32, 0.0f32), (0.5, 0.25), (0.0, 0.5), (-1.0, 1.0)] {
                assert_eq!(light.seen_from(view_up).open_side, if family == LightFamily::Heap { sky } else { 0.0 }, "{family:?} seen {view_up} up");
            }
            let (mut m, mut bare) = (VolumeMarch { transmittance: 0.4, ..VolumeMarch::CLEAR }, VolumeMarch { transmittance: 0.4, ..VolumeMarch::CLEAR });
            step(&mut m, &light, 0.05, 10.0, 12.0, 0.0, 30.0, 1.0, 0.5, 2000.0, 1.0);
            step(&mut bare, &RayLight { diffusion: [0.0; 2], ..light }, 0.05, 10.0, 12.0, 0.0, 30.0, 1.0, 0.5, 2000.0, 1.0);
            let w = 0.4 * (1.0 - (-0.5f32).exp());
            let (dome, side) = (w * td(30.0), w * open * (1.0 - td(30.0)) * td(0.25 - 0.4f32.ln()));
            assert!((m.ambient - dome).abs() <= 1.0e-6 * dome, "{family:?}: the step's dome {} against {dome}", m.ambient);
            assert!((m.open - side).abs() <= 1.0e-6 * dome, "{family:?}: the step's open side {} against {side}", m.open);
            for (k, (got, plain)) in [(m.forward_low + m.forward_high, bare.forward_low + bare.forward_high), (m.back_low + m.back_high, bare.back_low + bare.back_high)].into_iter().enumerate() {
                let phase = if family == LightFamily::Heap { K_MS * 4.0 * hg_phase(0.6, [G_FORWARD, G_BACK][k] * 0.25) } else { 0.0 };
                assert!((light.diffusion[k] - phase).abs() <= 1.0e-6 * phase.max(1.0e-3), "{family:?}: lobe {k}'s diffusion {} against {phase}", light.diffusion[k]);
                let want = phase * w * (1.0 - (-3.0f32).exp()) * td(12.0);
                assert!((got - plain - want).abs() <= 1.0e-5 * want.max(1.0e-3), "{family:?}: lobe {k} gains {} against {want}", got - plain);
            }
        }
        // The closed form of a uniform slab, nothing above (T_d 1), to where the march stops: with x the distance
        // into the slab, w(x) = sigma e^(-sigma x), tau_sun = alpha + beta x and h01 = h0 + h1 x linear in x.
        for tau in [1.0f32, 5.0, 20.0] {
            for view in [View::Backlit, View::Sunlit] {
                for elevation in [9.0f32, 30.0, 60.0] {
                    let m = slab(tau, N, view, elevation, FORWARD_SHARE, 0.0, true, None);
                    let got = channels(&m, FORWARD_SHARE);
                    let (sigma, slab_m) = (tau as f64 / SLAB_M as f64, SLAB_M as f64);
                    let sin_e = (elevation as f64).to_radians().sin();
                    let (alpha, beta, h0, h1, cos) = match view {
                        View::Backlit => (sigma * slab_m / sin_e, -sigma / sin_e, 0.0, 1.0 / slab_m, sin_e),
                        View::Sunlit => (0.0, sigma / sin_e, 1.0, -1.0 / slab_m, -sin_e),
                    };
                    // The march stops after the first step that leaves T at or under the stop.
                    let a = sigma * slab_m / N as f64;
                    let steps = ((1.0 / MIN_TRANSMITTANCE as f64).ln() / a).ceil().min(N as f64);
                    let reach = steps * a / sigma;
                    // The integral of sigma e^(-r x) over 0..reach, and of sigma x e^(-sigma x).
                    let falls = |r: f64| if r.abs() > 1.0e-12 { sigma * -(-r * reach).exp_m1() / r } else { sigma * reach };
                    let (all, first_moment) = (falls(sigma), (1.0 - (-sigma * reach).exp() * (1.0 + sigma * reach)) / sigma);
                    let light = RayLight::new(cos as f32, sin_e as f32, FORWARD_SHARE, LightFamily::Heap);
                    let powder = light.powder(tau * POWDER_REACH_M / SLAB_M) as f64;
                    let mut sun = 0.0f64;
                    let mut c = 1.0f64;
                    for k in 0..OCTAVES {
                        let phase = FORWARD_SHARE as f64 * light.forward[k] as f64 + (1.0 - FORWARD_SHARE as f64) * light.back[k] as f64;
                        sun += phase * powder * (-c * alpha).exp() * falls(sigma + c * beta);
                        c *= 0.5;
                    }
                    // The heap's diffusion: the integral of w(x) x T_d(tau_sun) x (1 - e^(-tau_sun / 4)) times its lobes'
                    // mix, by Simpson's rule over 20000 intervals (T_d has no closed form against the exponentials).
                    let diffusion = FORWARD_SHARE as f64 * light.diffusion[0] as f64 + (1.0 - FORWARD_SHARE as f64) * light.diffusion[1] as f64;
                    let built = |x: f64| {
                        let t = alpha + beta * x;
                        sigma * (-sigma * x).exp() * (1.0 - (-0.25 * t).exp()) / (1.0 + 0.11 * t)
                    };
                    let intervals = 20_000;
                    let h = reach / intervals as f64;
                    let simpson: f64 = (0..=intervals).map(|i| built(i as f64 * h) * if i == 0 || i == intervals { 1.0 } else if i % 2 == 1 { 4.0 } else { 2.0 }).sum::<f64>() * h / 3.0;
                    sun += diffusion * simpson;
                    // Nothing above: T_d(tau_up) is 1 and the open side, a share of 1 - T_d(tau_up), is 0.
                    let dome = all;
                    let bounce = (1.0 - h0) * all - h1 * first_moment;
                    let off = |x: f32, y: f64| (x as f64 - y) / y;
                    println!("tau {tau}, {view:?}, sun {elevation}: {N} steps against the closed form: sun {:+.3} %, dome {:+.3} %, bounce {:+.3} %", 100.0 * off(got.0, sun), 100.0 * off(got.1, dome), 100.0 * off(got.2, bounce));
                    for (name, x, y, within) in [("sun", got.0, sun, 1.0e-3), ("dome", got.1, dome, 1.0e-3), ("bounce", got.2, bounce, 1.0e-2)] {
                        assert!((x as f64 - y).abs() <= within * y.abs() + 1.0e-6, "tau {tau}, {view:?}, sun {elevation}: {name} {x} against the closed form's {y}");
                    }
                }
            }
        }
        // A ray's octaves against the classic volume's: the same lobes and octaves, at the heap's gain (HEAP_SUN_GAIN
        // over SUN_GAIN of the classic's), the powder off (the classic's weight 0, the volume's local depth deep); then
        // the powder alone, darkest where the point is at the edge.
        for cos in [1.0f32, 0.3, -0.6] {
            let light = RayLight::new(cos, 0.5, FORWARD_SHARE, LightFamily::Heap);
            for tau in [0.0f32, 1.0, 5.0] {
                let (got, want) = (light.sun_light(tau, 1.0e3), crate::volumetric::sun_light(tau, &crate::volumetric::phase_octaves(cos), 0.0) * (HEAP_SUN_GAIN / SUN_GAIN));
                assert!((got - want).abs() <= 1.0e-6 * want.abs().max(1.0), "cos {cos}, tau {tau}: {got} against the classic's {want}");
                let edge = light.sun_light(tau, 0.0);
                let darkened = 1.0 - POWDER * 0.5 * (1.0 - cos);
                assert!((edge - darkened * got).abs() <= 1.0e-6 * got.abs().max(1.0), "cos {cos}, tau {tau}: the powder's edge {edge} against {darkened} x {got}");
            }
        }
        // Silver lining after the march: the record of a march at the default lining, shaded at another, equals a
        // march whose every step mixed the lobes at that lining, integrated apart from `step`, shaded by hand.
        let inputs = |lining: f32| ShadeInputs {
            lining,
            sun_base: vec3f(1.0, 0.55, 0.3),
            sun_top: vec3f(1.2, 0.9, 0.7),
            brightness: 1.3,
            sky_light: 0.8,
            ambient: vec3f(0.2, 0.3, 0.5),
            ground: vec3f(0.1, 0.15, 0.05),
            ground_sun: vec3f(0.12, 0.1, 0.07),
            side: vec3f(0.15, 0.3, 0.6),
            sky: vec3f(0.3, 0.45, 0.8),
            visibility_m: 30_000.0,
        };
        for (family, tau) in [LightFamily::Heap, LightFamily::Slab].into_iter().flat_map(|family| [1.0f32, 5.0, 20.0].map(|tau| (family, tau))) {
            for view in [View::Backlit, View::Sunlit] {
                for elevation in [9.0f32, 30.0, 60.0] {
                    let record = march(&Slab::new(family, tau, N, view, elevation, FORWARD_SHARE, 20_000.0, false), N, None);
                    for lining in [0.0f32, 0.3, 1.0] {
                        let got = shade(&record, &inputs(lining));
                        // The fresh march: the same steps, the sun's light mixed at `lining` as each step is taken.
                        let sigma = tau / SLAB_M;
                        let sin_e = elevation.to_radians().sin();
                        let cos = match view {
                            View::Backlit => sin_e,
                            View::Sunlit => -sin_e,
                        };
                        // Seen straight up or straight down, as `Slab` sees it.
                        let view_up = match view {
                            View::Backlit => 1.0,
                            View::Sunlit => -1.0,
                        };
                        let light = RayLight::new(cos, sin_e, lining, family).seen_from(view_up);
                        let ds = SLAB_M / N as f32;
                        let (mut t, mut sun, mut dome, mut open, mut bounce, mut wd, mut ws) = (1.0f32, Vec3f::default(), 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
                        for i in 0..N {
                            if t <= MIN_TRANSMITTANCE {
                                break;
                            }
                            let s = (i as f32 + 0.5) * ds;
                            let height = match view {
                                View::Backlit => s,
                                View::Sunlit => SLAB_M - s,
                            };
                            let h01 = height / SLAB_M;
                            let tau_up = sigma * (SLAB_M - height);
                            let w = t * (1.0 - (-sigma * ds).exp());
                            let colours = inputs(lining);
                            let rate = match view {
                                View::Backlit => -sigma / sin_e,
                                View::Sunlit => sigma / sin_e,
                            };
                            let (a, tau_mid, change) = (sigma * ds, tau_up / sin_e, rate * ds);
                            let lit = exact_sun_step(&light, lining, a, tau_mid, change) * light.powder(sigma * POWDER_REACH_M);
                            // The slab family's diffuse share: sun.y x T_d / pi of the depth above, in either lobe.
                            let diffuse = if family == LightFamily::Slab { sin_e / std::f32::consts::PI * diffuse_transmittance(tau_up) } else { 0.0 };
                            // The heap's diffusion: the step's opacity less the third octave's integral across it, times
                            // T_d of the sun's depth at the point, at the lobes' mix.
                            let u = a + 0.25 * change;
                            let third = (-0.25 * (tau_mid - 0.5 * change)).exp() * a * if u.abs() > 1.0e-6 { -(-u).exp_m1() / u } else { 1.0 };
                            // T_d across the step: at its start, middle and end, by Simpson's weights times e^(-a x) x
                            // (1 - e^(-tau_sun / 4)) there.
                            let (t0, t1) = (tau_mid - 0.5 * change, tau_mid + 0.5 * change);
                            let at = [(t0, 1.0, 1.0), (tau_mid, 4.0, (-0.5 * a).exp()), (t1, 1.0, (-a).exp())].map(|(x, simpson, seen)| (x, simpson * seen * (1.0 - (-0.25 * x).exp())));
                            let td_across = at.iter().map(|&(x, k)| k * diffuse_transmittance(x)).sum::<f32>() / at.iter().map(|&(_, k)| k).sum::<f32>();
                            let heap = (lining * light.diffusion[0] + (1.0 - lining) * light.diffusion[1]) * t * ((1.0 - (-a).exp()) - third) * td_across;
                            sun += colours.sun_base.mix(colours.sun_top, h01) * (t * lit + w * diffuse + heap);
                            // The dome through the column above, and a heap's open side through the view's own depth.
                            let above = diffuse_transmittance(tau_up);
                            dome += w * above;
                            open += w * light.open_side * (1.0 - above) * diffuse_transmittance(0.5 * a - t.ln());
                            bounce += w * (1.0 - h01);
                            wd += w * (20_000.0 + BASE_M + s);
                            ws += w;
                            t *= (-sigma * ds).exp();
                        }
                        let i = inputs(lining);
                        let alpha = if t <= MIN_TRANSMITTANCE { 1.0 } else { 1.0 - t };
                        let haze = 1.0 - (-(wd / ws - HAZE_FROM_M) / i.visibility_m).exp();
                        let want = (sun + (i.ambient * dome + i.side * open + i.ground * (BOUNCE_SHARE * bounce)) * i.sky_light + i.ground_sun * (BOUNCE_SHARE * bounce)) * i.brightness * (1.0 - haze) + i.sky * (alpha * haze);
                        for (g, w) in [(got.x, want.x), (got.y, want.y), (got.z, want.z)] {
                            assert!((g - w).abs() <= 1.0e-5 * w.abs().max(1.0), "{family:?}, tau {tau}, {view:?}, sun {elevation}, lining {lining}: {got:?} against {want:?}");
                        }
                    }
                }
            }
        }
        // Segments: the march cut in two at a third of the way, each part its own record, the back one seen through
        // the front one's transmittance, composites to the uncut march; the ray stops in whichever segment it stops.
        for tau in [1.0f32, 5.0, 20.0] {
            for view in [View::Backlit, View::Sunlit] {
                for elevation in [9.0f32, 30.0, 60.0] {
                    let whole = slab(tau, N, view, elevation, FORWARD_SHARE, 0.0, false, None);
                    let steps = Slab::new(LightFamily::Heap, tau, N, view, elevation, FORWARD_SHARE, 0.0, false);
                    let cut = N / 3;
                    let (mut first, mut second) = (VolumeMarch::CLEAR, VolumeMarch::CLEAR);
                    for i in 0..cut {
                        if first.stops(1.0) {
                            break;
                        }
                        steps.step(&mut first, i, steps.ds, 1.0);
                    }
                    if first.stops(1.0) {
                        first.close();
                    }
                    let front = first.transmittance;
                    if front > 0.0 {
                        for i in cut..N {
                            if second.stops(front) {
                                break;
                            }
                            steps.step(&mut second, i, steps.ds, front);
                        }
                        if second.stops(front) {
                            second.close();
                        }
                    }
                    let i = inputs(FORWARD_SHARE);
                    let (got, want) = (shade(&first, &i) + shade(&second, &i) * front, shade(&whole, &i));
                    let alpha = 1.0 - front * second.transmittance;
                    assert!((alpha - whole.alpha()).abs() <= 1.0e-5, "tau {tau}, {view:?}, sun {elevation}: cut alpha {alpha} against {}", whole.alpha());
                    for (g, w) in [(got.x, want.x), (got.y, want.y), (got.z, want.z)] {
                        assert!((g - w).abs() <= 1.0e-5 * w.abs().max(1.0), "tau {tau}, {view:?}, sun {elevation}: cut {got:?} against {want:?}");
                    }
                }
            }
        }
        // The closing term at T 0.05 against the stop at T 0.01, both against a march to the end, on a slab of
        // optical depth 20 in fine steps (so the steps' own error stays out of it).
        const FINE: usize = 4096;
        for view in [View::Sunlit, View::Backlit] {
            for elevation in [9.0f32, 30.0, 60.0] {
                let full = channels(&slab(20.0, FINE, view, elevation, FORWARD_SHARE, 0.0, false, Some(0.0)), FORWARD_SHARE);
                let stopped = channels(&slab(20.0, FINE, view, elevation, FORWARD_SHARE, 0.0, false, None), FORWARD_SHARE);
                let closed = channels(&slab(20.0, FINE, view, elevation, FORWARD_SHARE, 0.0, false, Some(0.05)), FORWARD_SHARE);
                let off = |x: f32, y: f32| if y.abs() > 1.0e-6 { format!("{:+.2} %", 100.0 * (x - y) / y) } else { format!("{x:.1e} against {y:.1e}") };
                println!(
                    "closing term, tau 20, {view:?}, sun {elevation}: stop at T 0.01: sun {}, dome {}; closed at T 0.05: sun {}, dome {}",
                    off(stopped.0, full.0),
                    off(stopped.1, full.1),
                    off(closed.0, full.0),
                    off(closed.1, full.1)
                );
            }
        }
    }

    /// Design §2.3: a deck of optical depth 50 seen from below passes the dome's light diffusely as the slab law does,
    /// within 10 % of `cloud_light::diffuse_transmittance(50)`. The share is the base's dome light against the same
    /// deck's with nothing over any of its points.
    ///
    /// This pins the dome law [`step`] takes, T_d(tau_up) at each point, against the slab law it was taken from; it is
    /// not an independent check that a volume reproduces the slab. The exp(-0.25 tau_up) design §2.3 wrote passes 5e-6
    /// here, and would fail it.
    ///
    /// Grown with the sun: a slab-family deck's base takes, in absolute terms, the sun the flat slab law
    /// (`cloud_light::radiance`) gives the same deck, within 10 %, though its octaves are gone by then.
    #[test]
    fn a_thick_deck_passes_the_diffuse_share() {
        let tau = 50.0;
        for elevation in [9.0f32, 30.0, 60.0] {
            let deck = march(&Slab::new(LightFamily::Slab, tau, 512, View::Backlit, elevation, FORWARD_SHARE, 0.0, false), 512, None);
            let open = march(&Slab::new(LightFamily::Slab, tau, 512, View::Backlit, elevation, FORWARD_SHARE, 0.0, true), 512, None);
            let share = deck.ambient / open.ambient;
            let want = diffuse_transmittance(tau);
            println!("deck of tau {tau}, sun {elevation}: the dome's share {share:.4}, T_d {want:.4}");
            assert!((share - want).abs() <= 0.1 * want, "sun {elevation}: share {share} against T_d {want}");
            // The sun at the base, white, against the flat slab law's for the same deck seen straight up.
            let sin_e = elevation.to_radians().sin();
            let base = march(&Slab::new(LightFamily::Slab, tau, 512, View::Backlit, elevation, FORWARD_SHARE, 0.0, false), 512, None);
            let sun = channels(&base, FORWARD_SHARE).0;
            let flat_light = EntryLight {
                family: LightFamily::Slab,
                sun: vec3f(1.0, 1.0, 1.0),
                sun_top: vec3f(1.0, 1.0, 1.0),
                ambient: Vec3f::default(),
                ground: Vec3f::default(),
                ground_sun: Vec3f::default(),
                side: [Vec3f::default(); SIDE_AZIMUTHS],
                rows: LightRows { brightness: 1.0, sky_light: 1.0, lining: FORWARD_SHARE, shade_below: 1.0 },
                dome_share: 1.0,
                mean_tau: tau,
            };
            let flat_shape = Shape { cover: 1.0, alpha: 1.0, tau, tau_sun: 0.0, hit_m: Vec2f::default(), height_m: BASE_M };
            let flat = radiance(&flat_light, &flat_shape, vec3f(0.0, 1.0, 0.0), vec3f(elevation.to_radians().cos(), sin_e, 0.0), 1.0).x;
            println!("deck of tau {tau}, sun {elevation}: the base's sun {sun:.4}, the flat slab law's {flat:.4}; its dome {:.4}", base.ambient);
            assert!((sun - flat).abs() <= 0.1 * flat, "sun {elevation}: the base's sun {sun} against the flat slab law's {flat}");
            // A heap deck of the same depth seen straight up (`slab`, the heap family): its sun is the octaves' and its
            // diffusion (K_MS, tuned on the reference's cloud-to-sky ratios, design §12.8 item 5), its share of the
            // flat slab law's diffuse sun held to its known answer, 0.285 / 0.316 / 0.373 at sun 9 / 30 / 60.
            let heap = slab(tau, 512, View::Backlit, elevation, FORWARD_SHARE, 0.0, false, None);
            let heap_sun = channels(&heap, FORWARD_SHARE).0;
            let share = heap_sun / flat;
            println!("deck of tau {tau}, sun {elevation}: a heap deck's sun {heap_sun:.3e}, {share:.3} of the flat slab law's; its dome {:.4}", heap.ambient);
            let known = [(9.0f32, 0.285f32), (30.0, 0.316), (60.0, 0.373)].iter().find(|k| k.0 == elevation).expect("a known share").1;
            assert!((share - known).abs() <= 0.002, "sun {elevation}: a heap deck passes {share} of the flat slab law's sun, known {known}");
        }
    }
}
