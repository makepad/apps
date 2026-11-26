//! The Scene3D renderer: a Scene3D frame as a `makepad_scene::World`,
//! drawn by `makepad_render` (the lanes, shadows, clustered lights and
//! materials Sandbox uses), its lines and points by `makepad_render_lines`,
//! its post by `makepad_render_graph` (kit passes, the tone map, sub-frame
//! accumulation), to premultiplied sRGB BGRA8 or, for a host that tone maps
//! a composed frame itself, linear RGBA16F.
//!
//! [`to_world`] is the conversion, pure and testable; [`SharedRenderer`]
//! records a frame's chain inside the host's draw (`draw`, `painted`,
//! `output`, `reattach`). What a frame uses that the renderer does not draw
//! is listed per frame in [`SharedRenderer::gaps`], not a silent difference.
use crate::frame::*;
use crate::scene::{Blending, FaceSide, ShadingModel, SpriteShape};
use makepad_render::{GeometryData, Renderer};
use makepad_render_lines as lines;
use makepad_render_graph::{Grade, ToneCurve};
use crate::scene::ToneMapping;
use makepad_scene as scene;
use makepad_widgets::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn v3(a: [f32; 3]) -> Vec3f {
    vec3f(a[0], a[1], a[2])
}

/// Give back the storage of an effect's targets while it is off (their
/// handles stay; the effect's next frame allocates them again).
fn release_targets(cx: &mut Cx, targets: &[&Texture]) {
    for t in targets {
        if t.allocated_size(cx).is_some() {
            t.release(cx);
        }
    }
}

fn hash64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// A motion3d material as the shared lanes' material, and the parts of it
/// they cannot show yet.
/// `far` is the camera's far plane (a Depth material's white-to-black).
/// The host's named textures as materials show them: each one's picture
/// size in pixels and the crop the host wants shown.
pub type Pictures = HashMap<String, ((f32, f32), Option<[f32; 4]>)>;

/// `pictures` holds the host textures a material's `texture: "name"` may
/// show (a screen); a name not in it draws the material's own map.
pub fn to_material(id: scene::MaterialId, m: &MaterialFrame, far: f32, pictures: &Pictures) -> (scene::MaterialFrame, Vec<&'static str>) {
    let mut gaps = Vec::new();
    let color = vec4(m.color[0], m.color[1], m.color[2], m.color[3] * m.opacity);
    let generated = m.shader.is_none() && (m.wireframe || fitted(m) || matches!(m.model, ShadingModel::Toon | ShadingModel::Matcap | ShadingModel::Normal | ShadingModel::Depth));
    // A generated program's params: a fitted picture's placement (uv
    // offset, scale), the Depth material's far plane.
    let params = if !generated {
        vec4(m.custom[0], m.custom[1], m.custom[2], m.custom[3])
    } else if fitted(m) {
        let (size, crop) = match m.external.as_ref().and_then(|n| pictures.get(n)) {
            Some((size, crop)) => (Some(*size), m.crop.or(*crop)),
            None => (m.maps.base.as_ref().map(|i| (i.width as f32, i.height as f32)), m.crop),
        };
        let (p, _) = uv_placement(m.fit, m.aspect, size, crop);
        vec4(p[0], p[1], p[2], p[3])
    } else {
        vec4(far, 0.0, 0.0, 0.0)
    };
    let kind = match m.model {
        _ if m.shader.is_some() || generated => scene::MaterialKind::Splash(scene::SplashMaterial {
            base: scene::BaseKind::Pbr,
            program: program_id(m),
            uniforms: [params, vec4(m.custom[4], m.custom[5], m.custom[6], m.custom[7]), Vec4f::default(), Vec4f::default()],
            // A screen's picture is the host's texture, bound on the program.
            textures: if m.external.is_some() { Vec::new() } else { map_ref(&m.maps.base).into_iter().collect() },
        }),
        // Unlit: the colour plus what it emits (both linear, HDR).
        ShadingModel::Basic => scene::MaterialKind::Unlit(scene::UnlitParams {
            color: vec4(color.x + m.emissive[0], color.y + m.emissive[1], color.z + m.emissive[2], color.w),
            intensity: 1.0,
            map: map_ref(&m.maps.base),
        }),
        model => {
            let (metallic, roughness) = if model == ShadingModel::Lambert { (0.0, 1.0) } else { (m.metalness, m.roughness) };
            scene::MaterialKind::Pbr(scene::PbrParams {
                base_color: color,
                metallic: metallic.clamp(0.0, 1.0),
                roughness: roughness.clamp(0.0, 1.0),
                emissive: v3(m.emissive),
                clearcoat: m.clearcoat.clamp(0.0, 1.0),
                flake: m.flakes[0].clamp(0.0, 1.0),
                rim: 0.0,
                base_map: map_ref(&m.maps.base),
                normal_map: map_ref(&m.maps.normal),
                normal_scale: m.normal_scale,
                metal_rough_map: map_ref(&m.maps.metal_rough),
                emissive_map: map_ref(&m.maps.emissive),
                occlusion_map: map_ref(&m.maps.occlusion),
                occlusion_strength: m.occlusion_strength.clamp(0.0, 1.0),
            })
        }
    };
    if m.external.is_some() && !fitted(m) {
        gaps.push("a screen (host texture) on a hooked, wireframe or toon/matcap/normal/depth material");
    }
    if m.maps.base.is_some() && (m.fit != crate::scene::MapFit::Fill || m.crop.is_some() || m.map_space != crate::scene::MapSpace::default()) && !fitted(m) {
        gaps.push("a fitted picture on a hooked, wireframe or toon/matcap/normal/depth material");
    }
    if m.shader.is_some() && (m.maps.normal.is_some() || m.maps.metal_rough.is_some() || m.maps.emissive.is_some()) {
        gaps.push("normal, metal-rough or emission maps on a ShaderMaterial");
    }
    if m.is_transmissive() && !glass(m) {
        gaps.push("transmission on a hooked, wireframe, fitted or toon/matcap/normal/depth material");
    }
    if m.sheen > 0.001 || m.iridescence > 0.001 {
        gaps.push("sheen, iridescence");
    }
    if m.wireframe && m.shader.is_some() {
        gaps.push("a ShaderMaterial drawn as wireframe (its hooks win)");
    }
    if (m.rim_power > 0.0 || m.fade.is_some()) && (m.shader.is_some() || m.wireframe || !matches!(m.model, ShadingModel::Standard | ShadingModel::Lambert | ShadingModel::Basic)) {
        gaps.push("rim or ground fade on a hooked, wireframe or toon/matcap/normal/depth material");
    }
    let blend = if let Some(cutoff) = m.alpha_cutoff {
        scene::Blend::Mask { cutoff: cutoff.clamp(0.0, 1.0) }
    } else if m.wireframe && m.shader.is_none() && m.wire_fill.is_none() {
        // Only the edges: the rest of each face is cut.
        scene::Blend::Mask { cutoff: 0.01 }
    } else {
        match m.blending {
            Blending::Additive => scene::Blend::Add,
            Blending::Multiply => scene::Blend::Multiply,
            Blending::Normal if m.is_blended() => scene::Blend::Over,
            Blending::Normal => scene::Blend::Opaque,
        }
    };
    let side = match m.side {
        FaceSide::Front => scene::Side::Front,
        FaceSide::Back => scene::Side::Back,
        FaceSide::Double => scene::Side::Double,
    };
    let frame = scene::MaterialFrame {
        id,
        kind,
        blend,
        side,
        depth_write: m.depth_write,
        glow: 0.0,
        lighting: if draws_unlit(m) { scene::LightingModel::Unlit } else { scene::LightingModel::Lit },
    };
    (frame, gaps)
}

/// A material map's image as the renderer's reference (its content key).
fn map_ref(image: &Option<crate::scene::ImageData>) -> Option<scene::TextureRef> {
    image.as_ref().map(|i| scene::TextureRef(i.key))
}

/// Every image the frame's materials name, once each.
fn frame_images(f: &SceneFrame) -> Vec<&crate::scene::ImageData> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in &f.meshes {
        let mp = &m.material.maps;
        for i in [&mp.base, &mp.metal_rough, &mp.normal, &mp.emissive, &mp.occlusion].into_iter().flatten() {
            if seen.insert(i.key) {
                out.push(i);
            }
        }
    }
    out
}

/// The Splash program a ShaderMaterial draws with: its hooks, and the
/// lane values they read that have no uniform on the shared lanes (baked
/// into the program as constants).
pub fn program_id(m: &MaterialFrame) -> scene::MaterialProgramId {
    // Edit mode: the values baked into the program are live slots of one
    // program (patched as they change, see `Renderer3d::live_programs`),
    // not programs of their own.
    let baked = !makepad_widgets::makepad_script::literal::edit_mode();
    let mut h = m.shader.as_ref().map_or(0, |s| s.key);
    // Basic draws unlit: another program.
    if m.model == ShadingModel::Basic {
        h = hash64(h ^ 0xba51c);
    }
    // Cut or blended: another variant.
    h = hash64(h ^ (m.alpha_cutoff.map_or(0, |c| c.to_bits() as u64 | 1 << 40)) ^ if m.is_blended() { 0xb1e0 } else { 0 });
    if fitted(m) {
        h = hash64(h ^ 0xf17 ^ (m.fit as u64) << 8 ^ (m.map_space as u64) << 12);
        // Each screen binds its own texture on its program.
        if let Some(name) = &m.external {
            h = hash64(h ^ LiveId::from_str(name).0);
        }
    }
    if m.shader.is_none() {
        h = hash64(h ^ (m.model as u64 + 1) * 0x9e37 ^ (m.steps as u64) << 20 ^ (m.matcap as u64) << 28 ^ if baked { m.roughness.to_bits() as u64 } else { 0 });
    }
    if m.wireframe && m.shader.is_none() {
        h = hash64(h ^ 0x3e1f ^ if m.wire_fill.is_some() { 2 } else { 0 });
        let fill = m.wire_fill.unwrap_or([0.0; 4]);
        if baked {
            for v in fill.iter().chain(&[m.wire_width, if m.wire_fill.is_some() { 1.0 } else { 0.0 }]) {
                h = hash64(h ^ v.to_bits() as u64);
            }
        }
    }
    if baked {
        for v in m.rim.iter().chain(&[m.rim_power]).chain(m.sheen_color.iter()).chain(m.color.iter()).chain(&[m.opacity, m.metalness, m.roughness]).chain(m.emissive.iter()) {
            h = hash64(h ^ v.to_bits() as u64);
        }
    }
    scene::MaterialProgramId(h)
}

/// A stable id for a material's values (the shared renderer keys its
/// uploaded models by it).
fn material_id(m: &scene::MaterialFrame) -> scene::MaterialId {
    let s = format!("{:?}{:?}{:?}{:?}{:?}", m.kind, m.blend, m.side, m.depth_write, m.lighting);
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    scene::MaterialId(h)
}

/// The geometry id a mesh draw is resident under (a deformed mesh under
/// its deformers' values too: another shape).
pub fn geometry_id(draw: &MeshDraw) -> scene::GeometryId {
    let mut h = hash64(draw.geo_key ^ if draw.dynamic { 0xD1 } else { 0 });
    if draw.material.wireframe {
        h = hash64(h ^ 0xba7c);
    }
    if deformed(draw) {
        for v in draw.deform.iter().chain(&draw.deform2).chain(&draw.deform3).chain(&draw.deform4) {
            h = hash64(h ^ v.to_bits() as u64);
        }
    }
    scene::GeometryId(h)
}

/// Whether any of the mesh deformers is on: displace, twist, bend,
/// explode, wrap, ripple, curl.
pub fn deformed(m: &MeshDraw) -> bool {
    m.deform[0] != 0.0 || m.deform[3] != 0.0 || m.deform2[0] != 0.0 || m.deform2[2] != 0.0 || m.deform2[3] != 0.0 || m.deform3[0] != 0.0 || m.deform4[0] > 0.0
}

