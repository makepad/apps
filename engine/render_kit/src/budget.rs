//! The per-frame budget. An exact render (an MP4, the render cache) takes
//! every effect at full quality however long it takes, bounded only by
//! memory: temporal samples that would not fit are rendered in fewer,
//! wider slices of the shutter, with a log line. A live preview has a GPU
//! time budget per frame; when the stack's estimated cost is over it, the
//! plan degrades the most expensive effects first (motion blur falls back
//! to the velocity blur or fewer samples, blurs run at lower resolution),
//! never refusing a frame.
//!
//! Costs are milliseconds per megapixel per effect, measured with
//! `makepad-render-kit bench` on an Apple M-series GPU (see the report).

use crate::catalog::EffectKind;
use crate::stack::ResolvedEffect;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Quality {
    /// Every effect at full quality (renders).
    Exact,
    /// Fit the stack into `gpu_ms` per frame (previews).
    Preview { gpu_ms: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    pub quality: Quality,
    /// Largest total of transient targets one frame may allocate.
    pub memory_bytes: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self { quality: Quality::Exact, memory_bytes: 3 << 30 }
    }
}

impl Budget {
    pub fn preview(gpu_ms: f64) -> Self {
        Self { quality: Quality::Preview { gpu_ms }, ..Self::default() }
    }
}

/// Measured cost of one effect at full quality, ms per megapixel, for its
/// default parameters; `scale_param` names the parameter the cost grows
/// with (blur radius) and how.
pub fn cost_per_mpx(effect: &ResolvedEffect) -> f64 {
    use EffectKind::*;
    // Blur: the separable passes cost ~taps; the downsampled path keeps
    // the tap count bounded, so large radii are not much dearer.
    let blur = |radius: f64| if radius <= 0.0 { 0.0 } else { BLUR_BASE + BLUR_PER_TAP * (radius * 1.5).min(24.0) };
    match effect.kind {
        GaussianBlur => blur(effect.num("radius")),
        DirectionalBlur => PER_TAP * (effect.num("length")).clamp(2.0, 96.0),
        RadialBlur | ZoomBlur => PER_TAP * 48.0,
        Glow | Halation => PASS * 2.0 + blur(effect.num("radius")),
        Bloom => PASS * 3.0 + blur(effect.num("radius") / 8.0) * 1.3,
        DropShadow | InnerShadow => PASS * 2.0 + blur(effect.num("radius")),
        Stroke => PASS * (4.0 + (effect.num("width").max(1.0)).log2().ceil().max(1.0)),
        Bevel => PASS * 3.0 + blur(effect.num("size") * 0.5),
        Noise | Turbulence => PASS * effect.num("octaves").max(1.0) * 0.8,
        Particles | StrokePath => PASS * 2.0,
        Transform3d => PASS * 2.0,
        // Temporal effects multiply what comes before them; see `plan`.
        MotionBlur | Echo => PASS,
        _ => PASS,
    }
}

/// One full-frame pass reading one or two textures, ms per megapixel.
pub const PASS: f64 = 0.09;
/// One texture tap in a full-frame pass.
pub const PER_TAP: f64 = 0.012;
const BLUR_BASE: f64 = 0.25;
const BLUR_PER_TAP: f64 = 0.03;

/// What the planner decided for one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// Per effect of the resolved stack: resolution scale (1 = full).
    pub scale: Vec<f64>,
    /// Per temporal effect: the sample count to use.
    pub samples: Vec<Option<usize>>,
    /// Per motion blur: use the velocity blur instead of sub-frames.
    pub velocity: Vec<bool>,
    /// Estimated GPU milliseconds.
    pub estimate_ms: f64,
    /// What was degraded, for the log and the host's status line.
    pub notes: Vec<String>,
}

