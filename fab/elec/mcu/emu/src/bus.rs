//! The memory bus every CPU model runs on, and plain aliased memory behind it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width { B, H, W }

impl Width {
    pub fn bytes(self) -> usize {
        match self { Self::B => 1, Self::H => 2, Self::W => 4 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Fetch, Load, Store }

/// What a CPU model sees of its machine: memory, and devices mapped into it.
pub trait Bus {
    /// Little-endian read, zero-extended. `now` is the retired instruction count
    /// before this access. `Err(())` is an access fault. Faulting accesses must
    /// leave backing memory unchanged; successful reads may have device effects.
    fn read(&mut self, addr: u32, width: Width, kind: Kind, now: u64) -> Result<u32, ()>;
    /// Writes the low `width` bytes. On `Err(())`, must leave memory unchanged:
    /// the CPU cannot roll back a peripheral's partial write.
    fn write(&mut self, addr: u32, width: Width, value: u32, now: u64) -> Result<(), ()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Perm { pub read: bool, pub write: bool, pub exec: bool }

struct Mapping {
    base: u64,
    end: u64,
    store: usize,
    offset: usize,
    perm: Perm,
}

/// Mappings onto byte stores. Multiple mappings may alias one store. Accesses
/// must fit entirely inside one mapping and have the permission for their kind.
/// Memory itself permits unaligned accesses; the CPU enforces alignment.
/// Choice for a direct `read` with `Kind::Store`: require write permission.
/// The CPU only uses Fetch/Load reads and calls `write` for stores.
#[derive(Default)]
pub struct Memory {
    stores: Vec<Vec<u8>>,
    mappings: Vec<Mapping>,
}

impl Memory {
    pub fn new() -> Memory { Self::default() }

    /// Allocates a zero-filled store and returns its index.
    pub fn store(&mut self, len: usize) -> usize {
        let index = self.stores.len();
        self.stores.push(vec![0; len]);
        index
    }

    /// Maps `len` store bytes starting at `offset` to `base`. Panics on an
    /// invalid store/range, overlapping mappings, or a range beyond the 32-bit
    /// address space. Choice: a valid empty range creates no mapping.
    pub fn map(&mut self, base: u32, store: usize, offset: usize, len: usize, perm: Perm) {
        let bytes = &self.stores[store];
        assert!(offset <= bytes.len() && len <= bytes.len() - offset, "mapping exceeds store");
        let base = u64::from(base);
        let end = base.checked_add(len as u64).expect("mapping address overflow");
        assert!(end <= 1u64 << 32, "mapping exceeds 32-bit addresses");
        if len == 0 { return; }
        assert!(self.mappings.iter().all(|m| end <= m.base || base >= m.end), "overlapping mappings");
        self.mappings.push(Mapping { base, end, store, offset, perm });
    }

    pub fn bytes(&self, store: usize) -> &[u8] { &self.stores[store] }
    pub fn bytes_mut(&mut self, store: usize) -> &mut [u8] { &mut self.stores[store] }

    fn locate(&self, addr: u32, width: Width, kind: Kind) -> Result<(usize, usize), ()> {
        let addr = u64::from(addr);
        let end = addr + width.bytes() as u64;
        let m = self.mappings.iter().find(|m| addr >= m.base && end <= m.end).ok_or(())?;
        let allowed = match kind { Kind::Fetch => m.perm.exec, Kind::Load => m.perm.read, Kind::Store => m.perm.write };
        if !allowed { return Err(()); }
        Ok((m.store, m.offset + (addr - m.base) as usize))
    }
}

impl Bus for Memory {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, _now: u64) -> Result<u32, ()> {
        let (store, offset) = self.locate(addr, width, kind)?;
        let mut value = 0;
        for (i, byte) in self.stores[store][offset..offset + width.bytes()].iter().enumerate() {
            value |= u32::from(*byte) << (i * 8);
        }
        Ok(value)
    }

    fn write(&mut self, addr: u32, width: Width, value: u32, _now: u64) -> Result<(), ()> {
        // Check the entire access before touching any bytes, including aliases.
        let (store, offset) = self.locate(addr, width, Kind::Store)?;
        self.stores[store][offset..offset + width.bytes()]
            .copy_from_slice(&value.to_le_bytes()[..width.bytes()]);
        Ok(())
    }
}
