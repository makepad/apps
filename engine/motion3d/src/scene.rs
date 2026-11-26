//! The typed Scene3D document: what [`crate::doc::Scene3dDoc::load`] reads
//! out of a `Scene3D{...}` Splash value. Geometry is built once at load
//! (static meshes are shared `Arc`s); everything keyable stays an
//! [`Anim`] until [`crate::eval`] resolves it at a time.

use crate::anim::{Anim, Anim1, Anim3, Anim4};
use makepad_render_material::ibl::EnvPreset;
use makepad_geom3d::Curve3;
use crate::glb::GlbModel;
use crate::layout::InstanceBase;
use makepad_geom3d::mesh::Mesh;
use crate::particles::ParticleSystem;
use crate::script::{Loc, MediaRef};
use std::sync::Arc;

/// A static mesh with its content hash (the GPU upload key).
#[derive(Clone, Debug)]
pub struct SharedMesh {
    pub mesh: Arc<Mesh>,
    pub key: u64,
}

impl SharedMesh {
    pub fn new(mesh: Mesh) -> Self {
        let key = mesh.content_hash();
        Self { mesh: Arc::new(mesh), key }
    }
}

/// Position, rotation (Euler degrees, x then y then z), scale, all keyable;
/// `look_at` turns the node's +z toward a point (overrides rotation);
/// `pivot` is the point (in the node's own units) it rotates and scales
/// about.
#[derive(Clone, Debug)]
pub struct TransformDef {
    pub position: Anim3,
    pub rotation: Anim3,
    pub scale: Anim3,
    pub look_at: Option<Anim3>,
    /// `look_at: @camera`: always face the camera (labels, callouts).
    pub face_camera: bool,
    pub pivot: Anim3,
}

