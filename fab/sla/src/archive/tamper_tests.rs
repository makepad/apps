//! One broken archive rule at a time, named by the independent reader.

use super::*;
use super::tests::printer;
use crate::printer::{config_of, BUILTIN_PRINTER, BUILTIN_PROCESSES};
use crate::tests::field;
use fab_sla_archive::read::{read, Archive, Rule};
use makepad_csg_sdf::SdfNode;
use makepad_zip_file::ZipMethod;
use std::sync::Arc;

type Entries = Vec<(String, Vec<u8>)>;

/// A 2 × 1.5 × 1 mm block, with both bottom and normal layers, accepted before any mutation.
fn base() -> Archive {
    let job = printer([120, 80]);
    let block = Arc::new(field(SdfNode::Box { size: [2.0, 1.0, 1.5], radius: 0.0 }, [1.008, 0.992, 0.5], ([0.008, 0.242, 0.0], [2.008, 1.742, 1.0])));
    let stack = LayerStack::new(vec![block], &job);
    let (bytes, _) = archive(&stack, &job).expect("base archive");
    let back = read(bytes).expect("the base satisfies every archive rule");
    assert_eq!(back.layers, 10);
    assert!(back.job.bottom_layers > 0 && back.job.bottom_layers < back.layers);
    back
}

fn unpack(back: &Archive) -> Entries {
    back.names.iter().map(|name| (name.clone(), back.entry(name).expect("validated entry"))).collect()
}

fn repack(entries: &Entries) -> Vec<u8> {
    repack_with(entries, |_| {})
}

/// Recompute every digest and CRC, changing one packed entry only when testing those fields.
fn repack_with(entries: &Entries, mut change: impl FnMut(&mut Entry)) -> Vec<u8> {
    let mut zip = ZipWriter::new();
    for (name, data) in entries {
        let mut entry = Entry::of(name, data);
        change(&mut entry);
        zip.add_deflated(&entry.name, entry.size, &entry.packed, &entry.hash).unwrap();
    }
    zip.finish().unwrap()
}

fn contents<'a>(entries: &'a mut Entries, name: &str) -> &'a mut Vec<u8> {
    &mut entries.iter_mut().find(|e| e.0 == name).expect("entry to change").1
}

fn changed(entries: &Entries, name: &str, change: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut entries = entries.clone();
    change(contents(&mut entries, name));
    repack(&entries)
}

/// Replace one scalar value, selected by its key and occurrence in the writer's JSON text.
fn scalar(data: &mut Vec<u8>, key: &str, occurrence: usize, value: impl ToString) {
    let mut text = String::from_utf8(data.clone()).unwrap();
    let key = format!("\"{key}\": ");
    let start = text.match_indices(&key).nth(occurrence).expect("key occurrence").0 + key.len();
    let end = start + text[start..].find([',', '\n']).expect("end of scalar");
    text.replace_range(start..end, &value.to_string());
    *data = text.into_bytes();
}

