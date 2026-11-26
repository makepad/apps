//! One registry of every field the Motion and Motion3D readers accept: per
//! type, each field's name, what it takes, its unit, the range the reader
//! keeps it in, what it reads as when not written, and a line on it.
//!
//! Generated from the readers, so it cannot drift from them:
//! - a Motion type's fields are its component's (`layers.rs`), each with
//!   the kind it declares; its defaults and ranges are the loader's own
//!   typed reads of a document holding one of every layer type
//!   (`load::recording`);
//! - a 3D scene type's fields are the list the scene reader checks it
//!   against (`check_fields`), and its defaults and ranges the reader's
//!   typed reads of a scene with one of that type in every place a scene
//!   takes an object (`motion3d::script::recording`).
//!
//! A field's line is the readers' own hint for it, where they keep one (the
//! other tools' words both readers turn into "write `x` here: …").

use crate::layers::{self, FieldType};
use std::sync::OnceLock;

/// One field of a type.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldInfo {
    pub name: String,
    /// What it takes: `number`, `vec2`, `colour`, `words`, `@a | @b`, `time`,
    /// `layers`, … (`any`: read its own way).
    pub ty: String,
    pub unit: Option<&'static str>,
    /// The range the reader keeps a number in.
    pub range: Option<[f64; 2]>,
    /// What it reads as when not written, as Splash (`0`, `vec2(1, 1)`,
    /// `#ffffff`, `@butt`).
    pub default: Option<String>,
    pub doc: Option<&'static str>,
}

/// Where a type is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// The document's root, `Motion{…}`.
    Document,
    /// A layer of a Motion document.
    Layer,
    /// An object of a 3D scene (a node, light, geometry, material, pass, …).
    Scene,
}

impl Family {
    pub fn name(self) -> &'static str {
        match self {
            Family::Document => "document",
            Family::Layer => "layer",
            Family::Scene => "3D scene",
        }
    }
}

/// One type and its fields, in the order the reader declares them.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeInfo {
    pub name: String,
    pub family: Family,
    pub fields: Vec<FieldInfo>,
}

impl TypeInfo {
    pub fn field(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|f| f.name == name)
    }
}

impl FieldInfo {
    /// One line: `radius: number = 0 (0..1, s) — …`.
    pub fn line(&self) -> String {
        let mut out = format!("{}: {}", self.name, self.ty);
        if let Some(default) = &self.default {
            out.push_str(&format!(" = {default}"));
        }
        let range = self.range.map(|[lo, hi]| format!("{}..{}", crate::value::fmt_num(lo), crate::value::fmt_num(hi)));
        let extra: Vec<String> = range.into_iter().chain(self.unit.map(str::to_string)).collect();
        if !extra.is_empty() {
            out.push_str(&format!(" ({})", extra.join(", ")));
        }
        if let Some(doc) = self.doc {
            out.push_str(&format!(" — {doc}"));
        }
        out
    }
}

/// The Motion document's root and every layer type, in the guide's order.
pub fn motion_types() -> &'static [TypeInfo] {
    static TYPES: OnceLock<Vec<TypeInfo>> = OnceLock::new();
    TYPES.get_or_init(read_motion_types)
}

/// The type named `name` (case as written, or any case when no type has
/// it exactly): the Motion entry and, for a type that is a layer and a 3D
/// scene's object too (`Scene3D`, `Text3D`), the scene's.
pub fn lookup(name: &str) -> Vec<TypeInfo> {
    let exact: Vec<TypeInfo> = find(name, |a, b| a == b);
    if !exact.is_empty() {
        return exact;
    }
    find(name, |a, b| a.eq_ignore_ascii_case(b))
}

fn find(name: &str, same: impl Fn(&str, &str) -> bool) -> Vec<TypeInfo> {
    let found = motion_types().iter().filter(|t| same(&t.name, name)).cloned();
    #[cfg(feature = "scene3d")]
    let found = found.chain(scene::lookup(name, &same));
    found.collect()
}

/// Every type's name: Motion's, then the 3D scene types.
pub fn type_names() -> Vec<String> {
    let names = motion_types().iter().map(|t| t.name.clone());
    #[cfg(feature = "scene3d")]
    let names = names.chain(scene_type_names().map(str::to_string));
    names.collect()
}

/// The help text for type `name` (each of its entries, every field a
/// line), or for its one field `field`; with no type, the types' names.
/// Errors name what there is.
pub fn describe(name: Option<&str>, field: Option<&str>) -> Result<String, String> {
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return Ok(format!("types: {}", type_names().join(" ")));
    };
    let found = lookup(name);
    if found.is_empty() {
        if type_names().iter().any(|t| t.eq_ignore_ascii_case(name)) {
            return Err(format!("the scene reader does not check {name}'s fields, so they are not listed"));
        }
        return Err(format!("no type `{name}`; the types are {}", type_names().join(" ")));
    }
    let mut out = Vec::new();
    for t in &found {
        match field.filter(|f| !f.is_empty()) {
            Some(field) => match t.field(field) {
                Some(f) => out.push(format!("{} ({}): {}", t.name, t.family.name(), f.line())),
                None => out.push(format!("{} ({}) has no field `{field}`; its fields are {}", t.name, t.family.name(), t.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(" "))),
            },
            None => {
                out.push(format!("{} ({}):", t.name, t.family.name()));
                out.extend(t.fields.iter().map(|f| format!("  {}", f.line())));
            }
        }
    }
    Ok(out.join("\n"))
}

