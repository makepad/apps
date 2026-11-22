//! Splash fields and mesh distance queries compiled into GPU shader variants.
//! Kernel slots retain program identity and parameters; the shared dispatch feeds
//! field evaluation, plane tracing and slicing. AIR limits bound GPU evaluation cost.

use makepad_csg_sdf::{SdfKernel, SdfNode, SdfSplashExpr, GRID_INPUT, GRID_PARAM};
use makepad_draw::*;
use makepad_script_compute::shader_route::{self, RouteNames, ShaderRoute};
use std::sync::Arc;

/// Worst-case AIR ops of one sample accepted by the GPU route.
pub const MAX_KERNEL_COST: u64 = 20_000;
/// Parameters a kernel call passes (four texels).
pub const MAX_KERNEL_PARAMS: usize = 16;
/// Words of local arrays (the AIR frame) a GPU kernel may use.
pub const MAX_FRAME_WORDS: u32 = 64;
/// What a Splash field's kernel calls its parts.
const ROUTE_NAMES: RouteNames<'static> = RouteNames { points: "sdf_points", value: "sdf_dist", grid: GRID_INPUT, grid_at: GRID_PARAM, fn_prefix: "sdfk_" };

/// A kernel's GPU route ([`ShaderRoute`]: its AIR checked and translated
/// into shader IR inside the compiler), with its resource requirements.
pub struct KernelAir {
    /// The program's identity ([`SdfSplashExpr::structure_key`]).
    pub key: u64,
    pub params: usize,
    /// Worst-case AIR ops per sample.
    pub cost: u64,
    route: ShaderRoute,
}

impl KernelAir {
    /// The GPU route of a Splash field, or why it has none.
    pub fn new(splash: &SdfSplashExpr) -> Result<KernelAir, String> {
        let key = splash.structure_key();
        let route = ShaderRoute::new(splash.kernel(), key, ROUTE_NAMES, MAX_KERNEL_COST, MAX_KERNEL_PARAMS as u32, MAX_FRAME_WORDS);
        let info = route.info();
        if !info.error.is_empty() {
            return Err(info.error.to_string());
        }
        let (params, cost) = (info.params as usize, info.cost);
        Ok(KernelAir { key, params, cost, route })
    }
}

/// One field kernel (by program identity).
#[derive(Clone)]
pub struct KernelSlot {
    /// The program's identity (or the field's key when it is no Splash program).
    pub key: u64,
    route: FieldRoute,
}

#[derive(Clone)]
enum FieldRoute {
    Splash { field: Arc<SdfSplashExpr>, air: Arc<KernelAir> },
    /// A field no GPU route was made for: a host that runs it on the CPU ([`KernelSlot::cpu`]).
    Cpu(Arc<SdfSplashExpr>),
    Mesh(Arc<crate::mesh::MeshAsset>),
}

fn structure_key(k: &SdfKernel) -> u64 {
    k.splash.as_ref().map_or(k.key, |s| s.structure_key())
}

impl KernelSlot {
    pub fn new(k: &SdfKernel) -> Result<KernelSlot, crate::program::LowerError> {
        let field = k.splash.clone().ok_or(crate::program::LowerError::Unsupported("SDF kernels require a Splash field"))?;
        let air = Arc::new(KernelAir::new(&field).map_err(crate::program::LowerError::Kernel)?);
        Ok(KernelSlot { key: structure_key(k), route: FieldRoute::Splash { field, air } })
    }

    /// A slot for a host that compiles the program for the CPU, so no GPU limit applies. None: a
    /// field with no Splash program.
    pub fn cpu(k: &SdfKernel) -> Option<KernelSlot> {
        Some(KernelSlot { key: structure_key(k), route: FieldRoute::Cpu(k.splash.clone()?) })
    }

    /// The Splash field behind this slot (none for a mesh).
    pub fn splash(&self) -> Option<&Arc<SdfSplashExpr>> {
        match &self.route {
            FieldRoute::Splash { field, .. } | FieldRoute::Cpu(field) => Some(field),
            FieldRoute::Mesh(_) => None,
        }
    }

    pub fn mesh(m: &Arc<makepad_csg_sdf::mesh_field::MeshField>) -> KernelSlot {
        KernelSlot { key: m.key(), route: FieldRoute::Mesh(Arc::new(crate::mesh::MeshAsset::new(m))) }
    }

