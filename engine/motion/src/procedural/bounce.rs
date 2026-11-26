//! `Bounce{…}`: a ball bouncing along a baseline with automatic squash and
//! stretch (stretched along its velocity in the air, squashed flat on
//! contact with its bottom on the line), and `onion: {count spacing}` on any
//! layer: ghosts of its earlier selves trailing it (the onion skin).
//!
//! The bounce is closed form: parabolic hops whose launch speed falls by
//! `restitution` each time, scaled so the hops fill `duration`; the ball's
//! keys are sampled from it at 60 per second.

use super::*;

/// The ball's state at `t` seconds into the bounce.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BallState {
    /// Centre height above the baseline (0 on contact), and horizontal travel 0..1.
    pub height: f64,
    pub travel: f64,
    /// Vertical velocity (up positive, box units per second) and horizontal.
    pub vy: f64,
    pub vx: f64,
    /// 0..1: how hard the ball is pressed into the line now (squash).
    pub squash: f64,
}

/// The bounce's shape: drop height, hop count, restitution, duration and
/// the horizontal distance travelled.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bouncer {
    pub height: f64,
    pub bounces: usize,
    pub restitution: f64,
    pub duration: f64,
    pub distance: f64,
    /// Gravity that makes the hops fill `duration`.
    g: f64,
}

/// How long a contact squash lasts either side of the contact.
const CONTACT: f64 = 0.05;

impl Bouncer {
    pub fn new(height: f64, bounces: usize, restitution: f64, duration: f64, distance: f64) -> Self {
        // Drop time sqrt(2h/g), then hop k lasts 2 e^k sqrt(2h/g).
        let e = restitution;
        let sum: f64 = 1.0 + (1..=bounces).map(|k| 2.0 * e.powi(k as i32)).sum::<f64>();
        let drop = duration / sum;
        let g = 2.0 * height / (drop * drop).max(1e-9);
        Self { height, bounces, restitution, duration, distance, g }
    }

    /// The contact times (seconds).
    pub fn contacts(&self) -> Vec<f64> {
        let drop = (2.0 * self.height / self.g).sqrt();
        let mut out = vec![drop];
        let mut t = drop;
        for k in 1..=self.bounces {
            t += 2.0 * self.restitution.powi(k as i32) * drop;
            out.push(t);
        }
        out
    }

    pub fn state(&self, t: f64) -> BallState {
        let t = t.clamp(0.0, self.duration);
        let v0 = (2.0 * self.g * self.height).sqrt();
        let contacts = self.contacts();
        let vx = self.distance / self.duration;
        let travel = t / self.duration;
        // Which hop: before the first contact the drop, after the last a rest.
        let (height, vy) = if t < contacts[0] {
            (self.height - 0.5 * self.g * t * t, -self.g * t)
        } else {
            let k = contacts.iter().rposition(|c| *c <= t).unwrap();
            if k + 1 >= contacts.len() {
                (0.0, 0.0)
            } else {
                let v = v0 * self.restitution.powi(k as i32 + 1);
                let u = t - contacts[k];
                (v * u - 0.5 * self.g * u * u, v - self.g * u)
            }
        };
        // Squash: strongest at contact, by the impact speed.
        let mut squash: f64 = 0.0;
        for (k, c) in contacts.iter().enumerate() {
            let near = (t - c).abs();
            if near < CONTACT {
                let impact = self.restitution.powi(k as i32);
                squash = squash.max(impact * (1.0 - near / CONTACT).powi(2));
            }
        }
        BallState { height: height.max(0.0), travel, vy, vx, squash }
    }

    /// The fastest speed (the first impact).
    pub fn top_speed(&self) -> f64 {
        (2.0 * self.g * self.height).sqrt().hypot(self.distance / self.duration)
    }
}

/// The ball's look at one moment: along-axis angle (degrees, 0..180, so the
/// symmetric ball never spins a half turn), scale along that axis and
/// across it (area kept), and how far its centre sits above the line.
pub(crate) fn deform(b: &Bouncer, s: &BallState, radius: f64, squash_amount: f64) -> (f64, f64, f64, f64) {
    let speed = s.vx.hypot(s.vy);
    if s.squash > 0.0 {
        // On the line: flattened vertically, bottom on the line.
        let along = 1.0 - squash_amount * 0.55 * s.squash;
        return (90.0, along, 1.0 / along, radius * along);
    }
    // In the air: stretched along the velocity (screen y is down).
    let angle = (-s.vy).atan2(s.vx).to_degrees().rem_euclid(180.0);
    let along = 1.0 + squash_amount * 0.45 * (speed / b.top_speed().max(1e-9)).powi(2);
    // Stretched along a steep path the ball would dip under the line near
    // contact: keep its lowest point on or above it.
    let a = angle.to_radians();
    let half_height = radius * ((along * a.sin()).powi(2) + (a.cos() / along).powi(2)).sqrt();
    (angle, along, 1.0 / along, (s.height + radius).max(half_height))
}

