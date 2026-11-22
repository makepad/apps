//! Every fab-gpuslice kernel compiles for SPIR-V, MSL and HLSL, with a part's field spliced in
//! (none, a Splash field, a mesh, both), inside the limits every backend takes (D3D11's 128
//! bytes of params, eight writable buffers), and passes the native validator of the platform it
//! runs on (spirv-val on Linux, the Metal compiler on macOS, FXC as the D3D11 backend runs it
//! on Windows).
use makepad_platform::makepad_script::shader_ir_compute::{compile_compute_kernel, CompiledKernel, ComputeTarget, KernelCode};
use makepad_platform::makepad_script::ScriptMod;
use makepad_platform::*;
use fab_sdf_engine::kernel::{ComputeBake, KernelSlot};
use fab_sdf_engine::makepad_csg_sdf::mesh_field::{MeshData, MeshField};
use fab_sdf_engine::makepad_csg_sdf::{SdfKernel, SdfSplashExpr, SplashPoint};

#[cfg(target_os = "macos")]
#[path = "../../../../../platform/script/tests/support/native_shader.rs"]
mod native_shader;

/// FXC as the D3D11 backend runs it (d3d11/pipeline.rs compile_stage): its optimiser skipped,
/// IEEE strictness for precise math.
#[cfg(target_os = "windows")]
fn fxc(source: &str, precise: bool) -> Result<(), String> {
    use std::ffi::{c_char, c_void};
    #[repr(C)]
    struct Blob { vtable: *const Vtable }
    #[repr(C)]
    struct Vtable {
        query: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut Blob) -> u32,
        data: unsafe extern "system" fn(*mut Blob) -> *const u8,
        size: unsafe extern "system" fn(*mut Blob) -> usize,
    }
    #[link(name = "d3dcompiler")]
    extern "system" {
        fn D3DCompile(src: *const u8, size: usize, name: *const c_char, defines: *const c_void, include: *const c_void,
            entry: *const c_char, target: *const c_char, flags: u32, flags2: u32, code: *mut *mut Blob, errors: *mut *mut Blob) -> i32;
    }
    const SKIP_OPTIMIZATION: u32 = 1 << 2;
    const BACKWARDS_COMPATIBILITY: u32 = 1 << 12;
    const IEEE_STRICTNESS: u32 = 1 << 13;
    unsafe {
        let (mut code, mut errors) = (std::ptr::null_mut(), std::ptr::null_mut());
        let flags = SKIP_OPTIMIZATION | BACKWARDS_COMPATIBILITY | if precise { IEEE_STRICTNESS } else { 0 };
        let hr = D3DCompile(source.as_ptr(), source.len(), std::ptr::null(), std::ptr::null(), std::ptr::null(),
            b"kernel_main\0".as_ptr().cast(), b"cs_5_0\0".as_ptr().cast(), flags, 0, &mut code, &mut errors);
        let diagnostic = if errors.is_null() { format!("D3DCompile HRESULT {hr:#x}") } else {
            let vt = &*(*errors).vtable;
            String::from_utf8_lossy(std::slice::from_raw_parts((vt.data)(errors), (vt.size)(errors))).into_owned()
        };
        for blob in [code, errors] { if !blob.is_null() { ((*(*blob).vtable).release)(blob); } }
        if hr < 0 { Err(diagnostic) } else { Ok(()) }
    }
}

/// The kernels a part's field is spliced into.
const SPLICED: [&str; 7] = ["Sample", "TraceCoarse", "TraceSample", "TraceCross", "TraceCrossRetry", "TraceLinks", "TraceDense"];
/// The kernels with no field.
const PLAIN: [&str; 15] = ["ProbeTouch", "ProbeArgs", "ProbeIdle", "ProbeCopy", "ProbeAlu", "ScanSums", "ScanTop", "ScanApply", "Args", "TraceKeep", "TraceKept", "TraceCorners", "TraceCover", "TraceEdges", "TraceCrossList"];

