//! Declarative rigs are validated and bound once, before frame evaluation.
use super::{At, Reader};
use crate::layers;
use crate::model::*;
use crate::obj::Look;
use crate::rig::{self, Bend, Bone, Follow, IK, MeshGrid, Skin};
use std::sync::Arc;

impl Reader<'_, '_> {
    fn rig_records(&mut self, at: &At, field: &str, ty: &str) -> Vec<At> {
        let items = match self.get(at.obj, field) {
            None => return Vec::new(),
            Some(Look::Arr(items)) => items,
            _ => { self.err(at, field, format!("`{field}` expects an array of {ty}{{…}} records"), None); return Vec::new(); }
        };
        items.into_iter().enumerate().filter_map(|(i, value)| {
            match value {
                Look::Obj(obj, kind) if kind.is_none() || kind == Some(ty) => Some(self.at(obj, format!("{}[{i}]", at.field(field)), at.loc)),
                _ => { self.err(at, field, format!("`{field}[{i}]` expects {ty}{{…}}"), None); None }
            }
        }).collect()
    }

    fn bone_index(&mut self, at: &At, field: &str, bones: &[Bone]) -> Option<usize> {
        match self.get(at.obj, field) {
            Some(Look::Str(name)) => match bones.iter().position(|b| b.name == name) {
                Some(i) => Some(i),
                None => { self.err(at, field, format!("`{field}` names unknown bone \"{name}\"; expected a name in `bones`"), None); None }
            },
            _ => { self.err(at, field, format!("`{field}` expects a bone name string"), None); None }
        }
    }

    pub(super) fn mesh_grid(&mut self, at: &At, value: Option<Look>) -> Option<Arc<MeshGrid>> {
        let value = value?;
        let [cols, rows] = self.vec2_const(at, "mesh", &value)?;
        if ![cols, rows].iter().all(|n| n.is_finite() && *n >= 1.0 && n.fract() == 0.0) {
            self.err(at, "mesh", "`mesh` expects vec2(cols, rows), whole numbers of cells >= 1".into(), None);
            return None;
        }
        // The renderer's index format is u32. This is a representation
        // check, not a performance cap; never truncate authored geometry.
        if (cols + 1.0) * (rows + 1.0) > u32::MAX as f64 {
            self.err(at, "mesh", "`mesh` has more vertices than the u32 triangle index format can address; split it into smaller Image layers".into(), None);
            return None;
        }
        Some(Arc::new(MeshGrid::new(cols as usize, rows as usize)))
    }
}

