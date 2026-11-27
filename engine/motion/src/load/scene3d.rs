//! The Scene3D layer, read from its component: a glTF `model` with a
//! camera, lights, nodes and materials, or a full Splash scene (an object of
//! the document's own VM, which motion3d reads there).

use crate::plain::GroupProps;
use super::{At, Reader};
use crate::layers::Scene3D;
use crate::model::*;
use crate::obj::Look;
use crate::value::{SCENE_KEEP, SCENE_SIMPLE};

impl Scene3D {
    pub(super) fn read(&self, c: &mut Reader, at: &At, path: &str) -> LayerKind {
        // A scene with a field beyond the layer's own and the glTF scene's
        // is a full scene.
        let full = c.fields(at.obj).iter().any(|(name, _)| !name.starts_with("__") && !SCENE_KEEP.contains(&name.as_str()) && !SCENE_SIMPLE.contains(&name.as_str()));
        if full {
            return self.full_scene(c, at);
        }
        let _ = path;
        self.model_scene(c, at)
    }

    /// A full scene: the layer's own place, time and pictures are read here;
    /// the scene itself is the object, read by motion3d from the document's VM.
    #[cfg(feature = "scene3d")]
    fn full_scene(&self, c: &mut Reader, at: &At) -> LayerKind {
        let path = at.path.clone();
        let textures = c.named_media(at, "textures", self.textures.look(c));
        let videos = c.named_media(at, "videos", self.videos.look(c));
        let mut layers = Vec::new();
        match self.layers.look(c) {
            None => {}
            Some(Look::Obj(map, _)) => {
                for (name, value) in c.fields(map).into_iter().filter(|(k, _)| !k.starts_with("__")) {
                    if let Some(layer) = c.layer(at, "layers", &format!("{path}.layers.{name}"), &value) {
                        layers.push((name, layer));
                    }
                }
            }
            Some(other) => c.err(at, "layers", format!("a Scene3D's `layers` maps texture names to 2D layers (`{{intro: Group{{…}}}}`), not {}", other.kind()), None),
        }
        let Some(doc_vm) = c.doc_vm.clone() else {
            c.err(at, "", "a Scene3D is read from the document's VM".into(), None);
            return LayerKind::Group { layers: Vec::new(), mask: None, props: GroupProps { mask_mode: MaskMode::Alpha, stagger: Stagger::none(), size: None }, post: Default::default(), glyphs: false };
        };
        let assets = super::scene_assets(c.vm, at.obj);
        let scene = SceneSource::Object(makepad_stage_motion3d::doc::SceneObject::new(doc_vm, at.obj, c.map.prefix_cols));
        LayerKind::Scene3dDoc { assets, scene, loc: at.loc, written: crate::value::loading_editable(), size: self.size.size(c, at, "size"), textures, videos, layers }
    }

    #[cfg(not(feature = "scene3d"))]
    fn full_scene(&self, c: &mut Reader, at: &At) -> LayerKind {
        c.err(at, "", "a Scene3D with more than a `model` needs makepad-motion's `scene3d` feature (motion3d reads it)".to_string(), None);
        LayerKind::Group { layers: Vec::new(), mask: None, props: GroupProps { mask_mode: MaskMode::Alpha, stagger: Stagger::none(), size: None }, post: Default::default(), glyphs: false }
    }

