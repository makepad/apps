//! GLB models: nodes, meshes, materials, skins, morph targets and clips.
//!
//! [`GlbModel::load`] reads a `.glb` (or `.gltf`) into plain owned data —
//! no `Cx`, no GPU — so a Scene3D can sample a clip at any `t` and pose,
//! skin and morph a mesh deterministically on the CPU ([`GlbPose`],
//! [`GlbModel::apply_clip`], [`GlbModel::posed_primitive`]). Textures are
//! decoded to RGBA8 (PNG, JPEG, and the UASTC / BC5 KTX2 images built
//! models carry); other images are skipped with a warning and their
//! materials keep their factors.

mod accessor;
mod anim;
#[cfg(test)]
mod tests;

pub use accessor::{read_accessor, Accessor};
pub use anim::{GlbPose, MeshSpace};

use makepad_geom3d::math::{compose, quat_normalize, Quat4, QUAT_ID};
use makepad_geom3d::mesh::Mesh;
use makepad_gltf::{load_gltf_from_bytes, load_image_bytes, JsonValue, LoadedGltf};
use makepad_widgets::*;
use std::path::Path;
use std::sync::Arc;

/// A loaded model.
#[derive(Clone, Debug, Default)]
pub struct GlbModel {
    pub nodes: Vec<GlbNode>,
    /// The nodes of the default scene (or every parentless node).
    pub roots: Vec<usize>,
    pub meshes: Vec<GlbMesh>,
    pub materials: Vec<GlbMaterial>,
    /// Decoded images by glTF image index; `None` = not decodable (bad data).
    pub images: Vec<Option<GlbImage>>,
    pub textures: Vec<GlbTexture>,
    pub skins: Vec<GlbSkin>,
    pub clips: Vec<GlbClip>,
    pub cameras: Vec<GlbCamera>,
    pub lights: Vec<GlbLight>,
    /// What was skipped or repaired while loading.
    pub warnings: Vec<String>,
}

/// A node of the hierarchy, with its rest transform.
#[derive(Clone, Debug)]
pub struct GlbNode {
    pub name: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub translation: [f32; 3],
    pub rotation: Quat4,
    pub scale: [f32; 3],
    pub mesh: Option<usize>,
    pub skin: Option<usize>,
    pub camera: Option<usize>,
    /// A `KHR_lights_punctual` light index.
    pub light: Option<usize>,
    /// Morph weights on the node (override the mesh's defaults).
    pub weights: Vec<f32>,
}

/// A mesh: its primitives and morph target defaults.
#[derive(Clone, Debug, Default)]
pub struct GlbMesh {
    pub name: String,
    pub primitives: Vec<GlbPrimitive>,
    /// Default morph weights.
    pub weights: Vec<f32>,
    /// `extras.targetNames`, when the exporter wrote them.
    pub target_names: Vec<String>,
}

/// One triangle list with one material.
#[derive(Clone, Debug, Default)]
pub struct GlbPrimitive {
    pub mesh: Mesh,
    pub material: Option<usize>,
    /// JOINTS_0 per vertex (empty = not skinned).
    pub joints: Vec<[u16; 4]>,
    /// WEIGHTS_0 per vertex, normalised to sum 1.
    pub weights: Vec<[f32; 4]>,
    pub targets: Vec<MorphTarget>,
}

/// Position and normal deltas of one morph target (empty = none).
#[derive(Clone, Debug, Default)]
pub struct MorphTarget {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}

/// A texture slot of a material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexRef {
    /// Index into [`GlbModel::textures`].
    pub texture: usize,
    pub texcoord: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum AlphaMode {
    #[default]
    Opaque,
    Mask(f32),
    Blend,
}

