//! Per-move records for the animated print preview (R1 §10.7): one record
//! per motion primitive in emit order, with absolute start/end times from
//! the time model and the trapezoid, so playback can place the nozzle at
//! the true position for any time.

pub use makepad_fab_toolpath::Role;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum MoveKind {
    #[default]
    Extrude = 0,
    Travel = 1,
    Retract = 2,
    Unretract = 3,
    Wipe = 4,
    ZHop = 5,
    Dwell = 6,
    /// Time the firmware spends in a macro (start sequence, filament change,
    /// calibration): p0 = p1, duration only.
    FirmwareMacro = 7,
    Seam = 8,
    Pause = 9,
    ColorChange = 10,
}

/// Which experimental mode a move belongs to (`MoveRecord::mode`, `ExtrusionPath::mode`):
/// the writer tags changes with `; FAB_MODE: n`, the time model carries it to the records.
pub mod modes {
    pub use makepad_fab_toolpath::exp::mode::*;
    pub fn name(m: u8) -> &'static str {
        ["Standard", "Wall texture", "Spiral vase", "Scarf seam", "Adaptive layers", "Ironing", "Interlock", "Arc overhang", "Non-planar top"].get(m as usize).copied().unwrap_or("Standard")
    }
}

pub mod flags {
    pub const FIRST_LAYER: u16 = 1;
    pub const WIPE_TOWER: u16 = 2;
    pub const NOZZLE_CHANGE: u16 = 4;
    pub const SKIPPABLE: u16 = 8;
    pub const ARC: u16 = 16;
    pub const SLOPED: u16 = 32;
    /// The time a change's purge takes (a `FirmwareMacro`, the incoming filament's).
    pub const PURGE: u16 = 64;
    /// An extrusion inside a part closed in on every side (`ExtrusionPath::core`, tagged
    /// `; FAB_CORE: 1`): a fill is the preview's core, left out while its part shows whole, an
    /// inner wall the core's lining.
    pub const CORE: u16 = 128;
}

/// One motion primitive (or one timed macro) of the print.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MoveRecord {
    pub t_start: f64,
    pub t_end: f64,
    pub kind: MoveKind,
    pub role: Role,
    /// Printer coordinates in mm.
    pub p0: [f32; 3],
    pub p1: [f32; 3],
    /// Physical nozzle (firmware index; on the H2D 0 = right, 1 = left).
    pub tool: u8,
    /// The nozzle in Fab's numbering (the machine's nozzle list, the
    /// profile's extruder order): 0 = left, 1 = right.
    pub nozzle: u8,
    /// Project filament slot, 1-based (0 = none yet).
    pub filament: u8,
    pub width: f32,
    pub height: f32,
    /// Filament length (mm); negative for retractions.
    pub e: f32,
    pub v_entry: f32,
    pub v_cruise: f32,
    pub v_exit: f32,
    pub accel: f32,
    /// Commanded feedrate (mm/s).
    pub f_cmd: f32,
    /// Fan states in percent after this record.
    pub fan_part: u8,
    pub fan_aux: u8,
    pub fan_chamber: u8,
    /// The nozzles' target temperatures after this record: an index into its timeline's
    /// table (`process::Timeline::nozzle_temps`), one target (°C) per nozzle in Fab's
    /// numbering, as many as the machine has.
    pub temps: u32,
    pub t_bed: i16,
    pub layer: u32,
    pub z_layer: f32,
    /// The plate part being printed (index into the plate's parts, the
    /// slicer's object order), `u16::MAX` outside any object.
    pub object: u16,
    pub gcode_line: u32,
    pub flags: u16,
    /// Arcs (`flags::ARC`): the centre (mm) and the signed sweep (radians,
    /// positive counter-clockwise); p0/p1 are the ends.
    pub center: [f32; 2],
    pub sweep: f32,
    /// A block printed after its layer (non-planar finish): its id, 0 none.
    /// Written as `; BLOCK: <id>` .. `; BLOCK_END` comments.
    pub block: u16,
    /// The experimental mode ([`modes`]).
    pub mode: u8,
}