    pub fn mesh_asset(&self) -> Option<&Arc<crate::mesh::MeshAsset>> {
        match &self.route { FieldRoute::Mesh(m) => Some(m), _ => None }
    }

    pub fn air(&self) -> Option<&Arc<KernelAir>> {
        match &self.route { FieldRoute::Splash { air, .. } => Some(air), _ => None }
    }

    pub fn is_mesh(&self) -> bool { self.mesh_asset().is_some() }

    /// Whether `k` runs this slot's program.
    pub fn matches(&self, k: &SdfKernel) -> bool {
        !self.is_mesh() && self.key == structure_key(k)
    }

    /// [`KernelSlot::distance`] at many points (one kernel call).
    pub fn distance_many(&self, params: &[f32], grid: Option<&[f32]>, ps: &[[f32; 3]]) -> Vec<f32> {
        match &self.route {
            FieldRoute::Splash { field, .. } | FieldRoute::Cpu(field) => {
                let xyz: Vec<f32> = ps.iter().flatten().copied().collect();
                let mut out = vec![0.0f32; ps.len()];
                field.distance_batch_words(params, grid, &xyz, &mut out);
                out
            }
            FieldRoute::Mesh(m) => ps.iter().map(|p| m.signed(*p)).collect(),
        }
    }

    /// The distance at `p` with parameters `params` (by parameter index), a gridded kernel's
    /// reading `grid` (its words, as its program's payload holds them).
    pub fn distance(&self, params: &[f32], grid: Option<&[f32]>, p: [f32; 3]) -> f32 {
        match &self.route {
            FieldRoute::Splash { field, .. } | FieldRoute::Cpu(field) => {
                let mut out = [0.0f32];
                field.distance_batch_words(params, grid, &p, &mut out);
                out[0]
            }
            FieldRoute::Mesh(m) => m.signed(p),
        }
    }

}

/// Adds the kernels of `n` missing from `slots` (their order is stable:
/// a slot keeps its index while the program is held).
pub fn collect(n: &SdfNode, slots: &mut Vec<KernelSlot>) -> Result<(), crate::program::LowerError> {
    match n {
        SdfNode::Kernel(k) => {
            if !slots.iter().any(|s| s.matches(k)) {
                slots.push(KernelSlot::new(k)?);
            }
        }
        SdfNode::Mesh(m) => {
            if !slots.iter().any(|s| s.is_mesh() && s.key == m.key()) {
                slots.push(KernelSlot::mesh(m));
            }
        }
        SdfNode::Texture { tex, of } => {
            if !slots.iter().any(|s| s.matches(&tex.kernel)) {
                slots.push(KernelSlot::new(&tex.kernel)?);
            }
            collect(of, slots)?;
        }
        SdfNode::Combine { of, .. } => for c in of { collect(c, slots)?; },
        SdfNode::Shell { of, .. } | SdfNode::Round { of, .. } | SdfNode::Transform { of, .. } | SdfNode::Instance { of, .. } | SdfNode::Bone { of, .. } | SdfNode::Deform { of, .. } | SdfNode::Id { of, .. } | SdfNode::Sculpt { of, .. } => collect(of, slots)?,
        SdfNode::Material { within, of, .. } => {
            if let Some(w) = within {
                collect(w, slots)?;
            }
            collect(of, slots)?;
        }
        _ => {}
    }
    Ok(())
}

/// The kernels of `n` a CPU program can hold ([`KernelSlot::cpu`]). The rest are leaves the host
/// answers ([`crate::program::lower_hosted`]).
pub fn collect_cpu(n: &SdfNode, slots: &mut Vec<KernelSlot>) {
    let kernel = |k: &SdfKernel, slots: &mut Vec<KernelSlot>| {
        if !slots.iter().any(|s| s.matches(k)) {
            slots.extend(KernelSlot::cpu(k));
        }
    };
    match n {
        SdfNode::Kernel(k) => kernel(k, slots),
        SdfNode::Texture { tex, of } => {
            kernel(&tex.kernel, slots);
            collect_cpu(of, slots);
        }
        SdfNode::Combine { of, .. } => of.iter().for_each(|c| collect_cpu(c, slots)),
        SdfNode::Sculpt { strokes, of } => {
            if strokes.is_empty() {
                collect_cpu(of, slots);
            }
        }
        SdfNode::Shell { of, .. } | SdfNode::Round { of, .. } | SdfNode::Transform { of, .. } | SdfNode::Instance { of, .. } | SdfNode::Bone { of, .. } | SdfNode::Id { of, .. } | SdfNode::Deform { of, .. } => collect_cpu(of, slots),
        SdfNode::Material { within, of, .. } => {
            if let Some(w) = within {
                collect_cpu(w, slots);
            }
            collect_cpu(of, slots);
        }
        _ => {}
    }
}