/// The mesh deformers, as the gpu/ lane's vertex stage computes them, on
/// the mesh's vertices: the local position, and the normal of the deformed
/// surface by finite differences along two tangents.
pub fn deform_mesh(mesh: &makepad_geom3d::mesh::Mesh, m: &MeshDraw) -> makepad_geom3d::mesh::Mesh {
    type V = [f32; 3];
    fn add(a: V, b: V) -> V {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }
    fn sub(a: V, b: V) -> V {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn mul(a: V, k: f32) -> V {
        [a[0] * k, a[1] * k, a[2] * k]
    }
    fn dot(a: V, b: V) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn cross(a: V, b: V) -> V {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    }
    fn len(a: V) -> f32 {
        dot(a, a).sqrt()
    }
    fn norm(a: V) -> V {
        let l = len(a);
        if l > 0.0 { mul(a, 1.0 / l) } else { a }
    }
    fn hash3(p: V) -> f32 {
        let x = dot(p, [12.9898, 78.233, 37.719]).sin() * 43758.5453;
        x - x.floor()
    }
    fn vnoise(p: V) -> f32 {
        let i = [p[0].floor(), p[1].floor(), p[2].floor()];
        let f = sub(p, i);
        let u = [f[0] * f[0] * (3.0 - 2.0 * f[0]), f[1] * f[1] * (3.0 - 2.0 * f[1]), f[2] * f[2] * (3.0 - 2.0 * f[2])];
        let n = |x: f32, y: f32, z: f32| hash3(add(i, [x, y, z]));
        let l = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let x00 = l(n(0.0, 0.0, 0.0), n(1.0, 0.0, 0.0), u[0]);
        let x10 = l(n(0.0, 1.0, 0.0), n(1.0, 1.0, 0.0), u[0]);
        let x01 = l(n(0.0, 0.0, 1.0), n(1.0, 0.0, 1.0), u[0]);
        let x11 = l(n(0.0, 1.0, 1.0), n(1.0, 1.0, 1.0), u[0]);
        l(l(x00, x10, u[1]), l(x01, x11, u[1]), u[2]) * 2.0 - 1.0
    }
    fn fbm(p: V) -> f32 {
        vnoise(p) * 0.6 + vnoise(add(mul(p, 2.03), [3.1, 1.7, 5.3])) * 0.3 + vnoise(add(mul(p, 4.1), [7.7, 2.2, 1.1])) * 0.1
    }
    fn rot_y(p: V, a: f32) -> V {
        let (s, c) = a.sin_cos();
        [p[0] * c + p[2] * s, p[1], -p[0] * s + p[2] * c]
    }
    fn rot_z(p: V, a: f32) -> V {
        let (s, c) = a.sin_cos();
        [p[0] * c - p[1] * s, p[0] * s + p[1] * c, p[2]]
    }
    use std::f32::consts::{PI, TAU};
    const DEG: f32 = 0.017_453_3;
    let (d1, d2, d3, d4) = (m.deform, m.deform2, m.deform3, m.deform4);
    let pos = |p0: V, n: V, tangent: Option<[f32; 4]>| -> V {
        let mut p = p0;
        // Explode: triangles (their centre in the tangent, w = 2) fly out
        // from the origin and shrink toward their centre.
        if let Some(t) = tangent.filter(|t| d2[2].abs() > 0.0 && t[3] > 1.5) {
            let c = [t[0], t[1], t[2]];
            let e = d2[2];
            let h = hash3(mul(c, 13.0));
            let jitter = mul(sub([hash3(mul(c, 7.0)), hash3(mul(c, 11.0)), h], [0.5, 0.5, 0.5]), 0.8);
            let dir = norm(add(add(c, jitter), [0.0, 0.0001, 0.0]));
            let shrink = (e.abs() * 0.35).clamp(0.0, 0.9);
            p = add(add(p, mul(sub(c, p), shrink)), mul(dir, e * (0.6 + h)));
            p = add(c, rot_y(sub(p, c), e * (h - 0.5) * 3.0));
        }
        if d3[0].abs() > 0.0 {
            let r = len(p);
            p = add(p, mul(n, (r * d3[1] * TAU - d3[2] * TAU).sin() * d3[0]));
        }
        if d4[0] > 0.0 {
            // Page curl: past the curl line the sheet wraps round a cylinder
            // of the curl radius and comes back over itself.
            let r = d4[1];
            let xc = d4[2] - d4[0] * (d4[3] + PI * r);
            if p[0] > xc {
                let a = (p[0] - xc) / r;
                p = if a < PI { [xc + r * a.sin(), p[1], p[2] + r * (1.0 - a.cos())] } else { [xc - (p[0] - xc - PI * r), p[1], p[2] + 2.0 * r] };
            }
        }
        if d1[0].abs() > 0.0 {
            p = add(p, mul(n, fbm(add(mul(p, d1[1]), [d1[2], d1[2] * 0.7, 0.0])) * d1[0]));
        }
        if d1[3].abs() > 0.0 {
            p = rot_y(p, p[1] * d1[3] * DEG);
        }
        if d2[0].abs() > 0.0 {
            p = rot_z(p, d2[0] * DEG * (p[1] / d2[1].max(0.0001)));
        }
        if d2[3] > 0.0 {
            // Wrap: x runs around a vertical cylinder of this radius (its
            // axis behind the geometry, at z = -radius).
            let r = d2[3];
            let a = p[0] / r;
            p = [a.sin() * (r + p[2]), p[1], a.cos() * (r + p[2]) - r];
        }
        p
    };
    let mut out = mesh.clone();
    let has_normals = mesh.normals.len() == mesh.positions.len();
    for (i, p0) in mesh.positions.iter().enumerate() {
        let n0 = if has_normals { mesh.normals[i] } else { [0.0, 1.0, 0.0] };
        let t = mesh.tangents.get(i).copied();
        let p = pos(*p0, n0, t);
        out.positions[i] = p;
        if has_normals {
            let hp = if n0[1].abs() > 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
            let e1 = norm(cross(n0, hp));
            let e2 = cross(n0, e1);
            let h = 0.01;
            let p1 = pos(add(*p0, mul(e1, h)), n0, t);
            let p2 = pos(add(*p0, mul(e2, h)), n0, t);
            let nn = cross(sub(p1, p), sub(p2, p));
            if len(nn) > 1e-7 {
                out.normals[i] = norm(nn);
            }
        }
    }
    out
}

pub fn geometry_data(mesh: &makepad_geom3d::mesh::Mesh) -> GeometryData {
    let n = mesh.positions.len();
    GeometryData {
        positions: mesh.positions.clone(),
        normals: if mesh.normals.len() == n { mesh.normals.clone() } else { vec![[0.0, 1.0, 0.0]; n] },
        uvs: if mesh.uvs.len() == n { mesh.uvs.clone() } else { Vec::new() },
        colors: if mesh.colors.len() == n { mesh.colors.clone() } else { Vec::new() },
        indices: mesh.indices.clone(),
    }
}

/// The frame's world: lights, environment, fog, materials and mesh items.
/// Geometry is referenced by [`geometry_id`]; the host makes it resident.
/// Returns the world and the features it could not express.
pub fn to_world(f: &SceneFrame, pictures: &Pictures) -> (scene::World, Vec<&'static str>) {
    let mut w = scene::World::new();
    let mut gaps: Vec<&'static str> = Vec::new();
    let gap = |g: &'static str, gaps: &mut Vec<&'static str>| {
        if !gaps.contains(&g) {
            gaps.push(g);
        }
    };
    // The key light is the first directional one; the ambient is the
    // hemisphere (or the flat ambient above and below). The lanes' diffuse
    // lobe carries no 1/pi (lights are irradiance/pi units), motion3d's
    // does: direct lights are divided by pi to shade the same.
    const INV_PI: f32 = std::f32::consts::FRAC_1_PI;
    let mut sun = false;
    for l in &f.lights {
        match l.kind {
            LightType::Directional if !sun => {
                sun = true;
                let d = v3(l.direction);
                w.lights.push(scene::Light::Sun {
                    dir: d * -1.0,
                    color: v3(l.color) * INV_PI,
                    lux: 1.0,
                    shadow: scene::ShadowSpec { enabled: l.shadow.is_some(), bias: 0.0 },
                });
            }
            LightType::Directional => gap("a second directional light", &mut gaps),
            LightType::Point => w.lights.push(scene::Light::Lamp {
                pos: v3(l.position),
                color: v3(l.color) * INV_PI,
                intensity: 1.0,
                range: if l.range > 0.0 { l.range } else { 1000.0 },
                shadow: l.shadow.is_some(),
            }),
            LightType::Spot => w.lights.push(scene::Light::Spot {
                pos: v3(l.position),
                dir: v3(l.direction),
                color: v3(l.color) * INV_PI,
                intensity: 1.0,
                range: if l.range > 0.0 { l.range } else { 1000.0 },
                cone: vec2f(l.cos_inner.clamp(-1.0, 1.0).acos().to_degrees().min(89.0), l.cos_outer.clamp(-1.0, 1.0).acos().to_degrees().min(89.0)),
                shadow: l.shadow.is_some(),
            }),
        }
    }
    if !sun {
        w.lights.push(scene::Light::Sun { dir: vec3f(0.0, 1.0, 0.0), color: vec3f(0.0, 0.0, 0.0), lux: 0.0, shadow: Default::default() });
    }
    let (top, ground) = f.hemisphere.map(|(s, g)| (v3(s), v3(g))).unwrap_or((vec3f(0.0, 0.0, 0.0), vec3f(0.0, 0.0, 0.0)));
    let amb = v3(f.ambient);
    w.lights.push(scene::Light::Sky { top: top + amb, ground: ground + amb, intensity: 1.0 });
    if let Some(e) = &f.env {
        w.environment.ibl = Some(scene::Ibl { key_reflection: makepad_scene::KeyReflection::Light, source: scene::IblSource::Hdri(scene::TextureRef(e.key)), intensity: e.intensity, rotation_deg: e.rotation });
        if e.background {
            // The background is drawn at its own intensity; the lookup
            // already carries the IBL's.
            w.environment.background = scene::Background::Environment { blur: e.blur, intensity: e.background_intensity / e.intensity.max(1.0e-4) };
        }
    }
    w.environment.fog = match f.fog {
        Some(fog) if fog.density > 0.0 => scene::Fog::Exp2 { color: v3(fog.color), density: fog.density },
        Some(fog) if fog.far > fog.near => scene::Fog::Linear { color: v3(fog.color), start: fog.near, end: fog.far },
        _ => scene::Fog::None,
    };
    let mut ids: HashMap<scene::MaterialId, ()> = HashMap::new();
    for m in &f.meshes {
        let (mut mat, mgaps) = to_material(scene::MaterialId(0), &m.material, f.camera.far, pictures);
        for g in mgaps {
            gap(g, &mut gaps);
        }
        if glass(&m.material) {
            // Glass: transmission, ior, thickness, roughness.
            let g = &m.material;
            mat.kind = scene::MaterialKind::Splash(scene::SplashMaterial { base: scene::BaseKind::Pbr, program: glass_program_id(g), uniforms: [vec4(g.transmission, g.ior, g.thickness, g.roughness), Vec4f::default(), Vec4f::default(), Vec4f::default()], textures: map_ref(&g.maps.base).into_iter().collect() });
        } else if finishes(f, m) {
            // Reflection, rim, fade: the finishing program (the params:
            // reflectivity, blur, fade radii).
            mat.kind = scene::MaterialKind::Splash(scene::SplashMaterial { base: scene::BaseKind::Pbr, program: finish_program_id(f, m), uniforms: [finish_params(&m.material), Vec4f::default(), Vec4f::default(), Vec4f::default()], textures: map_ref(&m.material.maps.base).into_iter().collect() });
        }
        if m.ink > 0.0 {
            // An ink hull: pushed out along its normals by a width in
            // output pixels at 1080p (the params: width, pixel size per unit
            // of distance, per unit orthographic, orthographic).
            let c = &f.camera;
            let params = vec4(m.ink, 2.0 * (c.fov_y.to_radians() * 0.5).tan() / 1080.0, c.height / 1080.0, if c.orthographic { 1.0 } else { 0.0 });
            mat.kind = scene::MaterialKind::Splash(scene::SplashMaterial { base: scene::BaseKind::Pbr, program: ink_program_id(&m.material), uniforms: [params, Vec4f::default(), Vec4f::default(), Vec4f::default()], textures: Vec::new() });
            mat.lighting = scene::LightingModel::Unlit;
        }
        let id = material_id(&mat);
        if ids.insert(id, ()).is_none() {
            w.set_material(scene::MaterialFrame { id, ..mat });
        }
        let data = makepad_render::TransformTint::floats(&m.instances);
        let mut item = scene::Item::new(scene::ItemKind::Instances {
            geometry: scene::GeometryRef::Resident(geometry_id(m)),
            material: id,
            source: scene::InstanceSource::Packed { data: data.into(), layout: makepad_render::LAYOUT_TRANSFORM_TINT },
            count: m.instance_count() as u32,
        });
        item.flags = scene::ItemFlags::empty();
        if m.cast_shadow {
            item.flags.insert(scene::ItemFlags::CAST_SHADOW);
        }
        if m.receive_shadow {
            item.flags.insert(scene::ItemFlags::RECEIVE_SHADOW);
        }
        if m.outline {
            item.flags.insert(scene::ItemFlags::OUTLINE);
        }
        w.items.push(item);
    }
    (w, gaps)
}

/// A frame's lines and points as render-lines batches.
pub fn to_line_batches(f: &SceneFrame) -> (Vec<lines::LineBatch>, Vec<lines::PointBatch>) {
    let blend = |b: Blending| if b == Blending::Additive { lines::Blend::Add } else { lines::Blend::Over };
    let mut out = Vec::new();
    for l in &f.lines {
        let style = lines::LineStyle {
            width_unit: if l.world_width { lines::WidthUnit::World } else { lines::WidthUnit::Pixels },
            blend: blend(l.blending),
            dash: l.dash,
            gap: l.gap,
            dash_offset: l.dash_offset,
            fade: l.fade,
            // motion3d's strokes: soft inside their outline only.
            edge: lines::LineEdge::Inner,
            ..Default::default()
        };
        let mut b = lines::LineBatch::new(style);
        // Packed at its final size: grown segment by segment, a smear's
        // batch (tens of MB a frame) doubled and copied its way there, and
        // the browser's wasm heap kept the doubled peak for good.
        b.data.reserve_exact(l.segments.len() * lines::SEGMENT_FLOATS);
        for seg in &l.segments {
            b.push(*seg);
        }
        out.push(b);
    }
    let mut points = Vec::new();
    for p in &f.points {
        let shape = match p.shape {
            SpriteShape::Circle => lines::SpriteShape::Disc,
            SpriteShape::Soft => lines::SpriteShape::Soft,
            SpriteShape::Square => lines::SpriteShape::Square,
            SpriteShape::Star => lines::SpriteShape::Spark,
            SpriteShape::Ring => lines::SpriteShape::Ring,
        };
        let style = lines::PointStyle {
            size_unit: if p.size_attenuation { lines::WidthUnit::World } else { lines::WidthUnit::Pixels },
            blend: blend(p.blending),
            shape,
            ..Default::default()
        };
        let mut b = lines::PointBatch::new(style);
        b.data.reserve_exact(p.points.len() * lines::POINT_FLOATS);
        for sprite in &p.points {
            b.push(*sprite);
        }
        points.push(b);
    }
    (out, points)
}


/// motion3d's helper functions (gpu/shaders.rs), for hooks written
/// against that shader: the same text, so a hook computes the same values.
const HOOK_HELPERS: &str = r#"
        hash3: fn(p: vec3) -> float {
            return fract(sin(dot(p, vec3(12.9898, 78.233, 37.719))) * 43758.5453)
        }
        vnoise: fn(p: vec3) -> float {
            let i = floor(p)
            let f = fract(p)
            let u = f * f * (vec3(3.0, 3.0, 3.0) - f * 2.0)
            let n000 = self.hash3(i)
            let n100 = self.hash3(i + vec3(1.0, 0.0, 0.0))
            let n010 = self.hash3(i + vec3(0.0, 1.0, 0.0))
            let n110 = self.hash3(i + vec3(1.0, 1.0, 0.0))
            let n001 = self.hash3(i + vec3(0.0, 0.0, 1.0))
            let n101 = self.hash3(i + vec3(1.0, 0.0, 1.0))
            let n011 = self.hash3(i + vec3(0.0, 1.0, 1.0))
            let n111 = self.hash3(i + vec3(1.0, 1.0, 1.0))
            let x00 = mix(n000, n100, u.x)
            let x10 = mix(n010, n110, u.x)
            let x01 = mix(n001, n101, u.x)
            let x11 = mix(n011, n111, u.x)
            return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z) * 2.0 - 1.0
        }
        // Pixel-stage copies (a helper fn binds to one stage).
        phash3: fn(p: vec3) -> float {
            return fract(sin(dot(p, vec3(12.9898, 78.233, 37.719))) * 43758.5453)
        }
        pnoise: fn(p: vec3) -> float {
            let i = floor(p)
            let f = fract(p)
            let u = f * f * (vec3(3.0, 3.0, 3.0) - f * 2.0)
            let x00 = mix(self.phash3(i), self.phash3(i + vec3(1.0, 0.0, 0.0)), u.x)
            let x10 = mix(self.phash3(i + vec3(0.0, 1.0, 0.0)), self.phash3(i + vec3(1.0, 1.0, 0.0)), u.x)
            let x01 = mix(self.phash3(i + vec3(0.0, 0.0, 1.0)), self.phash3(i + vec3(1.0, 0.0, 1.0)), u.x)
            let x11 = mix(self.phash3(i + vec3(0.0, 1.0, 1.0)), self.phash3(i + vec3(1.0, 1.0, 1.0)), u.x)
            return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z) * 2.0 - 1.0
        }
        fbm: fn(p: vec3) -> float {
            return self.vnoise(p) * 0.6 + self.vnoise(p * 2.03 + vec3(3.1, 1.7, 5.3)) * 0.3 + self.vnoise(p * 4.1 + vec3(7.7, 2.2, 1.1)) * 0.1
        }
        rot_y: fn(p: vec3, a: float) -> vec3 {
            let c = cos(a)
            let s = sin(a)
            return vec3(p.x * c + p.z * s, p.y, 0.0 - p.x * s + p.z * c)
        }
        rot_z: fn(p: vec3, a: float) -> vec3 {
            let c = cos(a)
            let s = sin(a)
            return vec3(p.x * c - p.y * s, p.x * s + p.y * c, p.z)
        }
        material_color: fn(uv: vec2) -> vec4 { return self.tint }
"#;

/// The motion3d hook names and the render-material hooks they become.
const HOOK_ADAPTERS: &[(&str, &str)] = &[
    ("surface", "surface: fn(base: vec4) -> vec4 { return self.m3d_surface(self.mat_pos(), self.mat_geo_normal(), self.mat_uv(), self.mat_time()) }"),
    ("vertex_displace", "vertex: fn(p: vec3, n: vec3, uv: vec2) -> vec3 { return self.m3d_vertex_displace(p, n, uv, self.mat_vtime()) }"),
    ("emissive_fn", "emission: fn(e: vec3) -> vec3 { return e + self.m3d_emissive_fn(self.mat_pos(), self.mat_geo_normal(), self.mat_uv(), self.mat_time()) }"),
    ("shade_fn", "finish: fn(c: vec4) -> vec4 { return self.m3d_shade_fn(c, self.mat_pos(), self.mat_geo_normal(), self.mat_uv(), self.mat_time()) }"),
];

/// A ShaderMaterial's hooks as Splash material hooks: each motion3d hook is
/// kept as written under an `m3d_` name and called by an adapter with the
/// arguments it expects (world position, normal, uv, time). `u_custom` is
/// the material's params; `u_custom2` and the lane-specific uniforms
/// (`u_rim`, `u_sheen_color`) have no place on the shared lanes yet, and a
/// hook reading them does not compile (reported as a gap).
pub fn hook_source(hooks: &crate::scene::ShaderHooks, m: &MaterialFrame) -> String {
    let lit = |v: [f32; 4]| format!("vec4({:?}, {:?}, {:?}, {:?})", v[0], v[1], v[2], v[3]);
    let rim = lit([m.rim[0], m.rim[1], m.rim[2], m.rim_power]);
    let sheen = lit([m.sheen_color[0], m.sheen_color[1], m.sheen_color[2], m.sheen]);
    let color = lit([m.color[0], m.color[1], m.color[2], m.color[3] * m.opacity]);
    let emissive = lit([m.emissive[0], m.emissive[1], m.emissive[2], 1.0]);
    let mut body = String::new();
    for (name, text) in &hooks.fields {
        let text = text.replace("self.u_custom2", "vec4(0.0, 0.0, 0.0, 0.0)").replace("self.u_custom", "self.params")
            .replace("self.u_rim", &rim).replace("self.u_sheen_color", &sheen)
            .replace("self.u_color", &color).replace("self.u_emissive", &emissive)
            .replace("self.u_time", "vec4(self.mat_time(), 0.0, 0.0, 0.0)")
            // The lane's true world eye (a perspective camera's).
            .replace("self.u_cam", "vec4(self.eye.xyz, 0.0)");
        match HOOK_ADAPTERS.iter().find(|(n, _)| n == name) {
            Some((_, adapter)) => {
                // `surface: fn(...)` -> `m3d_surface: fn(...)`.
                let renamed = text.trim_start().strip_prefix(name.as_str()).map(|rest| format!("m3d_{name}{rest}")).unwrap_or(text.clone());
                body.push_str(&format!("    {renamed}\n    {adapter}\n"));
            }
            None => body.push_str(&format!("    {text}\n")),
        }
    }
    // The lane takes metallic and roughness from the material (a Splash
    // program's model carries none): the material's, baked.
    if !draws_unlit(m) && !hooks.fields.iter().any(|(n, _)| n == "metal_rough") {
        let (metal, rough) = if m.model == ShadingModel::Lambert { (0.0, 1.0) } else { (m.metalness.clamp(0.0, 1.0), m.roughness.clamp(0.045, 1.0)) };
        body.push_str(&format!("    metal_rough: fn(mr: vec2) -> vec2 {{ return vec2({metal:?}, {rough:?}) }}\n"));
    }
    // The material's colour (baked) times its base map (the item's `tex`,
    // white without one) and the instance's tint.
    let helpers = HOOK_HELPERS.replace("return self.tint }", &format!("let t = self.tex.sample_repeat(uv)\n return {color} * vec4(self.to_scene(t.xyz), t.w) * self.tint }}"));
    format!("use mod.prelude.widgets_internal.*\n{{\n{helpers}{body}}}\n")
}

/// A floor that reflects: the frame has a reflector, the mesh is flat
/// ground and its material reflects (and has no hooks of its own).
pub fn reflects(f: &SceneFrame, m: &MeshDraw) -> bool {
    f.reflector.is_some() && m.ground && m.material.reflectivity > 0.0 && m.material.shader.is_none() && !m.material.wireframe
}

/// A material the gpu/ lane finishes after lighting: a fade to the
/// background (grounds), a rim light; a standard, lambert or basic
/// material without a program of its own. (A floor's reflection is the
/// graph's reflect pass.)
pub fn finishes(_f: &SceneFrame, m: &MeshDraw) -> bool {
    let mat = &m.material;
    mat.shader.is_none() && !mat.wireframe && !fitted(mat) && matches!(mat.model, ShadingModel::Standard | ShadingModel::Lambert | ShadingModel::Basic)
        && (mat.fade.is_some() || mat.rim_power > 0.0)
}

/// The finishing program: the reflection, the background it fades to and
/// the rim are its constants.
pub fn finish_program_id(f: &SceneFrame, m: &MeshDraw) -> scene::MaterialProgramId {
    let mut h = hash64(program_id(&m.material).0 ^ 0x3ef1_ec7 ^ if m.material.fade.is_some() { 2 } else { 0 } ^ if f.clear.is_some() { 4 } else { 0 });
    // (Edit mode: the background is a live slot of the program.)
    if !makepad_widgets::makepad_script::literal::edit_mode() {
        for v in f.clear.unwrap_or([0.0; 4]).iter().chain(&[if f.clear.is_some() { 1.0 } else { 0.0 }]) {
            h = hash64(h ^ v.to_bits() as u64);
        }
    }
    scene::MaterialProgramId(h)
}

/// The params of the finishing program: reflectivity, reflection blur,
/// the fade's start and end radius.
fn finish_params(m: &MaterialFrame) -> Vec4f {
    let (fs, fe) = m.fade.unwrap_or((0.0, 0.0));
    vec4(m.reflectivity, m.reflection_blur, fs, fe)
}