/// What a field of `kind` takes, for people, and its unit.
fn kind_text(kind: &FieldType) -> (String, Option<&'static str>) {
    match kind {
        FieldType::Number { dims: 1 } => ("number".into(), None),
        FieldType::Number { dims } => (format!("vec{dims}"), None),
        FieldType::Color => ("colour".into(), None),
        FieldType::Text { multiline: false } => ("words".into(), None),
        FieldType::Text { multiline: true } => ("text".into(), None),
        FieldType::Font => ("font".into(), None),
        FieldType::Enum { choices } => (choices.iter().map(|c| format!("@{c}")).collect::<Vec<_>>().join(" | "), None),
        FieldType::Ease => ("ease".into(), None),
        FieldType::Time => ("time".into(), Some("s")),
        FieldType::Length => ("length".into(), Some("s")),
        FieldType::Bool => ("bool".into(), None),
        FieldType::Media => ("media".into(), None),
        FieldType::Data => ("data".into(), None),
        FieldType::Layer => ("layer".into(), None),
        FieldType::Layers => ("layers".into(), None),
        FieldType::Code => ("fn".into(), None),
        FieldType::Any => ("any".into(), None),
    }
}

/// A document of one of every layer type, loaded with its reads noted.
fn read_motion_types() -> Vec<TypeInfo> {
    let kinds: Vec<&str> = layers::layer_types().collect();
    let body: Vec<String> = kinds.iter().map(|kind| format!("{kind}{{}}")).collect();
    let source = format!("use mod.motion.*\nMotion{{size: vec2(640, 360) duration: 1 layers: [{}]}}\n", body.join(" "));
    let (_, reads) = crate::load::recording(|| crate::MotionDoc::load(&source).map(drop));
    let entry = |name: &str, family: Family, specs: Vec<layers::FieldSpec>, prefix: String| -> TypeInfo {
        let fields = specs
            .into_iter()
            .map(|spec| {
                let (ty, unit) = kind_text(&spec.ty);
                let path = format!("{prefix}{}", spec.name);
                let mine = || reads.iter().filter(|r| r.path == path);
                FieldInfo {
                    default: mine().find_map(|r| r.default.clone()),
                    range: mine().find_map(|r| r.range),
                    doc: crate::load::field_how(name, &spec.name),
                    name: spec.name,
                    ty,
                    unit,
                }
            })
            .collect();
        TypeInfo { name: name.to_string(), family, fields }
    };
    let mut out = vec![entry("Motion", Family::Document, layers::document_specs(), String::new())];
    for (i, kind) in kinds.iter().enumerate() {
        out.push(entry(kind, Family::Layer, layers::field_specs(kind), format!("layers[{i}].")));
    }
    out
}

/// The 3D scene types a document writes, as motion3d registers them.
#[cfg(feature = "scene3d")]
fn scene_type_names() -> impl Iterator<Item = &'static str> {
    makepad_stage_motion3d::script::TYPES.iter().map(|(name, _)| *name)
}

#[cfg(feature = "scene3d")]
mod scene {
    use super::{FieldInfo, Family, TypeInfo};
    use makepad_stage_motion3d::doc::{FileResolver, LoadOptions, Scene3dDoc};
    use makepad_stage_motion3d::script::{field_hint, recording, TYPES};

    /// Where a scene takes an object, `X` standing for it: every place at
    /// once (a type in a place it does not belong is an error there, and
    /// is read where it belongs); the camera's and the physics' parts in a
    /// second scene, as the camera and the physics are one field each.
    const PLACES: [&str; 2] = [
        "Scene3D{camera: X{} environment: X{} fog: X{} lights: [X{}] post: [X{}] physics: X{} children: [X{} Mesh{geometry: X{} material: X{}} Mesh{geometry: ExtrudeGeometry{shape: X{}}} Mesh{geometry: TubeGeometry{path: X{}}} Instances{geometry: BoxGeometry{} count: 4 layout: X{}} Particles{emitter: X{}}]}",
        "Scene3D{camera: Camera{shots: [X{}] moves: [X{}]} physics: Physics{bodies: [X{}] spawn: [X{}]} children: [Mesh{geometry: BoxGeometry{}}]}",
    ];

