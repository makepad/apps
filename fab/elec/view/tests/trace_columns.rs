use fab_elec_view::trace::columns::Columns;

#[test]
fn columns_reduce_exactly() {
    let t: Vec<f64> = (0..10_000).map(|i| i as f64 * 1e-6 + 0.3e-6 * (i as f64).sin()).collect();
    let y: Vec<f32> = t.iter().map(|time| (time * std::f64::consts::TAU * 3e3).sin() as f32).collect();
    let t0 = t[0];
    let t1 = t[t.len() - 1].next_up();
    let reduced = Columns::reduce(&t, &y, t0, t1, 200);
    assert_eq!(reduced.cols_data.iter().map(|c| u64::from(c.count)).sum::<u64>(), 10_000, "all samples counted once");
    assert!(reduced.before.is_none() && reduced.after.is_none(), "whole record has no outside neighbours");
    let mut previous = None;
    for column in &reduced.cols_data {
        let index = column.x0.floor() as u32;
        assert!(previous.is_none_or(|prev| prev < index), "columns are strictly ordered");
        assert!(column.x0 <= column.x1, "first sample precedes last sample");
        assert_eq!(column.x1.floor() as u32, index, "both endpoints remain inside the column");
        assert!(index < 200, "column stays inside the window");
        let samples: Vec<usize> = t.iter().enumerate()
            .filter(|(_, time)| (((**time - t0) / (t1 - t0) * 200.0).floor() as u32) == index)
            .map(|(i, _)| i).collect();
        assert_eq!(column.count as usize, samples.len(), "column count matches brute force");
        let lo = samples.iter().map(|&i| y[i]).fold(f32::INFINITY, f32::min);
        let hi = samples.iter().map(|&i| y[i]).fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(column.lo, lo, "column minimum is exact");
        assert_eq!(column.hi, hi, "column maximum is exact");
        assert_eq!(column.y0, y[samples[0]], "first sample value is exact");
        assert_eq!(column.y1, y[*samples.last().unwrap()], "last sample value is exact");
        previous = Some(index);
    }

    let start = (t[2000] + t[2001]) * 0.5;
    let end = t[7000];
    let inside = Columns::reduce(&t, &y, start, end, 200);
    let position = |time: f64| ((time - start) / (end - start) * 200.0) as f32;
    assert_eq!(inside.before, Some((position(t[2000]), y[2000])), "last sample strictly before the window");
    assert_eq!(inside.after, Some((position(t[7000]), y[7000])), "first sample at the excluded right edge");
    assert_eq!(inside.cols_data.iter().map(|c| c.count).sum::<u32>(), 4999, "interior window excludes both neighbours");

    let sparse = Columns::reduce(&t, &y, t[4000], t[4050], 400);
    assert!(sparse.density() < 1.0, "sparse means fewer samples than pixel columns");
    assert_eq!(sparse.cols_data.len(), 50, "all 50 samples have their own column");
    assert!(sparse.cols_data.iter().all(|c| c.count == 1), "sparse columns each hold one sample");

    let empty = Columns::reduce(&[], &[], 0.0, 1.0, 200);
    assert!(empty.cols_data.is_empty() && empty.before.is_none() && empty.after.is_none(), "empty record is empty");
    assert_eq!(empty.density(), 0.0, "empty density is zero");
    let outside = Columns::reduce(&t, &y, t1 + 1.0, t1 + 2.0, 200);
    assert!(outside.cols_data.is_empty(), "window beyond the record is empty");
    assert!(outside.before.is_some() && outside.after.is_none(), "outside window retains its preceding neighbour");
    let gap = Columns::reduce(&t, &y, t[50] + 0.1e-6, t[51], 200);
    assert!(gap.cols_data.is_empty() && gap.before.is_some() && gap.after.is_some(), "a gap retains both line endpoints");
}
