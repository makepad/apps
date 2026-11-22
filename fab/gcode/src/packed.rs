//! A layer's records as the stored timeline keeps them ([`crate::layers::LayerChunk`]): 40 bytes
//! a move instead of a [`MoveRecord`]'s 120, every record read back bit for bit. What changes
//! from move to move (its start, its end point, its filament and the trapezoid's speeds) is kept
//! per move; what changes by feature (kind, role, tools, width and height, feed and acceleration,
//! fans and temperatures, layer, object, flags) once per distinct value, in a table. A move
//! starts where the one before ended and ends as the next starts; what does not (a jump, an
//! arc's centre and sweep, a G-code line) is kept beside, by move.

use crate::record::{MoveKind, MoveRecord, Role};
use std::collections::HashMap;

/// What changes from move to move.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct Core {
    /// `t_start`'s bits in two halves: the row keeps a 4-byte alignment, 40 bytes.
    t_start: [u32; 2],
    p1: [f32; 3],
    e: f32,
    v_entry: f32,
    v_cruise: f32,
    v_exit: f32,
    /// Its entry in [`PackedMoves::states`].
    state: u32,
}

impl Core {
    fn t_start(&self) -> f64 {
        f64::from_bits(self.t_start[0] as u64 | (self.t_start[1] as u64) << 32)
    }
}

/// What changes by feature.
#[derive(Clone, Copy, Debug)]
struct State {
    kind: MoveKind,
    role: Role,
    tool: u8,
    nozzle: u8,
    filament: u8,
    fan_part: u8,
    fan_aux: u8,
    fan_chamber: u8,
    mode: u8,
    width: f32,
    height: f32,
    accel: f32,
    f_cmd: f32,
    temps: u32,
    t_bed: i16,
    layer: u32,
    z_layer: f32,
    object: u16,
    flags: u16,
    block: u16,
}

impl State {
    fn of(m: &MoveRecord) -> State {
        State {
            kind: m.kind,
            role: m.role,
            tool: m.tool,
            nozzle: m.nozzle,
            filament: m.filament,
            fan_part: m.fan_part,
            fan_aux: m.fan_aux,
            fan_chamber: m.fan_chamber,
            mode: m.mode,
            width: m.width,
            height: m.height,
            accel: m.accel,
            f_cmd: m.f_cmd,
            temps: m.temps,
            t_bed: m.t_bed,
            layer: m.layer,
            z_layer: m.z_layer,
            object: m.object,
            flags: m.flags,
            block: m.block,
        }
    }

    /// Its fields bit for bit: states with equal keys read back the same.
    fn key(&self) -> [u64; 6] {
        let bytes = [self.kind as u8, self.role as u8, self.tool, self.nozzle, self.filament, self.fan_part, self.fan_aux, self.fan_chamber];
        [
            u64::from_le_bytes(bytes),
            self.mode as u64 | (self.flags as u64) << 8 | (self.block as u64) << 24 | (self.object as u64) << 40,
            self.width.to_bits() as u64 | (self.height.to_bits() as u64) << 32,
            self.accel.to_bits() as u64 | (self.f_cmd.to_bits() as u64) << 32,
            self.temps as u64 | (self.t_bed as u16 as u64) << 32,
            self.layer as u64 | (self.z_layer.to_bits() as u64) << 32,
        ]
    }
}

/// A move whose start point, end time, arc or G-code line is not what its neighbours tell.
#[derive(Clone, Copy, Debug)]
struct Beside {
    at: u32,
    p0: [f32; 3],
    t_end: f64,
    center: [f32; 2],
    sweep: f32,
    gcode_line: u32,
}

/// A run of records, packed ([`PackedMoves::pack`]) and read back one at a time.
#[derive(Clone, Debug, Default)]
pub struct PackedMoves {
    core: Box<[Core]>,
    states: Box<[State]>,
    /// By move, ascending.
    beside: Box<[Beside]>,
    /// The first move's start point and the last move's end time.
    first_p0: [f32; 3],
    end: f64,
}

impl PackedMoves {
    /// The records packed, in their order.
    pub fn pack(moves: &[MoveRecord]) -> PackedMoves {
        let mut index: HashMap<[u64; 6], u32> = HashMap::new();
        let mut states: Vec<State> = Vec::new();
        let mut core = Vec::with_capacity(moves.len());
        let mut beside = Vec::new();
        let first_p0 = moves.first().map_or([0.0; 3], |m| m.p0);
        let end = moves.last().map_or(0.0, |m| m.t_end);
        for (i, m) in moves.iter().enumerate() {
            let s = State::of(m);
            let state = *index.entry(s.key()).or_insert_with(|| {
                states.push(s);
                states.len() as u32 - 1
            });
            let bits = m.t_start.to_bits();
            core.push(Core { t_start: [bits as u32, (bits >> 32) as u32], p1: m.p1, e: m.e, v_entry: m.v_entry, v_cruise: m.v_cruise, v_exit: m.v_exit, state });
            let p0 = if i == 0 { first_p0 } else { moves[i - 1].p1 };
            let t_end = moves.get(i + 1).map_or(end, |n| n.t_start);
            let told = m.p0.iter().zip(&p0).all(|(a, b)| a.to_bits() == b.to_bits())
                && m.t_end.to_bits() == t_end.to_bits()
                && m.center.iter().chain([&m.sweep]).all(|c| c.to_bits() == 0)
                && m.gcode_line == 0;
            if !told {
                beside.push(Beside { at: i as u32, p0: m.p0, t_end: m.t_end, center: m.center, sweep: m.sweep, gcode_line: m.gcode_line });
            }
        }
        PackedMoves { core: core.into(), states: states.into(), beside: beside.into(), first_p0, end }
    }