/// A glTF metallic-roughness material with the common KHR extensions.
#[derive(Clone, Debug)]
pub struct GlbMaterial {
    pub name: String,
    /// Linear RGBA.
    pub base_color: [f32; 4],
    pub base_color_texture: Option<TexRef>,
    pub metallic: f32,
    pub roughness: f32,
    pub metallic_roughness_texture: Option<TexRef>,
    pub normal_texture: Option<TexRef>,
    pub normal_scale: f32,
    pub occlusion_texture: Option<TexRef>,
    pub occlusion_strength: f32,
    /// Linear RGB.
    pub emissive: [f32; 3],
    pub emissive_texture: Option<TexRef>,
    /// `KHR_materials_emissive_strength`.
    pub emissive_strength: f32,
    pub alpha_mode: AlphaMode,
    pub double_sided: bool,
    /// `KHR_materials_unlit`.
    pub unlit: bool,
    /// `KHR_materials_transmission`.
    pub transmission: f32,
    /// `KHR_materials_ior`.
    pub ior: f32,
    /// `KHR_materials_volume`.
    pub thickness: f32,
    pub attenuation_color: [f32; 3],
    pub attenuation_distance: f32,
    /// `KHR_materials_clearcoat`.
    pub clearcoat: f32,
    pub clearcoat_roughness: f32,
    /// `KHR_materials_sheen`.
    pub sheen_color: [f32; 3],
    pub sheen_roughness: f32,
    /// `KHR_materials_specular` (factor, colour).
    pub specular: f32,
    pub specular_color: [f32; 3],
}

impl Default for GlbMaterial {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_color: [1.0; 4],
            base_color_texture: None,
            metallic: 1.0,
            roughness: 1.0,
            metallic_roughness_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            emissive: [0.0; 3],
            emissive_texture: None,
            emissive_strength: 1.0,
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
            unlit: false,
            transmission: 0.0,
            ior: 1.5,
            thickness: 0.0,
            attenuation_color: [1.0; 3],
            attenuation_distance: f32::INFINITY,
            clearcoat: 0.0,
            clearcoat_roughness: 0.0,
            sheen_color: [0.0; 3],
            sheen_roughness: 0.0,
            specular: 1.0,
            specular_color: [1.0; 3],
        }
    }
}

/// A decoded image, RGBA8 row-major, top row first.
#[derive(Clone, Debug)]
pub struct GlbImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Arc<Vec<u8>>,
    /// Used as colour (base colour, emissive, sheen): sample as sRGB.
    pub srgb: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Wrap {
    #[default]
    Repeat,
    Clamp,
    Mirror,
}

/// A texture: an image and how to sample it.
#[derive(Clone, Debug, Default)]
pub struct GlbTexture {
    /// Index into [`GlbModel::images`].
    pub image: Option<usize>,
    pub wrap_s: Wrap,
    pub wrap_t: Wrap,
    /// Nearest (magFilter 9728) instead of linear.
    pub nearest: bool,
}

/// A skin: its joints (node indices) and their inverse bind matrices.
#[derive(Clone, Debug, Default)]
pub struct GlbSkin {
    pub name: String,
    pub joints: Vec<usize>,
    pub inverse_bind: Vec<Mat4f>,
    pub skeleton: Option<usize>,
}

/// What a channel animates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelPath {
    Translation,
    Rotation,
    Scale,
    Weights,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    Step,
    Linear,
    CubicSpline,
}

/// One animated property of one node.
#[derive(Clone, Debug)]
pub struct Channel {
    pub node: usize,
    pub path: ChannelPath,
    pub interpolation: Interpolation,
    /// Key times in seconds, ascending.
    pub times: Vec<f32>,
    /// Flat values: per key `n` numbers (translation/scale 3, rotation 4,
    /// weights one per target), tripled (in-tangent, value, out-tangent) for
    /// cubic splines.
    pub values: Vec<f32>,
}

impl Channel {
    /// Numbers per key value.
    pub fn width(&self) -> usize {
        let keys = self.times.len().max(1);
        let per = if self.interpolation == Interpolation::CubicSpline { 3 } else { 1 };
        (self.values.len() / (keys * per)).max(1)
    }
}

/// A named animation.
#[derive(Clone, Debug, Default)]
pub struct GlbClip {
    pub name: String,
    /// The last key time over all channels.
    pub duration: f32,
    pub channels: Vec<Channel>,
}

#[derive(Clone, Debug)]
pub enum GlbProjection {
    /// Vertical field of view in radians, optional aspect, near, optional far.
    Perspective { yfov: f32, aspect: Option<f32>, znear: f32, zfar: Option<f32> },
    Orthographic { xmag: f32, ymag: f32, znear: f32, zfar: f32 },
}

