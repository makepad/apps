//! A resolved frame: everything the GPU draws at one time, as plain
//! numbers. [`crate::eval`] makes it from a document; the renderer never
//! looks at the document.

use makepad_render_material::ibl::EnvMap;
use makepad_geom3d::mesh::Mesh;
use crate::scene::*;
use makepad_render::TransformTint;
use makepad_render_lines::{Segment, Sprite};
use makepad_widgets::*;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct CameraFrame {
    pub view: Mat4f,
    pub proj: Mat4f,
    pub position: [f32; 3],
    pub forward: [f32; 3],
    pub near: f32,
    pub far: f32,
    /// Vertical field of view in degrees (after zoom).
    pub fov_y: f32,
    pub orthographic: bool,
    /// Orthographic visible height.
    pub height: f32,
    pub aspect: f32,
    /// Sensor height in mm (for lens maths).
    pub sensor: f32,
}

#[derive(Clone, Debug)]
pub struct EnvFrame {
    pub key: u64,
    pub map: Arc<EnvMap>,
    pub intensity: f32,
    pub rotation: f32,
    pub background: bool,
    pub blur: f32,
    pub background_intensity: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightType {
    Directional,
    Point,
    Spot,
}

#[derive(Clone, Debug)]
pub struct ShadowFrame {
    pub size: u32,
    pub bias: f32,
    pub normal_bias: f32,
    pub softness: f32,
    /// World → light clip.
    pub view_proj: Mat4f,
    /// World → light view (linear depth along -z of this).
    pub view: Mat4f,
    /// Linear depth range stored in the map: depth / range.
    pub range: f32,
    /// World units per shadow-map texel at the target (for the filter).
    pub texel_world: f32,
}

#[derive(Clone, Debug)]
pub struct LightFrame {
    pub kind: LightType,
    /// Colour × intensity, linear.
    pub color: [f32; 3],
    pub position: [f32; 3],
    /// Unit direction the light travels (from the light into the scene).
    pub direction: [f32; 3],
    /// 0 = infinite.
    pub range: f32,
    pub decay: f32,
    pub cos_outer: f32,
    pub cos_inner: f32,
    pub shadow: Option<ShadowFrame>,
}

#[derive(Clone, Copy, Debug)]
pub struct FogFrame {
    pub color: [f32; 3],
    pub near: f32,
    pub far: f32,
    /// > 0: exponential² fog instead of linear.
    pub density: f32,
}

/// A material with every value resolved.
#[derive(Clone, Debug)]
pub struct MaterialFrame {
    pub model: ShadingModel,
    pub color: [f32; 4],
    pub opacity: f32,
    pub metalness: f32,
    pub roughness: f32,
    /// Emissive colour × intensity.
    pub emissive: [f32; 3],
    pub clearcoat: f32,
    pub clearcoat_roughness: f32,
    pub transmission: f32,
    pub ior: f32,
    pub thickness: f32,
    pub attenuation: [f32; 3],
    pub sheen: f32,
    pub sheen_color: [f32; 3],
    pub iridescence: f32,
    pub specular: f32,
    pub env_intensity: f32,
    /// Rim colour × power's intensity; `rim_power` 0 = off.
    pub rim: [f32; 3],
    pub rim_power: f32,
    pub steps: u32,
    pub matcap: MatcapPreset,
    pub maps: MaterialMaps,
    pub normal_scale: f32,
    pub occlusion_strength: f32,
    pub alpha_cutoff: Option<f32>,
    pub vertex_colors: bool,
    pub flat_shading: bool,
    pub wireframe: bool,
    pub side: FaceSide,
    pub blending: Blending,
    pub transparent: bool,
    pub depth_write: bool,
    pub shader: Option<ShaderHooks>,
    pub custom: [f32; 8],
    /// Car-paint flakes (amount, scale per unit) and brush streaks
    /// (amount, scale).
    pub flakes: [f32; 2],
    pub brushed: [f32; 2],
    /// Planar reflection strength and blur (floors).
    pub reflectivity: f32,
    pub reflection_blur: f32,
    /// Refract into the environment only (no screen sampling).
    pub refraction_env: bool,
    /// Fade to the background between these radii (grounds).
    pub fade: Option<(f32, f32)>,
    pub external: Option<String>,
    /// How the base map fits the surface, the crop in the picture's pixels
    /// (evaluated), and the surface's aspect (width / height).
    pub fit: crate::scene::MapFit,
    pub crop: Option<[f32; 4]>,
    pub aspect: f32,
    pub map_space: crate::scene::MapSpace,
    /// Ink outline width in pixels (0 = none) and colour.
    pub ink: f32,
    pub ink_color: [f32; 4],
    pub wire_fill: Option<[f32; 4]>,
    pub wire_width: f32,
    pub shadow_catcher: bool,
}

impl MaterialFrame {
    /// The scene look's treatment of this material.
    pub fn with_look(mut self, look: Option<Look>) -> Self {
        let Some(look) = look else { return self };
        let plain = |m: &mut Self| {
            m.maps = MaterialMaps::default();
            m.transmission = 0.0;
            m.clearcoat = 0.0;
            m.flakes = [0.0, m.flakes[1]];
            m.brushed = [0.0, m.brushed[1]];
            m.iridescence = 0.0;
            m.sheen = 0.0;
            m.shader = None;
            m.external = None;
        };
        match look {
            Look::Studio | Look::NeonNight => {}
            Look::Toon | Look::Ink => {
                if !self.shadow_catcher && !self.wireframe && self.blending == Blending::Normal {
                    let ink = look == Look::Ink;
                    self.transmission = 0.0;
                    self.clearcoat = 0.0;
                    self.flakes = [0.0, self.flakes[1]];
                    self.brushed = [0.0, self.brushed[1]];
                    self.iridescence = 0.0;
                    self.sheen = 0.0;
                    if self.model != ShadingModel::Basic || self.maps.base.is_none() && self.external.is_none() {
                        self.model = ShadingModel::Toon;
                    }
                    self.steps = if ink { 2 } else { 3 };
                    if ink {
                        // Greys by brightness, lifted toward the paper.
                        let y = 0.2126 * self.color[0] + 0.7152 * self.color[1] + 0.0722 * self.color[2];
                        let g = 0.35 + 0.6 * y.sqrt();
                        self.color = [g, g, g, self.color[3]];
                        self.emissive = [0.0; 3];
                    }
                    if self.ink <= 0.0 && self.fade.is_none() && self.reflectivity == 0.0 {
                        self.ink = if ink { 3.5 } else { 3.0 };
                    }
                }
            }
            Look::Clay => {
                plain(&mut self);
                if !self.shadow_catcher {
                    self.model = ShadingModel::Standard;
                    self.color = [0.72, 0.68, 0.62, self.color[3]];
                    self.metalness = 0.0;
                    self.roughness = 0.82;
                    self.emissive = [0.0; 3];
                    self.rim_power = 0.0;
                }
            }
            // Floors (fading grounds) stay solid and dark under the line looks.
            Look::Blueprint | Look::Holographic if self.fade.is_some() && !self.shadow_catcher => {
                plain(&mut self);
                self.model = ShadingModel::Standard;
                self.color = if look == Look::Blueprint { [0.03, 0.09, 0.2, self.color[3]] } else { [0.01, 0.03, 0.05, self.color[3]] };
                self.metalness = 0.0;
                self.roughness = 0.5;
                self.emissive = [0.0; 3];
                self.rim_power = 0.0;
            }
            Look::Blueprint => {
                plain(&mut self);
                if !self.shadow_catcher && self.reflectivity == 0.0 {
                    self.model = ShadingModel::Basic;
                    self.wireframe = true;
                    self.color = [0.55, 0.85, 1.0, 1.0];
                    self.emissive = [0.0; 3];
                    self.blending = Blending::Additive;
                    self.depth_write = false;
                }
            }
            Look::Holographic => {
                plain(&mut self);
                if !self.shadow_catcher {
                    self.model = ShadingModel::Standard;
                    self.color = [0.0, 0.05, 0.08, 1.0];
                    self.metalness = 0.0;
                    self.roughness = 0.3;
                    self.emissive = [0.0, 0.25, 0.35];
                    self.rim = [0.2, 0.9, 1.6];
                    self.rim_power = 2.2;
                    self.opacity *= 0.6;
                    self.blending = Blending::Additive;
                    self.depth_write = false;
                    self.transparent = true;
                }
            }
        }
        self
    }

