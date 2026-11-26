//! Reading a `Character{…}` layer (lane CH1): the model (a kit character
//! with its look, or a character SVG), the cues (poses, expressions,
//! walks) and the performance settings (nod, bob, blinks, sway, lip sync).

use super::{At, Reader};
use crate::character::{emote, kit, lipsync, pose, svg, CharacterLayer, Cue, EmoteCue, Float, Model, PartKeys, Slot, Walk};
use crate::layers::Character;
use makepad_script::*;
use crate::model::*;
use crate::obj::Look;
use std::sync::Arc;

impl Character {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let model = c.character_model(at, self);
        let rig = match &model {
            Model::Rig(rig) => Some(rig.clone()),
            _ => None,
        };
        let default_height = c.height * 0.6;
        let height = self.height.konst(c, at, "height", default_height);
        if height <= 0.0 {
            c.err(at, "height", "a Character's `height` (head to feet, document units) is positive".into(), None);
        }
        let facing = match self.facing.look(c) {
            None => None,
            Some(Look::Id(id)) | Some(Look::Str(id)) if id == "right" => Some(1.0),
            Some(Look::Id(id)) | Some(Look::Str(id)) if id == "left" => Some(-1.0),
            Some(other) => {
                let kind = other.kind();
                c.err(at, "facing", format!("`facing` is @right (as drawn) or @left (mirrored), not {kind}"), None);
                None
            }
        };
        let pose_names: Vec<&str> = pose::POSES.iter().map(|(n, _)| *n).collect();
        let pose_value = self.pose.look(c);
        let (poses, pose_blend) = match &pose_value {
            Some(value) if value.is("Blend") || value.is("Blend2") => {
                let blend = c.blend_form(at, "pose", value, |c, at, field, value| match value {
                    Look::Id(name) | Look::Str(name) => match pose::preset(name) {
                        Some(pose) => Some(pose),
                        None => {
                            c.err(at, field, format!("`{field}`: no pose @{name}.{}", super::did_you_mean(name, &pose_names)), None);
                            None
                        }
                    },
                    other => {
                        c.err(at, field, format!("`{field}` blend values must be constant named poses (@idle, @cheer, ...), not {}; animate the x/y controls", other.kind()), None);
                        None
                    }
                });
                (Vec::new(), blend)
            }
            _ => (c.cues(at, "pose", pose_value, 0.3, &|name| pose::preset(name).is_some(), &pose_names), None),
        };
        let expressions = match &rig {
            Some(rig) => {
                let mut names: Vec<String> = Vec::new();
                for slot in [crate::character::Slot::Eyes, crate::character::Slot::Brows, crate::character::Slot::Mouth] {
                    for v in rig.variants(slot) {
                        if lipsync::Viseme::from_name(v).is_none() && !matches!(v, "open" | "closed" | "REST") && !names.iter().any(|n| n == v) {
                            names.push(v.to_string());
                        }
                    }
                }
                names.push("neutral".into());
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let refs2 = refs.clone();
                c.cues(at, "expression", self.expression.look(c), 0.0, &move |name| refs2.contains(&name), &refs)
            }
            None => c.cues(at, "expression", self.expression.look(c), 0.0, &|_| true, &[]),
        };
        let costumes = match &rig {
            Some(rig) => {
                let mut names: Vec<String> = rig.variants(Slot::Costume).iter().map(|v| v.to_string()).collect();
                names.push("none".into());
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let known = refs.clone();
                c.cues(at, "costume", self.costume.look(c), 0.0, &move |name| name.split('+').all(|c| known.contains(&c.trim())), &refs)
            }
            None => c.cues(at, "costume", self.costume.look(c), 0.0, &|_| true, &[]),
        };
        let visemes: Vec<&str> = lipsync::Viseme::ALL.iter().map(|v| v.name()).collect();
        let mouths = c.cues(at, "mouth", self.mouth.look(c), 0.0, &|name| lipsync::Viseme::from_name(name).is_some(), &visemes);
        let walks = c.walks(at, self.walk.look(c), height);
        let floats = c.floats(at, self.float.look(c), height);
        let emotes = c.emotes(at, self.emote.look(c));
        let seed = self.seed.konst(c, at, "seed", 1.0).max(0.0) as u32;
        let lip_sync = match self.lip_sync.look(c) {
            None => None,
            Some(voice) if voice.is("Voice") => c.voice_id(&voice),
            Some(Look::Id(id)) => Some(id),
            Some(other) => {
                let kind = other.kind();
                c.err(at, "lip_sync", format!("`lip_sync` is `voice(@vo)`: the voiceover or vocal stem the mouth follows, not {kind}"), None);
                None
            }
        };
        let mut parts = Vec::new();
        match self.parts.look(c) {
            None => {}
            Some(Look::Obj(map, _)) => {
                for (id, value) in c.fields(map) {
                    if id.starts_with("__") {
                        continue;
                    }
                    if let Some(rig) = &rig {
                        if rig.find(&id).is_none() {
                            let names: Vec<&str> = rig.parts.iter().map(|p| p.name.as_str()).collect();
                            c.warn(at, "parts", format!("the model has no part \"{id}\".{}", super::did_you_mean(&id, &names)), None);
                        }
                    }
                    let Look::Obj(part, _) = value else {
                        c.err(at, "parts", format!("part \"{id}\" is {}; write `{{rotate: … x: … y: …}}`", value.kind()), None);
                        continue;
                    };
                    let pat = c.at(part, format!("{}.{id}", at.field("parts")), at.loc);
                    c.unknown_fields(&pat, &["rotate", "x", "y", "scale", "opacity"], "part");
                    parts.push((id, PartKeys {
                        rotate: c.prop::<1>(&pat, "rotate", [0.0]),
                        x: c.prop::<1>(&pat, "x", [0.0]),
                        y: c.prop::<1>(&pat, "y", [0.0]),
                        scale: c.prop::<2>(&pat, "scale", [1.0, 1.0]),
                        opacity: c.prop::<1>(&pat, "opacity", [1.0]),
                    }));
                }
            }
            Some(other) => c.err(at, "parts", format!("`parts` maps part ids to offsets on top of the pose (`{{head: {{rotate: …}}}}`), not {}", other.kind()), None),
        }
        let mut reach = Vec::new();
        match self.reach.look(c) {
            None => {}
            Some(Look::Obj(map, _)) => {
                let rat = c.at(map, at.field("reach"), at.loc);
                for (name, value) in c.fields(map) {
                    if name.starts_with("__") { continue; }
                    let joint = match pose::Joint::from_name(&name) {
                        Some(j @ (pose::Joint::HandL | pose::Joint::HandR | pose::Joint::FootL | pose::Joint::FootR)) => j,
                        _ => { c.err(&rat, &name, "Character.reach expects hand_l, hand_r, foot_l or foot_r, each a keyable vec2 in Character layer coordinates".into(), None); continue; }
                    };
                    if let Some(rig) = &rig {
                        if rig.reach_chain(joint).is_none() {
                            c.err(&rat, &name, format!("Character.reach.{name} expects connected upper limb, lower limb and end joints in the model"), None);
                        }
                    }
                    if let Some(target) = c.prop_value(&rat, &name, &value) { reach.push((joint, target)); }
                }
            }
            Some(_) => c.err(at, "reach", "Character.reach expects {hand_r: vec2(x, y), foot_l: …}".into(), None),
        }
        LayerKind::Character(Box::new(CharacterLayer {
            model,
            height: height.max(1e-3),
            facing,
            poses,
            pose_blend,
            expressions,
            costumes,
            mouths,
            walks,
            floats,
            emotes,
            vary: self.vary.konst(c, at, "vary", 0.0).clamp(0.0, 1.0),
            nod: self.nod.konst(c, at, "nod", 0.0),
            bob: self.bob.konst(c, at, "bob", 0.0),
            blink: self.blink.konst(c, at, "blink", 3.5).max(0.0),
            seed,
            sway: self.sway.konst(c, at, "sway", 1.0).max(0.0),
            lip_sync,
            lead: self.lead.konst(c, at, "lead", 1.0),
            parts,
            reach,
        }))
    }
}