#[derive(Clone, Debug)]
pub struct GlbCamera {
    pub name: String,
    pub projection: GlbProjection,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GlbLightKind {
    Directional,
    Point,
    /// Cone angles in radians.
    Spot { inner: f32, outer: f32 },
}

/// A `KHR_lights_punctual` light (it shines down its node's -z).
#[derive(Clone, Debug)]
pub struct GlbLight {
    pub name: String,
    pub kind: GlbLightKind,
    pub color: [f32; 3],
    /// Candela (point/spot) or lux (directional), as the file says.
    pub intensity: f32,
    pub range: Option<f32>,
}

// ---------------------------------------------------------------------------
// JSON helpers.
// ---------------------------------------------------------------------------

fn jnum(v: Option<&JsonValue>) -> Option<f32> {
    v.and_then(|v| v.as_f64()).map(|v| v as f32)
}

fn jusize(v: Option<&JsonValue>) -> Option<usize> {
    v.and_then(|v| v.as_u64()).map(|v| v as usize)
}

fn jarr(v: Option<&JsonValue>) -> &[JsonValue] {
    match v {
        Some(JsonValue::Array(a)) => a,
        _ => &[],
    }
}

fn jvec<const N: usize>(v: Option<&JsonValue>) -> Option<[f32; N]> {
    let a = jarr(v);
    if a.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (slot, item) in out.iter_mut().zip(a) {
        *slot = item.as_f64()? as f32;
    }
    Some(out)
}

fn ext<'a>(extensions: Option<&'a JsonValue>, name: &str) -> Option<&'a JsonValue> {
    extensions.and_then(|e| e.get(name))
}

fn wrap_of(mode: Option<u32>) -> Wrap {
    match mode {
        Some(33071) => Wrap::Clamp,
        Some(33648) => Wrap::Mirror,
        _ => Wrap::Repeat,
    }
}

/// The name in `names` closest to `wanted` (case-insensitive edit distance,
/// a prefix/substring match wins), when one is plausibly what was meant.
pub fn closest_name<'a>(wanted: &str, names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let lw = wanted.to_lowercase();
    let mut best: Option<(usize, &str)> = None;
    for name in names {
        if name.is_empty() {
            continue;
        }
        let ln = name.to_lowercase();
        let d = if ln.contains(&lw) || lw.contains(&ln) { 0 } else { makepad_tween::named::edit_distance(&lw, &ln) };
        if best.map_or(true, |(bd, _)| d < bd) {
            best = Some((d, name));
        }
    }
    let (d, name) = best?;
    (d <= (wanted.chars().count() / 2).max(2)).then_some(name)
}

/// Decompose a column-major affine matrix into translation, rotation, scale.
fn decompose(m: &[f32; 16]) -> ([f32; 3], Quat4, [f32; 3]) {
    let col = |c: usize| [m[c * 4], m[c * 4 + 1], m[c * 4 + 2]];
    let (c0, c1, c2) = (col(0), col(1), col(2));
    let mut s = [makepad_geom3d::mesh::len3(c0), makepad_geom3d::mesh::len3(c1), makepad_geom3d::mesh::len3(c2)];
    let det = makepad_geom3d::mesh::dot3(makepad_geom3d::mesh::cross3(c0, c1), c2);
    if det < 0.0 {
        s[0] = -s[0];
    }
    let n = |c: [f32; 3], s: f32| if s.abs() > 1e-20 { [c[0] / s, c[1] / s, c[2] / s] } else { [0.0; 3] };
    let (r0, r1, r2) = (n(c0, s[0]), n(c1, s[1]), n(c2, s[2]));
    let (m00, m01, m02) = (r0[0], r1[0], r2[0]);
    let (m10, m11, m12) = (r0[1], r1[1], r2[1]);
    let (m20, m21, m22) = (r0[2], r1[2], r2[2]);
    let trace = m00 + m11 + m22;
    let q = if trace > 0.0 {
        let k = (trace + 1.0).sqrt() * 2.0;
        [(m21 - m12) / k, (m02 - m20) / k, (m10 - m01) / k, 0.25 * k]
    } else if m00 > m11 && m00 > m22 {
        let k = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [0.25 * k, (m01 + m10) / k, (m02 + m20) / k, (m21 - m12) / k]
    } else if m11 > m22 {
        let k = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [(m01 + m10) / k, 0.25 * k, (m12 + m21) / k, (m02 - m20) / k]
    } else {
        let k = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [(m02 + m20) / k, (m12 + m21) / k, 0.25 * k, (m10 - m01) / k]
    };
    ([m[12], m[13], m[14]], quat_normalize(q), s)
}