/// The gpu/ lane's finishing after lighting: the rim; the fade toward the
/// background (or to transparent) between two radii.
pub fn finish_hooks(f: &SceneFrame, m: &MeshDraw) -> crate::scene::ShaderHooks {
    let mat = &m.material;
    let mut body = String::from("finish: fn(c: vec4) -> vec4 {\n            var rgb = c.xyz\n            var alpha = c.w\n            let p = self.mat_pos()\n            let v = normalize(self.eye.xyz - p)\n            let ndv = clamp(dot(self.mat_geo_normal(), v), 0.0, 1.0)\n");
    let _ = f;
    if mat.rim_power > 0.0 {
        body.push_str(&format!("            rgb = rgb + vec3({:?}, {:?}, {:?}) * pow(1.0 - ndv, {:?})\n", mat.rim[0], mat.rim[1], mat.rim[2], mat.rim_power.max(0.5)));
    }
    if mat.fade.is_some() {
        body.push_str("            let r = sqrt(p.x * p.x + p.z * p.z)
            let ff = clamp((r - self.params.z) / max(self.params.w - self.params.z, 0.001), 0.0, 1.0)
            let fs = ff * ff * (3.0 - 2.0 * ff)
");
        match f.clear {
            Some(bg) => body.push_str(&format!("            rgb = mix(rgb, vec3({:?}, {:?}, {:?}), fs)\n", bg[0], bg[1], bg[2])),
            None => body.push_str("            alpha = alpha * (1.0 - fs)\n"),
        }
    }
    body.push_str("            return vec4(rgb, alpha)\n        }");
    let fields = vec![
        ("surface".to_string(), "surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 { return self.material_color(uv) }".to_string()),
        ("finish".to_string(), body),
    ];
    crate::scene::ShaderHooks { fields, key: finish_program_id(f, m).0, origins: Vec::new() }
}

/// The ink hull's program (its colour baked).
pub fn ink_program_id(m: &MaterialFrame) -> scene::MaterialProgramId {
    scene::MaterialProgramId(hash64(program_id(m).0 ^ 0x1_4b5_0bad))
}

/// An ink hull as the gpu/ lane draws it: the inverted hull in the ink
/// colour, each vertex pushed out along its normal by `params.x` output
/// pixels at 1080p at its distance from the eye (uniform scale assumed:
/// the push is divided by the instance's scale to stay in local units).
pub fn ink_hooks(m: &MaterialFrame) -> crate::scene::ShaderHooks {
    let fields = vec![
        ("surface".to_string(), "surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 { return self.u_color }".to_string()),
        ("vertex_displace".to_string(), "vertex_displace: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec3 {
            let w = self.transform * vec4(p.x, p.y, p.z, 1.0)
            let s = max(length((self.transform * vec4(1.0, 0.0, 0.0, 0.0)).xyz), 0.000001)
            var px = self.params.y * length(w.xyz - self.eye.xyz)
            if self.params.w > 0.5 {
                px = self.params.z
            }
            return p + normalize(n) * (self.params.x * px / s)
        }".to_string()),
    ];
    crate::scene::ShaderHooks { fields, key: ink_program_id(m).0, origins: Vec::new() }
}

/// A picture placed on its surface (fit, crop or picture space) on a
/// standard, lambert or basic material: a program that places it.
/// A picture placed on the surface by fit, crop or picture space, or a
/// screen (a host texture: `external` names one the host supplied).
pub fn fitted(m: &MaterialFrame) -> bool {
    m.shader.is_none() && !m.wireframe && matches!(m.model, ShadingModel::Standard | ShadingModel::Lambert | ShadingModel::Basic)
        && (m.external.is_some() || m.maps.base.is_some() && (m.fit != crate::scene::MapFit::Fill || m.crop.is_some() || m.map_space == crate::scene::MapSpace::Picture))
}

/// A glass surface (transmission): drawn by the glass program over the
/// scene behind it.
pub fn glass(m: &MaterialFrame) -> bool {
    m.is_transmissive() && m.shader.is_none() && !m.wireframe && !fitted(m) && matches!(m.model, ShadingModel::Standard | ShadingModel::Lambert)
}

/// The glass program (its base colour baked, as every generated program's).
pub fn glass_program_id(m: &MaterialFrame) -> scene::MaterialProgramId {
    scene::MaterialProgramId(hash64(program_id(m).0 ^ 0x6_1a55))
}

/// The glass program's hooks: the lit surface, then what is behind it
/// shown through (glass.splash).
pub fn glass_hooks(m: &MaterialFrame) -> crate::scene::ShaderHooks {
    let fields = vec![
        ("surface".to_string(), "surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 { return self.material_color(uv) }".to_string()),
        ("finish".to_string(), include_str!("glass.splash").trim().to_string()),
    ];
    crate::scene::ShaderHooks { fields, key: glass_program_id(m).0, origins: Vec::new() }
}

/// The shading models drawn without light (their colour is the surface's).
pub fn draws_unlit(m: &MaterialFrame) -> bool {
    matches!(m.model, ShadingModel::Basic | ShadingModel::Matcap | ShadingModel::Normal | ShadingModel::Depth)
}

/// The program a material without hooks needs on the shared lanes: its
/// wireframe edges, or the gpu/ lane's toon, matcap, normal or depth
/// shading. None: the stock lanes draw it.
pub fn model_hooks(m: &MaterialFrame) -> Option<crate::scene::ShaderHooks> {
    if m.shader.is_some() {
        return None;
    }
    if m.wireframe {
        return Some(wire_hooks(m));
    }
    let lit = |v: [f32; 3]| format!("vec3({:?}, {:?}, {:?})", v[0], v[1], v[2]);
    let emissive = lit(m.emissive);
    if fitted(m) {
        // The gpu/ lane's placement: q is where on the picture (top-left
        // origin) this point is, from the params' offset and scale.
        // A screen's picture (top row first) is the host's texture.
        let screen = m.external.is_some();
        let space = screen || m.map_space == crate::scene::MapSpace::Picture;
        let contain = m.fit == crate::scene::MapFit::Contain;
        let add = if m.model == ShadingModel::Basic { emissive.clone() } else { lit([0.0; 3]) };
        let muv = if space || contain { "q" } else { "uv" };
        let sample = if screen { "self.detail_map.sample(muv)" } else { "self.tex.sample_repeat(muv)" };
        // Contain: bars (black) outside the picture.
        let bars = if contain { "let inside = step(0.0, muv.x) * step(muv.x, 1.0) * step(0.0, muv.y) * step(muv.y, 1.0)\n            tc = mix(vec4(0.0, 0.0, 0.0, 1.0), tc, inside)" } else { "" };
        let surface = format!("surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 {{
            let q = self.params.xy + vec2(uv.x, 1.0 - uv.y) * self.params.zw
            let muv = {muv}
            let s = {sample}
            var tc = vec4(self.to_scene(s.xyz), s.w)
            {bars}
            let c = self.u_color * self.tint
            return vec4(c.xyz * tc.xyz + {add}, c.w * tc.w)
        }}");
        let mut fields = vec![("surface".to_string(), surface)];
        if m.model != ShadingModel::Basic && m.emissive.iter().any(|e| *e > 0.0) {
            fields.push(("emissive_fn".into(), "emissive_fn: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec3 { return self.u_emissive.xyz }".into()));
        }
        return Some(crate::scene::ShaderHooks { fields, key: program_id(m).0, origins: Vec::new() });
    }
    let fields: Vec<(String, String)> = match m.model {
        ShadingModel::Normal => vec![("surface".into(), "surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 { return vec4(n * 0.5 + vec3(0.5, 0.5, 0.5), 1.0) }".into())],
        ShadingModel::Depth => vec![("surface".into(), "surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 {
            let g = 1.0 - clamp(length(self.eye.xyz - p) / max(self.params.x, 0.001), 0.0, 1.0)
            return vec4(g, g, g, 1.0)
        }".into())],
        ShadingModel::Matcap => {
            let (base, hi, sharp) = match m.matcap as u32 {
                1 => ([0.1, 0.11, 0.13], [1.4, 1.4, 1.45], 40.0),
                2 => ([0.35, 0.2, 0.05], [1.4, 1.1, 0.6], 24.0),
                3 => ([0.8, 0.78, 0.82], [1.1, 1.05, 1.15], 6.0),
                4 => ([0.1, 0.4, 0.3], [0.7, 1.0, 0.85], 16.0),
                5.. => ([0.2, 0.2, 0.25], [1.0, 1.0, 1.0], 2.0),
                _ => ([0.72, 0.62, 0.55], [1.0, 0.95, 0.9], 8.0),
            };
            vec![("surface".into(), format!("surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 {{
            let vn = self.draw_pass.camera_view * vec4(n.x, n.y, n.z, 0.0)
            let m = normalize(vn.xyz)
            let key = clamp(dot(m, normalize(vec3(-0.4, 0.6, 0.7))), 0.0, 1.0)
            let rim = pow(1.0 - clamp(m.z, 0.0, 1.0), 2.0)
            let base = {}
            let d = mix(base * 0.35, base, key)
            let s = pow(key, {:?})
            let c = self.material_color(uv)
            return vec4((d + {} * s + base * rim * 0.6) * c.xyz + {emissive}, c.w)
        }}", lit(base), sharp, lit(hi)))]
        }
        ShadingModel::Toon => {
            // Banded diffuse and a hard highlight per light (the lanes'
            // lights are irradiance / pi: the gpu/ lane's are not).
            let steps = (m.steps as f32).max(2.0);
            let spec = 1.0 - m.roughness.clamp(0.04, 1.0);
            vec![
                ("surface".into(), "surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 { return self.material_color(uv) }".into()),
                ("light".into(), format!("light: fn(radiance: vec3, l: vec3, n: vec3, v: vec3, brdf: vec3) -> vec3 {{
            let ndl = max(dot(n, l), 0.0)
            let band = floor(ndl * {steps:?} + 0.5) / {steps:?}
            let s = step(0.92, max(dot(n, normalize(l + v)), 0.0)) * {spec:?}
            return (self.material_color(self.mat_uv()).xyz * band + vec3(s, s, s) * 0.5) * radiance * 3.14159265
        }}")),
            ]
        }
        _ => return None,
    };
    let mut fields = fields;
    if m.model == ShadingModel::Toon && m.emissive.iter().any(|e| *e > 0.0) {
        fields.push(("emissive_fn".into(), "emissive_fn: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec3 { return self.u_emissive.xyz }".into()));
    }
    Some(crate::scene::ShaderHooks { fields, key: program_id(m).0, origins: Vec::new() })
}

/// A wireframe material's program: the gpu/ lane's edges from the
/// triangle's barycentrics (in the vertex colour), `wire_width` pixels,
/// over the fill colour (hidden lines) or alone (the rest cut).
pub fn wire_hooks(m: &MaterialFrame) -> crate::scene::ShaderHooks {
    let lit = |v: [f32; 4]| format!("vec4({:?}, {:?}, {:?}, {:?})", v[0], v[1], v[2], v[3]);
    // A Basic material draws unlit: its emission is part of the colour.
    let emissive = if m.model == ShadingModel::Basic { lit([m.emissive[0], m.emissive[1], m.emissive[2], 0.0]) } else { lit([0.0; 4]) };
    let out = match m.wire_fill {
        Some(fill) => format!("return vec4(mix({}.xyz, c.xyz, line), 1.0)", lit(fill)),
        None => "return vec4(c.xyz, c.w * line)".to_string(),
    };
    let surface = format!(
        "surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 {{
            let b = self.v_tint.xyz
            let fw = vec3(abs(dFdx(b.x)) + abs(dFdy(b.x)), abs(dFdx(b.y)) + abs(dFdy(b.y)), abs(dFdx(b.z)) + abs(dFdy(b.z)))
            let e = min(min(b.x / max(fw.x, 0.00001), b.y / max(fw.y, 0.00001)), b.z / max(fw.z, 0.00001))
            let line = 1.0 - clamp(e - {:?} * 0.5, 0.0, 1.0)
            let c = self.material_color(uv) + {emissive}
            {out}
        }}",
        m.wire_width
    );
    let mut fields = vec![("surface".to_string(), surface)];
    if m.model != ShadingModel::Basic && m.emissive.iter().any(|e| *e > 0.0) {
        fields.push(("emissive_fn".to_string(), "emissive_fn: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec3 { return self.u_emissive.xyz }".to_string()));
    }
    let key = program_id(m).0;
    crate::scene::ShaderHooks { fields, key, origins: Vec::new() }
}

/// The live name of a hook program's generated text (edit mode).
fn generated_file(key: u64) -> String {
    format!("{}motion3d://shared-hooks/{key:016x}", makepad_widgets::makepad_script::literal::GENERATED)
}

/// Edit mode: a hook program installed from `compiled` now generates `now`
/// (the frame's material values baked in). When only values differ, they
/// are written to the program's slots (no compile): true. False when the
/// text changed otherwise, or a value cannot be patched (an integer the
/// program compiled in): the program must be built again.
fn patch_generated(cx: &mut Cx, key: u64, compiled: &str, now: &str) -> bool {
    let Some(edits) = makepad_widgets::makepad_script::literal::literal_edits(compiled, now) else { return false };
    let file = generated_file(key);
    for e in edits {
        let site = LiteralSite::new(&file, e.line(), e.col());
        let reach = cx.patch_literal(&site, e.value());
        if reach.needs_reload() {
            return false;
        }
    }
    true
}

/// Compile a ShaderMaterial into a Splash program on the Pbr lane.
pub fn hook_program(vm: &mut ScriptVm, hooks: &crate::scene::ShaderHooks, m: &MaterialFrame, desc: &makepad_render::makepad_render_material::MaterialDesc) -> Result<makepad_render::custom_material::CustomMaterial, String> {
    use makepad_render::makepad_render_material::{Hook, HookMask, HookSet};
    vm.capture_errors();
    let file = format!("motion3d://shared-hooks/{:016x}", hooks.key);
    let code = hook_source(hooks, m);
    // Edit mode: the hook functions' rows that read as written in the
    // document compile their literals as slots named by where they are.
    if vm.host.cx().live_literals_on() {
        let mut origins = Vec::new();
        for o in &hooks.origins {
            let Some((name, text)) = hooks.fields.iter().find(|(name, _)| *name == o.field) else { continue };
            let Some(copy) = text.strip_prefix(&format!("{name}: ")) else { continue };
            origins.extend(LiteralOrigin::of_copy(&code, copy, &o.file, o.line, o.col));
        }
        // The rest of the program (the values the host baked into it, its
        // helpers) is generated: live too, patched by the host as its
        // values change ([`patch_generated`]).
        origins.push(LiteralOrigin::new(0, u32::MAX, 0, &generated_file(hooks.key), 0, 0));
        vm.host.cx_mut().set_literal_source(&file, origins);
    }
    let v = vm.eval(ScriptMod::new("", "", &(file), 1, 0, &(code), &[]));
    let errors = vm.take_errors();
    let Some(obj) = v.as_object().filter(|_| errors.is_empty()) else {
        return Err(format!("the hooks did not evaluate: {}", errors.join("; ")));
    };
    let mut set = HookSet::new();
    let mut extra = Vec::new();
    for (key, hook) in [("surface", Hook::Surface), ("vertex", Hook::Vertex), ("emission", Hook::Emission), ("light", Hook::Light), ("metal_rough", Hook::MetalRough), ("finish", Hook::Finish)] {
        if let Some(f) = vm.bx.heap_ref().value(obj, LiveId::from_str(key).into(), NoTrap).as_object().filter(|f| vm.bx.heap_ref().is_fn(*f)) {
            set.set(hook, f);
        }
    }
    // The kept motion3d functions and helpers the adapters call.
    let entries: Vec<(ScriptValue, ScriptValue)> = vm.bx.heap_ref().map_entries(obj).iter().map(|e| (e.key, e.value)).collect();
    for (k, v) in entries {
        let Some(id) = k.as_id() else { continue };
        if ["surface", "vertex", "emission", "light", "metal_rough", "finish"].iter().any(|n| LiveId::from_str(n) == id) {
            continue;
        }
        if let Some(f) = v.as_object().filter(|f| vm.bx.heap_ref().is_fn(*f)) {
            extra.push((id, f));
        }
    }
    makepad_render::custom_material::DrawSceneCustom::build_with(vm, desc, &set, HookMask::ALL, Vec4f::default(), &extra).map_err(|e| e.to_string())
}

/// The name the mirrored scene is given to the graph under.
const REFLECT_MAP: &str = "m3_reflect_map";

/// The name the contact map is given to the graph under.
const CONTACT_MAP: &str = "m3_contact_map";

/// The module the post kits live in: the render graph's standard kits the
/// renderer uses (Bloom, DepthOfField, Vignette, Grain, Outline) and its
/// own (post.splash: the reflecting floor, contact shadows, the look, the
/// PostPass chain).
const POST_MODULE: &str = "scenepost";
const GRAPH_POST_KITS: &[&str] = &["bloom", "dof", "vignette", "grain", "outline"];

/// The id map's program: each outlined surface's id (its index + 1, low
/// byte in x, high in y, from the item's params), unlit.
const ID_PROGRAM: u64 = 0x1d_0a7c_e1d5;
fn id_hooks() -> crate::scene::ShaderHooks {
    crate::scene::ShaderHooks {
        fields: vec![("finish".to_string(), "finish: fn(c: vec4) -> vec4 { return vec4(self.params.x, self.params.y, 0.0, 1.0) }".to_string())],
        key: ID_PROGRAM,
        origins: Vec::new(),
    }
}

/// The casters' program for the contact map: their clip z / w from the
/// contact camera (under the ground, looking up), as the gpu/ lane's
/// caster writes it.
const CONTACT_PROGRAM: u64 = 0xc0_7ac7_de97;
fn contact_hooks() -> crate::scene::ShaderHooks {
    crate::scene::ShaderHooks {
        fields: vec![("finish".to_string(), "finish: fn(c: vec4) -> vec4 { return vec4(self.v_spos.z / max(self.v_spos.w, 0.000001), 0.0, 0.0, 1.0) }".to_string())],
        key: CONTACT_PROGRAM,
        origins: Vec::new(),
    }
}

/// Install the post kits into the VM once (under their module's marker).
fn install_post_kits(vm: &mut ScriptVm) -> Result<(), String> {
    let modules = vm.modules();
    let installed = vm.bx.heap_ref().value(modules, LiveId::from_str(POST_MODULE).into(), NoTrap).as_object()
        .is_some_and(|m| { let v = vm.bx.heap_ref().value(m, LiveId::from_str("kit_post_pass").into(), NoTrap); !v.is_nil() && !v.is_err() });
    if installed {
        return Ok(());
    }
    let module = vm.bx.heap_mut().new_module(LiveId::from_str(POST_MODULE));
    for kind in GRAPH_POST_KITS {
        let name = makepad_render_graph::kits::kit(kind).map(|k| k.name).unwrap_or(kind);
        let marker = vm.bx.heap_mut().new_object();
        vm.bx.heap_mut().set_value(module, LiveId::from_str(name).into(), marker.into(), NoTrap);
    }
    let mut errors = makepad_render_graph::script::install_kits(vm, &format!("mod.{POST_MODULE}"), Some(GRAPH_POST_KITS));
    vm.capture_errors();
    let v = vm.eval(ScriptMod::new("", "", "motion3d://post.splash", 1, 0, &(include_str!("post.splash")), &[]));
    errors.extend(vm.take_errors());
    if v.is_err() && errors.is_empty() {
        errors.push("post.splash did not evaluate".into());
    }
    if errors.is_empty() { Ok(()) } else { Err(format!("the post kits did not install: {}", errors.join("; "))) }
}

