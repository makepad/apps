//! Pure answer packing: breakpoints, count reservation and lossless wrapping.

use fab_elec_view::instruments::scope::{layout_answers, AnswerWidthClass};

fn row_width(widths: &[f64]) -> f64 {
    widths.iter().sum::<f64>() + widths.len().saturating_sub(1) as f64 * 16.0
}

#[test]
fn width_classes_and_counts_preserve_whole_answers() {
    use AnswerWidthClass::{Medium, Narrow, Wide};
    for (width, class) in [(519.99, Narrow), (520.0, Medium), (899.99, Medium), (900.0, Wide)] {
        assert_eq!(AnswerWidthClass::from_width(width), class, "the lane's own width selects its class");
    }
    let widths = [80.0; 4];
    let full = layout_answers(&widths, 368.0, Wide, false, 20.0);
    assert_eq!((full.head, full.more, full.chip), (4, 0, false), "an exact full row needs no count");
    let wide = layout_answers(&widths, 308.0, Wide, false, 20.0);
    assert_eq!((wide.head, wide.more, wide.chip), (3, 1, false), "wide reserves only plain count text");
    let medium = layout_answers(&widths, 308.0, Medium, false, 20.0);
    assert_eq!((medium.head, medium.more, medium.chip), (2, 2, true), "chip padding can move another whole answer off the head");
    let edge = layout_answers(&widths, 320.0, Medium, false, 20.0);
    assert_eq!((edge.head, edge.more, edge.chip), (3, 1, true), "the chip fits exactly after the third answer");
    let roomy = layout_answers(&widths, 1000.0, Medium, false, 20.0);
    assert_eq!((roomy.head, roomy.more, roomy.chip), (3, 1, true), "medium never exceeds three answers");
    let three = layout_answers(&widths[..3], 272.0, Medium, false, 20.0);
    assert_eq!((three.head, three.more, three.chip), (3, 0, false), "no chip is reserved when every answer fits");
    for class in [Wide, Medium] {
        for room in [0.0, 70.0, 111.0, 112.0, 212.0, 308.0, 320.0, 368.0, 1000.0] {
            let layout = layout_answers(&widths, room, class, false, 20.0);
            assert_eq!(layout.head + layout.more, widths.len(), "the count accounts for every omitted answer");
            assert!(layout.rows.is_empty(), "only narrow expanded lanes wrap");
            assert_eq!(layout.chip, class == Medium && layout.more > 0, "only a nonempty medium count is a chip");
            assert!(class != Medium || layout.head <= 3, "medium's answer cap survives every fit boundary");
            if layout.head > 0 {
                let count_width = if layout.more > 0 { 16.0 + 20.0 + if layout.chip { 12.0 } else { 0.0 } } else { 0.0 };
                assert!(row_width(&widths[..layout.head]) + count_width <= room, "whole answers leave room for the entire count");
            }
        }
    }
}

#[test]
fn narrow_rows_partition_the_answers_in_order() {
    let widths = [80.0, 110.0, 190.0, 20.0, 60.0, 390.0, 40.0];
    let packed = layout_answers(&widths, 200.0, AnswerWidthClass::Narrow, false, 20.0);
    assert_eq!(packed.head, 1, "head answers need no room for a count");
    assert_eq!(packed.rows, [1..2, 2..3, 3..5, 5..6, 6..7], "each extra row packs the next whole answers");
    for room in [0.0, 16.0, 80.0, 190.0, 200.0, 1000.0] {
        let layout = layout_answers(&widths, room, AnswerWidthClass::Narrow, false, 20.0);
        assert_eq!((layout.more, layout.chip), (0, false), "narrow overflow is always visible below the plot");
        assert!(row_width(&widths[..layout.head]) <= room, "the head contains only answers that fit");
        let mut seen: Vec<_> = (0..layout.head).collect();
        for row in &layout.rows {
            assert!(!row.is_empty(), "wrapping never adds an empty row");
            assert!(row.len() == 1 || row_width(&widths[row.clone()]) <= room, "only a single oversized answer may exceed the row's room");
            if row.end < widths.len() {
                assert!(row_width(&widths[row.start..row.end + 1]) > room, "a row never leaves room for its next answer");
            }
            seen.extend(row.clone());
        }
        assert_eq!(seen, (0..widths.len()).collect::<Vec<_>>(), "every answer appears exactly once and in source order");
    }
}

#[test]
fn meters_keep_the_chip_at_every_width_and_empty_lanes_stay_empty() {
    for class in [AnswerWidthClass::Wide, AnswerWidthClass::Medium, AnswerWidthClass::Narrow] {
        let meter = layout_answers(&[80.0; 4], 308.0, class, true, 20.0);
        assert_eq!((meter.head, meter.more, meter.chip), (2, 2, true), "a collapsed lane always reserves the chip's padding");
        assert!(meter.rows.is_empty(), "a meter never grows extra rows");
        let roomy = layout_answers(&[80.0; 4], 368.0, class, true, 20.0);
        assert_eq!((roomy.head, roomy.more, roomy.chip), (4, 0, false), "meters use every available answer slot, including in medium");
        let tiny = layout_answers(&[80.0; 4], 32.0, class, true, 20.0);
        assert_eq!((tiny.head, tiny.more, tiny.chip), (0, 4, true), "the chip still opens all answers when no answer fits");
        for collapsed in [false, true] {
            let empty = layout_answers(&[], 0.0, class, collapsed, 20.0);
            assert_eq!((empty.head, empty.more, empty.chip), (0, 0, false), "an empty lane has no count");
            assert!(empty.rows.is_empty(), "an empty lane takes no extra rows");
        }
    }
}
