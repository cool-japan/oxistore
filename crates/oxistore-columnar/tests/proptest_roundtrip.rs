/// Property-based round-trip tests for `oxistore-columnar` using `proptest`.
///
/// These tests generate random data for various column types, write them
/// through `ColumnarTable`, and assert exact value preservation on read-back.
use std::sync::Arc;

use oxistore_columnar::{
    Array, ColumnarTable, DataType, Field, Float32Array, Float64Array, Int32Array, Int64Array,
    RecordBatch, Schema, StringArray, UInt32Array, UInt64Array,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Int64 round-trip
// ---------------------------------------------------------------------------

proptest! {
    /// Random Int64 values survive write→read unchanged.
    #[test]
    fn prop_columnar_roundtrip_random_int64(
        values in proptest::collection::vec(any::<i64>(), 1..=256usize),
    ) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("v", DataType::Int64, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(Int64Array::from(values.clone()))],
        )
        .expect("batch");

        let mut table = ColumnarTable::new(Arc::clone(&schema));
        table.push(batch).expect("push");
        let bytes = table.write_to_bytes().expect("write_to_bytes");
        let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

        prop_assert_eq!(loaded.row_count(), values.len());
        let col = loaded.batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("Int64Array");
        for (i, &expected) in values.iter().enumerate() {
            prop_assert_eq!(col.value(i), expected, "mismatch at index {}", i);
        }
    }
}

// ---------------------------------------------------------------------------
// Int32 round-trip
// ---------------------------------------------------------------------------

proptest! {
    /// Random Int32 values survive write→read unchanged.
    #[test]
    fn prop_columnar_roundtrip_random_int32(
        values in proptest::collection::vec(any::<i32>(), 1..=256usize),
    ) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("v", DataType::Int32, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(Int32Array::from(values.clone()))],
        )
        .expect("batch");

        let mut table = ColumnarTable::new(Arc::clone(&schema));
        table.push(batch).expect("push");
        let bytes = table.write_to_bytes().expect("write_to_bytes");
        let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

        prop_assert_eq!(loaded.row_count(), values.len());
        let col = loaded.batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .expect("Int32Array");
        for (i, &expected) in values.iter().enumerate() {
            prop_assert_eq!(col.value(i), expected, "mismatch at index {}", i);
        }
    }
}

// ---------------------------------------------------------------------------
// UInt64 round-trip
// ---------------------------------------------------------------------------

proptest! {
    /// Random UInt64 values survive write→read unchanged.
    #[test]
    fn prop_columnar_roundtrip_random_uint64(
        values in proptest::collection::vec(any::<u64>(), 1..=128usize),
    ) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("v", DataType::UInt64, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(UInt64Array::from(values.clone()))],
        )
        .expect("batch");

        let mut table = ColumnarTable::new(Arc::clone(&schema));
        table.push(batch).expect("push");
        let bytes = table.write_to_bytes().expect("write_to_bytes");
        let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

        prop_assert_eq!(loaded.row_count(), values.len());
        let col = loaded.batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .expect("UInt64Array");
        for (i, &expected) in values.iter().enumerate() {
            prop_assert_eq!(col.value(i), expected, "mismatch at index {}", i);
        }
    }
}

// ---------------------------------------------------------------------------
// UInt32 round-trip
// ---------------------------------------------------------------------------

proptest! {
    /// Random UInt32 values survive write→read unchanged.
    #[test]
    fn prop_columnar_roundtrip_random_uint32(
        values in proptest::collection::vec(any::<u32>(), 1..=256usize),
    ) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("v", DataType::UInt32, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(UInt32Array::from(values.clone()))],
        )
        .expect("batch");

        let mut table = ColumnarTable::new(Arc::clone(&schema));
        table.push(batch).expect("push");
        let bytes = table.write_to_bytes().expect("write_to_bytes");
        let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

        prop_assert_eq!(loaded.row_count(), values.len());
        let col = loaded.batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .expect("UInt32Array");
        for (i, &expected) in values.iter().enumerate() {
            prop_assert_eq!(col.value(i), expected, "mismatch at index {}", i);
        }
    }
}

// ---------------------------------------------------------------------------
// Float64 round-trip (finite values only to avoid NaN != NaN weirdness)
// ---------------------------------------------------------------------------

proptest! {
    /// Random finite Float64 values survive write→read unchanged.
    #[test]
    fn prop_columnar_roundtrip_random_float64(
        values in proptest::collection::vec(
            proptest::num::f64::NORMAL
                | proptest::num::f64::POSITIVE
                | proptest::num::f64::NEGATIVE
                | proptest::num::f64::ZERO,
            1..=128usize,
        ),
    ) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("v", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(Float64Array::from(values.clone()))],
        )
        .expect("batch");

        let mut table = ColumnarTable::new(Arc::clone(&schema));
        table.push(batch).expect("push");
        let bytes = table.write_to_bytes().expect("write_to_bytes");
        let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

        prop_assert_eq!(loaded.row_count(), values.len());
        let col = loaded.batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("Float64Array");
        for (i, &expected) in values.iter().enumerate() {
            // Bitwise equality — both are the same finite float from Arrow.
            let got = col.value(i).to_bits();
            let exp = expected.to_bits();
            prop_assert_eq!(got, exp, "Float64 mismatch at index {}", i);
        }
    }
}