    /// Drawn after the opaque pass, sorted back to front.
    pub fn is_blended(&self) -> bool {
        self.transparent || self.opacity < 0.999 || self.blending != Blending::Normal || self.color[3] < 0.999
    }
    pub fn is_transmissive(&self) -> bool {
        self.transmission > 0.001
    }
}

#[derive(Clone, Debug)]
pub struct MeshDraw {
    /// The node it draws ([`crate::scene::NodeDef::id`]).
    pub node: u32,
    /// Upload key: the content hash for static meshes, a per-node key for
    /// meshes rebuilt every frame.
    pub geo_key: u64,
    pub mesh: Arc<Mesh>,
    pub dynamic: bool,
    pub material: MaterialFrame,
    /// Each instance's model matrix and tint (colour multiplier, alpha
    /// times its opacity).
    pub instances: Vec<TransformTint>,
    pub cast_shadow: bool,
    pub receive_shadow: bool,
    pub outline: bool,
    /// A flat horizontal receiver (floors): takes contact shadows and is
    /// not a contact-shadow caster.
    pub ground: bool,
    /// (displace amount, frequency, noise time, twist deg/unit).
    pub deform: [f32; 4],
    /// (bend degrees, height of the geometry, 0, 0).
    pub deform2: [f32; 4],
    /// (ripple amount, frequency, phase, 0), (curl progress, radius, x max, width).
    pub deform3: [f32; 4],
    pub deform4: [f32; 4],
    /// An ink hull: the mesh's inverted hull pushed out this many pixels.
    pub ink: f32,
    /// World-space bounds of all instances.
    pub bounds: ([f32; 3], [f32; 3]),
}

impl MeshDraw {
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }
}

