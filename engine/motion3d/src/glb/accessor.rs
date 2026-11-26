//! glTF accessors read into plain numbers: every component type, normalised
//! integers, interleaved buffer views (`byteStride`), sparse substitution and
//! accessors without a buffer view (all zeros).

use makepad_gltf::{GltfAccessor, LoadedGltf};

/// An accessor's elements, `comps` numbers each, as f64 (exact for every
/// glTF component type, u32 indices included).
#[derive(Clone, Debug, Default)]
pub struct Accessor {
    pub count: usize,
    pub comps: usize,
    pub data: Vec<f64>,
}

impl Accessor {
    pub fn vec2(&self) -> Vec<[f32; 2]> {
        self.rows::<2>()
    }

    pub fn vec3(&self) -> Vec<[f32; 3]> {
        self.rows::<3>()
    }

    pub fn vec4(&self) -> Vec<[f32; 4]> {
        self.rows::<4>()
    }

    /// Element `i`, component `k` as f32, 0 past the end.
    pub fn get(&self, i: usize, k: usize) -> f32 {
        self.data.get(i * self.comps + k).copied().unwrap_or(0.0) as f32
    }

    /// Fixed-width rows; missing components are 0 (a VEC3 read as 4 gets w = 0).
    pub fn rows<const N: usize>(&self) -> Vec<[f32; N]> {
        (0..self.count)
            .map(|i| {
                let mut row = [0.0f32; N];
                for (k, slot) in row.iter_mut().enumerate().take(self.comps) {
                    *slot = self.data[i * self.comps + k] as f32;
                }
                row
            })
            .collect()
    }

    /// Every number as f32 in order.
    pub fn flat(&self) -> Vec<f32> {
        self.data.iter().map(|&v| v as f32).collect()
    }

    pub fn u32s(&self) -> Vec<u32> {
        self.data.iter().map(|&v| v.max(0.0) as u32).collect()
    }
}

/// Numbers per element of an accessor type.
pub fn type_comps(accessor_type: &str) -> Option<usize> {
    Some(match accessor_type {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        "MAT2" => 4,
        "MAT3" => 9,
        "MAT4" => 16,
        _ => return None,
    })
}

/// Bytes per component of a glTF component type.
pub fn component_size(component_type: u32) -> Option<usize> {
    Some(match component_type {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        _ => return None,
    })
}

fn read_component(bytes: &[u8], at: usize, component_type: u32, normalized: bool) -> Option<f64> {
    let b = bytes.get(at..at + component_size(component_type)?)?;
    Some(match component_type {
        5120 => {
            let v = b[0] as i8 as f64;
            if normalized { (v / 127.0).max(-1.0) } else { v }
        }
        5121 => {
            let v = b[0] as f64;
            if normalized { v / 255.0 } else { v }
        }
        5122 => {
            let v = i16::from_le_bytes([b[0], b[1]]) as f64;
            if normalized { (v / 32767.0).max(-1.0) } else { v }
        }
        5123 => {
            let v = u16::from_le_bytes([b[0], b[1]]) as f64;
            if normalized { v / 65535.0 } else { v }
        }
        5125 => {
            let v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64;
            if normalized { v / 4_294_967_295.0 } else { v }
        }
        5126 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        _ => return None,
    })
}

/// The bytes of a buffer view, and its stride (0 = tightly packed).
fn view_bytes(gltf: &LoadedGltf, view: usize) -> Result<(&[u8], usize), String> {
    let v = gltf
        .document
        .buffer_views_slice()
        .get(view)
        .ok_or_else(|| format!("bufferView {view} out of range"))?;
    let buffer = gltf.buffers.get(v.buffer).ok_or_else(|| format!("bufferView {view}: buffer {} missing", v.buffer))?;
    let start = v.byte_offset.unwrap_or(0);
    let end = start + v.byte_length;
    let bytes = buffer
        .get(start..end)
        .ok_or_else(|| format!("bufferView {view} ({start}..{end}) overruns its buffer ({} bytes)", buffer.len()))?;
    Ok((bytes, v.byte_stride.unwrap_or(0)))
}

/// Read accessor `index` of `gltf`.
pub fn read_accessor(gltf: &LoadedGltf, index: usize) -> Result<Accessor, String> {
    let acc: &GltfAccessor = gltf
        .document
        .accessors_slice()
        .get(index)
        .ok_or_else(|| format!("accessor {index} out of range"))?;
    let comps = type_comps(&acc.accessor_type).ok_or_else(|| format!("accessor {index}: unknown type `{}`", acc.accessor_type))?;
    let csize = component_size(acc.component_type).ok_or_else(|| format!("accessor {index}: unknown componentType {}", acc.component_type))?;
    let normalized = acc.normalized.unwrap_or(false);
    let mut data = vec![0.0f64; acc.count * comps];
    if let Some(view) = acc.buffer_view {
        let (bytes, stride) = view_bytes(gltf, view)?;
        let elem = csize * comps;
        // Matrices of 1- and 2-byte components pad each column to 4 bytes.
        let (col_rows, col_stride) = match (acc.accessor_type.as_str(), csize) {
            ("MAT2", 1) => (2, 4),
            ("MAT3", 1) => (3, 4),
            ("MAT3", 2) => (3, 8),
            _ => (comps, comps * csize),
        };
        let elem_bytes = if col_rows != comps { col_stride * (comps / col_rows) } else { elem };
        let stride = if stride == 0 { elem_bytes } else { stride };
        let offset = acc.byte_offset.unwrap_or(0);
        for i in 0..acc.count {
            for k in 0..comps {
                let (col, row) = (k / col_rows, k % col_rows);
                let at = offset + i * stride + col * col_stride + row * csize;
                data[i * comps + k] = read_component(bytes, at, acc.component_type, normalized)
                    .ok_or_else(|| format!("accessor {index}: element {i} past the end of bufferView {view}"))?;
            }
        }
    }
    if let Some(sparse) = &acc.sparse {
        let (ibytes, _) = view_bytes(gltf, sparse.indices.buffer_view)?;
        let (vbytes, _) = view_bytes(gltf, sparse.values.buffer_view)?;
        let isize = component_size(sparse.indices.component_type).ok_or_else(|| format!("accessor {index}: bad sparse index type"))?;
        let ioff = sparse.indices.byte_offset.unwrap_or(0);
        let voff = sparse.values.byte_offset.unwrap_or(0);
        for s in 0..sparse.count {
            let target = read_component(ibytes, ioff + s * isize, sparse.indices.component_type, false)
                .ok_or_else(|| format!("accessor {index}: sparse index {s} out of range"))? as usize;
            if target >= acc.count {
                return Err(format!("accessor {index}: sparse index {target} >= count {}", acc.count));
            }
            for k in 0..comps {
                data[target * comps + k] = read_component(vbytes, voff + (s * comps + k) * csize, acc.component_type, normalized)
                    .ok_or_else(|| format!("accessor {index}: sparse value {s} out of range"))?;
            }
        }
    }
    Ok(Accessor { count: acc.count, comps, data })
}