impl MoveRecord {
    /// Position at absolute time `t` inside this record, from the trapezoid.
    pub fn position_at(&self, t: f64) -> [f32; 3] {
        let dur = (self.t_end - self.t_start).max(1e-12);
        let tt = (t - self.t_start).clamp(0.0, dur);
        let dx = self.p1[0] - self.p0[0];
        let dy = self.p1[1] - self.p0[1];
        let dz = self.p1[2] - self.p0[2];
        let mut len = ((dx * dx + dy * dy + dz * dz) as f64).sqrt();
        if self.flags & flags::ARC != 0 {
            let r = ((self.p0[0] - self.center[0]).powi(2) + (self.p0[1] - self.center[1]).powi(2)).sqrt() as f64;
            let lxy = (self.sweep.abs() as f64) * r;
            len = (lxy * lxy + (dz as f64) * (dz as f64)).sqrt();
        }
        if len <= 1e-9 {
            return self.p1;
        }
        let a = self.accel as f64;
        let (v0, vc, v1) = (self.v_entry as f64, self.v_cruise as f64, self.v_exit as f64);
        let s = if a > 0.0 && vc > 0.0 {
            let t_acc = ((vc - v0) / a).max(0.0);
            let t_dec = ((vc - v1) / a).max(0.0);
            let t_cru = (dur - t_acc - t_dec).max(0.0);
            if tt <= t_acc {
                v0 * tt + 0.5 * a * tt * tt
            } else if tt <= t_acc + t_cru {
                let s_acc = v0 * t_acc + 0.5 * a * t_acc * t_acc;
                s_acc + vc * (tt - t_acc)
            } else {
                let s_acc = v0 * t_acc + 0.5 * a * t_acc * t_acc;
                let s_cru = vc * t_cru;
                let td = tt - t_acc - t_cru;
                s_acc + s_cru + vc * td - 0.5 * a * td * td
            }
        } else {
            len * tt / dur
        };
        let f = (s / len).clamp(0.0, 1.0) as f32;
        if self.flags & flags::ARC != 0 {
            let (cx, cy) = (self.center[0], self.center[1]);
            let r = ((self.p0[0] - cx).powi(2) + (self.p0[1] - cy).powi(2)).sqrt();
            let a0 = (self.p0[1] - cy).atan2(self.p0[0] - cx);
            let a = a0 + self.sweep * f;
            return [cx + r * a.cos(), cy + r * a.sin(), self.p0[2] + dz * f];
        }
        [self.p0[0] + dx * f, self.p0[1] + dy * f, self.p0[2] + dz * f]
    }
}

/// The preview record file: `FABREC04`, record count (u32), then one
/// little-endian row per record (116 bytes):
/// t_start f64, t_end f64, kind u8, role u8, tool u8, filament u8,
/// p0 3×f32, p1 3×f32, width f32, height f32, e f32,
/// v_entry f32, v_cruise f32, v_exit f32, accel f32, f_cmd f32,
/// fan_part u8, fan_aux u8, fan_chamber u8, nozzle u8,
/// temps u32, t_bed i16, flags u16,
/// layer u32, z_layer f32, object u16, pad u16, gcode_line u32,
/// center 2×f32, sweep f32.
/// Then the layer index: u32 count, and per layer (layer u32, first record
/// u32, t_start f64). Then the temperature table `temps` indexes: u32 count, and per entry
/// a u32 nozzle count and that many i16 targets. Nothing follows the table.
pub const RECORD_MAGIC: &[u8; 8] = b"FABREC04";
pub const RECORD_SIZE: usize = 116;