fn decode_image(bytes: &[u8], mime: Option<&str>) -> Result<(usize, usize, Vec<u8>), String> {
    let is_png = bytes.starts_with(&[0x89, b'P', b'N', b'G']);
    let is_jpg = bytes.starts_with(&[0xFF, 0xD8]);
    // Load-ready images (UASTC or BC5 KTX2, what built models carry):
    // level 0, decoded on the CPU like the PNGs.
    let buffer = if let Some(decoded) = makepad_render::texture_pack::image_from_ktx2(bytes) {
        decoded.map_err(|e| format!("ktx2: {e}"))?
    } else if mime == Some("image/ktx2") {
        return Err("a KTX2 image that is neither UASTC nor BC5".into());
    } else if is_png || mime == Some("image/png") {
        ImageBuffer::from_png(bytes).map_err(|e| format!("png: {e:?}"))?
    } else if is_jpg || mime == Some("image/jpeg") {
        ImageBuffer::from_jpg(bytes).map_err(|e| format!("jpeg: {e:?}"))?
    } else {
        return Err(format!("unsupported image type {}", mime.unwrap_or("(unknown)")));
    };
    let (w, h) = (buffer.width, buffer.height);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in buffer.data.iter().take(w * h) {
        rgba.extend_from_slice(&[(px >> 16) as u8, (px >> 8) as u8, *px as u8, (px >> 24) as u8]);
    }
    Ok((w, h, rgba))
}

impl GlbModel {
    /// Read a `.glb` or `.gltf` (external files resolve against `base_dir`).
    /// Hard errors (bad container, broken accessors of a mesh) fail the load;
    /// what can be skipped is skipped and named in [`GlbModel::warnings`].
    pub fn load(bytes: &[u8], base_dir: Option<&Path>) -> Result<GlbModel, String> {
        let gltf = load_gltf_from_bytes(bytes, base_dir).map_err(|e| e.to_string())?;
        Self::from_loaded(&gltf)
    }