    /// The glTF scene: `model`, `camera`, `lights`, `nodes`, `materials`.
    fn model_scene(&self, c: &mut Reader, at: &At) -> LayerKind {
        let model = if self.model.is_set() {
            self.model.media(c, at, "model", false).unwrap_or(Media::Empty)
        } else {
            c.err(at, "model", "a Scene3D needs `model: asset(id, rev)` of a GLB/glTF model".into(), None);
            Media::Empty
        };
        let mut camera = Camera3D {
            position: Prop::Const([0.0, 1.0, 4.0]),
            target: Prop::Const([0.0, 0.0, 0.0]),
            orbit: false,
            yaw: Prop::scalar(0.0),
            pitch: Prop::scalar(15.0),
            distance: Prop::scalar(4.0),
            fov: Prop::scalar(40.0),
        };
        match self.camera.look(c) {
            None => camera.orbit = true,
            Some(Look::Obj(cobj, _)) => {
                let cat = c.at(cobj, at.field("camera"), at.loc);
                c.unknown_fields(&cat, &["position", "target", "yaw", "pitch", "distance", "fov"], "camera");
                camera.orbit = c.get(cobj, "position").is_none();
                if c.get(cobj, "position").is_some() && (c.get(cobj, "yaw").is_some() || c.get(cobj, "distance").is_some()) {
                    c.err(&cat, "position", "a camera is placed by `position` or orbits by `yaw`/`pitch`/`distance`, not both".into(), None);
                }
                camera.position = c.prop::<3>(&cat, "position", [0.0, 1.0, 4.0]);
                camera.target = c.prop::<3>(&cat, "target", [0.0, 0.0, 0.0]);
                camera.yaw = c.prop::<1>(&cat, "yaw", [0.0]);
                camera.pitch = c.prop::<1>(&cat, "pitch", [15.0]);
                camera.distance = c.prop::<1>(&cat, "distance", [4.0]);
                camera.fov = c.prop::<1>(&cat, "fov", [40.0]);
            }
            Some(other) => c.err(at, "camera", format!("`camera` is `{{target: vec3(…) yaw: … pitch: … distance: … fov: …}}` or `{{position: vec3(…) target: vec3(…)}}`, not {}", other.kind()), None),
        }
        let mut lights = Vec::new();
        match self.lights.look(c) {
            None => {}
            Some(Look::Arr(items)) => {
                for (index, item) in items.iter().enumerate() {
                    let Look::Obj(lobj, _) = item else {
                        c.err(at, "lights", format!("light {index} is {}; a light is `{{direction: vec3(…) color: #fff intensity: 1}}`", item.kind()), None);
                        continue;
                    };
                    let lat = c.at(*lobj, format!("{}[{index}]", at.field("lights")), at.loc);
                    c.unknown_fields(&lat, &["type", "direction", "position", "color", "intensity"], "light");
                    let point = matches!(c.get(*lobj, "type"), Some(Look::Id(id)) if id == "point");
                    lights.push(Light3D {
                        point,
                        direction: c.prop::<3>(&lat, "direction", [-0.4, -1.0, -0.6]),
                        position: c.prop::<3>(&lat, "position", [0.0, 3.0, 3.0]),
                        color: c.prop::<4>(&lat, "color", [1.0, 1.0, 1.0, 1.0]),
                        intensity: c.prop::<1>(&lat, "intensity", [1.0]),
                    });
                }
            }
            Some(other) => c.err(at, "lights", format!("`lights` is an array of lights, not {}", other.kind()), None),
        }
        let mut nodes = Vec::new();
        if let Some(value) = self.nodes.look(c) {
            match &value {
                Look::Obj(nobj, _) => {
                    for (name, node) in c.fields(*nobj) {
                        let Look::Obj(node_obj, _) = node else {
                            let nat = c.at(*nobj, at.field("nodes"), at.loc);
                            c.err(&nat, &name, format!("node `{name}` is {}; write `{{rotate: vec3(…) translate: vec3(…) scale: vec3(…)}}`", node.kind()), None);
                            continue;
                        };
                        let nat = c.at(node_obj, format!("{}.{name}", at.field("nodes")), at.loc);
                        c.unknown_fields(&nat, &["translate", "rotate", "scale", "visible"], "node");
                        nodes.push(Node3D {
                            name,
                            translate: c.prop::<3>(&nat, "translate", [0.0, 0.0, 0.0]),
                            rotate: c.prop::<3>(&nat, "rotate", [0.0, 0.0, 0.0]),
                            scale: c.prop::<3>(&nat, "scale", [1.0, 1.0, 1.0]),
                            visible: c.prop::<1>(&nat, "visible", [1.0]),
                        });
                    }
                }
                other => c.err(at, "nodes", format!("`nodes` is an object of node name: transform, not {}", other.kind()), None),
            }
        }
        let mut materials = Vec::new();
        if let Some(value) = self.materials.look(c) {
            match &value {
                Look::Obj(mobj, _) => {
                    for (name, material) in c.fields(*mobj) {
                        let Look::Obj(material, _) = material else { continue };
                        let mat = c.at(material, format!("{}.{name}", at.field("materials")), at.loc);
                        c.unknown_fields(&mat, &["base_color", "metallic", "roughness", "emissive"], "material");
                        materials.push(Material3D {
                            name,
                            base_color: c.opt_prop::<4>(&mat, "base_color"),
                            metallic: c.opt_prop::<1>(&mat, "metallic"),
                            roughness: c.opt_prop::<1>(&mat, "roughness"),
                            emissive: c.opt_prop::<4>(&mat, "emissive"),
                        });
                    }
                }
                other => c.err(at, "materials", format!("`materials` is an object of material name: overrides, not {}", other.kind()), None),
            }
        }
        LayerKind::Scene3D {
            model,
            size: self.size.size(c, at, "size"),
            camera,
            lights,
            nodes,
            materials,
            background: self.background.opt(c, at, "background"),
            exposure: self.exposure.prop(c, at, "exposure", 1.0),
        }
    }
}