impl layers::Bones {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let records = c.rig_records(at, "bones", "Bone");
        let mut bones = Vec::<Bone>::with_capacity(records.len());
        for bat in &records {
            c.unknown_fields(bat, &["name", "parent", "length", "rotate"], "Bone");
            let name = match c.get(bat.obj, "name") {
                Some(Look::Str(name)) if !name.is_empty() => name,
                _ => { c.err(bat, "name", "Bone.name expects a non-empty string".into(), None); String::new() }
            };
            if bones.iter().any(|b| b.name == name) { c.err(bat, "name", format!("Bone.name \"{name}\" is repeated; names must be unique"), None); }
            let length = c.get(bat.obj, "length").and_then(|v| c.num_const(bat, "length", &v)).unwrap_or(100.0);
            if !length.is_finite() || length <= 0.0 { c.err(bat, "length", "Bone.length expects a positive finite length in document units".into(), None); }
            let rotate = c.prop::<1>(bat, "rotate", [0.0]);
            let rest = match &rotate {
                Prop::Const([v]) => *v,
                Prop::Track(track) => track.keys.first().map_or(0.0, |k| k.v[0]),
                // A blend's rest is its value when its controls are constant, else its first stop.
                Prop::Blend(blend) => rotate.constant().map_or_else(|| blend.values.first().and_then(Prop::constant).map_or(0.0, |v| v[0]), |v| v[0]),
                Prop::Fn(f) => {
                    let function = c.fns[f.index];
                    c.vm.capture_errors_nested();
                    let (value, _) = c.vm.eval_bounded(&crate::value::FRAME_LIMITS, |vm| vm.call(function, &[0.0.into(), 0.0.into(), 0.0.into()]));
                    let errors = c.vm.take_error_records();
                    c.vm.capture_errors_restore();
                    if let Some(error) = errors.first() {
                        c.err(bat, "rotate", format!("Bone.rotate must give the rest angle at t=0: {}", error.message()), None);
                    }
                    let value = c.look(value);
                    c.num_const(bat, "rotate", &value).unwrap_or(0.0)
                }
            };
            if !rest.is_finite() { c.err(bat, "rotate", "Bone.rotate expects finite degrees".into(), None); }
            bones.push(Bone { name, parent: None, length: length.max(1e-9), rotate, rest });
        }
        if bones.is_empty() { c.err(at, "bones", "Bones.bones expects at least one Bone{name: … length: …}".into(), None); }
        for (i, bat) in records.iter().enumerate() {
            bones[i].parent = match c.get(bat.obj, "parent") {
                None | Some(Look::Nil) => i.checked_sub(1),
                Some(Look::Id(id)) if id == "none" => None,
                _ => c.bone_index(bat, "parent", &bones),
            };
        }
        let mut order = Vec::with_capacity(bones.len());
        let mut seen = vec![false; bones.len()];
        while order.len() < bones.len() {
            let before = order.len();
            for i in 0..bones.len() {
                if !seen[i] && bones[i].parent.is_none_or(|p| seen[p]) { seen[i] = true; order.push(i); }
            }
            if before == order.len() {
                let i = seen.iter().position(|v| !*v).unwrap();
                c.err(&records[i], "parent", format!("Bone.parent has a cycle in the ancestry of \"{}\"; expected an acyclic chain or tree", bones[i].name), None);
                // Invalid documents do not evaluate, but finish reading their
                // other fields without recursing through this graph.
                break;
            }
        }
        let rest = rig::fk(&bones, &order, |i| bones[i].rest);
        let mut follow = Vec::new();
        for fat in c.rig_records(at, "follow", "Follow") {
            c.unknown_fields(&fat, &["bone", "path", "along", "orient"], "Follow");
            let Some(bone) = c.bone_index(&fat, "bone", &bones) else { continue };
            let path = match c.get(fat.obj, "path") {
                Some(Look::Str(data)) => match super::check_path_data(&data) {
                    Ok(()) => Arc::new(crate::Polyline::of(&data)),
                    Err(e) => { c.err(&fat, "path", format!("Follow.path expects SVG path data: {e}"), None); continue; }
                },
                _ => { c.err(&fat, "path", "Follow.path expects SVG path data, e.g. \"M 0 0 L 100 0\"".into(), None); continue; }
            };
            if path.length() <= 0.0 { c.err(&fat, "path", "Follow.path expects a path with non-zero length".into(), None); }
            let orient = match c.get(fat.obj, "orient") {
                None => false,
                Some(Look::Bool(v)) => v,
                _ => { c.err(&fat, "orient", "Follow.orient expects true or false".into(), None); false }
            };
            follow.push(Follow { bone, path, along: c.prop(&fat, "along", [0.0]), orient });
        }
        let mut ik = Vec::new();
        for kat in c.rig_records(at, "ik", "IK") {
            c.unknown_fields(&kat, &["end", "target", "bend", "mix"], "IK");
            let Some(end) = c.bone_index(&kat, "end", &bones) else { continue };
            if bones[end].parent.is_none() { c.err(&kat, "end", "IK.end expects a bone with a parent (the two bones that reach the target)".into(), None); }
            if c.get(kat.obj, "target").is_none() { c.err(&kat, "target", "IK.target expects a vec2 in Bones layer coordinates, optionally keyed".into(), None); }
            let bend = match c.get(kat.obj, "bend") {
                None => Bend::Ccw,
                Some(Look::Id(v)) if v == "ccw" => Bend::Ccw,
                Some(Look::Id(v)) if v == "cw" => Bend::Cw,
                _ => { c.err(&kat, "bend", "IK.bend expects @cw or @ccw".into(), None); Bend::Ccw }
            };
            let mix = c.prop(&kat, "mix", [1.0]);
            let invalid = match &mix { Prop::Const([v]) => !(0.0..=1.0).contains(v), Prop::Track(t) => t.keys.iter().any(|k| !(0.0..=1.0).contains(&k.v[0])), _ => false };
            if invalid { c.err(&kat, "mix", "IK.mix expects a value in 0..1 (including its keys)".into(), None); }
            ik.push(IK { end, target: c.prop(&kat, "target", [0.0, 0.0]), bend, mix, loc: kat.loc, path: kat.field("target") });
        }
        let mut layers = Vec::new();
        let mut skins = Vec::new();
        match self.layers.look(c) {
            Some(Look::Arr(values)) => for (i, value) in values.iter().enumerate() {
                let path = format!("{}.layers[{i}]", at.path);
                let Some(mut layer) = c.layer(at, "layers", &path, value) else { continue };
                let child_at = c.at(value.obj().unwrap(), path, at.loc);
                let rigid = c.get(child_at.obj, "rigid").and_then(|_| c.bone_index(&child_at, "rigid", &bones));
                if let LayerKind::Image { mesh, size, .. } = &mut layer.kind {
                    if size.is_none() && rigid.is_none() { c.err(&child_at, "size", "a skinned Image needs size: vec2(width, height) in document units".into(), None); }
                    if mesh.is_none() { *mesh = Some(Arc::new(MeshGrid::new(8, 8))); }
                }
                if rigid.is_none() && !matches!(layer.kind, LayerKind::Path(_) | LayerKind::Rect(_) | LayerKind::Ellipse(_) | LayerKind::Image { .. }) {
                    c.err(&child_at, "rigid", "Bones skins Image, Path, Rect and Ellipse; other children need rigid: \"bone\"".into(), None);
                }
                skins.push(Skin::bind(&layer, &rest, rigid));
                layers.push(layer);
            },
            _ => c.err(at, "layers", "Bones.layers expects an array of layers".into(), None),
        }
        LayerKind::Bones(Box::new(rig::Bones { bones, order, rest, follow, ik, layers, skins }))
    }
}