pub(super) fn lower(cx: &mut Cx, o: ScriptObject) -> Made {
    let [w, h] = get_vec2(cx, o, "size", [cx.doc[0] * 0.8, cx.doc[1] * 0.5])?;
    let color = get_color(cx, o, "color", 0xff5a1fff)?;
    let ink = get_color(cx, o, "ink", 0x1a1a1aff)?;
    let radius = get_num(cx, o, "radius", h * 0.09)?.max(1.0);
    let height = get_num(cx, o, "height", h * 0.8)?.clamp(radius, h * 4.0) - radius;
    let bounces = get_num(cx, o, "bounces", 3.0)?.clamp(0.0, 12.0) as usize;
    let restitution = get_num(cx, o, "restitution", 0.6)?.clamp(0.05, 0.95);
    let squash = get_num(cx, o, "squash", 1.0)?.clamp(0.0, 1.5);
    let duration = get_num(cx, o, "duration", 2.0)?.max(0.2);
    let start = get_start(cx, o);
    let font = get_face(cx, o, "font", "JetBrains Mono")?;
    let b = Bouncer::new(height, bounces, restitution, duration, w - radius * 2.0);

    let samples = (duration * 60.0).ceil() as usize;
    let mut x: Vec<(f64, Lit)> = Vec::new();
    let mut y: Vec<(f64, Lit)> = Vec::new();
    let mut rot: Vec<(f64, Lit)> = Vec::new();
    let mut scale: Vec<(f64, Lit)> = Vec::new();
    for k in 0..=samples {
        let t = duration * k as f64 / samples as f64;
        let s = b.state(t);
        let (angle, along, across, lift) = deform(&b, &s, radius, squash);
        x.push((t, (s.travel * (w - radius * 2.0)).into()));
        y.push((t, (-lift).into()));
        rot.push((t, angle.into()));
        scale.push((t, v2(along, across)));
    }
    let base = h;
    let mut layers = Vec::new();
    let baseline = obj::opt(cx.vm, o, "baseline");
    if baseline.and_then(|v| v.as_bool()) != Some(false) {
        let line_color = match baseline.and_then(|v| v.as_color()) {
            Some(c) => c,
            None => with_alpha(ink, 0.8),
        };
        let trim = tween(cx, 0.0.into(), 1.0.into(), start, 0.4, "ease_out_cubic");
        layers.push(layer(cx, "Path", vec![
            ("name", "baseline".into()),
            ("svg", format!("M 0 {base:.1} L {w:.1} {base:.1}").into()),
            ("stroke", Lit::Color(line_color)),
            ("width", (h * 0.006).max(1.0).into()),
            ("trim_end", trim.into()),
        ]));
    }
    let kx = keys(cx, start, x);
    let ky = keys(cx, start, y);
    let krot = keys(cx, start, rot);
    let kscale = keys(cx, start, scale);
    let mut ball: Vec<(&str, Lit)> = vec![
        ("name", "ball".into()),
        ("size", v2(radius * 2.0, radius * 2.0)),
        ("at", v2(0.0, base - radius)),
        ("x", kx.into()),
        ("y", ky.into()),
        ("rotate", krot.into()),
        ("scale", kscale.into()),
        ("fill", Lit::Color(color)),
    ];
    // `onion` on a Bounce trails the ball (the Bounce itself stays put).
    if let Some(spec) = obj::opt(cx.vm, o, "onion").filter(|v| !v.is_nil()) {
        ball.push(("onion", spec.into()));
    }
    layers.push(layer(cx, "Ellipse", ball));
    if get_bool(cx, o, "labels", false) {
        // Pinned labels at the moments they name: the first contact
        // (SQUASH), the fastest point of the first hop (STRETCH) and its top
        // (ARC).
        let contacts = b.contacts();
        let small = h * 0.05;
        // (SQUASH under the line, the others above the ball.)
        let mut pins = vec![("SQUASH", contacts[0], true)];
        if contacts.len() > 1 {
            let hop = contacts[1] - contacts[0];
            pins.push(("STRETCH", contacts[0] + hop * 0.2, false));
            pins.push(("ARC", contacts[0] + hop * 0.5, false));
        }
        for (word, t, under) in pins {
            let s = b.state(t);
            let px = radius + s.travel * (w - radius * 2.0);
            let py = if under { base + small * 2.2 } else { base - radius * 3.2 - s.height };
            let when = at_time(cx, start, t);
            let t0 = at_time(cx, when, -0.05);
            let t2 = at_time(cx, when, 0.6);
            let t3 = at_time(cx, when, 0.9);
            let opacity = obj::keys(cx.vm, vec![
                (t0, 0.0.into(), None),
                (when, 1.0.into(), Some("ease_out")),
                (t2, 1.0.into(), None),
                (t3, 0.0.into(), Some("ease_in")),
            ]);
            layers.push(layer(cx, "Text", vec![
                ("name", format!("label {}", word.to_lowercase()).into()),
                ("text", word.into()),
                ("font", obj::Lit::Font(font.clone())),
                ("size", small.into()),
                ("color", Lit::Color(ink)),
                ("tracking", (small * 0.15).into()),
                ("align", Lit::Id("center")),
                ("at", v2(px, py)),
                ("opacity", opacity.into()),
            ]));
        }
    }
    // The group takes the Bounce's common fields except `onion` (the ball has it).
    let g = crate::layers::make(cx.vm, "Group", &[]);
    for (k, v) in common(cx, o) {
        if k != "onion" {
            obj::set(cx.vm, g, &k, v);
        }
    }
    let size = obj::lit(cx.vm, v2(w, h));
    obj::set(cx.vm, g, "size", size);
    let layers = obj::array(cx.vm, layers);
    obj::set(cx.vm, g, "layers", layers);
    let at = cx.vm.bx.heap_ref().made_at(o);
    cx.vm.bx.heap_mut().set_made_at(g, at);
    Ok(g.into())
}