#[derive(Clone, Debug)]
pub struct PointsDraw {
    pub points: Vec<Sprite>,
    pub shape: SpriteShape,
    pub blending: Blending,
    pub size_attenuation: bool,
}

impl PointsDraw {
    pub fn count(&self) -> usize {
        self.points.len()
    }
}

#[derive(Clone, Debug)]
pub struct LineDraw {
    pub segments: Vec<Segment>,
    pub world_width: bool,
    pub dash: f32,
    pub gap: f32,
    pub dash_offset: f32,
    pub blending: Blending,
    /// Distance fade (near, far); far <= near = none.
    pub fade: (f32, f32),
}

#[derive(Clone, Debug, Default)]
pub struct PostFrame {
    pub bloom: Option<(f32, f32, f32)>,
    pub ssao: Option<(f32, f32)>,
    /// focus distance, aperture, max blur px.
    pub dof: Option<(f32, f32, f32)>,
    pub outline: Option<([f32; 4], f32)>,
    pub vignette: Option<(f32, f32)>,
    pub grain: Option<f32>,
    /// Paper overlay: amount, halftone, cell px (1080p), paper colour.
    pub paper: Option<(f32, f32, f32, [f32; 4])>,
    pub zoom_blur: Option<f32>,
    /// Light shafts: the light's screen position (0..1, v up), strength,
    /// colour, decay, threshold; strength 0 when it is behind the camera.
    pub shafts: Option<([f32; 2], f32, [f32; 3], f32, f32)>,
    /// User post passes: the compiled chain's key, the code of each pass,
    /// their uniforms (four per pass) and the time.
    pub passes: Option<(u64, Vec<makepad_render_graph::pass::PassFn>, [f32; 16], f32)>,
    /// Supersampling factor (1 = off).
    pub antialias: u32,
    /// Render-graph passes and this frame's uniform values.
    pub graph: Option<(std::sync::Arc<Vec<makepad_render_graph::PassDecl>>, Vec<makepad_render_graph::PassValues>)>,
    /// Realtime motion blur (a preview): the VelocityBlur pass and its
    /// amount (the shutter's fraction of a frame).
    pub velocity_blur: Option<(makepad_render_graph::PassDecl, f32)>,
}

/// A named node on screen: `screen` is 0..1 from the top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Anchor {
    pub name: String,
    pub world: [f32; 3],
    pub screen: [f32; 2],
    pub depth: f32,
    pub in_front: bool,
}

#[derive(Clone, Debug)]
pub struct SceneFrame {
    pub t: f32,
    pub aspect: f32,
    pub camera: CameraFrame,
    /// Linear RGBA; `None` = transparent.
    pub clear: Option<[f32; 4]>,
    pub env: Option<EnvFrame>,
    pub ambient: [f32; 3],
    pub hemisphere: Option<([f32; 3], [f32; 3])>,
    pub lights: Vec<LightFrame>,
    pub fog: Option<FogFrame>,
    pub meshes: Vec<MeshDraw>,
    pub points: Vec<PointsDraw>,
    pub lines: Vec<LineDraw>,
    pub post: PostFrame,
    pub tone_mapping: ToneMapping,
    pub exposure: f32,
    /// Shutter degrees, the most sub-frames, the tolerance (8-bit levels).
    pub motion_blur: Option<(f32, u32, f32)>,
    /// Bounds of everything that casts a shadow.
    pub bounds: Option<([f32; 3], [f32; 3])>,
    /// Height of the reflecting floor plane, when one reflects.
    pub reflector: Option<f32>,
    pub contact: Option<ContactFrame>,
    pub grade: Option<GradeFrame>,
    /// Jittered sub-pixel samples accumulated per frame (deterministic TAA).
    pub temporal_aa: u32,
    /// Named nodes projected to the frame (HUD anchors).
    pub anchors: Vec<Anchor>,
    /// What went wrong evaluating this frame (a kernel that failed to run).
    pub errors: Vec<String>,
}

/// Contact shadows: a depth map looking up from under the ground.
#[derive(Clone, Debug)]
pub struct ContactFrame {
    pub opacity: f32,
    /// Blur radius in uv of the map.
    pub blur: f32,
    /// How far above the ground occluders still darken, as a fraction of
    /// the map's depth range.
    pub far: f32,
    pub view: Mat4f,
    pub view_proj: Mat4f,
    pub size: u32,
}

/// Colour grade applied after tone mapping.
#[derive(Clone, Copy, Debug)]
pub struct GradeFrame {
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub lift: [f32; 3],
    pub gamma: [f32; 3],
    pub gain: [f32; 3],
}

impl SceneFrame {
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|m| m.mesh.triangle_count() * m.instance_count()).sum()
    }
    pub fn instance_count(&self) -> usize {
        self.meshes.iter().map(|m| m.instance_count()).sum()
    }
}