    /// Build from an already loaded glTF.
    pub fn from_loaded(gltf: &LoadedGltf) -> Result<GlbModel, String> {
        let doc = &gltf.document;
        let mut model = GlbModel::default();

        // Nodes.
        for (i, n) in doc.nodes_slice().iter().enumerate() {
            let (translation, rotation, scale) = match &n.matrix {
                Some(m) => decompose(m),
                None => (
                    n.translation.unwrap_or([0.0; 3]),
                    n.rotation.map(quat_normalize).unwrap_or(QUAT_ID),
                    n.scale.unwrap_or([1.0; 3]),
                ),
            };
            let light = ext(n.extensions.as_ref(), "KHR_lights_punctual").and_then(|l| jusize(l.get("light")));
            model.nodes.push(GlbNode {
                name: n.name.clone().unwrap_or_else(|| format!("node{i}")),
                parent: None,
                children: n.children.clone().unwrap_or_default(),
                translation,
                rotation,
                scale,
                mesh: n.mesh,
                skin: n.skin,
                camera: n.camera,
                light,
                weights: n.weights.clone().unwrap_or_default(),
            });
        }
        let count = model.nodes.len();
        for i in 0..count {
            let children = std::mem::take(&mut model.nodes[i].children);
            let mut kept = Vec::new();
            for c in children {
                if c >= count || c == i {
                    model.warnings.push(format!("node {i}: child {c} is not a valid node"));
                } else if model.nodes[c].parent.is_some() {
                    model.warnings.push(format!("node {c} has two parents; kept the first"));
                } else {
                    model.nodes[c].parent = Some(i);
                    kept.push(c);
                }
            }
            model.nodes[i].children = kept;
        }
        let scene = doc.scene.or(if doc.scenes_slice().is_empty() { None } else { Some(0) });
        model.roots = match scene.and_then(|s| doc.scenes_slice().get(s)).and_then(|s| s.nodes.clone()) {
            Some(nodes) => nodes.into_iter().filter(|&n| n < count).collect(),
            None => (0..count).filter(|&n| model.nodes[n].parent.is_none()).collect(),
        };

        // Textures and images.
        for t in doc.textures_slice() {
            let sampler = t.sampler.and_then(|s| doc.samplers_slice().get(s));
            model.textures.push(GlbTexture {
                image: t.source,
                wrap_s: wrap_of(sampler.and_then(|s| s.wrap_s)),
                wrap_t: wrap_of(sampler.and_then(|s| s.wrap_t)),
                nearest: sampler.and_then(|s| s.mag_filter) == Some(9728),
            });
            if t.source.is_none() {
                model.warnings.push(format!(
                    "texture `{}` has no image source; its materials keep their factors",
                    t.name.clone().unwrap_or_default()
                ));
            }
        }
        for (i, image) in doc.images_slice().iter().enumerate() {
            let decoded = load_image_bytes(gltf, i)
                .map_err(|e| e.to_string())
                .and_then(|bytes| decode_image(&bytes, image.mime_type.as_deref()));
            match decoded {
                Ok((width, height, rgba)) => model.images.push(Some(GlbImage { width, height, rgba: Arc::new(rgba), srgb: false })),
                Err(e) => {
                    model.warnings.push(format!("image {i} `{}`: {e}; skipped", image.name.clone().unwrap_or_default()));
                    model.images.push(None);
                }
            }
        }

        // Materials.
        for (i, m) in doc.materials_slice().iter().enumerate() {
            let mut out = GlbMaterial { name: m.name.clone().unwrap_or_else(|| format!("material{i}")), ..GlbMaterial::default() };
            if let Some(pbr) = &m.pbr_metallic_roughness {
                out.base_color = pbr.base_color_factor.unwrap_or([1.0; 4]);
                out.metallic = pbr.metallic_factor.unwrap_or(1.0);
                out.roughness = pbr.roughness_factor.unwrap_or(1.0);
                out.base_color_texture = pbr.base_color_texture.as_ref().map(|t| TexRef { texture: t.index, texcoord: t.tex_coord.unwrap_or(0) });
                out.metallic_roughness_texture =
                    pbr.metallic_roughness_texture.as_ref().map(|t| TexRef { texture: t.index, texcoord: t.tex_coord.unwrap_or(0) });
            }
            if let Some(t) = &m.normal_texture {
                out.normal_texture = Some(TexRef { texture: t.index, texcoord: t.tex_coord.unwrap_or(0) });
                out.normal_scale = t.scale.unwrap_or(1.0);
            }
            if let Some(t) = &m.occlusion_texture {
                out.occlusion_texture = Some(TexRef { texture: t.index, texcoord: t.tex_coord.unwrap_or(0) });
                out.occlusion_strength = t.strength.unwrap_or(1.0);
            }
            out.emissive_texture = m.emissive_texture.as_ref().map(|t| TexRef { texture: t.index, texcoord: t.tex_coord.unwrap_or(0) });
            out.emissive = m.emissive_factor.unwrap_or([0.0; 3]);
            out.alpha_mode = match m.alpha_mode.as_deref() {
                Some("MASK") => AlphaMode::Mask(m.alpha_cutoff.unwrap_or(0.5)),
                Some("BLEND") => AlphaMode::Blend,
                _ => AlphaMode::Opaque,
            };
            out.double_sided = m.double_sided.unwrap_or(false);
            let exts = m.extensions.as_ref();
            out.unlit = ext(exts, "KHR_materials_unlit").is_some();
            if let Some(e) = ext(exts, "KHR_materials_emissive_strength") {
                out.emissive_strength = jnum(e.get("emissiveStrength")).unwrap_or(1.0);
            }
            if let Some(e) = ext(exts, "KHR_materials_transmission") {
                out.transmission = jnum(e.get("transmissionFactor")).unwrap_or(0.0);
            }
            if let Some(e) = ext(exts, "KHR_materials_ior") {
                out.ior = jnum(e.get("ior")).unwrap_or(1.5);
            }
            if let Some(e) = ext(exts, "KHR_materials_volume") {
                out.thickness = jnum(e.get("thicknessFactor")).unwrap_or(0.0);
                out.attenuation_color = jvec::<3>(e.get("attenuationColor")).unwrap_or([1.0; 3]);
                out.attenuation_distance = jnum(e.get("attenuationDistance")).unwrap_or(f32::INFINITY);
            }
            if let Some(e) = ext(exts, "KHR_materials_clearcoat") {
                out.clearcoat = jnum(e.get("clearcoatFactor")).unwrap_or(0.0);
                out.clearcoat_roughness = jnum(e.get("clearcoatRoughnessFactor")).unwrap_or(0.0);
            }
            if let Some(e) = ext(exts, "KHR_materials_sheen") {
                out.sheen_color = jvec::<3>(e.get("sheenColorFactor")).unwrap_or([0.0; 3]);
                out.sheen_roughness = jnum(e.get("sheenRoughnessFactor")).unwrap_or(0.0);
            }
            if let Some(e) = ext(exts, "KHR_materials_specular") {
                out.specular = jnum(e.get("specularFactor")).unwrap_or(1.0);
                out.specular_color = jvec::<3>(e.get("specularColorFactor")).unwrap_or([1.0; 3]);
            }
            model.materials.push(out);
        }
        // Colour images sample as sRGB.
        let srgb_textures: Vec<usize> =
            model.materials.iter().flat_map(|m| [m.base_color_texture, m.emissive_texture]).flatten().map(|t| t.texture).collect();
        for t in srgb_textures {
            if let Some(Some(img)) = model.textures.get(t).and_then(|t| t.image).and_then(|i| model.images.get_mut(i)) {
                img.srgb = true;
            }
        }

        // Meshes.
        for (mi, m) in doc.meshes_slice().iter().enumerate() {
            let mut mesh = GlbMesh {
                name: m.name.clone().unwrap_or_else(|| format!("mesh{mi}")),
                weights: m.weights.clone().unwrap_or_default(),
                target_names: jarr(m.extras.as_ref().and_then(|e| e.get("targetNames"))).iter().filter_map(|v| v.as_str().map(String::from)).collect(),
                primitives: Vec::new(),
            };
            for (pi, p) in m.primitives.iter().enumerate() {
                match read_primitive(gltf, p) {
                    Ok(Some(prim)) => mesh.primitives.push(prim),
                    Ok(None) => model.warnings.push(format!("mesh `{}` primitive {pi}: mode {} is not triangles; skipped", mesh.name, p.mode())),
                    Err(e) => return Err(format!("mesh `{}` primitive {pi}: {e}", mesh.name)),
                }
            }
            model.meshes.push(mesh);
        }

        // Skins.
        for (si, s) in doc.skins.as_deref().unwrap_or(&[]).iter().enumerate() {
            let joints: Vec<usize> = jarr(s.get("joints")).iter().filter_map(|j| j.as_u64().map(|j| j as usize)).collect();
            if let Some(bad) = joints.iter().find(|&&j| j >= count) {
                return Err(format!("skin {si}: joint {bad} is not a node"));
            }
            let inverse_bind = match jusize(s.get("inverseBindMatrices")) {
                Some(a) => {
                    let acc = read_accessor(gltf, a).map_err(|e| format!("skin {si}: {e}"))?;
                    (0..acc.count)
                        .map(|i| {
                            let mut v = [0.0f32; 16];
                            for (k, slot) in v.iter_mut().enumerate() {
                                *slot = acc.get(i, k);
                            }
                            Mat4f { v }
                        })
                        .collect()
                }
                None => vec![Mat4f::identity(); joints.len()],
            };
            model.skins.push(GlbSkin {
                name: s.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                inverse_bind,
                skeleton: jusize(s.get("skeleton")),
                joints,
            });
        }

        // Clips.
        for (ai, a) in doc.animations.as_deref().unwrap_or(&[]).iter().enumerate() {
            let name = a.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or_else(|| format!("clip{ai}"));
            let samplers = jarr(a.get("samplers"));
            let mut clip = GlbClip { name, duration: 0.0, channels: Vec::new() };
            for ch in jarr(a.get("channels")) {
                let target = ch.get("target");
                let Some(node) = jusize(target.and_then(|t| t.get("node"))) else { continue };
                if node >= count {
                    model.warnings.push(format!("clip `{}`: channel targets missing node {node}", clip.name));
                    continue;
                }
                let path = match target.and_then(|t| t.get("path")).and_then(|p| p.as_str()) {
                    Some("translation") => ChannelPath::Translation,
                    Some("rotation") => ChannelPath::Rotation,
                    Some("scale") => ChannelPath::Scale,
                    Some("weights") => ChannelPath::Weights,
                    other => {
                        model.warnings.push(format!("clip `{}`: channel path {other:?} not supported", clip.name));
                        continue;
                    }
                };
                let Some(sampler) = jusize(ch.get("sampler")).and_then(|s| samplers.get(s)) else {
                    model.warnings.push(format!("clip `{}`: channel without a sampler", clip.name));
                    continue;
                };
                let interpolation = match sampler.get("interpolation").and_then(|i| i.as_str()) {
                    Some("STEP") => Interpolation::Step,
                    Some("CUBICSPLINE") => Interpolation::CubicSpline,
                    _ => Interpolation::Linear,
                };
                let (Some(input), Some(output)) = (jusize(sampler.get("input")), jusize(sampler.get("output"))) else { continue };
                let times = read_accessor(gltf, input).map_err(|e| format!("clip `{}`: {e}", clip.name))?.flat();
                let values = read_accessor(gltf, output).map_err(|e| format!("clip `{}`: {e}", clip.name))?.flat();
                if times.is_empty() || values.is_empty() {
                    continue;
                }
                if let Some(&last) = times.last() {
                    clip.duration = clip.duration.max(last);
                }
                clip.channels.push(Channel { node, path, interpolation, times, values });
            }
            model.clips.push(clip);
        }

        // Cameras and lights.
        for (i, c) in doc.cameras_slice().iter().enumerate() {
            let projection = if let Some(p) = &c.perspective {
                GlbProjection::Perspective { yfov: p.yfov, aspect: p.aspect_ratio, znear: p.znear, zfar: p.zfar }
            } else if let Some(o) = &c.orthographic {
                GlbProjection::Orthographic { xmag: o.xmag, ymag: o.ymag, znear: o.znear, zfar: o.zfar }
            } else {
                model.warnings.push(format!("camera {i}: no projection"));
                continue;
            };
            model.cameras.push(GlbCamera { name: c.name.clone().unwrap_or_else(|| format!("camera{i}")), projection });
        }
        let lights = ext(doc.extensions.as_ref(), "KHR_lights_punctual").map(|l| jarr(l.get("lights"))).unwrap_or(&[]);
        for (i, l) in lights.iter().enumerate() {
            let kind = match l.get("type").and_then(|t| t.as_str()) {
                Some("directional") => GlbLightKind::Directional,
                Some("spot") => {
                    let spot = l.get("spot");
                    GlbLightKind::Spot {
                        inner: jnum(spot.and_then(|s| s.get("innerConeAngle"))).unwrap_or(0.0),
                        outer: jnum(spot.and_then(|s| s.get("outerConeAngle"))).unwrap_or(std::f32::consts::FRAC_PI_4),
                    }
                }
                _ => GlbLightKind::Point,
            };
            model.lights.push(GlbLight {
                name: l.get("name").and_then(|n| n.as_str()).map(String::from).unwrap_or_else(|| format!("light{i}")),
                kind,
                color: jvec::<3>(l.get("color")).unwrap_or([1.0; 3]),
                intensity: jnum(l.get("intensity")).unwrap_or(1.0),
                range: jnum(l.get("range")),
            });
        }
        Ok(model)
    }

