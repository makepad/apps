//! Host-owned item-state rows for one unit's sheet, stable across circuit recompilation.
//!
//! Sync when the circuit changes; views use the returned index while building instances.
//! The host clears texels for rows absent from that index. Historical keys stay allocated.

use fab_elec_circuit::Circuit;
use std::collections::{HashMap, HashSet};

/// A sheet item's identity across edits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SheetKey {
    /// Component path.
    Comp(String),
    /// Net name.
    Net(String),
    /// Bus name.
    Bus(String),
    /// Block path; the root block is `""`.
    Block(String),
}

/// Rows per item of one unit's circuit, issued once and never reused for another key.
/// A removed item's row stays dead; re-adding its key restores its original row.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SheetRows {
    rows: HashMap<SheetKey, u32>,
    next: u32,
}

/// O(1) row lookups by circuit index, for a view building instances.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RowIndex {
    pub comps: Vec<u32>,
    pub nets: Vec<u32>,
    pub buses: Vec<u32>,
    pub blocks: Vec<u32>,
}

impl SheetRows {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue rows for unseen keys: components by index, then nets, buses and blocks.
    /// Equal edit histories give equal rows; hash tables never determine output order.
    /// Expected O(items), with O(items) temporary storage.
    pub fn sync(&mut self, circuit: &Circuit) -> RowIndex {
        let comps = self.sync_names(circuit.comps.iter().map(|c| c.path.as_str()), SheetKey::Comp);
        let nets = self.sync_names(circuit.nets.iter().map(|n| n.name.as_str()), SheetKey::Net);
        let buses = self.sync_names(circuit.buses.iter().map(|b| b.name.as_str()), SheetKey::Bus);
        let blocks = self.sync_names(circuit.tree.iter().map(|b| b.path.as_str()), SheetKey::Block);
        RowIndex { comps, nets, buses, blocks }
    }

    pub fn row(&self, key: &SheetKey) -> Option<u32> {
        self.rows.get(key).copied()
    }

    /// Rows issued so far, including dead rows (the texture's texel count).
    pub fn count(&self) -> u32 {
        self.next
    }

    fn sync_names<'a>(
        &mut self,
        names: impl Iterator<Item = &'a str>,
        key: fn(String) -> SheetKey,
    ) -> Vec<u32> {
        let mut used = HashSet::new();
        let mut suffixes = HashMap::new();
        names
            .map(|name| {
                let mut unique = name.to_owned();
                if !used.insert(unique.clone()) {
                    let suffix = suffixes.entry(name).or_insert(2usize);
                    loop {
                        unique = format!("{name}#{suffix}");
                        *suffix += 1;
                        // Also avoid collisions with names that already contain a suffix.
                        if used.insert(unique.clone()) {
                            break;
                        }
                    }
                }
                let next = &mut self.next;
                *self.rows.entry(key(unique)).or_insert_with(|| {
                    let row = *next;
                    *next = next.checked_add(1).expect("sheet row count exceeds u32");
                    row
                })
            })
            .collect()
    }
}

impl RowIndex {
    pub fn comp(&self, c: u32) -> Option<u32> {
        self.comps.get(c as usize).copied()
    }

    pub fn net(&self, n: u32) -> Option<u32> {
        self.nets.get(n as usize).copied()
    }

    pub fn bus(&self, b: u32) -> Option<u32> {
        self.buses.get(b as usize).copied()
    }