/// A kit's parameter values.
enum Param {
    F(f32),
    V4([f32; 4]),
    M4([f32; 16]),
}

/// Build kit `kind`'s passes for these parameters: the kit's template
/// called with an object of them, its passes read as the graph reads any
/// pass (namespaced unless `keep_names`).
fn kit_passes(vm: &mut ScriptVm, kind: &str, params: &[(&str, Param)], keep_names: bool) -> Result<(Vec<makepad_render_graph::PassDecl>, Vec<makepad_render_graph::PassValues>), String> {
    use makepad_widgets::makepad_script::numeric::{NumericValue, NumericValueTag};
    let modules = vm.modules();
    let module = vm.bx.heap_ref().value(modules, LiveId::from_str(POST_MODULE).into(), NoTrap).as_object().ok_or("the post kits are not installed")?;
    let f = vm.bx.heap_ref().value(module, LiveId::from_str(&format!("kit_{kind}")).into(), NoTrap);
    let defaults = makepad_render_graph::kits::kit(kind).map(|k| k.name)
        .and_then(|name| vm.bx.heap_ref().value(module, LiveId::from_str(name).into(), NoTrap).as_object());
    // A parameter motion3d does not pass takes the kit's default (the DOF's `range` 0 keeps
    // the thin lens): a kit that gains a parameter does not break this host.
    let p = if let Some(defaults) = defaults {
        vm.bx.heap_mut().new_with_proto(defaults.into())
    } else {
        vm.bx.heap_mut().new_object()
    };
    for (name, v) in params {
        let finite = |x: f32| if x.is_finite() { x } else { 0.0 };
        let value = match v {
            Param::F(x) => ScriptValue::from(finite(*x) as f64),
            Param::V4(a) => {
                NumericValue::vec4(vec4(finite(a[0]), finite(a[1]), finite(a[2]), finite(a[3]))).to_script_value(vm)
            }
            Param::M4(m) => {
                NumericValue::mat4(m.map(finite)).to_script_value(vm)
            }
        };
        vm.bx.heap_mut().set_value(p, LiveId::from_str(name).into(), value, NoTrap);
    }
    vm.capture_errors();
    let built = vm.call(f, &[p.into()]);
    let errors = vm.take_errors();
    if !errors.is_empty() || built.is_err() {
        return Err(format!("the {kind} kit did not build: {}", errors.join("; ")));
    }
    let items: Vec<ScriptValue> = if let Some(a) = built.as_array() {
        (0..vm.bx.heap_ref().array_len(a)).map(|i| vm.bx.heap_ref().array_index(a, i, NoTrap)).collect()
    } else {
        Vec::new()
    };
    let mut decls = Vec::new();
    let mut values = Vec::new();
    for (i, v) in items.into_iter().enumerate() {
        let read = makepad_render_graph::script::read_pass(vm, v, &format!("{kind}[{i}]"))?;
        let mut decl = read.decl;
        let mut vals = Vec::new();
        for (name, value) in read.uniforms {
            let (width, v): (u8, makepad_render_graph::UniformValue) = { let numeric = NumericValue::from_script_value_heap(vm.heap(), value, Default::default()); match numeric.kind() {
                NumericValueTag::F64 => { let x = numeric.as_f64().unwrap(); (1, [x as f32, 0.0, 0.0, 0.0].into()) },
                NumericValueTag::Vec2 => { let v = numeric.as_vec2().unwrap(); (2, [v.x, v.y, 0.0, 0.0].into()) },
                NumericValueTag::Vec3 => { let v = numeric.as_vec3().unwrap(); (3, [v.x, v.y, v.z, 0.0].into()) },
                NumericValueTag::Vec4 | NumericValueTag::Color => { let v = numeric.as_vec4().or_else(|| numeric.as_color()).unwrap(); (4, [v.x, v.y, v.z, v.w].into()) },
                NumericValueTag::Mat4 => (16, numeric.as_mat4().unwrap().into()),
            } };
            decl.uniforms.push(makepad_render_graph::UniformDecl { name, width });
            vals.push(v);
        }
        decls.push(decl);
        values.push(vals);
    }
    if !keep_names {
        makepad_render_graph::pass::namespace(&mut decls, &format!("m3_{kind}_"));
    }
    Ok((decls, values))
}

/// The frame's post as graph passes: the renderer's own (the reflecting
/// floor, contact shadows) and the document's post nodes, each a kit called
/// with this frame's values (Bloom, DepthOfField, Vignette, FilmGrain and
/// Outline are the render graph's kits; the look and the PostPass chain
/// motion3d's, post.splash). `size` is the scene target's; `ss` its
/// supersampling.
pub fn post_kit_passes(vm: &mut ScriptVm, post: &PostFrame, reflect: Option<(f32, f32, f32)>, contact: Option<(&ContactFrame, f32)>, id_map: bool, size: (usize, usize), ss: usize) -> Result<(Vec<makepad_render_graph::PassDecl>, Vec<makepad_render_graph::PassValues>), String> {
    use Param::*;
    let mut decls = Vec::new();
    let mut values = Vec::new();
    // The realtime velocity blur (a preview) runs after the depth of field,
    // before the bloom.
    let mut velocity = post.velocity_blur.as_ref().map(|(d, amount)| {
        let v: makepad_render_graph::PassValues = d.uniforms.iter().map(|u| if u.name == "amount" { [*amount, 0.0, 0.0, 0.0].into() } else { [12.0, 0.0, 0.0, 0.0].into() }).collect();
        (d.clone(), v)
    });
    let out = ((size.0 / ss.max(1)).max(1), (size.1 / ss.max(1)).max(1));
    let any = reflect.is_some() || contact.is_some() || post.dof.is_some() || post.bloom.is_some() || post.zoom_blur.is_some_and(|z| z > 0.001) || post.shafts.is_some_and(|s| s.1 > 0.0)
        || post.paper.is_some() || post.passes.is_some() || (post.outline.is_some() && id_map) || post.vignette.is_some() || post.grain.is_some();
    if !any {
        return Ok(velocity.map(|(d, v)| (vec![d], vec![v])).unwrap_or_default());
    }
    install_post_kits(vm)?;
    let mut add = |r: (Vec<makepad_render_graph::PassDecl>, Vec<makepad_render_graph::PassValues>)| {
        decls.extend(r.0);
        values.extend(r.1);
    };
    if let Some((reflectivity, blur, h)) = reflect {
        add(kit_passes(vm, "reflect", &[("rp", V4([reflectivity, blur, h, 0.0]))], true)?);
    }
    if let Some((c, ground_y)) = contact {
        add(kit_passes(vm, "contact", &[("view_proj", M4(c.view_proj.v)), ("cp", V4([c.opacity, c.blur, c.far, ground_y]))], true)?);
    }
    if let Some((focus, aperture, max_blur)) = post.dof {
        add(kit_passes(vm, "dof", &[("focus", F(focus)), ("aperture", F(aperture)), ("max_blur", F(max_blur))], false)?);
    }
    if let Some(v) = velocity.take() {
        add((vec![v.0], vec![v.1]));
    }
    if let Some((strength, radius, threshold)) = post.bloom {
        // Scene3D's Bloom is the graph's Bloom kit, the one bloom every
        // host has: the hard threshold on the brightest channel (a
        // saturated emissive glows at its own intensity; the renderer
        // before the switch thresholded luma, so a blue light never
        // bloomed) and the bright light compressed (a hot core glows, it
        // does not flood the frame). Its pyramid spreads a fixed number of
        // pixels at each level, as the old one's level count did with the
        // frame size (WARTS-COORD, W5 item 5).
        add(kit_passes(vm, "bloom", &[("strength", F(strength * 3.0)), ("threshold", F(threshold)), ("knee", F(0.0001)), ("radius", F(radius)), ("halation", F(0.0)), ("halation_tint", V4([1.0, 0.18, 0.04, 1.0])), ("compress", F(1.0))], false)?);
    }
    if post.zoom_blur.is_some_and(|z| z > 0.001) || post.shafts.is_some_and(|s| s.1 > 0.0) || post.paper.is_some() {
        let (suv, sst, scol, sdec, sthr) = post.shafts.unwrap_or(([0.5, 0.5], 0.0, [1.0; 3], 0.9, 0.8));
        let (pa, ph, pc, pcol) = post.paper.unwrap_or((0.0, 0.0, 7.0, [1.0; 4]));
        // The halftone cell in output pixels (given at 1080p).
        let cell = (pc * out.1 as f32 / 1080.0).max(2.0);
        add(kit_passes(vm, "look", &[
            ("zoom", F(post.zoom_blur.unwrap_or(0.0))),
            ("s0", V4([suv[0], suv[1], sst, sthr])),
            ("s1", V4([scol[0], scol[1], scol[2], sdec])),
            ("p0", V4([pa, ph, cell, if post.paper.is_some() { 1.0 } else { 0.0 }])),
            ("p1", V4([pcol[0], pcol[1], pcol[2], 0.0])),
        ], false)?);
    }
    if let Some((_, codes, u, time)) = &post.passes {
        let v4 = |k: usize| V4([u[k * 4], u[k * 4 + 1], u[k * 4 + 2], u[k * 4 + 3]]);
        let (mut d, v) = kit_passes(vm, "post_pass", &[
            ("u_post0", v4(0)), ("u_post1", v4(1)), ("u_post2", v4(2)), ("u_post3", v4(3)),
            ("u_ptime", V4([*time, out.0 as f32 / out.1 as f32, 0.0, 0.0])),
            ("u_d", V4([1.0 / out.0 as f32, 1.0 / out.1 as f32, 0.0, 0.0])),
        ], false)?;
        // The document's functions in place of the kit's identities.
        for decl in &mut d {
            for (k, code) in codes.iter().enumerate().take(4) {
                let name = format!("post_{k}");
                if let Some(h) = decl.helpers.iter_mut().find(|h| h.name == name) {
                    h.code = makepad_render_graph::pass::PassCode::Fn(code.clone());
                }
            }
        }
        add((d, v));
    }
    if let (Some((c, thickness)), true) = (post.outline, id_map) {
        // From the renderer's @id map (mode 1): where the surface id
        // changes within the thickness, on the lower id's side, mixed in
        // linear light, its ink counting as coverage.
        add(kit_passes(vm, "outline", &[("color", V4(c)), ("thickness", F(thickness)), ("threshold", F(0.08)), ("mode", F(1.0))], false)?);
    }
    // Vignette and FilmGrain are the graph's kits too, the ones Motion and
    // the VJ effects draw: the vignette darkens toward black on the display
    // picture, round in the frame's own aspect, from 1 - `softness` of the
    // half diagonal (the old one multiplied linear light over an oval
    // stretched to the frame); the grain is film grain sized at 1080p, two
    // scales, strongest in the mid-tones, one pattern per frame (the old
    // one was flat noise per internal pixel, so its size followed the
    // supersampling).
    if let Some((amount, softness)) = post.vignette {
        add(kit_passes(vm, "vignette", &[("amount", F(amount)), ("softness", F(softness)), ("color", V4([0.0, 0.0, 0.0, 1.0])), ("mode", F(0.0))], false)?);
    }
    if let Some(amount) = post.grain {
        add(kit_passes(vm, "grain", &[("amount", F(amount)), ("size", F(1.0))], false)?);
    }
    Ok((decls, values))
}

/// Record `world` seen by `state` into an auxiliary pass of the frame (a
/// reflection, the contact map, the id map), cleared to `clear`, as a
/// child of `parent`. `scene_world`: the world is the scene's own with items
/// taken out (the mirror, the frame behind glass), so it names the scene's
/// environment and resolves it like the scene's draw, and sees what the
/// scene sees; a map of its own (contact, ids) names none and leaves the
/// scene's environment alone: resolved like the scene's, its default
/// environment would drop what the scene's draw prepares on every
/// recording, and the frame would wait for it for ever.
#[allow(clippy::too_many_arguments)]
fn record_aux(cx: &mut Cx2d, renderer: &mut Renderer, d: &mut Draws, apass: &mut DrawPass, aroot: &mut DrawList2d, alist: &mut DrawList, color: &Texture, depth: &Texture, world: &scene::World, state: SceneState3D, clear: Vec4f, parent: DrawPassId, scene_world: bool) {
    apass.set_color_texture(cx.cx, color, DrawPassClearColor::ClearWith(clear));
    apass.set_depth_texture(cx.cx, depth, DrawPassClearDepth::ClearWith(1.0));
    cx.begin_pass(apass, Some(1.0));
    apass.set_size(cx.cx, state.viewport_rect.size);
    makepad_render::set_pass_camera(cx.cx, apass, &state);
    aroot.begin_always(cx);
    cx.begin_root_turtle_for_pass(Layout::default());
    {
        let cx3d = &mut Cx3d::new(cx.cx);
        let mut draws = makepad_render::SceneDraws {
            cube: Some(&mut d.cube),
            alpha: Some(&mut d.alpha),
            sky: &mut d.sky,
            sky_analytic: None,
            terrain: Some(&mut d.terrain),
            shadow: None,
            shadow_sdf: None,
            firework: None,
            flare: None,
            water: None,
            screen: None,
            screen_instances: &[],
            view_model: None,
        };
        renderer.set_models(Vec::new());
        if scene_world {
            renderer.draw_scene_full(cx3d, alist, &mut draws, world, state, None, Some(&mut d.models));
        } else {
            renderer.draw_scene_aux(cx3d, alist, &mut draws, world, state, Some(&mut d.models));
        }
    }
    cx.end_pass_sized_turtle();
    aroot.end(cx);
    aroot.set_view_transform(cx.cx, &Mat4f::identity());
    alist.set_view_transform(cx.cx, &Mat4f::identity());
    cx.end_pass(apass);
    cx.cx.attach_child_pass(apass.draw_pass_id(), parent, None);
    cx.cx.repaint_pass(apass.draw_pass_id());
}

/// The scene's meshes that `keep` keeps, each drawn unlit in its id (its
/// index in the frame + 1: low byte in red, high in green) and nothing
/// else, for the outline and coverage id maps.
fn id_world(f: &SceneFrame, world: &scene::World, keep: impl Fn(&MeshDraw) -> bool) -> scene::World {
    let mut iworld = scene::World::new();
    for (i, (m, item)) in f.meshes.iter().zip(world.items.iter()).enumerate() {
        if !keep(m) {
            continue;
        }
        let id = (i + 1).min(65535);
        let mid = scene::MaterialId(ID_PROGRAM ^ (id as u64) << 40);
        let side = match m.material.side {
            FaceSide::Front => scene::Side::Front,
            FaceSide::Back => scene::Side::Back,
            FaceSide::Double => scene::Side::Double,
        };
        iworld.set_material(scene::MaterialFrame {
            id: mid,
            kind: scene::MaterialKind::Splash(scene::SplashMaterial { base: scene::BaseKind::Pbr, program: scene::MaterialProgramId(ID_PROGRAM), uniforms: [vec4((id & 255) as f32 / 255.0, (id >> 8) as f32 / 255.0, 0.0, 0.0), Vec4f::default(), Vec4f::default(), Vec4f::default()], textures: Vec::new() }),
            blend: scene::Blend::Opaque,
            side,
            depth_write: true,
            glow: 0.0,
            lighting: scene::LightingModel::Unlit,
        });
        let mut it = item.clone();
        if let scene::ItemKind::Instances { material, .. } = &mut it.kind {
            *material = mid;
        }
        it.flags = scene::ItemFlags::empty();
        iworld.items.push(it);
    }
    iworld
}

/// Where a picture of `size` pixels lands on a surface of `aspect` (w / h):
/// the uv transform `(offset.xy, scale.zw)` applied to the surface's uv,
/// and whether uv outside the picture is a bar (contain). `crop` is the
/// part to show in picture pixels (top-left origin); textures are sampled
/// with v = 0 at the picture's top row... measured by the fit pixel test.
pub fn uv_placement(fit: crate::scene::MapFit, aspect: f32, size: Option<(f32, f32)>, crop: Option<[f32; 4]>) -> ([f32; 4], bool) {
    use crate::scene::MapFit;
    let identity = [0.0, 0.0, 1.0, 1.0];
    let Some((pw, ph)) = size.filter(|s| s.0 > 0.0 && s.1 > 0.0) else { return (identity, false) };
    // The window into the picture, in pixels: the crop, or all of it.
    let [mut x, mut y, mut w, mut h] = match crop {
        Some(c) if c[2] > 0.0 && c[3] > 0.0 => c,
        _ => [0.0, 0.0, pw, ph],
    };
    let aspect = if aspect > 0.0 { aspect } else { w / h };
    match fit {
        MapFit::Fill => {}
        MapFit::Cover => {
            // Grow the window to the surface's aspect around its centre,
            // then fit it inside the picture (shrinking if it is too big)
            // and slide it back inside.
            let (cx, cy) = (x + w * 0.5, y + h * 0.5);
            if w / h < aspect { w = h * aspect } else { h = w / aspect }
            let k = (pw / w).min(ph / h).min(1.0);
            w *= k;
            h *= k;
            x = (cx - w * 0.5).clamp(0.0, pw - w);
            y = (cy - h * 0.5).clamp(0.0, ph - h);
        }
        MapFit::Contain => {
            // Grow the window the other way: bars where it leaves the picture.
            let (cx, cy) = (x + w * 0.5, y + h * 0.5);
            if w / h < aspect { w = h * aspect } else { h = w / aspect }
            x = cx - w * 0.5;
            y = cy - h * 0.5;
        }
    }
    ([x / pw, y / ph, w / pw, h / ph], fit == MapFit::Contain)
}

/// Register what the renderer draws with (the scene lanes, lines and
/// points, the graph's passes and tone map). After
/// `makepad_widgets::script_mod`; a host that does not call it gets them
/// registered on the first draw.
pub fn script_mod(vm: &mut ScriptVm) {
    if vm.bx.heap_ref().type_default_for_id(makepad_render::DrawScenePbr::script_type_id_static()).is_none() {
        makepad_render::script_mod(vm);
    }
    makepad_render_graph::script_mod_passes(vm);
}