pub fn encode_records(moves: &[MoveRecord], temps: &[Vec<i16>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + moves.len() * RECORD_SIZE);
    out.extend_from_slice(RECORD_MAGIC);
    out.extend_from_slice(&(moves.len() as u32).to_le_bytes());
    for m in moves {
        let start = out.len();
        out.extend_from_slice(&m.t_start.to_le_bytes());
        out.extend_from_slice(&m.t_end.to_le_bytes());
        out.extend_from_slice(&[m.kind as u8, m.role as u8, m.tool, m.filament]);
        for v in m.p0.iter().chain(m.p1.iter()).chain([m.width, m.height, m.e, m.v_entry, m.v_cruise, m.v_exit, m.accel, m.f_cmd].iter()) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[m.fan_part, m.fan_aux, m.fan_chamber, m.nozzle]);
        out.extend_from_slice(&m.temps.to_le_bytes());
        out.extend_from_slice(&m.t_bed.to_le_bytes());
        out.extend_from_slice(&m.flags.to_le_bytes());
        out.extend_from_slice(&m.layer.to_le_bytes());
        out.extend_from_slice(&m.z_layer.to_le_bytes());
        out.extend_from_slice(&m.object.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&m.gcode_line.to_le_bytes());
        for v in [m.center[0], m.center[1], m.sweep] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        debug_assert_eq!(out.len() - start, RECORD_SIZE);
    }
    let idx = layer_index(moves);
    out.extend_from_slice(&(idx.len() as u32).to_le_bytes());
    for l in idx {
        out.extend_from_slice(&l.layer.to_le_bytes());
        out.extend_from_slice(&(l.first as u32).to_le_bytes());
        out.extend_from_slice(&l.t_start.to_le_bytes());
    }
    out.extend_from_slice(&(temps.len() as u32).to_le_bytes());
    for entry in temps {
        out.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        for t in entry {
            out.extend_from_slice(&t.to_le_bytes());
        }
    }
    out
}

/// Where each layer starts in the record list, for jumping and scrubbing
/// by layer without a scan.
#[derive(Clone, Copy, Debug)]
pub struct LayerStart {
    pub layer: u32,
    pub first: usize,
    pub t_start: f64,
}

pub fn layer_index(moves: &[MoveRecord]) -> Vec<LayerStart> {
    let mut out: Vec<LayerStart> = Vec::new();
    for (i, m) in moves.iter().enumerate() {
        if out.last().map_or(true, |l| l.layer != m.layer) {
            out.push(LayerStart { layer: m.layer, first: i, t_start: m.t_start });
        }
    }
    out
}

/// A record file's contents: the records, the layer index and the temperature table.
pub type RecordFile = (Vec<MoveRecord>, Vec<LayerStart>, Vec<Vec<i16>>);