fn rejected(cases: impl IntoIterator<Item = (&'static str, Vec<u8>, Rule)>) {
    for (case, bytes, expected) in cases {
        let fault = read(bytes).err().unwrap_or_else(|| panic!("{case}: the reader accepted the broken rule {expected:?}"));
        assert_eq!(fault.rule, expected, "{case}: {fault}");
    }
}

/// Check 1: broken ZIP structure, storage method, required entries and layer names are named.
#[test]
fn the_archive_names_its_broken_structure() {
    let back = base();
    let entries = unpack(&back);
    let valid = repack(&entries);
    read(valid.clone()).expect("repacking preserves a valid archive");
    let mut truncated = valid.clone();
    truncated.truncate(truncated.len() - 1);
    // The ZIP writer refuses duplicate names. Rename both copies of layer 1's name to layer 0
    // after packing, keeping the local and central names equal and all lengths unchanged.
    let mut duplicate = valid;
    let from = format::layer_entry(1, format::RASTER_EXTENSION);
    let to = format::layer_entry(0, format::RASTER_EXTENSION);
    assert_eq!(from.len(), to.len());
    let places: Vec<_> = duplicate.windows(from.len()).enumerate().filter_map(|(at, bytes)| (bytes == from.as_bytes()).then_some(at)).collect();
    assert_eq!(places.len(), 2, "local and central entry names");
    for at in places { duplicate[at..at + to.len()].copy_from_slice(to.as_bytes()); }
    let mut zip = ZipWriter::new();
    for (name, data) in &entries {
        if name == format::SETTINGS {
            zip.add(name, data, ZipMethod::Store).unwrap();
        } else {
            let e = Entry::of(name, data);
            zip.add_deflated(name, e.size, &e.packed, &e.hash).unwrap();
        }
    }
    let mut missing = entries.clone();
    missing.retain(|e| e.0 != format::PRINT_INFO);
    let mut extra = entries.clone();
    extra.insert(extra.len() - 1, (format!("{}.extra", format::SOFTWARE), Vec::new()));
    let mut gap = entries.clone();
    gap.retain(|e| e.0 != from);
    let mut mixed = entries.clone();
    mixed.iter_mut().find(|e| e.0 == from).unwrap().0 = format::layer_entry(1, format::VECTOR_EXTENSION);
    rejected([
        ("truncated end record", truncated, Rule::Zip),
        ("duplicate entry", duplicate, Rule::Zip),
        ("stored entry", zip.finish().unwrap(), Rule::Deflated),
        ("missing print totals", repack(&missing), Rule::Entries),
        ("unknown entry", repack(&extra), Rule::Entries),
        ("layer 1 missing", repack(&gap), Rule::LayerEntries),
        ("vector extension among raster layers", repack(&mixed), Rule::LayerEntries),
    ]);
}

/// Check 2: absent, incorrect and malformed hashes, and an incorrect CRC, are distinguished.
#[test]
fn the_archive_names_its_broken_hashes() {
    let back = base();
    let entries = unpack(&back);
    let missing = repack_with(&entries, |e| { if e.name == format::SETTINGS { e.hash.clear(); } });
    let digest = repack_with(&entries, |e| { if e.name == format::SETTINGS { *e.hash.last_mut().unwrap() ^= 1; } });
    let tag = repack_with(&entries, |e| { if e.name == format::SETTINGS { e.hash[4..6].copy_from_slice(&(format::HASH_FIELD_TAG ^ 1).to_le_bytes()); } });
    let data = back.entry(format::SETTINGS).unwrap();
    let crc = repack_with(&entries, |e| { if e.name == format::SETTINGS { e.packed = Packed::from_deflated(crc32(&data) ^ 1, compress_to_vec(&data, LEVEL)); } });
    rejected([
        ("missing hash field", missing, Rule::Hash),
        ("wrong digest", digest, Rule::Hash),
        ("wrong hash tag", tag, Rule::Hash),
        ("wrong CRC", crc, Rule::Crc),
    ]);
}

/// Check 3: invalid JSON, an absent selected resin, inconsistent screen size and a wrong preview.
#[test]
fn the_archive_names_its_broken_documents() {
    let back = base();
    let entries = unpack(&back);
    let resin = changed(&entries, format::SETTINGS, |data| {
        let mut text = String::from_utf8(data.clone()).unwrap();
        let key = format!("\"{}\": [", format::settings::ACTIVE_RESINS);
        let start = text.find(&key).unwrap() + key.len();
        let end = start + text[start..].find(']').unwrap();
        text.replace_range(start..end, "\"absent resin\"");
        *data = text.into_bytes();
    });
    rejected([
        ("settings is not JSON", changed(&entries, format::SETTINGS, |data| *data = b"{".to_vec()), Rule::Json),
        ("selected resin is absent", resin, Rule::Resin),
        ("resolution differs by two pixels", changed(&entries, format::SETTINGS, |data| scalar(data, format::settings::RES_X, 0, back.screen.pixels[0] + 2)), Rule::ScreenSize),
        ("small preview contains the large PNG", changed(&entries, format::PREVIEW_SMALL, |data| *data = back.entry(format::PREVIEW_LARGE).unwrap()), Rule::Preview),
    ]);
}

/// Check 4: the scene table's size, markers, two counts, slicing planes and screen bounds.
#[test]
fn the_archive_names_its_broken_scene_table() {
    let back = base();
    let entries = unpack(&back);
    let patch = |at: usize, bytes: &[u8]| changed(&entries, format::SCENE, |data| data[at..at + bytes.len()].copy_from_slice(bytes));
    rejected([
        ("scene is one record short", changed(&entries, format::SCENE, |data| { data.drain(396..460); }), Rule::SceneSize),
        ("scene magic", patch(0, &[format::SCENE_MAGIC[0] ^ 1]), Rule::SceneMarkers),
        ("scene end marker", patch(396 + 64 * back.layers as usize, &[format::SCENE_TABLE_END[0] ^ 1]), Rule::SceneMarkers),
        ("scene header count", patch(100, &(back.layers + 1).to_le_bytes()), Rule::SceneCount),
        ("scene table count", patch(392, &(back.layers + 1).to_le_bytes()), Rule::SceneCount),
        ("slicing plane off by one layer", patch(396, &((back.scene.records[0].z as f64 + back.job.layer_height) as f32).to_le_bytes()), Rule::SceneHeights),
        ("layer box beyond half width", patch(412, &((back.screen.pixels[0] as f64 * back.screen.pitch[0] / 2.0 + back.screen.pitch[0]) as f32).to_le_bytes()), Rule::SceneBounds),
    ]);
}

/// Check 5: the per-layer table's count, rows, indices, heights, exposures and lift match the job.
#[test]
fn the_archive_names_its_broken_per_layer_table() {
    use format::layer_table as key;
    let back = base();
    let entries = unpack(&back);
    let patch = |key: &str, row: usize, value: String| changed(&entries, format::LAYER_TABLE, |data| scalar(data, key, row, value));
    let missing = changed(&entries, format::LAYER_TABLE, |data| {
        let mut text = String::from_utf8(data.clone()).unwrap();
        let start = text.find("        {\n").unwrap();
        let end = start + text[start..].find("        },\n").unwrap() + "        },\n".len();
        text.replace_range(start..end, "");
        *data = text.into_bytes();
    });
    rejected([
        ("per-layer count off by one", patch(key::COUNT, 0, (back.layers + 1).to_string()), Rule::TableCount),
        ("per-layer row missing", missing, Rule::TableCount),
        ("per-layer index out of order", patch(key::LAYER_INDEX, 1, "0".into()), Rule::TableIndex),
        ("layer bottom off by one layer", patch(key::LAYER_MINHEIGHT, 0, back.job.layer_height.to_string()), Rule::TableHeights),
        ("bottom layer with normal exposure", patch(key::EXPOSURE_TIME, 0, back.job.exposure.to_string()), Rule::TableExposure),
        ("normal layer with bottom exposure", patch(key::EXPOSURE_TIME, back.job.bottom_layers as usize, back.job.bottom_exposure.to_string()), Rule::TableExposure),
        ("layer lift differs from job", patch(key::ZUP_HEIGHT, 0, (back.job.lift_height + 1.0).to_string()), Rule::TableLift),
    ]);
}

/// Check 6: a run stream must end at the screen's last pixel and agree with its scene area.
#[test]
fn the_archive_names_its_broken_run_stream() {
    let back = base();
    let entries = unpack(&back);
    let layer = format::layer_entry(0, format::RASTER_EXTENSION);
    let short = changed(&entries, &layer, |data| {
        // R8 §2.8: black and full runs take two bytes; intermediate greys take one.
        let (mut at, mut last) = (0, 0);
        while at < data.len() {
            last = at;
            at += if matches!(data[at] >> 4, 0 | 15) { 2 } else { 1 };
        }
        assert_eq!(at, data.len());
        data.truncate(last);
    });
    rejected([
        ("extra run past the screen", changed(&entries, &layer, |data| data.extend_from_slice(&[0, 1])), Rule::Runs),
        ("one run short", short, Rule::Runs),
        ("scene area two percent high with volume unchanged", changed(&entries, format::SCENE, |data| data[400..404].copy_from_slice(&(back.scene.records[0].area * 1.02).to_le_bytes())), Rule::LitArea),
    ]);
}

/// Check 8: the print's volume must agree with its layers, with cost kept consistent with volume.
#[test]
fn the_archive_names_its_broken_totals() {
    let back = base();
    let entries = unpack(&back);
    let volume = |factor: f64| changed(&entries, format::PRINT_INFO, |data| {
        scalar(data, format::print_info::VOLUME, 0, back.totals.volume_ml * factor);
        scalar(data, format::print_info::COST, 0, back.totals.cost * factor);
    });
    // the cost is checked to a cent past its rounding: on this small block a cent is far more
    // than 5 % of it, so the broken cost is off by a whole unit
    let cost = changed(&entries, format::PRINT_INFO, |data| scalar(data, format::print_info::COST, 0, back.totals.cost + 1.0));
    rejected([
        ("volume five percent high", volume(1.05), Rule::Volume),
        ("volume five percent low", volume(0.95), Rule::Volume),
        ("cost a unit off its volume", cost, Rule::Cost),
    ]);
}

/// The profile's archive range check rejects normal exposure above 120 seconds.
#[test]
fn an_archive_job_refuses_exposure_above_its_bound() {
    let mut job = printer([120, 80]);
    job.exposure = 120.1;
    let mut config = config_of(&[BUILTIN_PRINTER.1, BUILTIN_PROCESSES[1].1]);
    config.set_str("fab_sla_exposure", &job.exposure.to_string());
    let error = ResinProfile::on(&job.machine(), &config).expect_err("exposure above the archive bound");
    assert!(error.contains("fab_sla_exposure"), "{error}");
}