impl Reader<'_, '_> {
    /// A Crowd's `member: Character{…}`: each member a varied copy of it.
    pub(super) fn crowd_member(&mut self, at: &At, member: Option<Look>) -> Option<Box<CharacterLayer>> {
        match member {
            None => None,
            Some(Look::Obj(obj, Some("Character"))) => {
                let mat = self.at(obj, at.field("member"), at.loc);
                match Character::script_from_value(self.vm, obj.into()).read(self, &mat) {
                    LayerKind::Character(ch) => Some(ch),
                    _ => None,
                }
            }
            Some(other) => {
                self.err(at, "member", format!("a Crowd's `member` is a `Character{{…}}` (each member a varied copy), not {}", other.kind()), Some("`member: Character{model: @pip vary: 0.8}`"));
                None
            }
        }
    }

    fn character_model(&mut self, at: &At, ch: &Character) -> Model {
        let kits: Vec<&str> = kit::KITS.iter().map(|(n, _)| *n).collect();
        match (ch.model.look(self), ch.svg.look(self), ch.src.look(self)) {
            (Some(Look::Id(name)) | Some(Look::Str(name)), None, None) => {
                let Some(mut look) = kit::look(&name) else {
                    self.err(at, "model", format!("no kit character @{name}.{}", super::did_you_mean(&name, &kits)), Some(&format!("kit characters: {}", kits.iter().map(|k| format!("@{k}")).collect::<Vec<_>>().join(" "))));
                    return Model::Empty;
                };
                match ch.look.look(self) {
                    None => {}
                    Some(Look::Obj(fields, _)) => {
                        let lat = self.at(fields, at.field("look"), at.loc);
                        for (key, value) in self.fields(fields).into_iter().filter(|(k, _)| !k.starts_with("__")) {
                            let value = match value {
                                Look::Num(n) => kit::LookValue::Num(n),
                                Look::Id(id) | Look::Str(id) => kit::LookValue::Id(id),
                                Look::Arr(items) => kit::LookValue::Ids(items.iter().filter_map(|v| match v {
                                    Look::Id(id) | Look::Str(id) => Some(id.clone()),
                                    _ => None,
                                }).collect()),
                                other => match self.color_const(&lat, &key, &other) {
                                    Some(c) => kit::LookValue::Color(c),
                                    None => continue,
                                },
                            };
                            if let Err(message) = look.set(&key, &value) {
                                let fields: Vec<&str> = kit::LOOK_FIELDS.iter().map(|(n, _)| *n).collect();
                                self.err(&lat, &key, message, Some(&format!("look fields: {}", fields.join(", "))));
                            }
                        }
                    }
                    Some(other) => self.err(at, "look", format!("`look` is `{{skin: #… head: @round …}}`, not {}", other.kind()), None),
                }
                Model::Rig(Arc::new(kit::build(&name, &look)))
            }
            (None, Some(value), None) | (None, None, Some(value)) => {
                let field = if ch.svg.is_set() { "svg" } else { "src" };
                if ch.look.is_set() {
                    self.warn(at, "look", "`look` changes kit characters (`model: @spark`); an SVG character is drawn as it is".into(), None);
                }
                match self.media(at, field, &value, true) {
                    Some(Media::Inline(markup)) => match svg::from_svg(&markup) {
                        Ok((rig, warnings)) => {
                            for warning in warnings {
                                self.warn(at, field, warning, Some("the Character docs list the part ids a character SVG uses"));
                            }
                            Model::Rig(Arc::new(rig))
                        }
                        Err(message) => {
                            self.err(at, field, message, None);
                            Model::Empty
                        }
                    },
                    Some(Media::Asset(asset)) => Model::Asset(asset),
                    _ => Model::Empty,
                }
            }
            (None, None, None) => {
                self.err(at, "model", "a Character needs a model: `model: @spark` (the kit), `svg: \"<svg…>\"` or `src: asset(id, rev)`".into(), Some(&format!("kit characters: {}", kits.iter().map(|k| format!("@{k}")).collect::<Vec<_>>().join(" "))));
                Model::Empty
            }
            (Some(other), None, None) => {
                self.err(at, "model", format!("`model` is a kit character (@spark, @mainframe, @pip), not {}", other.kind()), None);
                Model::Empty
            }
            _ => {
                self.err(at, "model", "a Character has one of `model`, `svg` or `src`".into(), None);
                Model::Empty
            }
        }
    }

