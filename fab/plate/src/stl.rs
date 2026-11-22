//! Binary STL: one triangle soup of every part (STL has no objects or
//! colours; use 3MF for those).

use crate::{Part, Plate};
use std::io::{self, Write};

/// Write `parts` as one binary STL.
pub fn write_binary(parts: &[&Part], header: &str, out: &mut impl Write) -> io::Result<()> {
    let mut head = [0u8; 80];
    let text = header.as_bytes();
    // A binary STL must not start with "solid" or readers take it for ASCII.
    let text = if text.starts_with(b"solid") { &text[5..] } else { text };
    let n = text.len().min(80);
    head[..n].copy_from_slice(&text[..n]);
    out.write_all(&head)?;
    let count: usize = parts.iter().map(|p| p.mesh.triangles.len()).sum();
    let count = u32::try_from(count).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many triangles for STL"))?;
    out.write_all(&count.to_le_bytes())?;
    let mut rec = [0u8; 50];
    for part in parts {
        let m = &part.mesh;
        for t in &m.triangles {
            let a = m.positions[t[0] as usize];
            let b = m.positions[t[1] as usize];
            let c = m.positions[t[2] as usize];
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let mut n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len > 0.0 {
                n = [n[0] / len, n[1] / len, n[2] / len];
            }
            let mut at = 0;
            for p in [n, a, b, c] {
                for d in 0..3 {
                    rec[at..at + 4].copy_from_slice(&(p[d] as f32).to_le_bytes());
                    at += 4;
                }
            }
            rec[48] = 0;
            rec[49] = 0;
            out.write_all(&rec)?;
        }
    }
    Ok(())
}

/// The whole plate as binary STL bytes.
pub fn plate_to_stl(plate: &Plate) -> Vec<u8> {
    let parts: Vec<&Part> = plate.parts.iter().collect();
    let mut out = Vec::with_capacity(84 + 50 * plate.triangle_count());
    write_binary(&parts, "Makepad Fab", &mut out).expect("writing to memory cannot fail");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_cube, Part};

    #[test]
    fn cube_stl_size_and_count() {
        let plate = Plate {
            excluded: Vec::new(), settings: Vec::new(),
            parts: vec![Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "c".into(), name: "c".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: test_cube([0.0; 3], 10.0), field_bounds: None, field_floor: None, ..Default::default() }],
        };
        let bytes = plate_to_stl(&plate);
        assert_eq!(bytes.len(), 84 + 12 * 50);
        assert_eq!(u32::from_le_bytes(bytes[80..84].try_into().unwrap()), 12);
        // First normal of the cube's bottom face points down.
        let nz = f32::from_le_bytes(bytes[92..96].try_into().unwrap());
        assert_eq!(nz, -1.0);
    }
}
