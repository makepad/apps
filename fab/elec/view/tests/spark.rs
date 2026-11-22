//! A spark's shared shadow reads the same columns as a direct reduction of the trace.
use fab_elec_view::trace::{
    columns::Columns,
    spark::{SparkShadow, SPARK_COLS},
    view::{ArraySource, TraceSource},
};
use std::sync::Arc;

#[test]
fn shadow_preserves_columns_and_range() {
    let t: Arc<[f64]> = (0..4096).map(|i| f64::from(i) * 0.001).collect();
    let square: Arc<[f32]> = (0..4096).map(|i| if i % 16 < 8 { -2.0 } else { 5.0 }).collect();
    let ramp: Arc<[f32]> = (0..4096).map(|i| i as f32 / 4095.0).collect();
    let source = ArraySource { t, ys: vec![square, ramp] };
    let window = (0.128, 3.968);

    for k in 0..source.traces() {
        let shadow = SparkShadow::read(&source, k, window);
        let expected = Columns::reduce(&source.t, &source.ys[k], window.0, window.1, SPARK_COLS);
        assert_eq!(shadow.k, k, "the shadow keeps its trace index");
        assert_eq!(shadow.columns.cols, SPARK_COLS, "trace {k} uses the fixed spark column count");
        assert_eq!(shadow.columns.cols_data.len(), SPARK_COLS as usize, "trace {k} fills every column of the dense window");
        assert_eq!(shadow.columns, expected, "trace {k} matches direct reduction over the same window");
        assert_eq!(shadow.range, source.range(k), "trace {k} keeps the source's whole-record range");
        if k == 0 {
            for (i, column) in shadow.columns.cols_data.iter().enumerate() {
                assert_eq!(column.lo, -2.0, "square-wave column {i} keeps the low level");
                assert_eq!(column.hi, 5.0, "square-wave column {i} keeps the high level");
            }
        }
    }
}