    pub fn len(&self) -> usize {
        self.core.len()
    }

    pub fn is_empty(&self) -> bool {
        self.core.is_empty()
    }

    /// Move `i` from its row, its state and what is kept beside it (`b`, when anything is).
    fn make(&self, i: usize, b: Option<&Beside>) -> MoveRecord {
        let c = &self.core[i];
        let s = &self.states[c.state as usize];
        let (p0, t_end, center, sweep, gcode_line) = match b {
            Some(b) => (b.p0, b.t_end, b.center, b.sweep, b.gcode_line),
            None => (if i == 0 { self.first_p0 } else { self.core[i - 1].p1 }, self.core.get(i + 1).map_or(self.end, Core::t_start), [0.0; 2], 0.0, 0),
        };
        MoveRecord {
            t_start: c.t_start(),
            t_end,
            kind: s.kind,
            role: s.role,
            p0,
            p1: c.p1,
            tool: s.tool,
            nozzle: s.nozzle,
            filament: s.filament,
            width: s.width,
            height: s.height,
            e: c.e,
            v_entry: c.v_entry,
            v_cruise: c.v_cruise,
            v_exit: c.v_exit,
            accel: s.accel,
            f_cmd: s.f_cmd,
            fan_part: s.fan_part,
            fan_aux: s.fan_aux,
            fan_chamber: s.fan_chamber,
            temps: s.temps,
            t_bed: s.t_bed,
            layer: s.layer,
            z_layer: s.z_layer,
            object: s.object,
            gcode_line,
            flags: s.flags,
            center,
            sweep,
            block: s.block,
            mode: s.mode,
        }
    }

    /// Every move in order, each read once: the moves kept beside are walked along, not looked up.
    pub fn iter(&self) -> Iter<'_> {
        Iter { moves: self, front: 0, back: self.len(), beside_front: 0, beside_back: self.beside.len() }
    }
}

/// [`PackedMoves::iter`].
#[derive(Clone)]
pub struct Iter<'a> {
    moves: &'a PackedMoves,
    front: usize,
    back: usize,
    /// The first and one past the last entry of `beside` not read yet.
    beside_front: usize,
    beside_back: usize,
}

impl Iterator for Iter<'_> {
    type Item = MoveRecord;

    fn next(&mut self) -> Option<MoveRecord> {
        if self.front == self.back {
            return None;
        }
        let i = self.front;
        self.front += 1;
        let b = self.moves.beside[self.beside_front..self.beside_back].first().filter(|b| b.at as usize == i);
        self.beside_front += b.is_some() as usize;
        Some(self.moves.make(i, b))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.back - self.front, Some(self.back - self.front))
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<MoveRecord> {
        if self.front == self.back {
            return None;
        }
        self.back -= 1;
        let i = self.back;
        let b = self.moves.beside[self.beside_front..self.beside_back].last().filter(|b| b.at as usize == i);
        self.beside_back -= b.is_some() as usize;
        Some(self.moves.make(i, b))
    }
}

impl ExactSizeIterator for Iter<'_> {}

/// Records as their holder keeps them: plain or packed, read in order.
#[derive(Clone, Copy, Debug)]
pub enum MovesRef<'a> {
    Records(&'a [MoveRecord]),
    Packed(&'a PackedMoves),
}

impl<'a> MovesRef<'a> {
    pub fn iter(self) -> MovesIter<'a> {
        match self {
            MovesRef::Records(r) => MovesIter::Records(r.iter().copied()),
            MovesRef::Packed(p) => MovesIter::Packed(p.iter()),
        }
    }
}

/// [`MovesRef::iter`].
#[derive(Clone)]
pub enum MovesIter<'a> {
    Records(std::iter::Copied<std::slice::Iter<'a, MoveRecord>>),
    Packed(Iter<'a>),
}

impl Iterator for MovesIter<'_> {
    type Item = MoveRecord;

    fn next(&mut self) -> Option<MoveRecord> {
        match self {
            MovesIter::Records(r) => r.next(),
            MovesIter::Packed(p) => p.next(),
        }
    }
}
