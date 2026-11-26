//! The bounded-loop check (`makepad_wasm_export::shader_loops`, shared with
//! wasm-export) on a shader pack's bytes.

pub use makepad_wasm_export::shader_loops::*;

fn with_shaders<T>(bytes: &[u8], f: impl FnOnce(&[Shader]) -> T) -> Result<T, String> {
    let entries = makepad_platform::makepad_script::shader_pack::inspect_shader_pack(bytes);
    if let Some(error) = entries.error() {
        return Err(error.to_owned());
    }
    let all: Vec<_> = (0..entries.len()).map(|i| entries.entry(i).unwrap()).collect();
    let shaders: Vec<Shader> = all.iter().map(|e| Shader { name: e.name(), vertex: e.vertex(), fragment: e.fragment() }).collect();
    Ok(f(&shaders))
}

/// Checks every shader of pack bytes (`makepad_platform::makepad_script::shader_pack`).
pub fn check_pack(bytes: &[u8]) -> Result<PackLoops, String> {
    with_shaders(bytes, check_shaders)
}

/// The costliest fragment shaders of a pack: (name, cost).
pub fn costliest_of_pack(bytes: &[u8], n: usize) -> Result<Vec<(String, Cost)>, String> {
    with_shaders(bytes, |shaders| costliest(shaders, n))
}