/// Counts and timings of the last recorded chain (CPU side).
#[derive(Clone, Debug, Default)]
pub struct RenderStats {
    pub passes: usize,
    pub instances: usize,
    pub record_ms: f64,
}

/// The draw structs the scene pass needs, created in the VM on first use.
struct Draws {
    cube: makepad_render::DrawSceneCube,
    alpha: makepad_render::DrawSceneAlpha,
    sky: makepad_render::DrawSceneSky,
    terrain: makepad_render::DrawSceneTerrain,
    models: makepad_render::DrawSceneSkinned,
    tonemap: makepad_render_graph::DrawToneMap,
}

/// Whether a sampled frame's sub-frames saw the scene's environment alike.
/// The renderer prepares the environment off the UI thread and a draw adopts
/// it when it lands, so the sub-frames recorded one per paint before the
/// landing lack the lane texture and the later ones have it, and their
/// average is a frame lit by neither. A frame with a sub-frame recorded while
/// the environment was pending, or whose draw adopted it, is not whole: it is
/// recorded again from its first sub-frame once the accumulation is done, by
/// which time the environment is in (and its item material compiled).
#[derive(Default)]
struct SubframeEnvironment {
    unsettled: bool,
}

impl SubframeEnvironment {
    /// A new frame starts (a first recording, or the re-record of an
    /// unsettled one).
    fn begin(&mut self) {
        self.unsettled = false;
    }

    /// A sub-frame was recorded; the renderer's `environment_pending`
    /// before and after its draw. Pending after: the sub-frame lacks the
    /// environment (its draw submitted the job, or the job is still
    /// running). Pending before and not after: the draw adopted the job. That
    /// holds for a frame's first sub-frame too: the draw that adopts is the
    /// first with the lane texture bound, so it builds the environment's item
    /// material (`__item_ibl`), whose pipeline compiles asynchronously while
    /// the instances draw through the stock matte lane, and the sub-frames
    /// after it wait for the compile (`next_subframe` gates on `chain_painted`
    /// from the second one), so the first alone would be matte. The cost is
    /// one more accumulation when a landing falls on a first sub-frame; the
    /// re-record finds the job adopted and the material built.
    fn recorded(&mut self, pending_before: bool, pending_after: bool) {
        self.unsettled |= pending_before || pending_after;
    }
}

/// Draws Scene3D frames through the shared renderer.
pub struct SharedRenderer {
    renderer: Renderer,
    draws: Option<Box<Draws>>,
    lines: Option<lines::LineRenderer>,
    /// The scene pass, its root list, the renderer's list and the lines' list.
    scene_pass: Option<(DrawPass, DrawList2d, DrawList, DrawList2d)>,
    /// The planar reflection: the mirrored scene's pass (root list, the
    /// renderer's list) and its colour and depth, made when a frame has a
    /// reflecting floor.
    refl_pass: Option<(DrawPass, DrawList2d, DrawList, Texture, Texture, (usize, usize))>,
    /// Contact shadows: the casters' depth seen from under the ground (the
    /// pass, its lists, colour (clip z / w) and depth, size), when a frame
    /// has them.
    contact_pass: Option<(DrawPass, DrawList2d, DrawList, Texture, Texture, u32)>,
    /// The last recorded frame drew the contact map.
    contact_live: bool,
    /// The outlined surfaces' id map (pass, lists, colour, depth, size),
    /// when a frame outlines some.
    id_pass: Option<(DrawPass, DrawList2d, DrawList, Texture, Texture, (usize, usize))>,
    id_live: bool,
    /// Screen coverage, when a host asks for it ([`Self::set_coverage`]):
    /// every mesh's id over one depth (pass, lists, colour, depth, size).
    cover_pass: Option<(DrawPass, DrawList2d, DrawList, Texture, Texture, (usize, usize))>,
    coverage: bool,
    /// The last recorded frame drew the mirrored scene.
    refl_live: bool,
    /// Glass: the frame without its glass (pass, lists, colour, depth,
    /// size), what the glass shows through.
    behind_pass: Option<(DrawPass, DrawList2d, DrawList, Texture, Texture, (usize, usize))>,
    out_pass: Option<(DrawPass, DrawList2d)>,
    color: Option<Texture>,
    depth: Option<Texture>,
    out: Option<Texture>,
    size: (usize, usize),
    out_size: (usize, usize),
    /// Geometry made resident, by id, with the mesh it was made from.
    resident: HashMap<scene::GeometryId, usize>,
    /// The deformed shapes the last frame drew (each frame's are made
    /// resident, the ones no longer drawn released).
    deformed: HashSet<scene::GeometryId>,
    envs: HashSet<u64>,
    /// ShaderMaterial programs installed, by (hooks key, with IBL).
    programs: HashSet<(u64, bool)>,
    /// Edit mode: each installed program's text as compiled and as last
    /// patched (see [`patch_generated`]).
    live_programs: HashMap<(u64, bool), (String, String)>,
    /// The recorded chain: the repaint id and every pass of it.
    recorded: Option<(u64, Vec<DrawPassId>)>,
    /// What the last recording drew without: the frame's auxiliary
    /// programs, the items' own lanes, the line shaders.
    recorded_without: [bool; 3],
    /// The chain's last pass (the host's child) and its output.
    tail: Option<(DrawPassId, Texture)>,
    /// The document's render-graph passes (`post.graph`).
    graph: makepad_render_graph::GraphRunner,
    /// The same passes planned at the output size, for the @display and
    /// @final stages when the scene is supersampled (they run after the
    /// tone map's resolve).
    graph_out: makepad_render_graph::GraphRunner,
    /// The velocity blur's camera history: the frame (its time) last
    /// recorded, its projection x view and the frame's before it (a frame
    /// is recorded more than once; its velocity is against the frame
    /// before, not its own last recording).
    view_proj_history: Option<(u32, [f32; 16], [f32; 16])>,
    /// Sub-frame accumulation (motion blur, temporal AA).
    accum: makepad_render_graph::Accumulator,
    pending: Option<Pending>,
    /// While sub-frames record: the accumulator's last pass (the host's child).
    sub_tail: Option<DrawPassId>,
    record_serial: u64,
    /// The last frame drawn, to record again if a pipeline was not ready.
    last: Option<(SceneFrame, (usize, usize), Option<makepad_render_graph::Sampling>)>,
    shadow_mode: Option<makepad_render::GpuLightmapMode>,
    /// The last frame's cascades were drawn (or it needs none).
    shadows_warm: bool,
    csm_frames: u32,
    /// Whether the sub-frames of the frame in progress saw the environment
    /// alike (see [`SubframeEnvironment`]).
    sub_env: SubframeEnvironment,
    /// The frame (time, size) last recorded. With SSAO a frame is whole
    /// from its second recording, which reads the depth the first drew;
    /// without SSAO it is whole from its first recording.
    ao_frame: Option<(u32, (usize, usize))>,
    /// How many recordings in a row drew that frame.
    ao_count: u32,
    /// Timeline frame index supplied by Scene3dLayer (direct callers without
    /// an fps use the evaluated frame time's bits as their deterministic key).
    ao_frame_index: Option<u64>,
    /// What the last frame used that this path does not draw yet.
    pub gaps: Vec<&'static str>,
    pub errors: Vec<String>,
    pub stats: RenderStats,
    /// Output linear, exposed, premultiplied RGBA16F instead of the tone
    /// mapped display picture (see [`Self::set_linear_output`]).
    linear_out: bool,
    /// The linear output target (RGBA16F at the output size).
    lin: Option<Texture>,
    /// The frame's grain seed (the host's frame time).
    grain_seed: f32,
    /// Host textures by name (`texture: "name"` on a material, a graph
    /// pass's `reads`): a video frame, an app-run capture, a `layers:` picture.
    external: HashMap<String, Texture>,
    /// Per named texture: its picture size in pixels, and the crop the host
    /// wants shown, `[x, y, w, h]` in pixels.
    external_info: HashMap<String, ((f32, f32), Option<[f32; 4]>)>,
}

impl Default for SharedRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedRenderer {
    pub fn new() -> Self {
        let mut renderer = Renderer::default();
        renderer.set_hdr_output(true);
        Self {
            renderer,
            draws: None,
            lines: None,
            scene_pass: None,
            refl_pass: None,
            contact_pass: None,
            contact_live: false,
            id_pass: None,
            id_live: false,
            cover_pass: None,
            coverage: false,
            refl_live: false,
            behind_pass: None,
            out_pass: None,
            color: None,
            depth: None,
            out: None,
            size: (0, 0),
            out_size: (0, 0),
            resident: HashMap::new(),
            deformed: HashSet::new(),
            envs: HashSet::new(),
            programs: HashSet::new(),
            live_programs: HashMap::new(),
            recorded: None,
            recorded_without: [false; 3],
            tail: None,
            graph: Default::default(),
            graph_out: Default::default(),
            view_proj_history: None,
            accum: Default::default(),
            pending: None,
            sub_tail: None,
            record_serial: 0,
            last: None,
            shadow_mode: None,
            shadows_warm: false,
            csm_frames: 0,
            sub_env: SubframeEnvironment::default(),
            ao_frame: None,
            ao_count: 0,
            ao_frame_index: None,
            gaps: Vec::new(),
            errors: Vec::new(),
            stats: RenderStats::default(),
            linear_out: false,
            lin: None,
            grain_seed: 0.0,
            external: HashMap::new(),
            external_info: HashMap::new(),
        }
    }

    /// Hand the host linear light: the exposed, premultiplied HDR colour
    /// after the scene's @hdr passes, without its bloom, tone map, grade or
    /// display passes. For a host that composes several layers into one
    /// linear frame and tone maps it once (the HDR Motion layer).
    pub fn set_linear_output(&mut self, on: bool) {
        self.linear_out = on;
    }

    /// Whether frames are a realtime preview's: the scene's graph passes
    /// read it as `self.realtime()`.
    pub fn set_realtime(&mut self, on: bool) {
        self.graph.set_realtime(on);
        self.graph_out.set_realtime(on);
    }

    /// Sample the key light's disc for this recording's shadow cascades.
    pub fn set_sun_disc_sample(&mut self, point: Option<[f32; 2]>) {
        self.renderer.set_sun_disc_sample(point);
    }

    /// Supply (or replace) a named texture materials show with
    /// `texture: "name"` and graph passes read by name. Call before `draw`.
    pub fn set_texture(&mut self, name: &str, texture: Texture) {
        self.external.insert(name.to_string(), texture);
    }

    /// The size of a named texture's picture (video frames do not carry it)
    /// and the crop to show for it this frame (a material's own `crop` wins).
    pub fn set_texture_frame(&mut self, name: &str, size: (u32, u32), crop: Option<[f32; 4]>) {
        self.external_info.insert(name.to_string(), ((size.0 as f32, size.1 as f32), crop));
    }

    /// The host textures materials may show, with their picture sizes (the
    /// host's, else the texture's own) and crops.
    fn pictures(&self, cx: &mut Cx) -> Pictures {
        self.external.iter().filter_map(|(n, t)| {
            let info = self.external_info.get(n);
            let size = info.map(|i| i.0).or_else(|| {
                let f = t.get_format(cx);
                f.vec_width_height().or_else(|| f.render_fixed_width_height()).map(|(w, h)| (w as f32, h as f32))
            })?;
            Some((n.clone(), (size, info.and_then(|i| i.1))))
        }).collect()
    }

