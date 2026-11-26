//! The Crowd layer, read from its component.

use super::{At, Reader};
use crate::layers::Crowd as CrowdFields;
use crate::model::*;
use crate::obj::Look;

impl CrowdFields {
    /// `Crowd{…}`: the 2D twin of R3's 3D crowd kit (same field names).
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let count = match self.count.look(c) {
            None => 120,
            Some(value) => match c.num_const(at, "count", &value) {
                Some(n) if (1.0..=2000.0).contains(&n) => n as usize,
                _ => {
                    c.err(at, "count", "a Crowd's `count` is 1 to 2000".into(), None);
                    120
                }
            },
        };
        let rows = self.rows.konst_opt(c, at, "rows").map_or(((count as f64 / 40.0).ceil() as usize).clamp(1, 12), |n| (n as usize).clamp(1, 40));
        let default_size = [c.width, c.height * 0.4];
        let size = self.size.konst(c, at, "size", default_size);
        let eyes = self.eyes.konst(c, at, "eyes").unwrap_or([1.0, 0.55, 0.12, 1.0]);
        let eyes2 = if !self.eyes2.is_set() && self.eyes.is_set() {
            eyes
        } else {
            self.eyes2.konst(c, at, "eyes2").unwrap_or([0.7, 1.0, 0.23, 1.0])
        };
        let react = match feature(c, at, "react", self.react.look(c), true).as_deref() {
            None | Some("beat") => CrowdReact::Beat,
            Some("none") => CrowdReact::None,
            Some(other) => CrowdReact::Feature(other.to_string()),
        };
        let cheer = feature(c, at, "cheer", self.cheer.look(c), false).filter(|f| f != "none");
        let arena = match self.shape.look(c) {
            None => false,
            Some(Look::Id(s)) if s == "block" => false,
            Some(Look::Id(s)) if s == "arena" => true,
            Some(Look::Id(s)) if s == "ring" => {
                c.err(at, "shape", "`@ring` is the 3D crowd's (Text3D/Scene3D); a 2D Crowd is @block or @arena".into(), None);
                false
            }
            Some(other) => {
                c.err(at, "shape", format!("`shape` is @block or @arena, not {}", other.kind()), None);
                false
            }
        };
        let sign = match self.sign.look(c) {
            None => "YES".to_string(),
            Some(Look::Str(s)) => s,
            Some(other) => {
                c.err(at, "sign", format!("`sign` is the word on the signs, not {}", other.kind()), None);
                "YES".to_string()
            }
        };
        let start = self.start.time(c, at, "start").unwrap_or(Time::seconds(-1.0));
        LayerKind::Crowd(Box::new(Crowd {
            count,
            rows,
            size,
            seed: self.seed.konst_opt(c, at, "seed").map_or(1, |n| n.max(0.0) as u32),
            variants: self.variants.konst_opt(c, at, "variants").map(|n| (n as usize).max(1)),
            arena,
            color: self.color.prop(c, at, "color", [0.05, 0.05, 0.07, 1.0]),
            color2: self.color2.opt(c, at, "color2"),
            outline: self.outline.opt(c, at, "outline"),
            eyes: [eyes, eyes2],
            bob: self.bob.prop(c, at, "bob", 0.04),
            stagger: self.stagger.konst_opt(c, at, "stagger").map(|s| s.max(0.0)),
            react,
            hop: self.hop.prop(c, at, "hop", 0.03),
            nod: self.nod.prop(c, at, "nod", 0.5),
            arms: self.arms.prop(c, at, "arms", 0.0),
            cheer,
            signs: self.signs.konst_opt(c, at, "signs").map_or(0.0, |n| n.clamp(0.0, 1.0)),
            sign,
            sign_up: self.sign_up.prop(c, at, "sign_up", 1.0),
            sign_color: self.sign_color.konst(c, at, "sign_color").unwrap_or([0.96, 0.94, 0.88, 1.0]),
            sign_ink: self.sign_ink.konst(c, at, "sign_ink").unwrap_or([0.08, 0.08, 0.08, 1.0]),
            start,
            member: c.crowd_member(at, self.member.look(c)),
        }))
    }
}

/// A Crowd's `react` / `cheer`: an audio feature (`@beat` only for `react`).
fn feature(c: &mut Reader, at: &At, field: &str, value: Option<Look>, allow_beat: bool) -> Option<String> {
    match value {
        None => None,
        Some(Look::Id(id)) if id == "none" => Some("none".into()),
        Some(Look::Id(id)) if (allow_beat && id == "beat") || ["energy", "low", "mid", "high"].contains(&id.as_str()) => Some(id),
        Some(other) => {
            let what = if allow_beat { "@beat, @energy, @low, @mid, @high or @none" } else { "@energy, @low, @mid, @high or @none" };
            c.err(at, field, format!("`{field}` is {what}, not {}", other.kind()), None);
            None
        }
    }
}
