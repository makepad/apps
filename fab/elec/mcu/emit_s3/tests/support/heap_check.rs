//! Read-only TLSF checker shared by the host and Xtensa model tables.

use fab_elec_mcu_emit_s3::radio::kernel::{heap_control_bytes, HEAP_HEADER_BYTES};

// Layout source: src/radio/kernel/objects.splash. Keep these word offsets,
// tags and derived class/control sizes in step with that file. Check the
// magic/cookie before interpreting any block or free-list metadata.
const H_MAGIC: u32 = 0;
const H_FREE: u32 = 8;
const H_FL: u32 = 12;
const H_COUNT: u32 = 16;
const H_COOKIE: u32 = 20;
const H_TAG: u32 = 0x544c_5346;
const H_SL_COUNT: u32 = 8;
const B_SIZE: u32 = 0;
const B_PREV: u32 = 4;
const B_REQUEST: u32 = 8;
const B_HEADER: u32 = 16;
const B_NEXT_FREE: u32 = 16;
const B_PREV_FREE: u32 = 20;
const B_MIN: u32 = 32;

fn class_of(size: u32) -> u32 {
    let high = 31 - size.leading_zeros();
    (high - 5) * H_SL_COUNT + (size >> (high - 3)) - H_SL_COUNT
}

/// Check an initialized heap with a validated KernelLayout. First failure:
/// 1 magic/cookie; 2 physical size/bounds; 3 physical back-link; 4 adjacent
/// free blocks; 5 allocated request; 6 free totals; 7 bitmap/list agreement;
/// 8 list address; 9 list back-link/class; 10 membership/duplicate. Retain
/// the original terminal code 11 for an incomplete list walk/count.
/// No device memory is written, and no unvalidated list node is read.
pub fn heap_check(
    read32: impl Fn(u32) -> u32,
    _read8: impl Fn(u32) -> u8,
    heap_base: u32,
    heap_size: u32,
) -> u32 {
    // Persisted metadata is word-wide; the byte reader is part of the
    // shared memory-reader interface, but the scratch bitmap is Rust-owned.
    if read32(heap_base + H_MAGIC) != H_TAG || read32(heap_base + H_COOKIE) != !H_TAG { return 1; }
    let end = heap_base.checked_add(heap_size).expect("heap checker layout wraps");
    let fl_count = (31 - heap_size.leading_zeros()).saturating_sub(4).max(1);
    let classes = fl_count * H_SL_COUNT;
    let heads = HEAP_HEADER_BYTES + fl_count * 4;
    let control = heap_control_bytes(heap_size);
    if heap_size < control + B_MIN {
        if read32(heap_base + H_FREE) != 0 || read32(heap_base + H_COUNT) != 0 { return 6; }
        return 0;
    }
    let first = heap_base + control;
    let blocks = heap_size / B_MIN;
    // One bit per possible eight-aligned address. Only physical free block
    // starts are marked; consuming a mark proves unique list membership.
    let mut free = vec![0u8; (heap_size as usize + 63) / 64];
    let mut block = first;
    let mut prev = 0;
    let mut was_free = false;
    let mut count = 0;
    let mut bytes = 0;
    for _ in 0..blocks {
        if block == end { break; }
        if block > end - B_MIN { return 2; }
        let flags = read32(block + B_SIZE);
        let size = flags & !7;
        if flags & 6 != 0 || size < B_MIN || size > end - block { return 2; }
        if read32(block + B_PREV) != prev { return 3; }
        let is_free = flags & 1 != 0;
        if is_free {
            if was_free { return 4; }
            count += 1;
            bytes += size - B_HEADER;
            let bit = ((block - heap_base) / 8) as usize;
            free[bit / 8] |= 1 << (bit & 7);
        } else if read32(block + B_REQUEST) > size - B_HEADER { return 5; }
        was_free = is_free;
        prev = block;
        block += size;
    }
    if block != end { return 2; }
    if count != read32(heap_base + H_COUNT) || bytes != read32(heap_base + H_FREE) { return 6; }
    let flbits = read32(heap_base + H_FL);
    if flbits >> fl_count != 0 { return 7; }
    let mut index = 0;
    let mut node = 0;
    let mut last = 0;
    let mut seen = 0;
    // Match the original order: one iteration per class or per free node.
    // Even cyclic or out-of-region links cannot extend the walk's bound.
    for _ in 0..classes + blocks {
        if node == 0 {
            if index == classes { break; }
            let fl = index >> 3;
            let sl = index & 7;
            let bits = read32(heap_base + HEAP_HEADER_BYTES + fl * 4);
            if bits >> H_SL_COUNT != 0 { return 7; }
            if (flbits >> fl) & 1 != u32::from(bits != 0) { return 7; }
            node = read32(heap_base + heads + index * 4);
            if (bits >> sl) & 1 != u32::from(node != 0) { return 7; }
            last = 0;
            index += 1;
        } else {
            if node & 7 != 0 || node < first || node > end - B_MIN { return 8; }
            let bit = ((node - heap_base) / 8) as usize;
            let mask = 1 << (bit & 7);
            if free[bit / 8] & mask == 0 { return 10; }
            if read32(node + B_PREV_FREE) != last || class_of(read32(node + B_SIZE) & !7) != index - 1 { return 9; }
            free[bit / 8] &= !mask;
            seen += 1;
            last = node;
            node = read32(node + B_NEXT_FREE);
        }
    }
    if node != 0 || index != classes || seen != count { return 11; }
    0
}