    pub fn block(&self, b: u32) -> Option<u32> {
        self.blocks.get(b as usize).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_circuit::{
        AvoidRegion, Component, GuidePoint, Intent, QtyUnit, Quantity, Target, Topology, Value,
    };
    use fab_elec_core::{BlockId, CompId, NetId};
    use fab_elec_design::{fixture, Design};
    use fab_elec_parts::{Fields, PartDef};
    use std::sync::Arc;

    // Copied from explore's edit tests so these edits exercise the same index remapping.
    fn component(
        c: &mut Circuit,
        path: &str,
        refdes: &str,
        part: Arc<PartDef>,
        value: Option<Value>,
        connections: &[(&str, NetId)],
        nc: &[&str],
        block: BlockId,
    ) -> CompId {
        let id = CompId(c.comps.len() as u32);
        let mut conns: Vec<_> = connections
            .iter()
            .map(|&(name, net)| (part.pin(name).expect("fixture connection names a part pin"), net))
            .collect();
        conns.sort_by_key(|&(pin, _)| pin);
        let nc = nc.iter().map(|name| part.pin(name).expect("fixture NC names a part pin")).collect();
        c.comps.push(Component {
            id, path: path.into(), refdes: refdes.into(), ref_kind: Default::default(), part, value, conns, nc, block, dnp: false,
            fields: Fields::default(),
        });
        id
    }

    fn add_part(d: &Design) -> (Design, BlockId) {
        let mut edited = d.clone();
        let mut c = (*edited.units[0].circuit).clone();
        let block = c.comp("mcu.u").expect("fixture has MCU").block;
        let v33 = c.net("3V3").expect("fixture has 3V3").id;
        let gnd = c.net("GND").expect("fixture has GND").id;
        component(
            &mut c,
            "mcu.dec[3]",
            "C7",
            fixture::parts::capacitor("0402"),
            Some(Value {
                text: "100nF".into(),
                qty: Some(Quantity { si: 100e-9, unit: QtyUnit::Farad }),
                tol: None,
            }),
            &[("1", v33), ("2", gnd)],
            &[],
            block,
        );
        edited.units[0].circuit = Arc::new(c);
        (edited, block)
    }

    fn remove_part(d: &Design) -> (Design, BlockId) {
        let mut edited = d.clone();
        let mut c = (*edited.units[0].circuit).clone();
        let removed = c.comp("mcu.boot_r").expect("fixture has boot resistor").id;
        let block = c.comps.remove(removed.ix()).block;
        for (i, comp) in c.comps.iter_mut().enumerate().skip(removed.ix()) {
            comp.id = CompId(i as u32);
        }
        let remap = |target: &mut Target| match target {
            Target::Comp(id) | Target::Pin(id, _) => {
                if *id == removed {
                    return false;
                }
                if id.0 > removed.0 {
                    id.0 -= 1;
                }
                true
            }
            Target::Block(_) => true,
        };
        // Drop an entire intent if any of its targets names the removed component.
        c.intents.retain_mut(|intent| match intent {
            Intent::Near { a, b, .. } => remap(a) && remap(b),
            Intent::Group { members, .. } => members.iter_mut().all(remap),
            Intent::Side { t, .. } | Intent::Fixed { t, .. } | Intent::Star { at: t, .. } => remap(t),
            Intent::Guide { through, .. } => through.iter_mut().all(|point| match point {
                GuidePoint::At(_) => true,
                GuidePoint::Pass { t, .. } => remap(t),
            }),
            Intent::Topology { topo, .. } => match topo {
                Topology::Tree => true,
                Topology::Star(t) => remap(t),
                Topology::Chain { order, .. } => order.iter_mut().all(remap),
            },
            Intent::Avoid { region, .. } => match region {
                AvoidRegion::Area(_) => true,
                AvoidRegion::Near(t, _) => remap(t),
            },
            Intent::Pair { .. }
            | Intent::LengthMatch { .. }
            | Intent::Width { .. }
            | Intent::LayerPref { .. }
            | Intent::Bundle { .. }
            | Intent::Lane { .. }
            | Intent::MaxVias { .. }
            | Intent::NeedsRef { .. }
            | Intent::Style { .. } => true,
        });
        // Nets retain their indices; mcu.u.BOOT0 now has only the MCU pin.
        edited.units[0].circuit = Arc::new(c);
        (edited, block)
    }

    fn keyed_rows(c: &Circuit, index: &RowIndex) -> Vec<(SheetKey, u32)> {
        c.comps.iter().map(|c| SheetKey::Comp(c.path.clone())).zip(index.comps.iter().copied())
            .chain(c.nets.iter().map(|n| SheetKey::Net(n.name.clone())).zip(index.nets.iter().copied()))
            .chain(c.buses.iter().map(|b| SheetKey::Bus(b.name.clone())).zip(index.buses.iter().copied()))
            .chain(c.tree.iter().map(|b| SheetKey::Block(b.path.clone())).zip(index.blocks.iter().copied()))
            .collect()
    }

    #[test]
    fn rows_survive_edits() {
        let design = fab_elec_fixtures::ledctl();
        let mut rows = SheetRows::new();
        let before = rows.sync(&design.units[0].circuit);
        let original: HashMap<_, _> = keyed_rows(&design.units[0].circuit, &before).into_iter().collect();
        let removed_key = SheetKey::Comp("mcu.boot_r".into());
        let removed_row = original[&removed_key];
        let count_before = rows.count();
        assert_eq!(
            before.comps.iter().chain(&before.nets).chain(&before.buses).chain(&before.blocks)
                .copied().collect::<Vec<_>>(),
            (0..count_before).collect::<Vec<_>>(),
            "issuance order: components, nets, buses and blocks receive consecutive rows",
        );

        let (removed, _) = remove_part(&design);
        let after = rows.sync(&removed.units[0].circuit);
        let c = &removed.units[0].circuit;
        assert_eq!(
            (after.comps.len(), after.nets.len(), after.buses.len(), after.blocks.len()),
            (c.comps.len(), c.nets.len(), c.buses.len(), c.tree.len()),
            "index coverage: every surviving item has a row",
        );
        for (key, row) in keyed_rows(c, &after) {
            assert_eq!(Some(&row), original.get(&key), "surviving identity: {key:?} keeps its row");
            assert_eq!(rows.row(&key), Some(row), "key lookup: {key:?} agrees with the index");
            assert_ne!(row, removed_row, "dead row: {key:?} does not reuse the removed component's row");
        }
        assert_eq!(rows.row(&removed_key), Some(removed_row), "retention: a removed key keeps its row");
        assert_eq!(rows.count(), count_before, "count retention: removal does not shrink or grow the count");

        let count_before_add = rows.count();
        let (added, _) = add_part(&removed);
        let added_index = rows.sync(&added.units[0].circuit);
        let new_comp = added.units[0].circuit.comp("mcu.dec[3]").expect("addition: new capacitor exists");
        assert_eq!(added_index.comp(new_comp.id.0), Some(count_before_add), "addition: new component gets the previous count");
        assert_eq!(rows.count(), count_before_add + 1, "addition: exactly one new row is issued");
        assert!(
            added_index.comps.iter().chain(&added_index.nets).chain(&added_index.buses)
                .chain(&added_index.blocks).all(|&row| row != removed_row),
            "dead row: adding a component does not reuse the removed row",
        );
        let count_after_add = rows.count();
        assert_eq!(rows.sync(&added.units[0].circuit), added_index, "idempotence: repeated sync returns equal indexes");
        assert_eq!(rows.count(), count_after_add, "idempotence: repeated sync does not issue rows");
        assert_eq!(rows.sync(&design.units[0].circuit), before, "restoration: re-added keys regain their original rows");
        assert_eq!(rows.count(), count_after_add, "restoration: historical rows remain allocated");
        assert_eq!(before.net(0), before.nets.first().copied(), "net lookup: index zero returns its row");
        assert_eq!(before.bus(0), before.buses.first().copied(), "bus lookup: index zero returns its row");
        assert_eq!(before.block(0), rows.row(&SheetKey::Block(String::new())), "root identity: empty block path maps to the root row");
        assert_eq!(before.comp(before.comps.len() as u32), None, "component bounds: absent index returns None");
        assert_eq!(before.net(before.nets.len() as u32), None, "net bounds: absent index returns None");
        assert_eq!(before.bus(before.buses.len() as u32), None, "bus bounds: absent index returns None");
        assert_eq!(before.block(before.blocks.len() as u32), None, "block bounds: absent index returns None");

        let mut duplicates = (*design.units[0].circuit).clone();
        let net = duplicates.nets[0].clone();
        let first_duplicate = duplicates.nets.len() as u32;
        duplicates.nets.push(net.clone());
        duplicates.nets.push(net.clone());
        let duplicate_index = rows.sync(&duplicates);
        for (offset, suffix) in [(0, 2), (1, 3)] {
            let row = duplicate_index.net(first_duplicate + offset);
            assert_ne!(row, duplicate_index.net(0), "duplicate isolation: net #{suffix} gets its own row");
            assert_eq!(row, rows.row(&SheetKey::Net(format!("{}#{suffix}", net.name))), "duplicate identity: suffix #{suffix} follows index order");
        }
        assert_ne!(duplicate_index.net(first_duplicate), duplicate_index.net(first_duplicate + 1), "duplicate isolation: repeated names never share rows");
        let duplicate_count = rows.count();
        assert_eq!(rows.sync(&duplicates), duplicate_index, "duplicate idempotence: repeated sync returns equal indexes");
        assert_eq!(rows.count(), duplicate_count, "duplicate idempotence: repeated sync does not issue rows");
    }
}