impl Default for TransformDef {
    fn default() -> Self {
        Self { position: Anim::Const([0.0; 3]), rotation: Anim::Const([0.0; 3]), scale: Anim::Const([1.0; 3]), look_at: None, face_camera: false, pivot: Anim::Const([0.0; 3]) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaceSide {
    Front,
    Back,
    Double,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blending {
    Normal,
    Additive,
    Multiply,
}

/// The shading model of a material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadingModel {
    /// PBR metal/roughness (standard and physical share it; physical adds
    /// clearcoat, transmission, sheen, iridescence).
    Standard,
    /// Unlit colour.
    Basic,
    /// Diffuse only (Lambert; Phong maps to Standard).
    Lambert,
    /// Banded diffuse with `steps` and a rim.
    Toon,
    /// A material-capture sphere lookup (`matcap` preset).
    Matcap,
    /// World normal as colour.
    Normal,
    /// View depth as grey.
    Depth,
}

/// A built-in matcap: the sphere lookup is procedural, so no image asset is
/// needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatcapPreset {
    Clay,
    Chrome,
    Gold,
    Pearl,
    Jade,
    Toon,
}

impl MatcapPreset {
    pub const NAMES: &'static [&'static str] = &["clay", "chrome", "gold", "pearl", "jade", "toon"];
    pub fn by_name(s: &str) -> Option<Self> {
        Some(match s {
            "clay" => Self::Clay,
            "chrome" => Self::Chrome,
            "gold" => Self::Gold,
            "pearl" => Self::Pearl,
            "jade" => Self::Jade,
            "toon" => Self::Toon,
            _ => return None,
        })
    }
}

/// A decoded RGBA8 image (a texture source), with its content key.
#[derive(Clone, Debug)]
pub struct ImageData {
    pub width: usize,
    pub height: usize,
    pub rgba: Arc<Vec<u8>>,
    /// Colour data: the GPU samples it as sRGB.
    pub srgb: bool,
    pub key: u64,
}

impl ImageData {
    pub fn new(width: usize, height: usize, rgba: Arc<Vec<u8>>, srgb: bool) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (width as u64) << 32 ^ height as u64;
        for chunk in rgba.chunks(64) {
            for &b in chunk.iter().step_by(7) {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        h ^= rgba.len() as u64;
        Self { width, height, rgba, srgb, key: makepad_geom3d::math::hash64(h) }
    }

    /// Decode PNG or JPEG bytes.
    pub fn decode(bytes: &[u8], srgb: bool) -> Result<Self, String> {
        use makepad_widgets::ImageBuffer;
        let image = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            ImageBuffer::from_png(bytes).map_err(|e| format!("png: {e:?}"))?
        } else {
            ImageBuffer::from_jpg(bytes).map_err(|e| format!("jpeg: {e:?}"))?
        };
        let rgba: Vec<u8> = image.data.iter().flat_map(|p| { let [b, g, r, a] = p.to_le_bytes(); [r, g, b, a] }).collect();
        Ok(Self::new(image.width, image.height, Arc::new(rgba), srgb))
    }
}

/// The textures of a material (glTF's set).
#[derive(Clone, Debug, Default)]
pub struct MaterialMaps {
    pub base: Option<ImageData>,
    pub metal_rough: Option<ImageData>,
    pub normal: Option<ImageData>,
    pub emissive: Option<ImageData>,
    pub occlusion: Option<ImageData>,
}

/// A custom Splash shader material: hook functions as written (source
/// text), compiled into a subclass of the mesh shader on the GPU side.
#[derive(Clone, Debug, PartialEq)]
pub struct ShaderHooks {
    /// `name: fn(...) {...}` fields, each as written.
    pub fields: Vec<(String, String)>,
    /// Content hash of the hooks (the compiled-shader cache key).
    pub key: u64,
    /// Where hook functions are written in an edited document (edit mode's
    /// live shader literals); empty otherwise.
    pub origins: Vec<HookOrigin>,
}

/// Where a hook function's text (from its `fn`) is written: the field it
/// is, the file by its live literal name, and the zero-based line and
/// column of the `fn`.
#[derive(Clone, Debug, PartialEq)]
pub struct HookOrigin {
    pub field: String,
    pub file: String,
    pub line: u32,
    pub col: u32,
}

/// A material, every numeric parameter keyable. Colours are linear.
#[derive(Clone, Debug)]
pub struct MaterialDef {
    pub name: Option<String>,
    pub model: ShadingModel,
    pub color: Anim4,
    pub opacity: Anim1,
    pub metalness: Anim1,
    pub roughness: Anim1,
    pub emissive: Anim4,
    pub emissive_intensity: Anim1,
    pub clearcoat: Anim1,
    pub clearcoat_roughness: Anim1,
    pub transmission: Anim1,
    pub ior: Anim1,
    pub thickness: Anim1,
    pub attenuation_color: Anim4,
    pub sheen: Anim1,
    pub sheen_color: Anim4,
    pub iridescence: Anim1,
    pub specular_intensity: Anim1,
    pub env_intensity: Anim1,
    /// Fresnel rim light: colour × intensity at grazing angles (a motion
    /// graphics staple; 0 = off).
    pub rim_color: Anim4,
    pub rim_power: Anim1,
    /// Toon bands.
    pub steps: u32,
    pub matcap: MatcapPreset,
    pub maps: MaterialMaps,
    pub normal_scale: f32,
    pub occlusion_strength: f32,
    /// Alpha test threshold (glTF MASK), `None` = off.
    pub alpha_cutoff: Option<f32>,
    pub vertex_colors: bool,
    pub flat_shading: bool,
    pub wireframe: bool,
    pub side: FaceSide,
    pub blending: Blending,
    pub transparent: bool,
    pub depth_write: bool,
    pub shader: Option<ShaderHooks>,
    /// Uniforms a shader material reads (`uniforms: {amount: tween(...)}`),
    /// four at most, as `self.u_custom.xyzw` in its hooks.
    pub uniforms: Vec<(String, Anim1)>,
    /// Car-paint flakes: amount (normal perturbation) and cells per unit.
    pub flakes: Anim1,
    pub flake_scale: f32,
    /// Brushed metal streaks: amount and streaks per uv unit.
    pub brushed: Anim1,
    pub brush_scale: f32,
    /// Planar reflection of the scene (a floor), 0..1, and its blur 0..1.
    pub reflectivity: Anim1,
    pub reflection_blur: Anim1,
    /// `refraction: @environment` bends into the environment only.
    pub refraction_env: bool,
    pub fade: Option<(f32, f32)>,
    /// A texture the host supplies by name (a video or app-run frame on a
    /// device screen): used as the base map.
    pub external: Option<String>,
    /// How the base map's picture fits the surface: stretched (`@fill`, the
    /// three.js default), cropped to the surface's aspect (`@cover`), or
    /// letterboxed (`@contain`). Device screens default to `@cover`.
    pub fit: MapFit,
    /// The part of the picture to show, `[x, y, w, h]` in its pixels (top
    /// left origin), keyable: a reframe's `crop: keys(...)` follows the
    /// action. `@cover` grows it to the surface's aspect.
    pub crop: Option<Anim4>,
    /// The surface's width / height; 0 = from the mesh's x/y extent.
    pub aspect: f32,
    /// How the base map's rows run against the surface's uv: glTF maps are
    /// sampled as their uv say; pictures (images, and host textures: videos,
    /// app runs, 2D layers) are top row first on three.js-style uv (v up).
    pub map_space: MapSpace,
    /// An ink outline (inverted hull) this many pixels wide, and its colour.
    pub ink: Anim1,
    pub ink_color: Anim4,
    /// Wireframe with hidden lines: the faces filled with this colour (they
    /// hide the edges behind them), the edges `wire_width` pixels wide.
    pub wire_fill: Option<Anim4>,
    pub wire_width: f32,
    /// Draw only the shadows falling on it (compositing over 2D layers).
    pub shadow_catcher: bool,
    pub loc: Loc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapSpace {
    #[default]
    Raw,
    Picture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapFit {
    #[default]
    Fill,
    Cover,
    Contain,
}

impl MapFit {
    pub const NAMES: &'static [&'static str] = &["fill", "cover", "contain"];
    pub fn by_name(s: &str) -> Option<Self> {
        Some(match s {
            "fill" | "stretch" => Self::Fill,
            "cover" | "crop" => Self::Cover,
            "contain" | "fit" | "letterbox" => Self::Contain,
            _ => return None,
        })
    }
}

impl Default for MaterialDef {
    fn default() -> Self {
        Self {
            name: None,
            model: ShadingModel::Standard,
            color: Anim::Const([1.0; 4]),
            opacity: Anim::Const([1.0]),
            metalness: Anim::Const([0.0]),
            roughness: Anim::Const([0.5]),
            emissive: Anim::Const([0.0, 0.0, 0.0, 1.0]),
            emissive_intensity: Anim::Const([1.0]),
            clearcoat: Anim::Const([0.0]),
            clearcoat_roughness: Anim::Const([0.03]),
            transmission: Anim::Const([0.0]),
            ior: Anim::Const([1.5]),
            thickness: Anim::Const([0.5]),
            attenuation_color: Anim::Const([1.0; 4]),
            sheen: Anim::Const([0.0]),
            sheen_color: Anim::Const([1.0; 4]),
            iridescence: Anim::Const([0.0]),
            specular_intensity: Anim::Const([1.0]),
            env_intensity: Anim::Const([1.0]),
            rim_color: Anim::Const([1.0; 4]),
            rim_power: Anim::Const([0.0]),
            steps: 3,
            matcap: MatcapPreset::Clay,
            maps: MaterialMaps::default(),
            normal_scale: 1.0,
            occlusion_strength: 1.0,
            alpha_cutoff: None,
            vertex_colors: false,
            flat_shading: false,
            wireframe: false,
            side: FaceSide::Front,
            blending: Blending::Normal,
            transparent: false,
            depth_write: true,
            shader: None,
            uniforms: Vec::new(),
            flakes: Anim::Const([0.0]),
            flake_scale: 400.0,
            brushed: Anim::Const([0.0]),
            brush_scale: 300.0,
            reflectivity: Anim::Const([0.0]),
            reflection_blur: Anim::Const([0.2]),
            refraction_env: false,
            fade: None,
            external: None,
            fit: MapFit::Fill,
            crop: None,
            aspect: 0.0,
            map_space: MapSpace::Raw,
            ink: Anim::Const([0.0]),
            ink_color: Anim::Const([0.0, 0.0, 0.0, 1.0]),
            wire_fill: None,
            wire_width: 1.0,
            shadow_catcher: false,
            loc: Loc::default(),
        }
    }
}

/// Animated geometry deformation, done in the vertex shader (so it costs
/// nothing on the CPU and applies per instance).
#[derive(Clone, Debug, Default)]
pub struct DeformDef {
    /// Noise displacement along the normal: amount (units), frequency,
    /// speed (noise scroll per second), all keyable.
    pub displace: Option<(Anim1, Anim1, Anim1)>,
    /// Twist about y, degrees per unit of height.
    pub twist: Option<Anim1>,
    /// Bend about z, degrees over the geometry's height.
    pub bend: Option<Anim1>,
    /// A radial ripple along the normals: amount, frequency (waves per
    /// unit), speed (units per second).
    pub ripple: Option<(Anim1, Anim1, Anim1)>,
    /// Explode (> 0) / assemble (→ 0): every triangle flies out from the
    /// centre by this many units (the geometry is split per triangle).
    pub explode: Option<Anim1>,
    /// Page curl of a flat sheet in xy from its right edge: progress 0..1
    /// and curl radius.
    pub curl: Option<(Anim1, f32)>,
    /// Wrap around a vertical cylinder of this radius (x becomes the angle): text on a drum or around an object.
    pub wrap: Option<Anim1>,
}

/// Where a geometry comes from: built at load, or rebuilt per frame when it
/// has an animated build parameter (a tube's draw-on range).
#[derive(Clone, Debug)]
pub enum GeometrySrc {
    Static(SharedMesh),
    /// A tube drawn between `start` and `end` (0..1 along its curve).
    Tube { tube: makepad_geom3d::TubeGeo, start: Anim1, end: Anim1 },
    /// A vj_fx engine's geometry evaluated at `t` (metaballs, ribbons).
    Engine(crate::engines::EngineParams),
}

/// A GLB animation clip played by a Model node.
#[derive(Clone, Debug)]
pub struct ClipDef {
    pub clip: usize,
    /// Clip time in seconds; default `t * speed` (looping when `loop`).
    pub time: Option<Anim1>,
    pub speed: f32,
    pub looping: bool,
    pub weight: Anim1,
    pub offset: f32,
}

#[derive(Clone, Debug)]
pub struct NodeOverride {
    pub node: usize,
    pub position: Option<Anim3>,
    pub rotation: Option<Anim3>,
    pub scale: Option<Anim3>,
    pub visible: Option<Anim1>,
}

#[derive(Clone, Debug)]
pub struct MaterialOverride {
    /// glTF material index, or every material.
    pub material: Option<usize>,
    pub def: MaterialDef,
    /// Which of `def`'s fields were written (the rest keep the model's).
    pub set: Vec<String>,
}

/// How a staggered group of letters or instances is ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Forward,
    Reverse,
    Center,
    Edges,
    Random,
    /// Instances: by distance from the layout's centre.
    Distance,
    /// Instances: by each instance's own random (0..1), so `stagger` is
    /// the whole spread in seconds and parts that share a layout's
    /// randoms (a crowd's people) keep one phase.
    Phase,
}

impl Order {
    pub const NAMES: &'static [&'static str] = &["forward", "reverse", "center", "edges", "random", "distance", "phase"];
    pub fn by_name(s: &str) -> Option<Self> {
        Some(match s {
            "forward" | "index" => Self::Forward,
            "reverse" => Self::Reverse,
            "center" | "centre" => Self::Center,
            "edges" => Self::Edges,
            "random" => Self::Random,
            "distance" | "radial" => Self::Distance,
            "phase" => Self::Phase,
            _ => return None,
        })
    }
}