// ---------------------------------------------------------------------------
// Float32 round-trip
// ---------------------------------------------------------------------------

proptest! {
    /// Random finite Float32 values survive write→read unchanged.
    #[test]
    fn prop_columnar_roundtrip_random_float32(
        values in proptest::collection::vec(
            proptest::num::f32::NORMAL
                | proptest::num::f32::POSITIVE
                | proptest::num::f32::ZERO,
            1..=128usize,
        ),
    ) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("v", DataType::Float32, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(Float32Array::from(values.clone()))],
        )
        .expect("batch");

        let mut table = ColumnarTable::new(Arc::clone(&schema));
        table.push(batch).expect("push");
        let bytes = table.write_to_bytes().expect("write_to_bytes");
        let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

        prop_assert_eq!(loaded.row_count(), values.len());
        let col = loaded.batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Float32Array>()
            .expect("Float32Array");
        for (i, &expected) in values.iter().enumerate() {
            let got = col.value(i).to_bits();
            let exp = expected.to_bits();
            prop_assert_eq!(got, exp, "Float32 mismatch at index {}", i);
        }
    }
}

// ---------------------------------------------------------------------------
// String round-trip with nullable values
// ---------------------------------------------------------------------------

proptest! {
    /// Random nullable String values survive write→read unchanged.
    #[test]
    fn prop_columnar_roundtrip_random_strings(
        items in proptest::collection::vec(
            proptest::option::of("[a-z]{0,32}"),
            1..=128usize,
        ),
    ) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("s", DataType::Utf8, true),
        ]));
        let arr: StringArray = items
            .iter()
            .map(|opt| opt.as_deref())
            .collect::<Vec<Option<&str>>>()
            .into();

        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(arr)],
        )
        .expect("batch");

        let mut table = ColumnarTable::new(Arc::clone(&schema));
        table.push(batch).expect("push");
        let bytes = table.write_to_bytes().expect("write_to_bytes");
        let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

        prop_assert_eq!(loaded.row_count(), items.len());
        let col = loaded.batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("StringArray");
        for (i, expected_opt) in items.iter().enumerate() {
            match expected_opt {
                None => prop_assert!(col.is_null(i), "expected null at index {}", i),
                Some(expected) => {
                    prop_assert!(!col.is_null(i), "unexpected null at index {}", i);
                    prop_assert_eq!(col.value(i), expected.as_str(),
                        "string mismatch at index {}", i);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Multi-column batch round-trip
// ---------------------------------------------------------------------------

proptest! {
    /// Random multi-column batch (Int64 + Float64 + Bool) survives round-trip.
    #[test]
    fn prop_columnar_roundtrip_multi_column(
        int_vals in proptest::collection::vec(any::<i64>(), 1..=64usize),
        float_vals in proptest::collection::vec(
            proptest::num::f64::NORMAL | proptest::num::f64::ZERO,
            1..=64usize,
        ),
        bool_vals in proptest::collection::vec(any::<bool>(), 1..=64usize),
    ) {
        use arrow::array::BooleanArray;

        // Ensure all columns have the same length.
        let n = int_vals.len().min(float_vals.len()).min(bool_vals.len());
        let int_vals = &int_vals[..n];
        let float_vals = &float_vals[..n];
        let bool_vals = &bool_vals[..n];

        let schema = Arc::new(Schema::new(vec![
            Field::new("i", DataType::Int64, false),
            Field::new("f", DataType::Float64, false),
            Field::new("b", DataType::Boolean, false),
        ]));

        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(int_vals.to_vec())),
                Arc::new(Float64Array::from(float_vals.to_vec())),
                Arc::new(BooleanArray::from(bool_vals.to_vec())),
            ],
        )
        .expect("batch");

        let mut table = ColumnarTable::new(Arc::clone(&schema));
        table.push(batch).expect("push");
        let bytes = table.write_to_bytes().expect("write_to_bytes");
        let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

        prop_assert_eq!(loaded.row_count(), n);

        let i_col = loaded.batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("Int64Array");
        for (i, &expected) in int_vals.iter().enumerate() {
            prop_assert_eq!(i_col.value(i), expected);
        }

        let b_col = loaded.batches[0]
            .column(2)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("BooleanArray");
        for (i, &expected) in bool_vals.iter().enumerate() {
            prop_assert_eq!(b_col.value(i), expected);
        }
    }
}