/// `onion: {count: 5 spacing: 0.06 opacity: 0.5 outline: true}` on any
/// layer: `count` ghosts of it, each `spacing` seconds further in the past
/// and fainter, under it. `outline` draws the ghosts as outlines (for a
/// filled shape: its fill as a thin stroke). `onion: 5` is the count alone.
pub(super) fn onion(cx: &mut Cx, inner: ScriptObject, spec: ScriptValue) -> Made {
    let (count, spacing, opacity, outline) = if let Some(n) = spec.as_number() {
        (n, 0.05, 0.5, false)
    } else if spec.as_bool() == Some(true) {
        (5.0, 0.05, 0.5, false)
    } else if let Some(o) = settings(cx, spec, "onion", &["count", "spacing", "opacity", "outline"])? {
        (get_num(cx, o, "count", 5.0)?, get_num(cx, o, "spacing", 0.05)?, get_num(cx, o, "opacity", 0.5)?, get_bool(cx, o, "outline", false))
    } else {
        return Err(("onion".into(), format!("`onion` is `{{count: 5 spacing: 0.05}}` or a count, not {}", kind(cx, spec))));
    };
    let count = count.clamp(1.0, 32.0) as usize;
    if !(spacing > 0.0) {
        return Err(("onion".into(), "`onion.spacing` is a positive number of seconds".into()));
    }
    // The live layer and a ghost template: the layer run `spacing` late
    // (its clock starts later) without its own span and placement, which
    // the wrapper keeps.
    let mut ghost = obj::copy_without(cx.vm, inner, &["name", "span"]);
    if outline {
        let fill = obj::opt(cx.vm, ghost, "fill");
        if let Some(fill) = fill {
            ghost = obj::copy_without(cx.vm, ghost, &["fill", "stroke", "width"]);
            let size = obj::opt(cx.vm, ghost, "size");
            let width = match size.and_then(|v| obj::items(cx.vm, v)) {
                Some(s) => s.into_iter().filter_map(|v| obj::num(cx.vm, v)).fold(f64::MAX, f64::min) * 0.04,
                None => 2.0,
            };
            obj::set(cx.vm, ghost, "stroke", fill);
            obj::set(cx.vm, ghost, "width", width.clamp(1.0, 6.0).into());
        }
    }
    let span = obj::array(cx.vm, [spacing.into(), 1.0e6.into()]);
    let ghosts = array(cx, vec![ghost.into()]);
    let late = layer(cx, "Group", vec![("span", span.into()), ("layers", ghosts.into())]);
    let step = record(cx, vec![("opacity", (-0.9 / count as f64).into())]);
    let repeat = layer(cx, "Repeat", vec![
        ("layer", late.into()),
        ("count", (count as f64).into()),
        ("layout", Lit::Id("line")),
        ("delay", spacing.into()),
        ("step", step.into()),
    ]);
    let trail_layers = array(cx, vec![repeat]);
    let trail = layer(cx, "Group", vec![
        ("name", "onion".into()),
        ("opacity", opacity.clamp(0.0, 1.0).into()),
        ("layers", trail_layers.into()),
    ]);
    let live = obj::copy_without(cx.vm, inner, &["span"]);
    Ok(wrapper(cx, inner, "", vec![], vec![trail, live.into()]))
}