    /// `pose: @hero` or `pose: [[0, @walk], [beat(8), @on_mic, 0.4]]`
    /// (time, name, blend seconds).
    fn cues(&mut self, at: &At, field: &str, value: Option<Look>, blend: f64, known: &dyn Fn(&str) -> bool, names: &[&str]) -> Vec<Cue> {
        let mut out = Vec::new();
        let list = names.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" ");
        let check = |c: &mut Self, name: &str| {
            if !known(name) {
                let message = format!("`{field}`: no @{name} here.{}", super::did_you_mean(name, names));
                if field == "pose" || field == "costume" || field == "mouth" {
                    c.err(at, field, message, Some(&format!("{field}s: {list}")));
                } else {
                    c.warn(at, field, format!("{message} It shows neutral."), Some(&format!("this model's expressions: {list}")));
                }
            }
        };
        match value {
            None => {}
            Some(Look::Id(name)) | Some(Look::Str(name)) => {
                check(self, &name);
                out.push(Cue { t: Time::seconds(0.0), name, blend: 0.0 });
            }
            Some(Look::Arr(items)) => {
                for item in items {
                    let Look::Arr(parts) = &item else {
                        self.err(at, field, format!("each `{field}` cue is `[time, @name]` or `[time, @name, blend]`, not {}", item.kind()), None);
                        continue;
                    };
                    let (Some(t), Some(Look::Id(first) | Look::Str(first))) = (parts.first(), parts.get(1)) else {
                        self.err(at, field, format!("each `{field}` cue is `[time, @name]` or `[time, @name, blend]`"), Some("`[beat(8), @on_mic]`, `[word(@vo, 3), @smile]`, `[2.5, @hero, 0.4]`"));
                        continue;
                    };
                    let Some(t) = self.time(at, field, t) else { continue };
                    // Costumes: several names are worn together (`[2, @crown, @shades]`).
                    let mut name = first.clone();
                    let mut rest = 2;
                    while let Some(Look::Id(more) | Look::Str(more)) = parts.get(rest).filter(|_| field == "costume") {
                        name = format!("{name}+{more}");
                        rest += 1;
                    }
                    let name = &name;
                    check(self, name);
                    let blend = match parts.get(rest) {
                        Some(Look::Num(b)) if *b >= 0.0 => *b,
                        None => blend,
                        Some(other) => {
                            let kind = other.kind();
                            self.err(at, field, format!("a cue's blend is seconds (≥ 0), not {kind}"), None);
                            blend
                        }
                    };
                    out.push(Cue { t, name: name.clone(), blend });
                }
            }
            Some(other) => self.err(at, field, format!("`{field}` is `@name` or a list of `[time, @name]` cues, not {}", other.kind()), None),
        }
        out
    }

    /// `float: {start: beat(8) end: beat(16) height: 120 bob: 15 period: 2}`
    /// (height and bob in document units, the bob's period in beats), or a
    /// list of them.
    fn floats(&mut self, at: &At, value: Option<Look>, height: f64) -> Vec<Float> {
        let items = match value {
            None => return Vec::new(),
            Some(Look::Arr(items)) => items,
            Some(one) => vec![one],
        };
        let mut out = Vec::new();
        for item in items {
            let &Look::Obj(f, _) = &item else {
                self.err(at, "float", format!("a float is `{{start: beat(8) height: 120}}`, not {}", item.kind()), None);
                continue;
            };
            let fat = self.at(f, at.field("float"), at.loc);
            self.unknown_fields(&fat, &["start", "end", "height", "bob", "period"], "float");
            let start = match self.get(f, "start") {
                Some(value) => self.time(&fat, "start", &value),
                None => Some(Time::seconds(0.0)),
            };
            let end = match self.get(f, "end") {
                None | Some(Look::Nil) => None,
                Some(value) => self.time(&fat, "end", &value),
            };
            let num = |c: &mut Self, field: &str, default: f64| c.get(f, field).and_then(|v| c.num_const(&fat, field, &v)).unwrap_or(default);
            let (lift, bob, period) = (num(self, "height", 0.25 * height), num(self, "bob", 0.03 * height), num(self, "period", 2.0));
            if period <= 0.0 {
                self.err(&fat, "period", "a float's `period` (beats a bob takes) is positive".into(), None);
                continue;
            }
            if let Some(start) = start {
                out.push(Float { start, end, height: lift, bob, period });
            }
        }
        out
    }

    /// `emote: [[beat(4), @sweat], [word(@vo, 3), @hearts, 2.0]]` (time,
    /// name, seconds; 1.5 s by default).
    fn emotes(&mut self, at: &At, value: Option<Look>) -> Vec<EmoteCue> {
        let names: Vec<&str> = emote::EMOTES.iter().map(|(n, _)| *n).collect();
        let list = names.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" ");
        let items = match value {
            None => return Vec::new(),
            Some(Look::Arr(items)) => items,
            Some(other) => {
                self.err(at, "emote", format!("`emote` is a list of `[time, @name, seconds]`, not {}", other.kind()), Some(&format!("emotes: {list}")));
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for item in items {
            let (Some(t), Some(Look::Id(name) | Look::Str(name))) = (item_at(&item, 0), item_at(&item, 1)) else {
                self.err(at, "emote", "each emote is `[time, @name]` or `[time, @name, seconds]`".into(), Some("`[beat(4), @sweat, 2.0]`"));
                continue;
            };
            if !emote::is_emote(name) {
                self.err(at, "emote", format!("no emote @{name}.{}", super::did_you_mean(name, &names)), Some(&format!("emotes: {list}")));
                continue;
            }
            let Some(t) = self.time(at, "emote", t) else { continue };
            let length = match item_at(&item, 2) {
                None => 1.5,
                Some(Look::Num(s)) if *s > 0.0 => *s,
                Some(other) => {
                    let kind = other.kind();
                    self.err(at, "emote", format!("an emote's length is positive seconds, not {kind}"), None);
                    1.5
                }
            };
            out.push(EmoteCue { t, name: name.clone(), length, path: emote::path(name).into() });
        }
        out
    }

    /// `walk: {start: beat(0) steps: 8 stride: 160 dir: @right per_beat: 1}`
    /// or `{start: … distance: 900}`, or a list of them.
    fn walks(&mut self, at: &At, value: Option<Look>, height: f64) -> Vec<Walk> {
        let items = match value {
            None => return Vec::new(),
            Some(Look::Arr(items)) => items,
            Some(one) => vec![one],
        };
        let mut out = Vec::new();
        for item in items {
            let &Look::Obj(w, _) = &item else {
                self.err(at, "walk", format!("a walk is `{{start: beat(0) steps: 8 dir: @right}}`, not {}", item.kind()), None);
                continue;
            };
            let wat = self.at(w, at.field("walk"), at.loc);
            self.unknown_fields(&wat, &["start", "steps", "stride", "distance", "dir", "per_beat"], "walk");
            let start = match self.get(w, "start") {
                Some(value) => self.time(&wat, "start", &value),
                None => Some(Time::seconds(0.0)),
            };
            let num = |c: &mut Self, field: &str| c.get(w, field).and_then(|v| c.num_const(&wat, field, &v));
            let default_stride = 0.3 * height;
            let (steps, stride) = match (num(self, "steps"), num(self, "stride"), num(self, "distance")) {
                (Some(steps), stride, None) => (steps.max(0.0), stride.unwrap_or(default_stride).max(0.0)),
                (None, stride, Some(distance)) => {
                    let stride = stride.unwrap_or(default_stride).max(1e-3);
                    let steps = (distance.abs() / stride).round().max(1.0);
                    (steps, distance.abs() / steps)
                }
                (None, stride, None) => (8.0, stride.unwrap_or(default_stride)),
                (Some(_), _, Some(_)) => {
                    self.err(&wat, "distance", "a walk gives `steps` or `distance`, not both".into(), None);
                    continue;
                }
            };
            let dir = match self.get(w, "dir") {
                None | Some(Look::Nil) => 1.0,
                Some(Look::Id(d)) | Some(Look::Str(d)) if d == "right" => 1.0,
                Some(Look::Id(d)) | Some(Look::Str(d)) if d == "left" => -1.0,
                Some(other) => {
                    let kind = other.kind();
                    self.err(&wat, "dir", format!("`dir` is @right or @left, not {kind}"), None);
                    1.0
                }
            };
            let per_beat = num(self, "per_beat").unwrap_or(1.0);
            if per_beat <= 0.0 {
                self.err(&wat, "per_beat", "`per_beat` (steps a beat) is positive: 1 is one step per beat, 0.5 one every two".into(), None);
                continue;
            }
            if let Some(start) = start {
                out.push(Walk { start, steps, stride, dir, per_beat });
            }
        }
        out
    }
}

fn item_at(item: &Look, k: usize) -> Option<&Look> {
    match item {
        Look::Arr(parts) => parts.get(k),
        _ => None,
    }
}