fn validate(name: &str, k: &CompiledKernel) {
    assert!(k.layout.push_size <= 128, "{name}: {} bytes of params (D3D11 takes 128)", k.layout.push_size);
    assert!(k.layout.buffers.iter().filter(|b| b.writable).count() <= 8, "{name}: more than eight writable buffers");
    match &k.code {
        KernelCode::Spirv(words) => {
            #[cfg(target_os = "linux")]
            {
                let file = std::env::temp_dir().join(format!("gpuslice-{}-{name}.spv", std::process::id()));
                std::fs::write(&file, words.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
                let out = std::process::Command::new("spirv-val").args(["--target-env", "vulkan1.2"]).arg(&file).output().expect("spirv-val required");
                std::fs::remove_file(&file).unwrap();
                assert!(out.status.success(), "{name}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            }
            assert!(!words.is_empty());
        }
        KernelCode::Msl(_source) => {
            #[cfg(target_os = "macos")]
            native_shader::metal(_source, k.layout.precise_math).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        KernelCode::Hlsl(_source) => {
            #[cfg(target_os = "windows")]
            fxc(_source, k.layout.precise_math).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}

fn cube() -> MeshData {
    let positions = (0..8).map(|i| [if i & 1 == 0 { -1.0 } else { 1.0 }, if i & 2 == 0 { -1.0 } else { 1.0 }, if i & 4 == 0 { -1.0 } else { 1.0 }]).collect();
    let triangles = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]].iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect();
    MeshData { positions, triangles }
}

#[test]
fn kernels_compile_on_every_backend() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.with_vm(|vm| {
        vm.capture_errors();
        fab_gpuslice::script_mod(vm);
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "registration: {errors:?}");
        let field = vm.eval(ScriptMod::new("", "", "knob_field", 0, 0,
            "use mod.math.*\nlet field = fn(p) { return smooth_union(sdf_sphere(p - vec3(0, 12, 0), 8), sdf_cylinder(p - vec3(0, 6, 0), 4, 12), 3) }\n(field)", &[])).as_object().unwrap();
        let _keep = vm.new_object_ref(field);
        let field = SdfSplashExpr::compile(vm, field, SplashPoint::Vec3, &[]).unwrap();
        let splash = KernelSlot::new(&SdfKernel::splash(field, None)).unwrap();
        assert!(splash.air().is_some(), "the fixture has a GPU route");
        let mesh = KernelSlot::mesh(&std::sync::Arc::new(MeshField::new(cube())));
        let module = vm.heap().module(id!(gpuslice));
        let targets = [ComputeTarget::Spirv, ComputeTarget::Msl, ComputeTarget::Hlsl];
        for name in PLAIN {
            let object = vm.value(module, LiveId::from_str(name)).as_object().unwrap_or_else(|| panic!("{name} missing"));
            for target in targets {
                let k = compile_compute_kernel(vm, object, target).unwrap_or_else(|e| panic!("{name} {target:?}: {e}"));
                validate(&format!("{name} {target:?}"), &k);
            }
        }
        for slots in [Vec::new(), vec![splash.clone()], vec![mesh.clone()], vec![splash, mesh]] {
            let mut bake = ComputeBake::new(vm, &slots).unwrap();
            for name in SPLICED {
                let base = vm.value(module, LiveId::from_str(name)).as_object().unwrap_or_else(|| panic!("{name} missing"));
                let object = bake.draw_variant(vm, base).unwrap();
                for target in targets {
                    let k = compile_compute_kernel(vm, object, target).unwrap_or_else(|e| panic!("{name} {target:?} ({} slots): {e}", slots.len()));
                    assert!(k.layout.precise_math, "{name}: precise math");
                    validate(&format!("{name} {target:?} ({} slots)", slots.len()), &k);
                }
            }
            bake.release(vm);
        }
    });
}