    /// The frame as drawn: a screen whose texture the host did not supply
    /// shows the material's own map.
    fn resolved<'a>(&self, f: &'a SceneFrame) -> std::borrow::Cow<'a, SceneFrame> {
        if f.meshes.iter().all(|m| m.material.external.as_ref().is_none_or(|n| self.external.contains_key(n))) {
            return std::borrow::Cow::Borrowed(f);
        }
        let mut f = f.clone();
        for m in &mut f.meshes {
            if m.material.external.as_ref().is_some_and(|n| !self.external.contains_key(n)) {
                m.material.external = None;
            }
        }
        std::borrow::Cow::Owned(f)
    }

    /// The host's named textures as graph passes read them (sorted, so a
    /// plan's key does not change with the map's order).
    fn host_textures(&self) -> Vec<(LiveId, Texture)> {
        let mut v: Vec<(LiveId, Texture)> = self.external.iter().map(|(n, t)| (LiveId::from_str(n), t.clone())).collect();
        v.sort_by_key(|(n, _)| n.0);
        v
    }

    /// The passes of the chain recorded last (a host orders what the scene
    /// samples, its `layers:` pictures, before the deepest of them).
    pub fn recorded_passes(&self) -> &[DrawPassId] {
        self.recorded.as_ref().map_or(&[], |(_, passes)| passes)
    }

    /// Give back the GPU storage of every render target (a host whose layer
    /// stopped drawing; a later draw allocates what it renders into again).
    /// The last output is released too: do not show it after this.
    pub fn release_targets(&mut self, cx: &mut Cx) {
        let mut all: Vec<Texture> = [&self.color, &self.depth, &self.out, &self.lin].into_iter().flatten().cloned().collect();
        for r in [&self.refl_pass, &self.id_pass, &self.behind_pass, &self.cover_pass].into_iter().flatten() {
            all.push(r.3.clone());
            all.push(r.4.clone());
        }
        if let Some(r) = &self.contact_pass {
            all.push(r.3.clone());
            all.push(r.4.clone());
        }
        release_targets(cx, &all.iter().collect::<Vec<_>>());
        // The graph's and the accumulator's targets go with them.
        self.graph = Default::default();
        self.graph_out = Default::default();
        self.accum = Default::default();
        self.recorded = None;
        self.pending = None;
        self.tail = None;
        self.sub_tail = None;
        self.last = None;
        // Allocated again (fresh handles) by the next draw.
        self.color = None;
        self.size = (0, 0);
    }

    /// Everything goes (its targets, passes, draws) but its static meshes'
    /// geometry and the programs and environments installed in its
    /// renderer: for a host that stops drawing the scene and keeps it for
    /// later, whose next draw then uses the resident meshes as they are (a
    /// 462k-vertex terrain is 39 MB of packing and upload). The draws go
    /// too: they hold the textures they last sampled (the tone map its
    /// frame), which would keep a full-size target alive with the scene.
    pub fn reset_keeping_geometry(&mut self) {
        for id in std::mem::take(&mut self.deformed) {
            self.renderer.unregister_geometry(id);
            self.resident.remove(&id);
        }
        let renderer = std::mem::take(&mut self.renderer);
        let (resident, programs, envs) = (std::mem::take(&mut self.resident), std::mem::take(&mut self.programs), std::mem::take(&mut self.envs));
        *self = Self::new();
        (self.renderer, self.resident, self.programs, self.envs) = (renderer, resident, programs, envs);
    }

    /// Its static meshes' geometry goes (see [`Self::reset_keeping_geometry`]).
    pub fn drop_geometry(&mut self) {
        for id in self.resident.keys().copied().collect::<Vec<_>>() {
            self.renderer.unregister_geometry(id);
        }
        self.resident.clear();
        self.deformed.clear();
    }

    /// Sub-frames of the last accumulated frame and its error estimate
    /// after each step (8-bit levels).
    pub fn last_subframes(&self) -> (u32, &[f32]) {
        (self.accum.last_count, &self.accum.last_errors)
    }

    fn ensure(&mut self, cx: &mut Cx) -> bool {
        if self.draws.is_none() {
            self.draws = cx.try_with_vm(|vm| {
                if vm.bx.heap_ref().type_default_for_id(makepad_render::DrawScenePbr::script_type_id_static()).is_none() {
                    makepad_render::script_mod(vm);
                }
                Box::new(Draws {
                    cube: makepad_render::DrawSceneCube::script_new_with_default(vm),
                    alpha: makepad_render::DrawSceneAlpha::script_new_with_default(vm),
                    sky: makepad_render::DrawSceneSky::script_new_with_default(vm),
                    terrain: makepad_render::DrawSceneTerrain::script_new_with_default(vm),
                    models: makepad_render::DrawSceneSkinned::script_new_with_default(vm),
                    tonemap: makepad_render_graph::DrawToneMap::script_new_with_default(vm),
                })
            });
        }
        if self.lines.is_none() {
            let registered = cx.try_with_vm(|vm| {
                let draw = vm.bx.heap_ref().value(vm.modules(), LiveId::from_str("draw").into(), NoTrap).as_object();
                let have = draw.is_some_and(|d| {
                    let v = vm.bx.heap_ref().value(d, LiveId::from_str("DrawLineSegment").into(), NoTrap);
                    !v.is_nil() && !v.is_err()
                });
                if !have {
                    lines::script_mod(vm);
                }
            });
            if registered.is_some() {
                self.lines = lines::LineRenderer::new(cx);
            }
        }
        if self.scene_pass.is_none() {
            let pass = DrawPass::new_with_name(cx, "motion3d shared scene");
            pass.set_keep_camera_matrix(cx, true);
            self.scene_pass = Some((pass, DrawList2d::new(cx), DrawList::new(cx), DrawList2d::new(cx)));
            let out = DrawPass::new_with_name(cx, "motion3d shared out");
            self.out_pass = Some((out, DrawList2d::new(cx)));
        }
        self.draws.is_some()
    }

    fn alloc(&mut self, cx: &mut Cx, size: (usize, usize), out_size: (usize, usize)) {
        if self.size == size && self.out_size == out_size && self.color.is_some() {
            return;
        }
        self.size = size;
        self.out_size = out_size;
        let fixed = || TextureSize::Fixed { width: size.0, height: size.1 };
        let out_fixed = TextureSize::Fixed { width: out_size.0, height: out_size.1 };
        self.color = Some(Texture::new_with_format(cx, TextureFormat::RenderRGBAf16 { size: fixed(), initial: true }).label(cx, "motion3d/scene-color").transient(cx));
        self.depth = Some(Texture::new_with_format(cx, TextureFormat::DepthD32Sampled { size: fixed(), initial: true }).label(cx, "motion3d/scene-depth").transient(cx));
        self.out = Some(Texture::new_with_format(cx, TextureFormat::RenderBGRAu8 { size: out_fixed.clone(), initial: true }).label(cx, "motion3d/scene-out").transient(cx));
        self.lin = Some(Texture::new_with_format(cx, TextureFormat::RenderRGBAf16 { size: out_fixed, initial: true }).label(cx, "motion3d/scene-linear").transient(cx));
    }

    /// Make every mesh of the frame resident (a dynamic mesh each frame).
    fn upload(&mut self, cx: &mut Cx, f: &SceneFrame) {
        let mut deformed_now = HashSet::new();
        for m in &f.meshes {
            let id = geometry_id(m);
            let ptr = Arc::as_ptr(&m.mesh) as usize;
            let deform = deformed(m);
            if deform {
                deformed_now.insert(id);
            }
            // A static mesh's id is its content (the geometry key): resident
            // is enough, whatever Arc the evaluation built it in this time
            // (a PlaneGeometry of computed size is rebuilt every evaluation,
            // and re-registering it rebuilt and re-uploaded its models on
            // every frame and sub-frame). A dynamic or deformed one is new
            // content when its mesh is.
            if self.resident.get(&id) == Some(&ptr) || (!m.dynamic && !deform && self.resident.contains_key(&id)) {
                continue;
            }
            let shaped = if deform { Some(deform_mesh(&m.mesh, m)) } else { None };
            let mesh = shaped.as_ref().unwrap_or(&m.mesh);
            let data = if m.material.wireframe { geometry_data(&mesh.with_barycentrics()) } else { geometry_data(mesh) };
            match self.renderer.register_geometry(id, data) {
                Ok(()) => {
                    self.resident.insert(id, ptr);
                }
                Err(e) => {
                    let e = format!("shared renderer: a mesh was refused: {e}");
                    if !self.errors.contains(&e) {
                        self.errors.push(e);
                    }
                }
            }
        }
        for image in frame_images(f) {
            let r = scene::TextureRef(image.key);
            if !self.renderer.has_image(r) {
                if let Err(e) = self.renderer.register_image(r, image.width, image.height, image.rgba.clone()) {
                    let e = format!("shared renderer: a material map was refused: {e}");
                    if !self.errors.contains(&e) {
                        self.errors.push(e);
                    }
                }
            }
        }
        for id in self.deformed.difference(&deformed_now) {
            self.renderer.unregister_geometry(*id);
            self.resident.remove(id);
        }
        self.deformed = deformed_now;
        // ShaderMaterials: their hooks as Splash programs on the Pbr lane.
        let ibl = f.env.is_some();
        for m in &f.meshes {
            let generated;
            let (h, program) = if m.ink > 0.0 {
                generated = ink_hooks(&m.material);
                (&generated, ink_program_id(&m.material))
            } else if glass(&m.material) {
                generated = glass_hooks(&m.material);
                (&generated, glass_program_id(&m.material))
            } else if finishes(f, m) {
                generated = finish_hooks(f, m);
                (&generated, finish_program_id(f, m))
            } else {
                match &m.material.shader {
                    Some(h) => (h, program_id(&m.material)),
                    None => match model_hooks(&m.material) {
                        Some(g) => {
                            generated = g;
                            (&generated, program_id(&m.material))
                        }
                        None => continue,
                    },
                }
            };
            let live = makepad_widgets::makepad_script::literal::edit_mode();
            if self.programs.contains(&(program.0, ibl)) {
                if !live {
                    continue;
                }
                // Edit mode: the same program with this frame's values.
                let now = hook_source(h, &m.material);
                match self.live_programs.get_mut(&(program.0, ibl)) {
                    Some((_, last)) if *last == now => continue,
                    Some((compiled, last)) => {
                        if patch_generated(cx, h.key, compiled, &now) {
                            *last = now;
                            continue;
                        }
                    }
                    None => continue,
                }
            }
            self.programs.insert((program.0, ibl));
            // Basic: the surface colour unlit (as the gpu/ lane's Basic).
            let unlit = draws_unlit(&m.material) || m.ink > 0.0;
            // The program cuts (Mask) or blends (Over) as the material does;
            // the model lane draws no other blend.
            let blend = match to_material(scene::MaterialId(0), &m.material, f.camera.far, &Pictures::new()).0.blend {
                b @ (scene::Blend::Mask { .. } | scene::Blend::Over) => b,
                _ => scene::Blend::Opaque,
            };
            let desc = makepad_render::makepad_render_material::MaterialDesc {
                // A screen's texture takes the IBL's slot.
                ibl: ibl && !unlit && m.material.external.is_none() && !glass(&m.material),
                lighting: if unlit { scene::LightingModel::Unlit } else { scene::LightingModel::Lit },
                blend,
                ..Default::default()
            };
            let built = cx.try_with_vm(|vm| hook_program(vm, h, &m.material, &desc));
            match built {
                Some(Ok(p)) => {
                    self.renderer.install_custom_material(makepad_render::splash_material_name(program), p);
                    if live {
                        let text = hook_source(h, &m.material);
                        self.live_programs.insert((program.0, ibl), (text.clone(), text));
                    }
                }
                Some(Err(e)) => {
                    let e = format!("ShaderMaterial hooks on the shared lanes: {e}");
                    if !self.errors.contains(&e) {
                        self.errors.push(e);
                    }
                }
                None => {
                    self.programs.remove(&(program.0, ibl));
                }
            }
        }
        if let (Some(_), Some(m0)) = (&f.contact, f.meshes.first()) {
            if self.programs.insert((CONTACT_PROGRAM, false)) {
                let desc = makepad_render::makepad_render_material::MaterialDesc { lighting: scene::LightingModel::Unlit, ..Default::default() };
                match cx.try_with_vm(|vm| hook_program(vm, &contact_hooks(), &m0.material, &desc)) {
                    Some(Ok(p)) => {
                        self.renderer.install_custom_material(makepad_render::splash_material_name(scene::MaterialProgramId(CONTACT_PROGRAM)), p);
                    }
                    Some(Err(e)) => {
                        let e = format!("contact shadows on the shared lanes: {e}");
                        if !self.errors.contains(&e) {
                            self.errors.push(e);
                        }
                    }
                    None => {
                        self.programs.remove(&(CONTACT_PROGRAM, false));
                    }
                }
            }
        }
        let outlines = f.post.outline.is_some() && f.meshes.iter().any(|m| m.outline);
        if let (true, Some(m0)) = (outlines || self.coverage, f.meshes.first()) {
            if self.programs.insert((ID_PROGRAM, false)) {
                let desc = makepad_render::makepad_render_material::MaterialDesc { lighting: scene::LightingModel::Unlit, ..Default::default() };
                match cx.try_with_vm(|vm| hook_program(vm, &id_hooks(), &m0.material, &desc)) {
                    Some(Ok(p)) => {
                        self.renderer.install_custom_material(makepad_render::splash_material_name(scene::MaterialProgramId(ID_PROGRAM)), p);
                    }
                    Some(Err(e)) => {
                        let e = format!("outlines on the shared lanes: {e}");
                        if !self.errors.contains(&e) {
                            self.errors.push(e);
                        }
                    }
                    None => {
                        self.programs.remove(&(ID_PROGRAM, false));
                    }
                }
            }
        }
        if let Some(e) = &f.env {
            if self.envs.insert(e.key) {
                self.renderer.register_environment(scene::TextureRef(e.key), e.map.clone());
            }
        }
    }

    /// Why the last recorded frame has not painted (for a log).
    pub fn painting_report(&self, cx: &Cx) -> String {
        let Some((serial, passes)) = &self.recorded else { return "nothing recorded".into() };
        let mut out = format!(
            "recorded at {serial}, shaders pending {}, environment pending {} ({} preparations), sub-frames saw it unsettled {}",
            cx.draw_shaders_pending(),
            self.renderer.environment_pending(),
            self.renderer.environment_preparations(),
            self.sub_env.unsettled,
        );
        for (i, id) in passes.iter().enumerate() {
            let p = &cx.passes[*id];
            out.push_str(&format!("; pass {i} dirty {} painted {}", p.paint_dirty, p.painted_serial));
        }
        out
    }

    /// The scene's environment is still preparing: the renderer builds it
    /// off the UI thread, so the first frame (and the first after the
    /// environment changes) is recorded without it. [`Self::painted`] waits
    /// for it and the next draw adopts it; a host that draws on demand
    /// keeps drawing while this holds (the job's completion raises the UI
    /// signal).
    pub fn environment_pending(&self) -> bool {
        self.renderer.environment_pending()
    }

    /// Whether the last recorded frame has painted whole.
    pub fn painted(&self, cx: &Cx) -> bool {
        let Some((serial, passes)) = &self.recorded else { return false };
        self.pending.is_none()
            && self.sub_tail.is_none()
            && !self.sub_env.unsettled
            && !cx.draw_shaders_pending()
            && self.shadows_warm
            && self.ao_warm()
            && self.aux_ready(cx)
            && self.renderer.items_ready(cx)
            && self.lines.as_ref().is_none_or(|l| l.ready(cx, true))
            && passes.iter().all(|&id| !cx.passes[id].paint_dirty && cx.passes[id].painted_serial >= *serial)
    }

    /// Draw the screen-coverage id map with every frame from now on (an
    /// inspect's ask; off, nothing of it is drawn and its targets go).
    pub fn set_coverage(&mut self, on: bool) {
        self.coverage = on;
    }

    /// The last frame's coverage id map (8-bit; a pixel's red + 256 ×
    /// green is the index + 1 of the mesh seen there, 0 for none) and the
    /// node each of the frame's meshes draws, while coverage is on.
    pub fn coverage(&self) -> Option<(Texture, Vec<u32>)> {
        let (_, _, _, ids, _, _) = self.cover_pass.as_ref().filter(|_| self.coverage)?;
        let (frame, _, _) = self.last.as_ref()?;
        Some((ids.clone(), frame.meshes.iter().map(|m| m.node).collect()))
    }

    pub fn output(&self) -> Option<Texture> {
        self.recorded.as_ref().and(self.tail.as_ref().map(|t| t.1.clone()))
    }

    pub fn record_serial(&self) -> u64 {
        self.record_serial
    }

    /// Whether a frame is still being recorded (sub-frames to come).
    pub fn in_progress(&self) -> bool {
        self.pending.is_some()
    }

    pub fn reattach(&mut self, cx: &mut Cx2d) {
        // Record a fallback frame again once what it drew without is ready,
        // never while it is still building. A finished environment preparation
        // is adopted by a draw. Shadows, SSAO and unsettled sub-frames still
        // need their warm-up recordings.
        // An AO warm-up recording counts only once the one before it painted whole (`ao_count`):
        // one recorded before that cannot count and leaves the chain to paint again. A pass that
        // samples the AO before the AO's passes paint (the id map shares the scene's draws) is owed
        // a paint in the repaint after every recording, so recording every frame never settles.
        let stale = self.pending.is_none() && self.recorded.is_some() && (
            self.sub_env.unsettled || !self.shadows_warm || (!self.ao_warm() && self.chain_painted(cx.cx))
                || self.renderer.environment_needs_draw()
                || (self.recorded_without[0] && self.aux_ready(cx.cx))
                || (self.recorded_without[1] && self.renderer.items_ready(cx.cx))
                || (self.recorded_without[2] && self.lines.as_ref().is_none_or(|l| l.ready(cx.cx, true)))
        );
        if stale {
            if let Some((frame, size, sampling)) = self.last.take() {
                self.draw(cx, frame, size, self.grain_seed, sampling);
                return;
            }
        }
        if let Some(id) = self.sub_tail {
            if let Some(p) = self.accum.draw_pass(id) {
                cx.make_child_pass(p);
            }
            return;
        }
        if self.recorded.is_some() {
            if let Some((id, _)) = &self.tail {
                if let Some((pass, _)) = self.out_pass.as_ref().filter(|(p, _)| p.draw_pass_id() == *id) {
                    cx.make_child_pass(pass);
                } else if let Some(p) = self.graph.draw_pass(*id).or_else(|| self.graph_out.draw_pass(*id)) {
                    cx.make_child_pass(p);
                }
            }
        }
    }

    /// Render one frame at `size` output pixels: `center` is the frame at
    /// its own time. Without `sampling` the whole chain records now; with
    /// it the frame is a set of sub-frames the host evaluates and records
    /// one per paint ([`Self::next_subframe`], [`Self::record_subframe`])
    /// until [`Self::painted`]. Returns the output texture (whole once
    /// painted).
    /// A host's new frame: with SSAO it is whole from its second recording,
    /// without it from its first, whatever was recorded before (a frame
    /// must not depend on what the renderer drew earlier).
    pub fn new_frame(&mut self) {
        self.ao_frame = None;
        self.ao_count = 0;
        self.ao_frame_index = None;
    }

    pub(crate) fn set_ao_frame_index(&mut self, index: u64) {
        self.ao_frame_index = Some(index);
    }

    /// The frame is kept (to record again once a pipeline is ready),
    /// so it is given, not copied: a frame of a large line smear is tens
    /// of MB, and a copy a draw was that much wasm heap churned a frame.
    pub fn draw(&mut self, cx: &mut Cx2d, f: SceneFrame, size: (usize, usize), grain_seed: f32, sampling: Option<makepad_render_graph::Sampling>) -> Option<Texture> {
        if !self.ensure(cx.cx) {
            return None;
        }
        self.grain_seed = grain_seed;
        self.sub_env.begin();
        let out = self.draw_frame(cx, &f, size, sampling);
        self.last = Some((f, size, sampling));
        out
    }

    fn draw_frame(&mut self, cx: &mut Cx2d, f: &SceneFrame, size: (usize, usize), sampling: Option<makepad_render_graph::Sampling>) -> Option<Texture> {
        let out_size = (size.0.max(1), size.1.max(1));
        // Supersampling: the scene at ss x the output size while that stays
        // within 4096 px a side, resolved in the tone map. Sub-frames are
        // jittered and averaged instead, at the output size.
        let want = if sampling.is_some() { 1 } else { f.post.antialias.clamp(1, 4) as usize };
        let ss = (1..=want).rev().find(|s| out_size.0 * s <= 4096 && out_size.1 * s <= 4096).unwrap_or(1);
        let size = (out_size.0 * ss, out_size.1 * ss);
        self.alloc(cx.cx, size, out_size);
        for e in &f.errors {
            if !self.errors.contains(e) {
                self.errors.push(e.clone());
            }
        }
        if let Some(sampling) = sampling {
            self.accum.exposure = f.exposure.max(1e-4);
            self.accum.block_px = (2.0 * size.1 as f32 / 720.0).round().max(1.0) as u32;
            self.accum.begin(sampling);
            self.pending = Some(Pending { center: f.clone(), size, out_size, recorded: 0 });
            // The texture the frame will end in (whole once painted).
            return self.output().or_else(|| self.out.clone());
        }
        self.pending = None;
        let started = Cx::monotonic_now();
        let scene = self.record_scene(cx, f, size, 0);
        let color = self.color.clone().unwrap();
        let mut chain = vec![scene];
        let tail = self.record_post(cx, f, &color, scene, size, out_size, ss, &mut chain);
        let out = self.finish_chain(cx, chain, tail);
        self.stats.record_ms = (Cx::monotonic_now() - started) * 1000.0;
        out
    }

    /// The next sub-frame to render this paint: its index, its shutter
    /// offset (-0.5..0.5) and supersampling tap. `None` while a measurement
    /// is read back, or once the frame's last chain is recorded (this call
    /// records it when the accumulator is done).
    pub fn next_subframe(&mut self, cx: &mut Cx2d) -> Option<(u32, f32, Option<u32>)> {
        let pending = self.pending.as_ref()?;
        // One chain per paint: the previous one must have painted.
        if pending.recorded > 0 && !self.chain_painted(cx.cx) {
            return None;
        }
        match self.accum.step(cx.cx) {
            makepad_render_graph::Step::Render { k, u, tap } => Some((k, u, tap)),
            makepad_render_graph::Step::Finish { .. } => {
                let p = self.pending.take().unwrap();
                // Sub-frames settle each other: the accumulated frame is
                // whole once it painted.
                self.ao_count = self.ao_count.max(2);
                if let Some((avg, avg_pass)) = self.accum.finish(cx, (p.size.0 as u32, p.size.1 as u32), None) {
                    let mut chain = vec![avg_pass];
                    let tail = self.record_post(cx, &p.center, &avg, avg_pass, p.size, p.out_size, 1, &mut chain);
                    self.finish_chain(cx, chain, tail);
                }
                None
            }
            makepad_render_graph::Step::Wait | makepad_render_graph::Step::Idle => None,
        }
    }

    /// Record sub-frame `k` (the frame evaluated at the offset
    /// [`Self::next_subframe`] gave), jittered by a fixed sub-pixel
    /// sequence, and add it to the accumulator.
    pub fn record_subframe(&mut self, cx: &mut Cx2d, mut frame: SceneFrame, k: u32, tap: Option<u32>) {
        let Some(p) = self.pending.as_ref() else { return };
        let size = p.size;
        let (jx, jy) = match tap {
            Some(tap) => [(0.125, 0.375), (0.375, -0.125), (-0.125, -0.375), (-0.375, 0.125)][tap as usize % 4],
            None => halton_jitter(k as usize),
        };
        let (dx, dy) = (2.0 * jx / size.0 as f32, 2.0 * jy / size.1 as f32);
        if frame.camera.orthographic {
            frame.camera.proj.v[12] += dx;
            frame.camera.proj.v[13] += dy;
        } else {
            frame.camera.proj.v[8] -= dx;
            frame.camera.proj.v[9] -= dy;
        }
        let pending_before = self.renderer.environment_pending();
        let scene = self.record_scene(cx, &frame, size, k);
        self.sub_env.recorded(pending_before, self.renderer.environment_pending());
        let color = self.color.clone().unwrap();
        let mut chain = vec![scene];
        if let Some(first) = self.accum.add(cx, &color, (size.0 as u32, size.1 as u32), None) {
            cx.cx.attach_child_pass(scene, first, None);
            chain.extend(self.accum.recorded_ids());
        }
        let tail_id = *chain.last().unwrap();
        self.record_serial += 1;
        for id in &chain {
            cx.cx.repaint_pass(*id);
        }
        self.recorded = Some((cx.cx.repaint_id(), chain));
        self.sub_tail = Some(tail_id);
        if let Some(p) = &mut self.pending {
            p.recorded += 1;
        }
    }

    /// The programs the frame's own passes draw with besides the scene's
    /// items (the contact map's) have their pipelines.
    fn aux_ready(&self, cx: &Cx) -> bool {
        let ready = |p: u64| self.renderer.custom_material_shader(&makepad_render::splash_material_name(scene::MaterialProgramId(p))).is_some_and(|id| cx.draw_shader_ready(id, true));
        (!self.contact_live || ready(CONTACT_PROGRAM)) && (!(self.id_live || self.coverage) || ready(ID_PROGRAM))
    }

    /// The frame has its own depth for SSAO, or needs only one recording.
    fn ao_warm(&self) -> bool {
        let ssao = self.last.as_ref().is_some_and(|(frame, _, _)| frame.post.ssao.is_some_and(|(_, intensity)| intensity.min(1.0) > 0.0));
        self.ao_count >= if ssao { 2 } else { 1 }
    }

    fn chain_painted(&self, cx: &Cx) -> bool {
        let Some((serial, passes)) = &self.recorded else { return false };
        !cx.draw_shaders_pending() && passes.iter().all(|&id| !cx.passes[id].paint_dirty && cx.passes[id].painted_serial >= *serial)
    }

    fn finish_chain(&mut self, cx: &mut Cx2d, chain: Vec<DrawPassId>, tail: (DrawPassId, Texture)) -> Option<Texture> {
        self.record_serial += 1;
        for id in &chain {
            cx.cx.repaint_pass(*id);
        }
        self.stats.passes = chain.len();
        self.recorded = Some((cx.cx.repaint_id(), chain));
        self.sub_tail = None;
        let out = tail.1.clone();
        self.tail = Some(tail);
        self.recorded_without = [
            !self.aux_ready(cx.cx),
            self.renderer.drew_fallback() || !self.renderer.items_ready(cx.cx),
            !self.lines.as_ref().is_none_or(|l| l.ready(cx.cx, true)),
        ];
        Some(out)
    }

    /// Record the scene pass for frame `f` into the colour target at
    /// `size` (lanes, then lines and points). Returns the pass.
    fn record_scene(&mut self, cx: &mut Cx2d, f: &SceneFrame, size: (usize, usize), sample: u32) -> DrawPassId {
        let resolved = self.resolved(f);
        let f: &SceneFrame = &resolved;
        self.upload(cx.cx, f);
        let pictures = self.pictures(cx.cx);
        let (world, gaps) = to_world(f, &pictures);
        // Each screen's program shows its host texture.
        for m in f.meshes.iter().filter(|m| fitted(&m.material)) {
            if let Some(t) = m.material.external.as_ref().and_then(|n| self.external.get(n)) {
                self.renderer.set_custom_material_texture(&makepad_render::splash_material_name(program_id(&m.material)), Some(t.clone()));
            }
        }
        self.gaps = gaps;
        // The key light's shadow: realtime cascades for every caster, or none.
        let sun_shadow = world.lights.iter().any(|l| matches!(l, scene::Light::Sun { shadow, .. } if shadow.enabled));
        let mode = if sun_shadow { makepad_render::GpuLightmapMode::Realtime } else { makepad_render::GpuLightmapMode::Off };
        if self.shadow_mode != Some(mode) {
            self.shadow_mode = Some(mode);
            self.renderer.set_gpu_lightmap_mode(mode);
        }
        // Fit the cascades around what the camera looks at (the frame's
        // bounds are its casters', not where their shadows land).
        let focus = f.bounds.map(|(a, b)| (v3([(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5]) - v3(f.camera.position)).length());
        self.renderer.set_csm_focus_distance(focus);
        let (line_batches, point_batches) = to_line_batches(f);
        let dsize = dvec2(size.0 as f64, size.1 as f64);
        let (color, depth) = (self.color.clone().unwrap(), self.depth.clone().unwrap());
        // The background: the document's colour; none = transparent (the
        // fog colours what is drawn, not the empty frame).
        let clear = f.clear.unwrap_or([0.0; 4]);
        let scene_pass = &self.scene_pass.as_ref().unwrap().0;
        // CORTAO on the ambient light only, from the depth this pass drew last
        // time (its camera uniforms still hold that recording's matrices).
        // A recording counts toward warm only once the one before it at
        // this key has painted: its depth is what this one's AO reads (a
        // recording replaced before it painted left the depth cleared, and
        // a still frame then showed no AO at all).
        let key = (f.t.to_bits(), size);
        self.ao_count = match self.ao_frame == Some(key) {
            true if self.chain_painted(cx.cx) => self.ao_count + 1,
            true => self.ao_count,
            false => 1,
        };
        self.ao_frame = Some(key);
        // `SSAO{radius intensity}`: the radius in world units (metres).
        // CORTAO: provisional, tuned from frames
        let (radius, ao) = f.post.ssao.map(|s| (s.0.max(0.01), s.1.min(1.0))).unwrap_or((0.0, 0.0));
        if ao > 0.0 {
            // k distinguishes both temporal-AA samples at one time and motion-blur
            // subframes. Include evaluated time for hosts sampling their own shutter.
            // ao_count only gates warm-up: compile retries must not change the seed.
            let index = self.ao_frame_index.unwrap_or(f.t.to_bits() as u64);
            let frame = index.wrapping_mul(0x9e3779b97f4a7c15)
                ^ ((f.t.to_bits() as u64) << 32) ^ sample as u64;
            self.renderer.run_screen_ao(cx, scene_pass, &depth, dsize, makepad_render::ScreenAoParams {
                strength: ao, radius, tier: makepad_render::cortao::CortaoTier::High,
                resolution: makepad_render::cortao::CortaoResolution::Auto,
                history: false, frame, reset: false,
            });
            // Native compiles the AO's pipelines in the background (and
            // `draw_shaders_pending` sees only WebGL's): a recording without
            // them is not a settled frame.
            if self.renderer.screen_ao_pending() {
                self.ao_count = 0;
            }
        } else {
            self.renderer.set_ssao(None);
        }
        let scene_id = scene_pass.draw_pass_id();
        // Contact shadows: the casters (opaque, casting, not ground) drawn
        // from under the ground looking up by the contact camera, their clip
        // z / w into the map the graph's contact pass reads.
        self.contact_live = f.contact.is_some();
        if let (false, Some(r)) = (self.contact_live, &self.contact_pass) {
            release_targets(cx.cx, &[&r.3, &r.4]);
        }
        if let Some(c) = &f.contact {
            if self.contact_pass.as_ref().is_none_or(|r| r.5 != c.size) {
                let fixed = TextureSize::Fixed { width: c.size as usize, height: c.size as usize };
                let pass = self.contact_pass.take().map(|r| (r.0, r.1, r.2)).unwrap_or_else(|| {
                    let p = DrawPass::new_with_name(cx.cx, "motion3d shared contact");
                    p.set_keep_camera_matrix(cx.cx, true);
                    (p, DrawList2d::new(cx.cx), DrawList::new(cx.cx))
                });
                let cc = Texture::new_with_format(cx.cx, TextureFormat::RenderRGBAf16 { size: fixed.clone(), initial: true }).label(cx.cx, "motion3d/contact-color").transient(cx.cx);
                let cd = Texture::new_with_format(cx.cx, TextureFormat::DepthD32 { size: fixed, initial: true }).label(cx.cx, "motion3d/contact-depth").transient(cx.cx);
                self.contact_pass = Some((pass.0, pass.1, pass.2, cc, cd, c.size));
            }
            let mut cworld = scene::World::new();
            let cmat = scene::MaterialFrame {
                id: scene::MaterialId(CONTACT_PROGRAM),
                kind: scene::MaterialKind::Splash(scene::SplashMaterial { base: scene::BaseKind::Pbr, program: scene::MaterialProgramId(CONTACT_PROGRAM), uniforms: [Vec4f::default(); 4], textures: Vec::new() }),
                blend: scene::Blend::Opaque,
                side: scene::Side::Double,
                depth_write: true,
                glow: 0.0,
                lighting: scene::LightingModel::Unlit,
            };
            cworld.set_material(cmat);
            for (m, item) in f.meshes.iter().zip(world.items.iter()) {
                if m.cast_shadow && !m.ground && !m.material.is_blended() {
                    let mut it = item.clone();
                    if let scene::ItemKind::Instances { material, .. } = &mut it.kind {
                        *material = scene::MaterialId(CONTACT_PROGRAM);
                    }
                    it.flags = scene::ItemFlags::empty();
                    cworld.items.push(it);
                }
            }
            let cview = c.view;
            let cproj = Mat4f::mul(&c.view_proj, &c.view.invert());
            let ceye = c.view.invert();
            let csize = dvec2(c.size as f64, c.size as f64);
            let cstate = SceneState3D {
                time: f.t as f64,
                camera_pos: vec3f(ceye.v[12], ceye.v[13], ceye.v[14]),
                view: cview,
                projection: cproj,
                viewport_rect: Rect { pos: dvec2(0.0, 0.0), size: csize },
            };
            let (p, root, list, ccolor, cdepth, _) = self.contact_pass.as_mut().unwrap();
            record_aux(cx, &mut self.renderer, self.draws.as_mut().unwrap(), p, root, list, ccolor, cdepth, &cworld, cstate, vec4(1.0, 1.0, 1.0, 1.0), scene_id, false);
        }
        // The outlined surfaces' ids, each over its own depth (an outline
        // goes round whole shapes, seen through what covers them).
        self.id_live = f.post.outline.is_some() && f.meshes.iter().any(|m| m.outline);
        if let (false, Some(r)) = (self.id_live, &self.id_pass) {
            release_targets(cx.cx, &[&r.3, &r.4]);
        }
        if self.id_live {
            if self.id_pass.as_ref().is_none_or(|r| r.5 != size) {
                let fixed = TextureSize::Fixed { width: size.0, height: size.1 };
                let pass = self.id_pass.take().map(|r| (r.0, r.1, r.2)).unwrap_or_else(|| {
                    let p = DrawPass::new_with_name(cx.cx, "motion3d shared ids");
                    p.set_keep_camera_matrix(cx.cx, true);
                    (p, DrawList2d::new(cx.cx), DrawList::new(cx.cx))
                });
                let ic = Texture::new_with_format(cx.cx, TextureFormat::RenderRGBAf16 { size: fixed.clone(), initial: true }).label(cx.cx, "motion3d/id-color").transient(cx.cx);
                let idp = Texture::new_with_format(cx.cx, TextureFormat::DepthD32 { size: fixed, initial: true }).label(cx.cx, "motion3d/id-depth").transient(cx.cx);
                self.id_pass = Some((pass.0, pass.1, pass.2, ic, idp, size));
            }
            let iworld = id_world(f, &world, |m| m.outline);
            let istate = SceneState3D { time: f.t as f64, camera_pos: v3(f.camera.position), view: f.camera.view, projection: f.camera.proj, viewport_rect: Rect { pos: dvec2(0.0, 0.0), size: dsize } };
            let (p, root, list, ic, idp, _) = self.id_pass.as_mut().unwrap();
            record_aux(cx, &mut self.renderer, self.draws.as_mut().unwrap(), p, root, list, ic, idp, &iworld, istate, vec4(0.0, 0.0, 0.0, 0.0), scene_id, false);
        }
        // Screen coverage (an inspect asks; an export never does): every
        // mesh's id over one depth, so each pixel names the surface seen
        // there. An 8-bit target that is kept, for the host's readback.
        if let (false, Some(r)) = (self.coverage, &self.cover_pass) {
            release_targets(cx.cx, &[&r.3, &r.4]);
        }
        if self.coverage {
            if self.cover_pass.as_ref().is_none_or(|r| r.5 != size) {
                let fixed = TextureSize::Fixed { width: size.0, height: size.1 };
                let pass = self.cover_pass.take().map(|r| (r.0, r.1, r.2)).unwrap_or_else(|| {
                    let p = DrawPass::new_with_name(cx.cx, "motion3d shared coverage");
                    p.set_keep_camera_matrix(cx.cx, true);
                    (p, DrawList2d::new(cx.cx), DrawList::new(cx.cx))
                });
                let cc = Texture::new_with_format(cx.cx, TextureFormat::RenderBGRAu8 { size: fixed.clone(), initial: true }).label(cx.cx, "motion3d/coverage-ids");
                let cd = Texture::new_with_format(cx.cx, TextureFormat::DepthD32 { size: fixed, initial: true }).label(cx.cx, "motion3d/coverage-depth").transient(cx.cx);
                self.cover_pass = Some((pass.0, pass.1, pass.2, cc, cd, size));
            }
            let cworld = id_world(f, &world, |_| true);
            let cstate = SceneState3D { time: f.t as f64, camera_pos: v3(f.camera.position), view: f.camera.view, projection: f.camera.proj, viewport_rect: Rect { pos: dvec2(0.0, 0.0), size: dsize } };
            let (p, root, list, cc, cd, _) = self.cover_pass.as_mut().unwrap();
            record_aux(cx, &mut self.renderer, self.draws.as_mut().unwrap(), p, root, list, cc, cd, &cworld, cstate, vec4(0.0, 0.0, 0.0, 0.0), scene_id, false);
        }
        // The planar reflection: the scene without its reflecting floors,
        // mirrored about the floor plane, into its own target (parented
        // under the scene pass, which samples it); every surface two-sided,
        // as the mirror turns the winding.
        let refl_floors: Vec<usize> = f.meshes.iter().enumerate().filter(|(_, m)| reflects(f, m)).map(|(i, _)| i).collect();
        self.refl_live = f.reflector.is_some() && !refl_floors.is_empty();
        if let (false, Some(r)) = (self.refl_live, &self.refl_pass) {
            release_targets(cx.cx, &[&r.3, &r.4]);
        }
        if let (Some(h), false) = (f.reflector, refl_floors.is_empty()) {
            if self.refl_pass.as_ref().is_none_or(|r| r.5 != size) {
                let fixed = TextureSize::Fixed { width: size.0, height: size.1 };
                let pass = self.refl_pass.take().map(|r| (r.0, r.1, r.2)).unwrap_or_else(|| {
                    let p = DrawPass::new_with_name(cx.cx, "motion3d shared reflection");
                    p.set_keep_camera_matrix(cx.cx, true);
                    (p, DrawList2d::new(cx.cx), DrawList::new(cx.cx))
                });
                let rc = Texture::new_with_format(cx.cx, TextureFormat::RenderRGBAf16 { size: fixed.clone(), initial: true }).label(cx.cx, "motion3d/reflection-color").transient(cx.cx);
                let rd = Texture::new_with_format(cx.cx, TextureFormat::DepthD32 { size: fixed, initial: true }).label(cx.cx, "motion3d/reflection-depth").transient(cx.cx);
                self.refl_pass = Some((pass.0, pass.1, pass.2, rc, rd, size));
            }
            let mut rworld = world.clone();
            let mut k = 0;
            rworld.items.retain(|_| {
                k += 1;
                !refl_floors.contains(&(k - 1))
            });
            for m in rworld.materials.iter_mut() {
                m.side = scene::Side::Double;
            }
            let mut mirror = Mat4f::identity();
            mirror.v[5] = -1.0;
            mirror.v[13] = 2.0 * h;
            let eye = f.camera.position;
            let rstate = SceneState3D {
                time: f.t as f64,
                camera_pos: v3([eye[0], 2.0 * h - eye[1], eye[2]]),
                view: Mat4f::mul(&f.camera.view, &mirror),
                projection: f.camera.proj,
                viewport_rect: Rect { pos: dvec2(0.0, 0.0), size: dsize },
            };
            let (p, root, list, rcolor, rdepth, _) = self.refl_pass.as_mut().unwrap();
            record_aux(cx, &mut self.renderer, self.draws.as_mut().unwrap(), p, root, list, rcolor, rdepth, &rworld, rstate, vec4(clear[0], clear[1], clear[2], clear[3]), scene_id, true);
        }
        // Glass: the frame without its glass surfaces, from the camera, into
        // its own target the glass programs sample (parented under the
        // scene pass).
        let glass_items: Vec<usize> = f.meshes.iter().enumerate().filter(|(_, m)| glass(&m.material)).map(|(i, _)| i).collect();
        if let (true, Some(r)) = (glass_items.is_empty(), &self.behind_pass) {
            release_targets(cx.cx, &[&r.3, &r.4]);
        }
        if !glass_items.is_empty() {
            if self.behind_pass.as_ref().is_none_or(|r| r.5 != size) {
                let fixed = TextureSize::Fixed { width: size.0, height: size.1 };
                let pass = self.behind_pass.take().map(|r| (r.0, r.1, r.2)).unwrap_or_else(|| {
                    let p = DrawPass::new_with_name(cx.cx, "motion3d behind glass");
                    p.set_keep_camera_matrix(cx.cx, true);
                    (p, DrawList2d::new(cx.cx), DrawList::new(cx.cx))
                });
                let bc = Texture::new_with_format(cx.cx, TextureFormat::RenderRGBAf16 { size: fixed.clone(), initial: true }).label(cx.cx, "motion3d/behind-color").transient(cx.cx);
                let bd = Texture::new_with_format(cx.cx, TextureFormat::DepthD32 { size: fixed, initial: true }).label(cx.cx, "motion3d/behind-depth").transient(cx.cx);
                self.behind_pass = Some((pass.0, pass.1, pass.2, bc, bd, size));
            }
            let mut bworld = world.clone();
            let mut k = 0;
            bworld.items.retain(|_| {
                k += 1;
                !glass_items.contains(&(k - 1))
            });
            let bstate = SceneState3D { time: f.t as f64, camera_pos: v3(f.camera.position), view: f.camera.view, projection: f.camera.proj, viewport_rect: Rect { pos: dvec2(0.0, 0.0), size: dsize } };
            let (p, root, list, bcolor, bdepth, _) = self.behind_pass.as_mut().unwrap();
            record_aux(cx, &mut self.renderer, self.draws.as_mut().unwrap(), p, root, list, bcolor, bdepth, &bworld, bstate, vec4(clear[0], clear[1], clear[2], clear[3]), scene_id, true);
            let behind = self.behind_pass.as_ref().unwrap().3.clone();
            for &i in &glass_items {
                self.renderer.set_custom_material_texture(&makepad_render::splash_material_name(glass_program_id(&f.meshes[i].material)), Some(behind.clone()));
            }
        }
        let (scene_pass, root_list, scene_list, line_list) = self.scene_pass.as_mut().unwrap();
        scene_pass.set_color_texture(cx.cx, &color, DrawPassClearColor::ClearWith(vec4(clear[0], clear[1], clear[2], clear[3])));
        scene_pass.set_depth_texture(cx.cx, &depth, DrawPassClearDepth::ClearWith(1.0));
        cx.begin_pass(scene_pass, Some(1.0));
        scene_pass.set_size(cx.cx, dsize);
        let state = SceneState3D {
            time: f.t as f64,
            camera_pos: v3(f.camera.position),
            view: f.camera.view,
            projection: f.camera.proj,
            viewport_rect: Rect { pos: dvec2(0.0, 0.0), size: dsize },
        };
        makepad_render::set_pass_camera(cx.cx, scene_pass, &state);
        // One root list for the pass: the renderer's and the lines' lists
        // are its children, drawn in that order. The pass's own root
        // turtle: nothing it draws is clipped to the host's rectangle.
        root_list.begin_always(cx);
        cx.begin_root_turtle_for_pass(Layout::default());
        let d = self.draws.as_mut().unwrap();
        {
            let cx3d = &mut Cx3d::new(cx.cx);
            let mut draws = makepad_render::SceneDraws {
                cube: Some(&mut d.cube),
                alpha: Some(&mut d.alpha),
                sky: &mut d.sky,
                sky_analytic: None,
                terrain: Some(&mut d.terrain),
                shadow: None,
                shadow_sdf: None,
                firework: None,
                flare: None,
                water: None,
                screen: None,
                screen_instances: &[],
                view_model: None,
            };
            self.renderer.set_models(Vec::new());
            let st = self.renderer.draw_scene_full(cx3d, scene_list, &mut draws, &world, state, None, Some(&mut d.models));
            self.stats.instances = (st.static_instances + st.dyn_instances) as usize;
            // The cascades a frame renders are the ones its receivers read
            // from the second recording on: whole once two in a row drew.
            let casters = world.items.iter().any(|i| i.flags.contains(scene::ItemFlags::CAST_SHADOW));
            self.csm_frames = if st.csm_draws > 0 { self.csm_frames + 1 } else { 0 };
            self.shadows_warm = !(sun_shadow && casters) || self.csm_frames >= 3;
        }
        if let Some(lr) = self.lines.as_mut() {
            line_list.begin_always(cx);
            // Widths in the scene target's pixels, supersampled or not (as
            // the gpu/ lane draws them).
            let view = lines::LineView::world(&f.camera.view, &f.camera.proj, (size.0 as f32, size.1 as f32), 1.0);
            for b in &line_batches {
                lr.draw_lines(cx, &view, b);
            }
            for b in &point_batches {
                lr.draw_points(cx, &view, b);
            }
            line_list.end(cx);
        }
        cx.end_pass_sized_turtle();
        root_list.end(cx);
        // World space: no view transform inherited from the host's lists.
        root_list.set_view_transform(cx.cx, &Mat4f::identity());
        scene_list.set_view_transform(cx.cx, &Mat4f::identity());
        line_list.set_view_transform(cx.cx, &Mat4f::identity());
        cx.end_pass(scene_pass);
        // A pipeline still compiling drew nothing this recording (a
        // cascade's casters, an item): the frame settles only from two
        // recordings after every pipeline exists.
        if cx.cx.draw_shaders_pending() {
            self.ao_count = 0;
        }
        scene_pass.draw_pass_id()
    }

    /// Post for frame `center` on the linear image `hdr_in` (made by pass
    /// `producer`): the graph's @hdr passes, the tone map, then the graph's
    /// @display and @final passes. Appends the passes to `chain`; returns
    /// the last one and its texture.
    #[allow(clippy::too_many_arguments)]
    fn record_post(&mut self, cx: &mut Cx2d, center: &SceneFrame, hdr_in: &Texture, producer: DrawPassId, size: (usize, usize), out_size: (usize, usize), ss: usize, chain: &mut Vec<DrawPassId>) -> (DrawPassId, Texture) {
        let out_dsize = dvec2(out_size.0 as f64, out_size.1 as f64);
        let linear = self.linear_out;
        let (depth, out) = (self.depth.clone().unwrap(), if linear { self.lin.clone().unwrap() } else { self.out.clone().unwrap() });
        // motion3d's own post nodes as the graph's kits, then the
        // document's graph passes, planned for this size.
        // Contact shadows read the map the scene recording drew.
        let ground_y = center.meshes.iter().filter(|m| m.ground).map(|m| m.bounds.0[1]).fold(f32::MAX, f32::min);
        let contact = center.contact.as_ref().filter(|_| self.contact_pass.is_some() && ground_y < f32::MAX).map(|c| (c, ground_y));
        // The scene's named textures (its `layers:` drawn by the host,
        // `textures:`) are readable by name, as a pass's `reads`.
        let mut named: Vec<(LiveId, Texture)> = self.host_textures();
        if let (Some(_), Some(cp)) = (&contact, &self.contact_pass) {
            named.push((LiveId::from_str(CONTACT_MAP), cp.3.clone()));
        }
        let reflect = match (&self.refl_pass, self.refl_live, center.reflector) {
            (Some(rp), true, Some(h)) => center.meshes.iter().find(|m| reflects(center, m)).map(|m| {
                named.push((LiveId::from_str(REFLECT_MAP), rp.3.clone()));
                (m.material.reflectivity, m.material.reflection_blur, h)
            }),
            _ => None,
        };
        // The outlined surfaces' ids: the graph's @id attachment.
        let id_map = self.id_live && self.id_pass.is_some();
        let id_tex = self.id_pass.as_ref().filter(|_| id_map).map(|ip| ip.3.clone());
        let host_named: Vec<LiveId> = named.iter().map(|n| n.0).collect();
        // Linear output: only what acts on the scene's light (depth of
        // field, the realtime velocity blur); the display look is the host's.
        let display_post;
        let post = if linear {
            display_post = PostFrame { dof: center.post.dof, velocity_blur: center.post.velocity_blur.clone(), ..PostFrame::default() };
            &display_post
        } else {
            &center.post
        };
        let (mut decls, mut graph_values) = match cx.cx.try_with_vm(|vm| post_kit_passes(vm, post, reflect, contact, id_map && !linear, size, ss)) {
            Some(Ok(k)) => k,
            Some(Err(e)) => {
                if !self.errors.contains(&e) {
                    self.errors.push(e);
                }
                (Vec::new(), Vec::new())
            }
            None => (Vec::new(), Vec::new()),
        };
        if let Some((d, v)) = &center.post.graph {
            decls.extend(d.iter().cloned());
            graph_values.extend(v.iter().cloned());
        }
        // Supersampled, the stages after the resolve run at the output
        // size on their own plan.
        let split = size != out_size;
        self.graph.set_passes(&decls);
        if split {
            self.graph_out.set_passes(&decls);
        }
        let locked = self.pending.is_some() || chain.len() > 1;
        let graph_ok = !decls.is_empty() && {
            let at = makepad_render_graph::Attachments { depth: true, id: id_tex.is_some(), ..Default::default() };
            let mut ok = self.graph.prepare(cx.cx, (size.0 as u32, size.1 as u32), at, &host_named, locked, 1 << 32);
            if split {
                ok &= self.graph_out.prepare(cx.cx, (out_size.0 as u32, out_size.1 as u32), at, &host_named, locked, 1 << 32);
            }
            for e in self.graph.errors.drain(..).chain(self.graph_out.errors.drain(..)) {
                if !self.errors.contains(&e) {
                    self.errors.push(e);
                }
            }
            ok
        };
        let frame_uniforms = makepad_render_graph::FrameUniforms {
            time: center.t,
            frame: (center.t * 60.0).round(),
            ss_tap: -1.0,
            seed: self.grain_seed.fract(),
            exposure: center.exposure,
            camera: [center.camera.proj.v[10], center.camera.proj.v[14], if center.camera.orthographic { 1.0 } else { 0.0 }, 0.0],
        };
        // Passes see the camera (raymarch passes: ray_dir, eye,
        // view_distance; the velocity blur: where the camera was a frame
        // ago), and px_scale() is frame pixels per output pixel.
        let ss_frame = (size.0 / out_size.0.max(1)).max(1);
        if graph_ok {
            let cam = &center.camera;
            let vp = Mat4f::mul(&cam.proj, &cam.view);
            let prev = match self.view_proj_history {
                Some((t, _, prev)) if t == center.t.to_bits() => prev,
                Some((_, last, _)) => last,
                None => vp.v,
            };
            let view = makepad_render_graph::PassView { inv_view_proj: vp.invert().v, eye: cam.position, forward: cam.forward, prev_view_proj: prev };
            // px_scale(): a pass's pixels per output pixel. The scene-size
            // plan is ss; the output-size plan (after the resolve) is 1.
            self.graph.set_view(Some(view));
            self.graph.set_px_scale(ss_frame as f32);
            self.graph_out.set_view(Some(view));
            self.graph_out.set_px_scale(1.0);
        }
        if self.pending.is_none() {
            let cam = &center.camera;
            let vp = Mat4f::mul(&cam.proj, &cam.view).v;
            let key = center.t.to_bits();
            self.view_proj_history = Some(match self.view_proj_history {
                Some((t, _, prev)) if t == key => (key, vp, prev),
                Some((_, last, _)) => (key, vp, last),
                None => (key, vp, vp),
            });
        }
        let (out_pass, out_list) = self.out_pass.as_mut().unwrap();
        let out_id = out_pass.draw_pass_id();

        // ---- the graph's @hdr passes on the linear frame -----------------
        let mut hdr = hdr_in.clone();
        let mut fed = false;
        if graph_ok && self.graph.has_stage(makepad_render_graph::Stage::Hdr) {
            let inputs = makepad_render_graph::StageInputs { color: hdr_in, depth: Some(&depth), normal: None, velocity: None, glow: None, id: id_tex.as_ref(), named: &named };
            if let Some((o, first)) = self.graph.run_stage(cx, makepad_render_graph::Stage::Hdr, &inputs, &graph_values, frame_uniforms, Some(out_id)) {
                cx.cx.attach_child_pass(producer, first, None);
                chain.extend(self.graph.pass_ids(makepad_render_graph::Stage::Hdr));
                hdr = o;
                fed = true;
            }
        }
        if !fed {
            cx.cx.attach_child_pass(producer, out_id, None);
        }

        // ---- composite: exposure and the tone curve to sRGB -------------
        out_pass.set_color_texture(cx.cx, &out, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
        cx.make_child_pass(out_pass);
        cx.begin_pass(out_pass, Some(1.0));
        out_pass.set_size(cx.cx, out_dsize);
        out_list.begin_always(cx);
        // The pass's own root turtle: its quad is clipped to this pass, not
        // to the host's rectangle.
        cx.begin_root_turtle_for_pass(Layout::default());
        let curve = match center.tone_mapping {
            ToneMapping::None => ToneCurve::None,
            ToneMapping::Linear => ToneCurve::Linear,
            ToneMapping::Reinhard => ToneCurve::Reinhard,
            ToneMapping::Aces => ToneCurve::Aces,
            ToneMapping::AgX => ToneCurve::Agx,
            ToneMapping::Filmic => ToneCurve::Filmic,
        };
        let grade = center.grade.map(|g| Grade { contrast: g.contrast, saturation: g.saturation, temperature: g.temperature, tint: g.tint, lift: g.lift, gamma: g.gamma, gain: g.gain });
        let d = self.draws.as_mut().unwrap();
        d.tonemap.set(curve, center.exposure, ss as u32, size, grade);
        if linear {
            d.tonemap.set_linear_output();
        }
        d.tonemap.draw_vars.set_texture(0, &hdr);
        d.tonemap.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: out_dsize });
        cx.end_pass_sized_turtle();
        out_list.end(cx);
        cx.end_pass(out_pass);
        chain.push(out_id);

        // ---- the graph's @display then @final passes on the output -------
        let mut tail = (out_id, out);
        if graph_ok && !linear {
            for stage in [makepad_render_graph::Stage::Display, makepad_render_graph::Stage::Final] {
                let inputs = makepad_render_graph::StageInputs { color: &tail.1, depth: Some(&depth), normal: None, velocity: None, glow: None, id: id_tex.as_ref(), named: &named };
                let graph = if split { &mut self.graph_out } else { &mut self.graph };
                if let Some((o, first)) = graph.run_stage(cx, stage, &inputs, &graph_values, frame_uniforms, None) {
                    // The pass before feeds the graph now, not the host.
                    cx.cx.attach_child_pass(tail.0, first, None);
                    chain.extend(graph.pass_ids(stage));
                    tail = (*graph.pass_ids(stage).last().unwrap(), o);
                }
            }
        }
        tail
    }
}