    /// The 3D type a document writes as `name`, read where it is written
    /// (another name for a type reads as that type: `PerspectiveCamera` is
    /// a `Camera`). None for a type whose fields the reader does not check.
    pub(super) fn lookup(name: &str, same: &dyn Fn(&str, &str) -> bool) -> Option<TypeInfo> {
        let (written, kind) = TYPES.iter().find(|(n, _)| same(n, name))?;
        let read = read_types(written);
        let named = read.iter().find(|t| same(&t.name, name));
        let alias = || TYPES.iter().filter(|(_, k)| k == kind).find_map(|(n, _)| read.iter().find(|t| t.name == *n));
        named.or_else(alias).cloned()
    }

    /// The types the reader checks when `written` (a type's name in a
    /// document) is in every place a scene takes an object: that type, and
    /// those its reader reads inside it.
    pub(super) fn read_types(written: &str) -> Vec<TypeInfo> {
        let resolver = FileResolver::default();
        let (_, reads) = recording(|| {
            for place in PLACES {
                let source = format!("use mod.scene.*\n{}\n", place.replace("X{", &format!("{written}{{")));
                let options = LoadOptions { name: "fields".into(), inputs: Default::default(), resolver: &resolver, first_line: 1, origin_file: None, song: None };
                drop(Scene3dDoc::load(&source, options));
            }
        });
        reads
            .types()
            .into_iter()
            .map(|t| TypeInfo {
                fields: t
                    .fields
                    .into_iter()
                    .map(|(name, read)| FieldInfo {
                        doc: field_hint(&t.name, &name),
                        ty: read.as_ref().map_or("any", |r| r.ty).to_string(),
                        unit: None,
                        range: read.as_ref().and_then(|r| r.range),
                        default: read.and_then(|r| r.default),
                        name,
                    })
                    .collect(),
                name: t.name,
                family: Family::Scene,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layer's fields with the kind each declares, the loader's
    /// defaults and ranges, and the readers' hints.
    #[test]
    fn a_layers_fields_carry_the_loaders_defaults_and_hints() {
        let rect = lookup("Rect");
        assert_eq!(rect.iter().map(|t| t.family).collect::<Vec<_>>(), vec![Family::Layer]);
        let field = |name: &str| rect[0].field(name).unwrap_or_else(|| panic!("Rect has no `{name}`")).clone();
        assert_eq!(field("radius").line(), "radius: number = 0 — the corner radius in px");
        assert_eq!(field("cap").line(), "cap: @round | @butt | @square = @butt");
        assert_eq!(field("opacity").range, Some([0.0, 1.0]));
        assert_eq!(field("anchor").default.as_deref(), Some("vec2(0.5, 0.5)"));
        assert_eq!(field("rotate").doc, Some("degrees, clockwise"));
        assert_eq!(field("exit_at").line(), "exit_at: time (s)");
        assert_eq!(lookup("rect"), rect, "any case");
        assert_eq!(describe(Some("Rect"), Some("radius")).as_deref(), Ok("Rect (layer): radius: number = 0 — the corner radius in px"));
        assert!(describe(Some("Rectangle"), None).is_err_and(|e| e.starts_with("no type `Rectangle`; the types are Motion Rect ")));
        // The document and every layer type, each with every field its
        // component declares.
        assert_eq!(lookup("Motion")[0].family, Family::Document);
        for kind in layers::layer_types() {
            let found = lookup(kind);
            let layer = found.iter().find(|t| t.family == Family::Layer).unwrap_or_else(|| panic!("no {kind}"));
            assert_eq!(layer.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>(), layers::fields(kind), "{kind}");
        }
    }

    /// A 3D type's fields are those its reader checks it against, with its
    /// typed reads' defaults and ranges; another name for a type is that
    /// type.
    #[cfg(feature = "scene3d")]
    #[test]
    fn a_scene_types_fields_are_the_ones_its_reader_checks() {
        let light = lookup("PointLight");
        assert_eq!(light.iter().map(|t| (t.name.as_str(), t.family)).collect::<Vec<_>>(), vec![("PointLight", Family::Scene)]);
        let field = |name: &str| light[0].field(name).unwrap_or_else(|| panic!("PointLight has no `{name}`")).clone();
        assert_eq!(field("intensity").line(), "intensity: number = 20 — a number");
        assert_eq!(field("color").default.as_deref(), Some("#ffffff"));
        assert_eq!(field("position").doc, Some("vec3(x, y, z) in scene units, y up"));
        let mut names: Vec<&str> = light[0].fields.iter().map(|f| f.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), light[0].fields.len(), "each field once");
        let camera = lookup("PerspectiveCamera");
        assert_eq!(camera.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["Camera"]);
        assert_eq!(camera[0].field("fov").map(|f| f.line()).as_deref(), Some("fov: number = 35 — the vertical field of view in degrees"));
        assert_eq!(describe(Some("PointEmitter"), None), Err("the scene reader does not check PointEmitter's fields, so they are not listed".to_string()));
        // A type that is a layer and a scene's root has both entries.
        assert_eq!(lookup("Scene3D").iter().map(|t| t.family).collect::<Vec<_>>(), vec![Family::Layer, Family::Scene]);
    }
}