/// Reads a record file written by [`encode_records`].
pub fn decode_records(bytes: &[u8]) -> Result<RecordFile, String> {
    struct R<'a> {
        b: &'a [u8],
        at: usize,
    }
    impl<'a> R<'a> {
        fn take<const N: usize>(&mut self) -> Result<[u8; N], String> {
            let s = self.b.get(self.at..self.at + N).ok_or("record file is truncated")?;
            self.at += N;
            Ok(s.try_into().unwrap())
        }
        fn u8(&mut self) -> Result<u8, String> {
            Ok(self.take::<1>()?[0])
        }
        fn u16(&mut self) -> Result<u16, String> {
            Ok(u16::from_le_bytes(self.take()?))
        }
        fn i16(&mut self) -> Result<i16, String> {
            Ok(i16::from_le_bytes(self.take()?))
        }
        fn u32(&mut self) -> Result<u32, String> {
            Ok(u32::from_le_bytes(self.take()?))
        }
        fn f32(&mut self) -> Result<f32, String> {
            Ok(f32::from_le_bytes(self.take()?))
        }
        fn f64(&mut self) -> Result<f64, String> {
            Ok(f64::from_le_bytes(self.take()?))
        }
    }
    if bytes.get(..8) != Some(&RECORD_MAGIC[..]) {
        return Err("not a Fab preview record file".into());
    }
    let mut r = R { b: bytes, at: 8 };
    let n = r.u32()? as usize;
    if bytes.len() < 12 + n * RECORD_SIZE {
        return Err("record file is truncated".into());
    }
    let kind = |k: u8| match k {
        1 => MoveKind::Travel,
        2 => MoveKind::Retract,
        3 => MoveKind::Unretract,
        4 => MoveKind::Wipe,
        5 => MoveKind::ZHop,
        6 => MoveKind::Dwell,
        7 => MoveKind::FirmwareMacro,
        8 => MoveKind::Seam,
        9 => MoveKind::Pause,
        10 => MoveKind::ColorChange,
        _ => MoveKind::Extrude,
    };
    let mut moves = Vec::with_capacity(n);
    for _ in 0..n {
        let mut m = MoveRecord { t_start: r.f64()?, t_end: r.f64()?, ..Default::default() };
        m.kind = kind(r.u8()?);
        m.role = Role::from_u8(r.u8()?);
        m.tool = r.u8()?;
        m.filament = r.u8()?;
        for i in 0..3 {
            m.p0[i] = r.f32()?;
        }
        for i in 0..3 {
            m.p1[i] = r.f32()?;
        }
        m.width = r.f32()?;
        m.height = r.f32()?;
        m.e = r.f32()?;
        m.v_entry = r.f32()?;
        m.v_cruise = r.f32()?;
        m.v_exit = r.f32()?;
        m.accel = r.f32()?;
        m.f_cmd = r.f32()?;
        m.fan_part = r.u8()?;
        m.fan_aux = r.u8()?;
        m.fan_chamber = r.u8()?;
        m.nozzle = r.u8()?;
        m.temps = r.u32()?;
        m.t_bed = r.i16()?;
        m.flags = r.u16()?;
        m.layer = r.u32()?;
        m.z_layer = r.f32()?;
        m.object = r.u16()?;
        r.u16()?;
        m.gcode_line = r.u32()?;
        m.center = [r.f32()?, r.f32()?];
        m.sweep = r.f32()?;
        moves.push(m);
    }
    let nl = r.u32()? as usize;
    let mut layers = Vec::with_capacity(nl.min(bytes.len()));
    for _ in 0..nl {
        layers.push(LayerStart { layer: r.u32()?, first: r.u32()? as usize, t_start: r.f64()? });
    }
    let nt = r.u32()? as usize;
    let mut temps = Vec::with_capacity(nt.min(bytes.len()));
    for _ in 0..nt {
        let n = r.u32()? as usize;
        if (bytes.len() - r.at) / 2 < n {
            return Err("record file is truncated".into());
        }
        temps.push((0..n).map(|_| r.i16()).collect::<Result<Vec<_>, _>>()?);
    }
    if r.at != bytes.len() {
        return Err("record file has bytes past its temperature table".into());
    }
    if moves.iter().any(|m| m.temps as usize >= temps.len()) {
        return Err("a record names a temperature entry the file does not have".into());
    }
    Ok((moves, layers, temps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_file_round_trips() {
        let mut a = MoveRecord { t_start: 1.0, t_end: 2.5, kind: MoveKind::Travel, role: Role::OuterWall, tool: 0, nozzle: 1, filament: 2, object: 3, layer: 4, ..Default::default() };
        a.p1 = [10.0, 20.0, 0.2];
        a.temps = 1;
        let b = MoveRecord { layer: 5, t_start: 2.5, t_end: 3.0, object: u16::MAX, ..a };
        // Any number of nozzles: nine here.
        let temps = vec![vec![0; 9], vec![220, -1, 0, 215, 0, 0, 0, 0, 190]];
        let bytes = encode_records(&[a, b], &temps);
        assert_eq!(bytes.len(), 12 + 2 * RECORD_SIZE + 4 + 2 * 16 + 4 + 2 * (4 + 9 * 2));
        let (m, l, t) = decode_records(&bytes).unwrap();
        assert_eq!(t, temps);
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].kind, m[0].role, m[0].nozzle, m[0].tool, m[0].object), (MoveKind::Travel, Role::OuterWall, 1, 0, 3));
        assert_eq!(m[0].p1, [10.0, 20.0, 0.2]);
        assert_eq!(t[m[0].temps as usize][8], 190);
        assert_eq!(m[1].object, u16::MAX);
        assert_eq!(l.len(), 2);
        assert_eq!((l[1].layer, l[1].first, l[1].t_start), (5, 1, 2.5));
        // The reader is strict: a byte past the table, a cut table, an entry the table lacks.
        let mut long = bytes.clone();
        long.push(0);
        assert!(decode_records(&long).is_err());
        assert!(decode_records(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode_records(&encode_records(&[a], &temps[..1])).is_err());
        // An earlier layout is not read as this one.
        let mut old = bytes.clone();
        old[..8].copy_from_slice(b"FABREC03");
        assert!(decode_records(&old).is_err());
    }
}