/// Plans a stack for a frame of `size` pixels. `samples` holds the sample
/// count each temporal effect wants at full quality (None for others);
/// `can_velocity` says the host knows the layer's motion (so the velocity
/// blur is available).
pub fn plan(budget: &Budget, stack: &[ResolvedEffect], size: (u32, u32), samples: &[Option<usize>], can_velocity: bool) -> Plan {
    let mpx = size.0 as f64 * size.1 as f64 / 1e6;
    let bytes_per_target = size.0 as u64 * size.1 as u64 * 4;
    let mut plan = Plan {
        scale: vec![1.0; stack.len()],
        samples: samples.to_vec(),
        velocity: vec![false; stack.len()],
        estimate_ms: 0.0,
        notes: Vec::new(),
    };
    // Memory: each sub-frame of a temporal effect holds a target until its
    // reduction; nested temporal effects multiply.
    let product = |s: &[Option<usize>]| s.iter().flatten().product::<usize>().max(1) as u64;
    while product(&plan.samples) * bytes_per_target * 9 / 8 > budget.memory_bytes {
        let Some((i, n)) = plan.samples.iter().enumerate().filter_map(|(i, s)| s.map(|n| (i, n))).filter(|(_, n)| *n > 1).max_by_key(|(_, n)| *n) else { break };
        let fewer = (n * 3 / 4).max(1);
        plan.notes.push(format!(
            "effect {i}: {n} sub-frames of {}x{} need {} MB, over the {} MB frame budget; using {fewer}",
            size.0,
            size.1,
            product(&plan.samples) * bytes_per_target >> 20,
            budget.memory_bytes >> 20
        ));
        plan.samples[i] = Some(fewer);
    }
    let estimate = |plan: &Plan| -> f64 {
        // Effects before a temporal effect run once per sub-frame.
        let mut total = 0.0;
        for (i, effect) in stack.iter().enumerate() {
            let repeat: usize = plan.samples[i + 1..].iter().zip(&plan.velocity[i + 1..]).map(|(s, v)| if *v { 1 } else { s.unwrap_or(1) }).product();
            let own = if plan.velocity[i] { PER_TAP * 24.0 } else { cost_per_mpx(effect) * plan.scale[i] * plan.scale[i] };
            total += own * mpx * repeat.max(1) as f64;
        }
        // The layer itself, drawn once per sub-frame.
        let draws: usize = plan.samples.iter().zip(&plan.velocity).map(|(s, v)| if *v { 1 } else { s.unwrap_or(1) }).product();
        total + PASS * mpx * draws.max(1) as f64
    };
    plan.estimate_ms = estimate(&plan);
    let Quality::Preview { gpu_ms } = budget.quality else { return plan };
    // Degrade until it fits: velocity blur first, then halve samples, then
    // halve the resolution of the dearest blur-like effect.
    let mut guard = 0;
    while plan.estimate_ms > gpu_ms && guard < 64 {
        guard += 1;
        if let Some(i) = (0..stack.len()).find(|&i| stack[i].kind == EffectKind::MotionBlur && can_velocity && !plan.velocity[i]) {
            plan.velocity[i] = true;
            plan.notes.push(format!("effect {i}: velocity motion blur in preview (sub-frames need {:.1} ms)", plan.estimate_ms));
        } else if let Some(i) = (0..stack.len()).filter(|&i| !plan.velocity[i] && plan.samples[i].is_some_and(|n| n > 2)).max_by_key(|&i| plan.samples[i]) {
            let n = plan.samples[i].unwrap();
            plan.samples[i] = Some((n / 2).max(2));
            plan.notes.push(format!("effect {i}: {} sub-frames in preview instead of {n}", (n / 2).max(2)));
        } else if let Some(i) = (0..stack.len())
            .filter(|&i| plan.scale[i] > 0.25 && matches!(stack[i].kind, EffectKind::GaussianBlur | EffectKind::Glow | EffectKind::Bloom | EffectKind::Halation | EffectKind::DropShadow | EffectKind::InnerShadow | EffectKind::Bevel))
            .max_by(|&a, &b| cost_per_mpx(&stack[a]).total_cmp(&cost_per_mpx(&stack[b])))
        {
            plan.scale[i] *= 0.5;
            plan.notes.push(format!("effect {i}: {} at {}x resolution in preview", crate::catalog::info(stack[i].kind).name, plan.scale[i]));
        } else {
            break;
        }
        plan.estimate_ms = estimate(&plan);
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Value;

    #[test]
    fn exact_renders_are_never_degraded_by_time() {
        let stack = vec![ResolvedEffect::new(EffectKind::GaussianBlur).with("radius", Value::Num(200.0)), ResolvedEffect::new(EffectKind::MotionBlur)];
        let p = plan(&Budget::default(), &stack, (1920, 1080), &[None, Some(64)], true);
        assert_eq!(p.samples, vec![None, Some(64)]);
        assert_eq!(p.scale, vec![1.0, 1.0]);
        assert!(p.notes.is_empty());
        assert!(p.estimate_ms > 1.0);
    }

    #[test]
    fn memory_bounds_sub_frames_gracefully() {
        let stack = vec![ResolvedEffect::new(EffectKind::MotionBlur)];
        let budget = Budget { quality: Quality::Exact, memory_bytes: 1 << 30 };
        let p = plan(&budget, &stack, (3840, 2160), &[Some(256)], false);
        let n = p.samples[0].unwrap();
        assert!(n >= 16 && (n as u64) * 3840 * 2160 * 4 * 9 / 8 <= 1 << 30, "{n}");
        assert!(!p.notes.is_empty());
    }

    #[test]
    fn previews_degrade_to_fit() {
        let stack = vec![ResolvedEffect::new(EffectKind::Glow).with("radius", Value::Num(40.0)), ResolvedEffect::new(EffectKind::MotionBlur)];
        let p = plan(&Budget::preview(4.0), &stack, (1920, 1080), &[None, Some(32)], true);
        assert!(p.velocity[1], "{p:?}");
        assert!(p.estimate_ms <= 4.0 || p.scale[0] < 1.0, "{p:?}");
        let p = plan(&Budget::preview(4.0), &stack, (1920, 1080), &[None, Some(32)], false);
        assert!(p.samples[1].unwrap() < 32, "{p:?}");
    }
}
