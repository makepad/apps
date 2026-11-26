//! Words other 3D tools use for a scene's fields, for its warnings only.
//!
//! An author (often a model) writes the vocabulary of the 3D packages and
//! engines it knows: `location`, `albedo`, `power`, `field_of_view`.
//! Edit distance does not reach `position`, `color`, `intensity` or `fov`
//! from those, so this table makes the unknown-field warning name our
//! field and how its value is written.
//!
//! These words are never read as the field: the warning stays a warning
//! and the value is ignored. A test loads each word and its target on each
//! type it is given for: the target is read there, the word is not.

pub(crate) struct Foreign {
    pub words: &'static [&'static str],
    /// The Splash type names it is hinted on (as `check_fields` names them).
    pub types: &'static [&'static str],
    pub field: &'static str,
    pub how: &'static str,
}

const NODES: &[&str] = &["Mesh", "Group3D"];
const LIGHTS: &[&str] = &["AmbientLight", "HemisphereLight", "DirectionalLight", "PointLight", "SpotLight"];
const PLACED: &[&str] = &["Mesh", "Group3D", "DirectionalLight", "PointLight", "SpotLight"];
const MATERIALS: &[&str] = &["MeshStandardMaterial", "MeshPhysicalMaterial", "MeshBasicMaterial", "MeshLambertMaterial", "MeshPhongMaterial", "MeshToonMaterial"];

pub(crate) const FOREIGN: &[Foreign] = &[
    // Nodes and placed lights.
    Foreign { words: &["location", "translation", "pos", "origin", "center", "centre"], types: PLACED, field: "position", how: "vec3(x, y, z) in scene units, y up" },
    Foreign { words: &["rotation_euler", "euler", "orientation", "angles", "rotation_degrees"], types: NODES, field: "rotation", how: "vec3(x, y, z) in degrees" },
    Foreign { words: &["hide", "hidden", "hide_render", "hide_viewport", "visibility", "enabled"], types: NODES, field: "visible", how: "true or false" },
    Foreign { words: &["alpha", "transparency"], types: NODES, field: "opacity", how: "0..1" },
    Foreign { words: &["target", "aim", "face", "look_target"], types: NODES, field: "look_at", how: "vec3(x, y, z), the point the node faces" },
    Foreign { words: &["shadows", "cast_shadows", "casts_shadow", "shadow"], types: PLACED, field: "cast_shadow", how: "true or false" },
    Foreign { words: &["receive_shadows", "receives_shadow", "shadow_receiver"], types: NODES, field: "receive_shadow", how: "true or false" },
    Foreign { words: &["shape", "mesh", "geo", "primitive"], types: &["Mesh"], field: "geometry", how: "a geometry, e.g. BoxGeometry{width: 1 height: 1 depth: 1}" },
    Foreign { words: &["mat", "materials", "shader", "surface"], types: &["Mesh"], field: "material", how: "a material, e.g. MeshStandardMaterial{color: #ffffff roughness: 0.5}" },
    // Materials.
    Foreign { words: &["albedo", "diffuse", "diffuse_color", "base_colour", "colour", "tint"], types: MATERIALS, field: "color", how: "a colour (#rrggbb)" },
    Foreign { words: &["emission", "emission_color", "emissive_color", "glow"], types: MATERIALS, field: "emissive", how: "a colour (#rrggbb), the light the surface gives off" },
    Foreign { words: &["emission_strength", "emission_intensity", "glow_intensity"], types: MATERIALS, field: "emissive_intensity", how: "a factor on `emissive`" },
    Foreign { words: &["alpha", "transparency"], types: MATERIALS, field: "opacity", how: "0..1 (with `transparent: true`)" },
    Foreign { words: &["glossiness", "gloss", "smoothness"], types: MATERIALS, field: "roughness", how: "0..1, 0 a mirror (gloss is 1 − roughness)" },
    Foreign { words: &["specular", "spec"], types: MATERIALS, field: "specular_intensity", how: "0..1" },
    Foreign { words: &["index_of_refraction", "refraction_index"], types: MATERIALS, field: "ior", how: "the index of refraction, 1.5 for glass" },
    Foreign { words: &["coat", "coat_weight", "clearcoat_weight"], types: MATERIALS, field: "clearcoat", how: "0..1" },
    Foreign { words: &["albedo_map", "diffuse_map", "base_color_texture", "color_map", "image"], types: MATERIALS, field: "map", how: "an image: asset(\"id\", \"rev\")" },
    // Lights.
    Foreign { words: &["power", "strength", "energy", "brightness", "watts"], types: LIGHTS, field: "intensity", how: "a number" },
    Foreign { words: &["colour", "light_color", "tint"], types: LIGHTS, field: "color", how: "a colour (#rrggbb)" },
    Foreign { words: &["cone_angle", "spot_size", "spot_angle", "outer_angle", "cone"], types: &["SpotLight"], field: "angle", how: "the cone's half angle in degrees" },
    Foreign { words: &["spot_blend", "inner_angle", "falloff", "softness"], types: &["SpotLight"], field: "penumbra", how: "0..1, how soft the cone's edge is" },
    Foreign { words: &["range", "radius", "cutoff", "max_distance"], types: &["PointLight", "SpotLight"], field: "distance", how: "where the light ends, in scene units" },
    // The camera.
    Foreign { words: &["field_of_view", "fov_y", "fovy", "view_angle"], types: &["Camera"], field: "fov", how: "the vertical field of view in degrees" },
    Foreign { words: &["lens", "focal", "lens_mm"], types: &["Camera"], field: "focal_length", how: "the lens in mm" },
    Foreign { words: &["clip_start", "near_clip", "z_near", "clip_near"], types: &["Camera"], field: "near", how: "the near clipping distance" },
    Foreign { words: &["clip_end", "far_clip", "z_far", "clip_far"], types: &["Camera"], field: "far", how: "the far clipping distance" },
    Foreign { words: &["aim", "focus_point", "look_target", "center", "centre", "point_of_interest"], types: &["Camera"], field: "target", how: "vec3(x, y, z), the point the camera looks at" },
    Foreign { words: &["location", "translation", "pos", "eye"], types: &["Camera"], field: "position", how: "vec3(x, y, z) in scene units" },
    // The scene.
    Foreign { words: &["background_color", "bg", "clear_color", "world_color", "backdrop"], types: &["Scene3D"], field: "background", how: "a colour (#rrggbb); leave it out for a transparent background" },
    Foreign { words: &["hdri", "env", "env_map", "sky", "world", "skybox"], types: &["Scene3D"], field: "environment", how: "Environment{preset: @studio} or Environment{hdri: asset(\"id\", \"rev\")}" },
    Foreign { words: &["tonemapping", "tone_map", "view_transform", "color_transform"], types: &["Scene3D"], field: "tone_mapping", how: "@aces, @agx, @filmic, @reinhard, @linear or @none" },
    Foreign { words: &["objects", "items", "meshes", "elements", "entities"], types: &["Scene3D"], field: "children", how: "a list of nodes: Mesh{…}, Group3D{…}, Model{…}, lights" },
    Foreign { words: &["effects", "filters", "post_processing", "postprocessing", "compositor"], types: &["Scene3D"], field: "post", how: "a list of post effects: [Bloom{strength: 0.5}, …]" },
    Foreign { words: &["length", "end", "total_time"], types: &["Scene3D"], field: "duration", how: "the scene's length in seconds" },
];