    pub fn node_by_name(&self, name: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.name == name)
    }

    pub fn clip_by_name(&self, name: &str) -> Option<usize> {
        self.clips.iter().position(|c| c.name == name)
    }

    pub fn material_by_name(&self, name: &str) -> Option<usize> {
        self.materials.iter().position(|m| m.name == name)
    }

    /// Morph target index of mesh `mesh` by name (`extras.targetNames`).
    pub fn target_by_name(&self, mesh: usize, name: &str) -> Option<usize> {
        self.meshes.get(mesh)?.target_names.iter().position(|n| n == name)
    }

    /// Every drawable (node, mesh) pair reachable from [`GlbModel::roots`],
    /// parents before children.
    pub fn scene_nodes(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut seen = vec![false; self.nodes.len()];
        let mut stack: Vec<usize> = self.roots.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            if std::mem::replace(&mut seen[n], true) {
                continue;
            }
            if let Some(m) = self.nodes[n].mesh.filter(|&m| m < self.meshes.len()) {
                out.push((n, m));
            }
            stack.extend(self.nodes[n].children.iter().rev());
        }
        out
    }

    /// The local rest matrix of a node.
    pub fn rest_local(&self, node: usize) -> Mat4f {
        let n = &self.nodes[node];
        compose(n.translation, n.rotation, n.scale)
    }
}