/// `base` with the kernels' dispatch and parameter fetching.
fn variant(vm: &mut ScriptVm, base: ScriptObject, own: ScriptObject) -> Option<ScriptObject> {
    let fetching = vm.value(base, id!(kernel_at_fetch)).as_object()?;
    let obj = vm.new_with_proto(base);
    vm.set_value(obj, id!(kernel_call), own);
    vm.set_value(obj, id!(kernel_at), fetching);
    Some(obj)
}

/// One dispatch splice per kernel set, shared by field and tracing shaders.
/// Keep it alive while compiling derived variants, then call release.
pub struct ComputeBake {
    pub keys: Vec<u64>,
    dispatch: Option<ScriptObject>,
    mesh: Option<ScriptObject>,
    mesh_slots: Vec<usize>,
    refs: Vec<ScriptObjectRef>,
}

impl ComputeBake {
    pub fn new(vm: &mut ScriptVm, slots: &[KernelSlot]) -> Option<Self> {
        let module = vm.heap().module(id!(sdf));
        let base = vm.value(module, id!(Eval)).as_object()?;
        let keys: Vec<_> = slots.iter().map(|s| s.key).collect();
        let mut refs = Vec::new();
        let dispatch = if slots.iter().any(|s| s.air().is_some()) {
            let key = slots.iter().fold(0xcbf2_9ce4_8422_2325u64, |key, s| {
                (key ^ s.key ^ s.air().is_some() as u64).wrapping_mul(0x100_0000_01b3)
            });
            let stub = vm.value(base, id!(kernel_call)).as_object()?;
            let routes: Vec<_> = slots.iter().map(|s| s.air().map(|g| &g.route)).collect();
            let own = shader_route::dispatch_fn(vm, stub, key, &routes)?;
            refs.push(vm.new_object_ref(own));
            Some(own)
        } else { None };
        let mesh_slots: Vec<_> = slots.iter().enumerate().filter_map(|(i, s)| s.is_mesh().then_some(i)).collect();
        let mesh = if mesh_slots.is_empty() { None } else {
            // One binding per asset; choose the binding before each buffer read.
            // Object instances share the uploaded buffers.
            let mut code = String::from("use mod.pod.*\nuse mod.shader\n{\n");
            for i in &mesh_slots { code += &format!("mesh_{i}: shader.buffer(vec4u)\n"); }
            code += "mesh_read: fn(slot: u32, at: u32) -> vec4u {\n";
            for i in &mesh_slots { code += &format!("if slot == {i}u {{ return self.mesh_{i}[at] }}\n"); }
            code += "return vec4u(0u)\n}\n}";
            let mesh = vm.eval(ScriptMod::new("", "", "sdf_mesh_bindings", 0, 0, &code, &[])).as_object()?;
            refs.push(vm.new_object_ref(mesh));
            Some(mesh)
        };
        Some(Self { keys, dispatch, mesh, mesh_slots, refs })
    }

    fn attach_mesh(vm: &mut ScriptVm, object: ScriptObject, mesh: ScriptObject, slots: &[usize]) {
        for name in slots.iter().map(|i| crate::mesh::binding(*i)).chain([id!(mesh_read)]) {
            let value = vm.value(mesh, name);
            vm.set_value(object, name, value);
        }
    }

    /// Apply the dispatch and mesh bindings to a host's shader.
    pub fn draw_variant(&mut self, vm: &mut ScriptVm, base: ScriptObject) -> Option<ScriptObject> {
        let object = match self.dispatch {
            Some(own) => variant(vm, base, own)?,
            None => vm.new_with_proto(base),
        };
        if let Some(mesh) = self.mesh { Self::attach_mesh(vm, object, mesh, &self.mesh_slots); }
        self.refs.push(vm.new_object_ref(object));
        Some(object)
    }

    pub fn release(self, vm: &mut ScriptVm) {
        if let Some(own) = self.dispatch { shader_route::release_dispatch(vm, own); }
    }
}