/// `fieldOfView` as `field_of_view`.
fn normal(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 4);
    for (i, c) in word.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The field another tool's `word` means on type `ty`, and how it is written.
pub(crate) fn lookup(word: &str, ty: &str) -> Option<&'static Foreign> {
    let word = normal(word);
    FOREIGN.iter().find(|f| f.types.contains(&ty) && f.words.contains(&word.as_str()))
}

/// How field `field` of type `ty` is written, where the table says it.
pub(crate) fn how(ty: &str, field: &str) -> Option<&'static str> {
    FOREIGN.iter().find(|f| f.field == field && f.types.contains(&ty)).map(|f| f.how)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{FileResolver, LoadOptions, Scene3dDoc};
    use std::collections::HashMap;

    /// `field: 0` written on a `ty` in a small scene.
    fn scene(ty: &str, field: &str) -> String {
        let at = format!("{ty}{{{field}: 0}}");
        let body = match ty {
            "Scene3D" => return format!("use mod.scene.*; Scene3D{{{field}: 0 children: [Mesh{{geometry: BoxGeometry{{}}}}]}}"),
            "Camera" => format!("camera: {at} children: [Mesh{{geometry: BoxGeometry{{}}}}]"),
            "Mesh" => format!("children: [Mesh{{geometry: BoxGeometry{{}} {field}: 0}}]"),
            t if t.ends_with("Material") => format!("children: [Mesh{{geometry: BoxGeometry{{}} material: {at}}}]"),
            t if t.ends_with("Light") => format!("lights: [{at}] children: [Mesh{{geometry: BoxGeometry{{}}}}]"),
            _ => format!("children: [Mesh{{geometry: BoxGeometry{{}}}} {at}]"),
        };
        format!("use mod.scene.*; Scene3D{{{body}}}")
    }

    /// Every warning and error loading `source` says.
    fn said(source: &str) -> Vec<String> {
        let resolver = FileResolver::default();
        let options = LoadOptions { name: "test".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None };
        match Scene3dDoc::load(source, options) {
            Ok(doc) => doc.warnings.iter().map(|d| d.to_string()).collect(),
            Err(diags) => diags.iter().map(|d| d.to_string()).collect(),
        }
    }

    /// Each target is a field the reader takes on each type it is hinted
    /// for, and each word is one it does not take: there the warning names
    /// the target.
    #[test]
    fn every_hint_names_a_field_its_types_read() {
        for f in FOREIGN {
            for ty in f.types {
                let target = said(&scene(ty, f.field));
                assert!(!target.iter().any(|d| d.contains(&format!("no parameter `{}`", f.field))), "{ty} does not read `{}`: {target:?}", f.field);
                for word in f.words {
                    assert_eq!(normal(word), *word, "`{word}` is written in its normal form");
                    let warned = said(&scene(ty, word));
                    let expected = format!("{ty} has no parameter `{word}` (ignored); write `{}` here: {}", f.field, f.how);
                    assert!(warned.iter().any(|d| d.contains(&expected)), "{ty}.{word}: no {expected:?} in {warned:?}");
                }
            }
        }
    }
}
