//! The shader packs of the trace runs (`makepad-web-trace trace --record-pack`) as one pack:
//! entries deduplicated by key (the first run's wins; two runs must agree on a key's code), and
//! the bounded-loop check of the browser safety gate ([`crate::shader_loops`]) on it: a pack
//! with a loop that is not provably bounded is not shipped.

use crate::shader_loops::{check_shaders, PackLoops, Shader};
use makepad_platform::makepad_script::shader_pack::{read_shader_pack, write_shader_pack, ShaderPackEntry};
use std::path::{Path, PathBuf};

/// The merged pack.
#[derive(Clone, Debug, Default)]
pub struct MergedPack {
    pub bytes: Vec<u8>,
    pub entries: usize,
    pub loops: PackLoops,
    /// Keys two packs gave different code for: `<path>: <shader> (key)`.
    pub disagree: Vec<String>,
}

impl MergedPack {
    pub fn summary(&self) -> String {
        format!(
            "{} shaders, {} bytes; loops: {} bounded (at most {} passes), {} guarded, {} unbounded",
            self.entries, self.bytes.len(), self.loops.bounded, self.loops.max_trips, self.loops.guarded, self.loops.unbounded.len()
        )
    }

    /// Why it may not ship (empty: it may).
    pub fn problems(&self) -> Vec<String> {
        let mut out: Vec<String> = self.loops.unbounded.iter().map(|u| format!("{} ({} line {}): `{}`: {}", u.shader, u.stage, u.line, u.head, u.reason)).collect();
        out.extend(self.disagree.iter().map(|d| format!("{d}: differs between runs")));
        out
    }
}

pub fn merge(inputs: &[PathBuf]) -> Result<MergedPack, String> {
    let mut entries: Vec<ShaderPackEntry> = Vec::new();
    let mut disagree = Vec::new();
    for path in inputs {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        for e in read_shader_pack(&bytes).map_err(|e| format!("{}: {e}", path.display()))? {
            match entries.iter().find(|x| x.key == e.key) {
                Some(x) if x.vertex != e.vertex || x.fragment != e.fragment => disagree.push(format!("{}: {} (key {:016x})", path.display(), e.name, e.key)),
                Some(_) => {}
                None => entries.push(e),
            }
        }
    }
    let shaders: Vec<Shader> = entries.iter().map(|e| Shader { name: &e.name, vertex: &e.vertex, fragment: &e.fragment }).collect();
    let loops = check_shaders(&shaders);
    Ok(MergedPack { bytes: write_shader_pack(&entries), entries: entries.len(), loops, disagree })
}

/// Merges `inputs` into `out` (written even when it may not ship, for a look).
pub fn merge_to(out: &Path, inputs: &[PathBuf]) -> Result<MergedPack, String> {
    let pack = merge(inputs)?;
    std::fs::write(out, &pack.bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(pack)
}