/// Per-unit animation of letters or instances, evaluated at `t − delay`
/// where the delay is the unit's rank × `stagger`. Offsets add to the rest
/// pose; `scale` multiplies it; `color` replaces the material colour.
#[derive(Clone, Debug, Default)]
pub struct EachDef {
    pub position: Option<Anim3>,
    pub rotation: Option<Anim3>,
    pub scale: Option<Anim3>,
    pub opacity: Option<Anim1>,
    pub color: Option<Anim4>,
    pub emissive: Option<Anim4>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextUnit {
    Letter,
    Word,
    Line,
    Whole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpriteShape {
    Circle,
    Soft,
    Square,
    Star,
    Ring,
}

impl SpriteShape {
    pub const NAMES: &'static [&'static str] = &["circle", "soft", "square", "star", "ring"];
    pub fn by_name(s: &str) -> Option<Self> {
        Some(match s {
            "circle" | "disc" => Self::Circle,
            "soft" | "glow" => Self::Soft,
            "square" => Self::Square,
            "star" | "sparkle" => Self::Star,
            "ring" => Self::Ring,
            _ => return None,
        })
    }
}

/// A node of the scene graph.
#[derive(Clone, Debug)]
pub struct NodeDef {
    pub name: Option<String>,
    /// `children[2].children[0]`.
    pub path: String,
    /// Its number in the scene, depth first from the root's 0: what a
    /// frame's meshes name their node by ([`crate::frame::MeshDraw::node`]).
    pub id: u32,
    pub loc: Loc,
    pub transform: TransformDef,
    pub visible: Anim1,
    /// Multiplies every material's opacity below (fades).
    pub opacity: Anim1,
    /// Shown on `[start, start + length)` of the scene's clock.
    pub span: Option<(f32, f32)>,
    pub cast_shadow: bool,
    pub receive_shadow: bool,
    /// Drawn with the Outline effect's edge.
    pub outline: bool,
    pub kind: NodeKind,
    pub children: Vec<NodeDef>,
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    Group,
    Mesh { geometry: GeometrySrc, material: MaterialDef, deform: DeformDef },
    Model(Box<ModelDef>),
    Text3D(Box<TextDef>),
    Instances(Box<InstancesDef>),
    Points(Box<PointsDef>),
    Line(Box<LineDef>),
    Light(LightDef),
}

#[derive(Clone, Debug)]
pub struct ModelDef {
    pub model: Arc<GlbModel>,
    pub src: MediaRef,
    pub clips: Vec<ClipDef>,
    pub nodes: Vec<NodeOverride>,
    pub materials: Vec<MaterialOverride>,
    /// Morph weights by (mesh node, target) index.
    pub morphs: Vec<(usize, usize, Anim1)>,
    /// Scale the model so its rest bounds fit this size (0 = as authored),
    /// centred on the node's origin with its base at y = 0 when `ground`.
    pub fit: f32,
    pub ground: bool,
    /// Per-node-mesh uploaded materials resolved at load.
    pub base_materials: Vec<MaterialDef>,
    pub deform: DeformDef,
}

pub use crate::text3d_node::TextDef;

#[derive(Clone, Debug)]
pub struct InstancesDef {
    pub geometry: SharedMesh,
    pub material: MaterialDef,
    pub bases: Vec<InstanceBase>,
    pub stagger: f32,
    pub order: Order,
    pub each: EachDef,
    /// `each: fn(i, n, t) { return {position: ..., rotation: ..., scale: ..., color: ...} }`
    pub each_fn: Option<u32>,
    /// Face each instance along its layout normal.
    pub align: bool,
    /// Per-instance colour variation 0..1 (hue jitter by the instance's random).
    pub color_jitter: f32,
    pub scale_jitter: f32,
    pub rotation_jitter: f32,
    pub deform: DeformDef,
    /// Voxel morph: each instance travels from its layout point to the
    /// matching point of a second layout as this goes 0 → 1 (staggered).
    pub morph: Option<(Vec<InstanceBase>, Anim1)>,
    /// Scatter / assemble: instances fly out along random directions by
    /// `amount × radius` (0 = assembled), tumbling as they go.
    pub scatter: Option<(Anim1, f32)>,
    /// Per-instance height from a function of the instance's place and time
    /// (an extruded slab's blocks lifting and sinking).
    pub heights: Option<HeightfieldDef>,
    /// `source: bodies("g")`: one instance per live body of physics group
    /// `g`, at its pose, with its scale and colour.
    pub bodies: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightKind {
    /// A travelling 2D wave.
    Wave,
    /// Rings out from the centre.
    Ripple,
    /// Rectangular regions lift or sink, a new set on every beat.
    Blocks,
    /// Blocks on the beat over a gentle wave.
    BlocksWave,
    /// Smooth value noise drifting.
    Noise,
}

impl HeightKind {
    pub const NAMES: &'static [&'static str] = &["wave", "ripple", "blocks", "blocks_wave", "noise"];
    pub fn by_name(s: &str) -> Option<Self> {
        Some(match s {
            "wave" => Self::Wave,
            "ripple" => Self::Ripple,
            "blocks" => Self::Blocks,
            "blocks_wave" => Self::BlocksWave,
            "noise" => Self::Noise,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct HeightfieldDef {
    pub kind: HeightKind,
    /// Peak lift in world units (keyable: fade the motion in and out).
    pub amount: Anim1,
    /// Wavelength / block size in world units (x), and the block depth (z).
    pub cell: [f32; 2],
    /// Cycles per second (waves, noise).
    pub speed: f32,
    /// Seconds per new block pattern (a beat by default).
    pub every: f32,
    pub seed: u64,
}

#[derive(Clone, Debug)]
pub enum PointSource {
    Layout(Vec<InstanceBase>),
    /// `path`: the emitter travels along a curve once every `duration`
    /// seconds; each particle keeps the place it was born at (trails).
    Particles { system: ParticleSystem, emitter_points: Option<Vec<InstanceBase>>, path: Option<(Curve3, f32)> },
    /// `source: k.out`: a kernel run every frame writes the points
    /// (`Sprite` records: pos, size, color; or vec4 pos + size; or vec3).
    Kernel(crate::kernels::KernelRun),
    /// `source: bodies("g")`: a point per live body of a physics group.
    Bodies(String),
    /// `source: <Sim>`: a point per state record (`pos`; `size`, `color`
    /// when the state has them).
    Sim(String),
}

#[derive(Clone, Debug)]
pub struct PointsDef {
    pub source: PointSource,
    /// World units, or pixels when `size_attenuation` is false.
    pub size: Anim1,
    pub size_attenuation: bool,
    pub color: Anim4,
    pub opacity: Anim1,
    pub shape: SpriteShape,
    pub blending: Blending,
    pub stagger: f32,
    pub each: EachDef,
    /// Mesh particles: each point draws this geometry with `material`.
    pub mesh: Option<(SharedMesh, MaterialDef)>,
}

#[derive(Clone, Debug)]
pub struct LineDef {
    pub curve: Curve3,
    pub segments: u32,
    /// Pixels (screen-space fat lines) or world units when `world_width`.
    pub width: Anim1,
    pub world_width: bool,
    pub color: Anim4,
    pub color_end: Option<Anim4>,
    pub opacity: Anim1,
    pub trim_start: Anim1,
    pub trim_end: Anim1,
    /// Dash and gap lengths in world units (0 = solid), scrolled by `dash_offset`.
    pub dash: f32,
    pub gap: f32,
    pub dash_offset: Anim1,
    pub blending: Blending,
    /// More polylines drawn with the same style (isolines, grids, graphs):
    /// straight segments through each list of points.
    pub polylines: Vec<Vec<[f32; 3]>>,
    /// Colour multiplier for glow (> 1 blooms), keyable.
    pub intensity: Anim1,
    /// A comet fade: this fraction of the drawn window, behind its head,
    /// ramps from transparent to full (0 = none), keyable.
    pub tail: Anim1,
    /// Fade out with camera distance between these (world units).
    pub fade: Option<(f32, f32)>,
    /// Segments a kernel writes each frame (`Segment` records: a, b, colour,
    /// width), drawn in this line's style: a line batch.
    pub source: Option<crate::kernels::KernelRun>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind {
    Ambient,
    Hemisphere,
    Directional,
    Point,
    Spot,
}

#[derive(Clone, Debug)]
pub struct ShadowDef {
    /// Map size in texels.
    pub size: u32,
    pub bias: f32,
    pub normal_bias: f32,
    /// Penumbra softness: filter radius in texels (PCF), keyable.
    pub softness: Anim1,
    /// Directional: half-extent of the shadow frustum (world units);
    /// 0 = fitted to the scene's bounds.
    pub extent: f32,
}

impl Default for ShadowDef {
    fn default() -> Self {
        Self { size: 2048, bias: 0.0005, normal_bias: 0.02, softness: Anim::Const([2.0]), extent: 0.0 }
    }
}

#[derive(Clone, Debug)]
pub struct LightDef {
    pub kind: LightKind,
    pub color: Anim4,
    /// Hemisphere: the ground colour.
    pub ground: Anim4,
    pub intensity: Anim1,
    /// Directional/spot: where it points at (world, or the node's frame
    /// when a child). Directional also takes `direction`.
    pub target: Anim3,
    pub direction: Option<Anim3>,
    /// Point/spot: 0 = infinite.
    pub distance: Anim1,
    pub decay: f32,
    /// Spot: cone half-angle (degrees) and penumbra (0..1).
    pub angle: Anim1,
    pub penumbra: Anim1,
    pub shadow: Option<ShadowDef>,
}

#[derive(Clone, Debug)]
pub enum CameraAim {
    /// Look at a point.
    Target(Anim3),
    /// Look at a node's world position.
    Node(String),
}

#[derive(Clone, Debug)]
pub struct CameraDef {
    pub orthographic: bool,
    pub position: Anim3,
    pub aim: CameraAim,
    pub up: Anim3,
    /// Vertical field of view, degrees (perspective).
    pub fov: Anim1,
    /// Orthographic: visible height in world units.
    pub height: Anim1,
    /// Zoom multiplies the focal length (a dolly-zoom partner to `position`).
    pub zoom: Anim1,
    pub roll: Anim1,
    pub near: f32,
    pub far: f32,
    /// Orbit the target: yaw/pitch degrees, distance.
    pub orbit: Option<(Anim1, Anim1, Anim1)>,
    /// Fly along a curve: `along` 0..1 (keyable); aims along the path
    /// unless an aim was given.
    pub path: Option<(Curve3, Anim1, bool)>,
    /// Handheld shake: amount (world units / degrees), frequency, seed.
    pub shake: Option<(Anim1, f32, u64)>,
    /// A lens: focal length in mm (overrides `fov`) on a sensor this tall
    /// (mm, 24 = full frame).
    pub focal_length: Option<Anim1>,
    pub sensor: f32,
    /// Named moves layered on the base camera, in order.
    pub moves: Vec<CameraMoveDef>,
    /// A continuous flight through set-ups: the camera arrives at each
    /// shot's position/target/roll/fov at `at`, holds `hold` seconds, and
    /// flies on along a smooth spline through all of them.
    pub shots: Vec<ShotDef>,
}

#[derive(Clone, Debug)]
pub struct ShotDef {
    pub at: f32,
    pub hold: f32,
    pub position: [f32; 3],
    pub target: [f32; 3],
    pub roll: f32,
    pub fov: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveKind {
    PushIn,
    PullOut,
    TruckLeft,
    TruckRight,
    OrbitLeft,
    OrbitRight,
    CraneUp,
    CraneDown,
    DollyZoom,
    Roll,
    Rise,
    /// Swing overhead and plunge into the target (an exit into a surface;
    /// pair with MotionBlur): `amount` 1 reaches it.
    Dive,
}

impl MoveKind {
    pub const NAMES: &'static [&'static str] =
        &["push_in", "pull_out", "truck_left", "truck_right", "orbit_left", "orbit_right", "crane_up", "crane_down", "dolly_zoom", "roll", "rise", "dive"];
    pub fn by_name(s: &str) -> Option<Self> {
        Some(match s {
            "push_in" | "dolly_in" => Self::PushIn,
            "pull_out" | "dolly_out" => Self::PullOut,
            "truck_left" | "dolly_left" => Self::TruckLeft,
            "truck_right" | "dolly_right" => Self::TruckRight,
            "orbit_left" | "orbit" => Self::OrbitLeft,
            "orbit_right" => Self::OrbitRight,
            "crane_up" | "crane" => Self::CraneUp,
            "crane_down" => Self::CraneDown,
            "dolly_zoom" | "vertigo" => Self::DollyZoom,
            "roll" | "dutch" => Self::Roll,
            "rise" | "pedestal_up" => Self::Rise,
            "dive" | "plunge" => Self::Dive,
            _ => return None,
        })
    }
}

/// A camera move over `[start, end]` seconds: `amount` is a fraction of the
/// camera's distance to its target (push, pull, truck, crane, rise, dolly
/// zoom) or degrees (orbit, roll).
#[derive(Clone, Debug)]
pub struct CameraMoveDef {
    pub kind: MoveKind,
    pub start: f32,
    pub end: f32,
    pub amount: f32,
    pub ease: crate::anim::Ease,
}

impl Default for CameraDef {
    fn default() -> Self {
        Self {
            orthographic: false,
            position: Anim::Const([0.0, 1.5, 6.0]),
            aim: CameraAim::Target(Anim::Const([0.0; 3])),
            up: Anim::Const([0.0, 1.0, 0.0]),
            fov: Anim::Const([35.0]),
            height: Anim::Const([5.0]),
            zoom: Anim::Const([1.0]),
            roll: Anim::Const([0.0]),
            near: 0.05,
            far: 500.0,
            orbit: None,
            path: None,
            shake: None,
            focal_length: None,
            sensor: 24.0,
            moves: Vec::new(),
            shots: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum EnvSource {
    /// One of render-material's looks (`@studio` .. `@gradient`).
    Preset(EnvPreset),
    /// An HDRI generator preset by its name in the renderer's
    /// `PROCEDURAL_ENVIRONMENTS` (`@hdri_golden_hour`), baked at load.
    HdriPreset(&'static str),
    /// A Radiance `.hdr`, turned into the engine's convention at load.
    Hdri(MediaRef),
}

#[derive(Clone, Debug)]
pub struct EnvironmentDef {
    pub source: EnvSource,
    /// The loaded or generated map (linear HDR equirect) and its key.
    pub map: Arc<makepad_render_material::ibl::EnvMap>,
    pub key: u64,
    pub intensity: Anim1,
    /// Degrees about y.
    pub rotation: Anim1,
    /// Show it behind the scene (else only lighting).
    pub background: bool,
    /// Background blur 0..1 (a roughness level of the prefiltered map).
    pub blur: f32,
    pub background_intensity: Anim1,
}

#[derive(Clone, Debug)]
pub struct FogDef {
    pub color: Anim4,
    /// Linear fog between near and far, or exponential² with density.
    pub near: Anim1,
    pub far: Anim1,
    pub density: Option<Anim1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToneMapping {
    None,
    Linear,
    Reinhard,
    Aces,
    AgX,
    Filmic,
}

impl ToneMapping {
    pub const NAMES: &'static [&'static str] = &["none", "linear", "reinhard", "aces", "agx", "filmic"];
    pub fn by_name(s: &str) -> Option<Self> {
        Some(match s {
            "none" => Self::None,
            "linear" => Self::Linear,
            "reinhard" => Self::Reinhard,
            "aces" | "aces_filmic" => Self::Aces,
            "agx" => Self::AgX,
            "filmic" | "uncharted" => Self::Filmic,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct PostDef {
    pub bloom: Option<(Anim1, Anim1, Anim1)>,
    /// radius (world units), intensity.
    pub ssao: Option<(Anim1, Anim1)>,
    /// focus distance (or a node), aperture (blur px at infinity, at
    /// 1080p), max blur px.
    pub dof: Option<(Anim1, Option<String>, Anim1, f32)>,
    /// A physical f-stop: with the camera's focal length it sets the blur.
    pub f_stop: Option<Anim1>,
    /// Shutter in degrees (keyable: open it only for a whip or a dive, so
    /// still frames render once), the most sub-frames (rounded to 4·3^l,
    /// up to 324) and the tolerance in 8-bit levels: sub-frames are added
    /// until the frame converges (makepad-render-graph's adaptive schedule).
    pub motion_blur: Option<(Anim1, u32, f32)>,
    /// colour, thickness px, glow.
    pub outline: Option<(Anim4, Anim1)>,
    pub vignette: Option<(Anim1, f32)>,
    pub grain: Option<Anim1>,
    /// Paper overlay: amount, halftone strength, halftone cell (px at
    /// 1080p), paper colour.
    pub paper: Option<(Anim1, Anim1, f32, Anim4)>,
    /// A radial zoom blur toward the frame's centre (0..1): the whip of a
    /// dive or a push, from one render (no extra samples).
    pub zoom_blur: Option<Anim1>,
    /// Light shafts (volumetric spill): from a world point or a named node,
    /// strength, colour, decay.
    pub shafts: Option<ShaftsDef>,
    /// Render-graph passes (`Pass{}` and the graph's kits: Glow, Halation,
    /// Aberration, FramePost, Lut), in order.
    pub graph: Vec<GraphPassDef>,
    /// The graph passes' programs (shared by every frame).
    pub graph_decls: Arc<Vec<makepad_render_graph::PassDecl>>,
    /// The realtime form of `motion_blur` (a preview): the graph's
    /// VelocityBlur pass, its `amount` the shutter's fraction of a frame.
    pub velocity_blur: Option<makepad_render_graph::PassDecl>,
    /// User post passes written in the shader DSL, applied in order in the
    /// final pass: each `fn(uv: vec2, c: vec3) -> vec3` with up to four
    /// uniforms (`self.u_post0..3`).
    pub passes: Vec<PostPassDef>,
    /// Supersampling: 1 (off) .. 4; applied while the internal size stays
    /// within 4096 px a side.
    pub antialias: u32,
    /// Jittered sub-pixel samples per frame (1 = off).
    pub temporal_aa: u32,
    pub grade: Option<GradeDef>,
    pub contact: Option<ContactDef>,
}

#[derive(Clone, Debug)]
pub struct GradeDef {
    pub contrast: Anim1,
    pub saturation: Anim1,
    pub temperature: Anim1,
    pub tint: Anim1,
    pub lift: Anim4,
    pub gamma: Anim4,
    pub gain: Anim4,
}

#[derive(Clone, Debug)]
pub struct ContactDef {
    pub opacity: Anim1,
    /// Blur in world units at the ground.
    pub blur: Anim1,
    /// Height above the ground where occluders stop darkening.
    pub far: f32,
    pub size: u32,
}

impl Default for PostDef {
    fn default() -> Self {
        Self { bloom: None, ssao: None, dof: None, f_stop: None, motion_blur: None, outline: None, vignette: None, grain: None, paper: None, zoom_blur: None, shafts: None, graph: Vec::new(), graph_decls: Arc::new(Vec::new()), velocity_blur: None, passes: Vec::new(), antialias: 2, temporal_aa: 1, grade: None, contact: None }
    }
}

/// A whole Scene3D document, ready to evaluate.
#[derive(Clone, Debug)]
pub struct SceneDef {
    pub size: Option<[f32; 2]>,
    pub duration: Option<f32>,
    /// `None` = transparent (the layers below show through).
    pub background: Option<Anim4>,
    pub environment: Option<EnvironmentDef>,
    pub fog: Option<FogDef>,
    pub camera: CameraDef,
    pub tone_mapping: ToneMapping,
    pub exposure: Anim1,
    pub post: PostDef,
    pub root: NodeDef,
    /// The default audio source for `follow_music(...)` bindings.
    pub music: Option<String>,
    /// A whole-scene look that restyles every material.
    pub look: Option<Look>,
    /// Effect physics and sims (`physics: Physics{..}`, `Sim{..}`).
    pub physics: Option<std::sync::Arc<crate::physics::PhysicsDef>>,
}

/// Environment looks (`look: @clay`): lighting, background and a material
/// treatment for the whole scene in one word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Studio,
    NeonNight,
    Clay,
    Blueprint,
    Holographic,
    /// Cartoon: flat 3-band shading, ink outlines, a paper overlay.
    Toon,
    /// Black ink on paper: 2 bands in greys, heavy outlines, halftone.
    Ink,
}

impl Look {
    pub const NAMES: &'static [&'static str] = &["studio", "neon_night", "clay", "blueprint", "holographic", "toon", "ink"];
    pub fn by_name(s: &str) -> Option<Self> {
        Some(match s {
            "studio" => Self::Studio,
            "neon_night" | "neon" => Self::NeonNight,
            "clay" => Self::Clay,
            "blueprint" | "wireframe" => Self::Blueprint,
            "holographic" | "hologram" => Self::Holographic,
            "toon" | "cartoon" | "cel" => Self::Toon,
            "ink" | "manga" | "comic" => Self::Ink,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ShaftsDef {
    pub position: Anim3,
    pub node: Option<String>,
    pub strength: Anim1,
    pub color: Anim4,
    pub decay: f32,
    pub threshold: f32,
}

/// One render-graph pass: its program and its keyable uniforms (read at
/// the frame point).
#[derive(Clone, Debug)]
pub struct GraphPassDef {
    pub decl: makepad_render_graph::PassDecl,
    pub uniforms: Vec<GraphUniform>,
}

#[derive(Clone, Debug)]
pub enum GraphUniform {
    One(Anim1),
    /// A colour (linear) or a vector of `width` components.
    Many(Anim4),
}

#[derive(Clone, Debug)]
pub struct PostPassDef {
    /// The document's `code` fn (moved to the GPU's VM by its tokens).
    pub code: makepad_render_graph::pass::PassFn,
    pub uniforms: Vec<Anim1>,
}