fn read_primitive(gltf: &LoadedGltf, p: &makepad_gltf::GltfPrimitive) -> Result<Option<GlbPrimitive>, String> {
    let mode = p.mode();
    if !(4..=6).contains(&mode) {
        return Ok(None);
    }
    let attr = |name: &str| -> Result<Option<Accessor>, String> {
        match p.attributes.get(name) {
            Some(&a) => read_accessor(gltf, a).map(Some).map_err(|e| format!("{name}: {e}")),
            None => Ok(None),
        }
    };
    let positions = attr("POSITION")?.ok_or("no POSITION attribute")?.vec3();
    let n = positions.len();
    let mut indices: Vec<u32> = match p.indices {
        Some(i) => read_accessor(gltf, i)?.u32s(),
        None => (0..n as u32).collect(),
    };
    indices = match mode {
        5 => {
            let mut out = Vec::new();
            for k in 2..indices.len() {
                let (a, b, c) = (indices[k - 2], indices[k - 1], indices[k]);
                if k % 2 == 0 { out.extend_from_slice(&[a, b, c]) } else { out.extend_from_slice(&[b, a, c]) }
            }
            out
        }
        6 => {
            let mut out = Vec::new();
            for k in 2..indices.len() {
                out.extend_from_slice(&[indices[0], indices[k - 1], indices[k]]);
            }
            out
        }
        _ => {
            indices.truncate(indices.len() / 3 * 3);
            indices
        }
    };
    if let Some(bad) = indices.iter().find(|&&i| i as usize >= n) {
        return Err(format!("index {bad} out of range ({n} vertices)"));
    }
    let mut mesh = Mesh { positions, indices, ..Mesh::default() };
    mesh.uvs = attr("TEXCOORD_0")?.map(|a| a.vec2()).filter(|v| v.len() == n).unwrap_or_else(|| vec![[0.0, 0.0]; n]);
    if let Some(c) = attr("COLOR_0")?.filter(|a| a.count == n) {
        mesh.colors = if c.comps == 3 { c.vec3().into_iter().map(|v| [v[0], v[1], v[2], 1.0]).collect() } else { c.vec4() };
    }
    match attr("NORMAL")?.map(|a| a.vec3()).filter(|v| v.len() == n) {
        Some(normals) => mesh.normals = normals.into_iter().map(makepad_geom3d::mesh::normalize_or_up).collect(),
        None => mesh.compute_normals(),
    }
    if let Some(t) = attr("TANGENT")?.filter(|a| a.count == n) {
        mesh.tangents = t.vec4().into_iter().map(|t| [t[0], t[1], t[2], if t[3] < 0.0 { -1.0 } else { 1.0 }]).collect();
    }
    let mut joints = Vec::new();
    let mut weights = Vec::new();
    if let (Some(j), Some(w)) = (attr("JOINTS_0")?, attr("WEIGHTS_0")?) {
        if j.count == n && w.count == n {
            joints = j.vec4().into_iter().map(|v| [v[0] as u16, v[1] as u16, v[2] as u16, v[3] as u16]).collect();
            weights = w
                .vec4()
                .into_iter()
                .map(|v| {
                    let s = v[0] + v[1] + v[2] + v[3];
                    if s > 1e-8 { [v[0] / s, v[1] / s, v[2] / s, v[3] / s] } else { [1.0, 0.0, 0.0, 0.0] }
                })
                .collect();
        }
    }
    let mut targets = Vec::new();
    for t in p.targets.as_deref().unwrap_or(&[]) {
        let get = |name: &str| -> Result<Vec<[f32; 3]>, String> {
            match t.get(name) {
                Some(&a) => {
                    let v = read_accessor(gltf, a).map_err(|e| format!("target {name}: {e}"))?.vec3();
                    Ok(if v.len() == n { v } else { Vec::new() })
                }
                None => Ok(Vec::new()),
            }
        };
        targets.push(MorphTarget { positions: get("POSITION")?, normals: get("NORMAL")? });
    }
    Ok(Some(GlbPrimitive { mesh, material: p.material, joints, weights, targets }))
}