/// The sub-pixel jitter of sub-frame `k` (Halton 2, 3), in pixels.
fn halton_jitter(k: usize) -> (f32, f32) {
    let h = |mut i: usize, b: usize| {
        let (mut f, mut r) = (1.0f32, 0.0f32);
        i += 1;
        while i > 0 {
            f /= b as f32;
            r += f * (i % b) as f32;
            i /= b;
        }
        r
    };
    (h(k, 2) - 0.5, h(k, 3) - 0.5)
}

/// A frame being recorded as sub-frames.
struct Pending {
    center: SceneFrame,
    size: (usize, usize),
    out_size: (usize, usize),
    recorded: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_of(src: &str) -> SceneFrame {
        use crate::doc::{FileResolver, LoadOptions, Scene3dDoc};
        let resolver = FileResolver::default();
        let opts = LoadOptions { name: "shared".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None };
        let mut doc = Scene3dDoc::load(src, opts).unwrap_or_else(|e| panic!("{}", e.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
        let mut cache = crate::eval::EvalCache::default();
        doc.eval(&crate::eval::EvalInput { t: 0.0, aspect: 16.0 / 9.0, audio: &crate::anim::NoAudio, edit_offset: 0.0 }, &mut cache)
    }

    /// Sub-frames recorded one per paint while the environment prepares:
    /// the ones before the landing lack it, so the frame is recorded again.
    #[test]
    fn a_sampled_frame_is_unsettled_when_its_sub_frames_saw_the_environment_differently() {
        let mut e = SubframeEnvironment::default();
        // The environment was in from the start.
        e.recorded(false, false);
        e.recorded(false, false);
        assert!(!e.unsettled);

        // The first sub-frame's draw adopted a finished job (it landed in
        // the gap between two accumulations): that draw is the first with the
        // lane texture bound, so it builds the environment's item material,
        // whose pipeline compiles while the sub-frame is drawn through the
        // stock matte lane; the later sub-frames wait for the compile. The
        // frame is mixed and is recorded again.
        e.begin();
        e.recorded(true, false);
        assert!(e.unsettled, "adopted by the first sub-frame: it is drawn before the item material compiles");
        // Once set it stays, however clean the later sub-frames are.
        e.recorded(false, false);
        assert!(e.unsettled);
        // The re-record finds the job adopted and is whole: no loop.
        e.begin();
        assert!(!e.unsettled);
        e.recorded(false, false);
        e.recorded(false, false);
        assert!(!e.unsettled, "nothing pending, before or after");

        // The first sub-frame's draw submitted the job (or it is running):
        // no environment in it.
        e.begin();
        e.recorded(false, true);
        assert!(e.unsettled);
        e.recorded(false, false);
        assert!(e.unsettled);

        // The job was in flight before the first sub-frame and still is.
        e.begin();
        e.recorded(true, true);
        assert!(e.unsettled);

        // A landing between sub-frames: the earlier ones lack it.
        e.begin();
        assert!(!e.unsettled);
        e.recorded(false, false);
        e.recorded(true, false);
        assert!(e.unsettled, "adopted by a later sub-frame");
    }

    #[test]
    fn materials_map_and_name_what_they_lose() {
        let f = frame_of("use mod.scene.*; Scene3D{\n camera: Camera{position: vec3(0, 0, 5) target: vec3(0, 0, 0) fov: 40}\n lights: [DirectionalLight{position: vec3(4, 8, 3) intensity: 2}]\n children: [\n Mesh{geometry: BoxGeometry{width: 1 height: 1 depth: 1} material: MeshStandardMaterial{color: #ff8800 metalness: 0.5 roughness: 0.3}}\n Mesh{geometry: BoxGeometry{width: 1 height: 1 depth: 1} position: vec3(2, 0, 0) material: MeshToonMaterial{color: #33aaff steps: 4}}\n ]\n}");
        let (w, gaps) = to_world(&f, &Pictures::new());
        assert!(gaps.is_empty(), "{gaps:?}");
        let doc = f;
        let (w2, _) = to_world(&doc, &Pictures::new());
        assert_eq!(w.items.len(), w2.items.len());
        assert_eq!(w.items.len(), 2);
        assert!(w.validate_frame().is_ok(), "{:?}", w.validate_frame());
        let pbr: Vec<_> = w.items.iter().filter_map(|i| match &i.kind {
            scene::ItemKind::Instances { material, count, .. } => { assert_eq!(*count, 1); w.material(*material) }
            _ => None,
        }).filter_map(|m| match &m.kind { scene::MaterialKind::Pbr(p) => Some(*p), _ => None }).collect();
        assert!(pbr.iter().any(|p| (p.metallic - 0.5).abs() < 1e-6 && (p.roughness - 0.3).abs() < 1e-6), "{pbr:?}");
        assert!(w.lights.iter().any(|l| matches!(l, scene::Light::Sun { dir, .. } if dir.y > 0.0)), "the sun points back toward the light");
    }

    #[test]
    fn shader_materials_become_splash_programs_on_the_pbr_lane() {
        let f = frame_of("use mod.scene.*; Scene3D{\n camera: Camera{position: vec3(0, 0, 5) target: vec3(0, 0, 0) fov: 40}\n children: [\n Mesh{geometry: BoxGeometry{width: 1 height: 1 depth: 1} material: ShaderMaterial{uniforms: {amount: 0.5}\n surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 { return vec4(n * 0.5 + 0.5, 1.0) }\n vertex_displace: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec3 { return p + n * (self.fbm(p * 3.0) * self.u_custom.x) }\n emissive_fn: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec3 { return vec3(0.1, 0.0, 0.0) * sin(t) }\n shade_fn: fn(c: vec4, p: vec3, n: vec3, uv: vec2, t: float) -> vec4 { return vec4(c.xyz * 0.9, c.w) }}}\n ]\n}");
        let hooks = f.meshes[0].material.shader.clone().expect("a ShaderMaterial");
        let (w, _) = to_world(&f, &Pictures::new());
        let pid = program_id(&f.meshes[0].material);
        assert!(w.materials.iter().any(|m| matches!(&m.kind, scene::MaterialKind::Splash(s) if s.program == pid && (s.uniforms[0].x - 0.5).abs() < 1e-6)));
        let mut cx = Cx::new(Box::new(|_, _| {}));
        cx.with_vm(|vm| {
            makepad_draw::script_mod(vm);
            vm.bx.heap_mut().new_module(id!(prelude));
            makepad_draw::makepad_platform::makepad_script::script_eval!(vm, { mod.prelude.widgets_internal = { ..mod.std, ..mod.pod, ..mod.math, ..mod.sdf, ..mod.shader, draw:mod.draw } });
            vm.bx.heap_mut().new_module(id!(widgets));
            makepad_render::custom_material::register(vm);
            let desc = makepad_render::makepad_render_material::MaterialDesc::default();
            let m = hook_program(vm, &hooks, &f.meshes[0].material, &desc).unwrap_or_else(|e| panic!("{e}\n{}", hook_source(&hooks, &f.meshes[0].material)));
            assert!(m.draw.draw_vars.can_instance());
            assert!(m.shadow.is_some(), "the displaced ShaderMaterial casts a displaced shadow");
        });
    }
}
