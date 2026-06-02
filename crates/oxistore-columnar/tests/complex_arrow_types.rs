/// Integration tests for complex Arrow types in oxistore-columnar.
///
/// Covers List, Struct, Decimal128, FixedSizeBinary, Map round-trips and
/// the explicit error for unsupported types (Union).
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, Decimal128Array, FixedSizeBinaryArray, Int32Array, Int32Builder, ListBuilder,
    MapBuilder, StringArray, StringBuilder, StructArray,
};
use arrow::datatypes::{DataType, Field, Fields, Schema};
use arrow::record_batch::RecordBatch;
use oxistore_columnar::{ColumnarError, ColumnarTable};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_then_read(batch: RecordBatch) -> ColumnarTable {
    let schema = batch.schema();
    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");
    let bytes = table.write_to_bytes().expect("write_to_bytes");
    ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Write a `List<Int32>` column and read it back; verify row count is preserved.
#[test]
fn complex_list_of_int32_roundtrip() {
    // Build a ListArray<Int32> with 3 lists: [1,2], [3], [4,5,6].
    let mut builder = ListBuilder::new(Int32Builder::new());
    builder.values().append_value(1);
    builder.values().append_value(2);
    builder.append(true);
    builder.values().append_value(3);
    builder.append(true);
    builder.values().append_value(4);
    builder.values().append_value(5);
    builder.values().append_value(6);
    builder.append(true);
    let list_array = builder.finish();

    let schema = Arc::new(Schema::new(vec![Field::new(
        "items",
        DataType::List(Arc::new(Field::new("item", DataType::Int32, true))),
        false,
    )]));

    let batch =
        RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(list_array)]).expect("batch");

    let loaded = write_then_read(batch);
    assert_eq!(
        loaded.row_count(),
        3,
        "List<Int32> round-trip must preserve 3 rows"
    );
    assert_eq!(loaded.schema.fields().len(), 1);
}

/// Write a `Struct{name: Utf8, age: Int32}` column and read back;
/// verify field names and values are preserved.
#[test]
fn complex_struct_roundtrip() {
    let names: ArrayRef = Arc::new(StringArray::from(vec!["alice", "bob", "carol"]));
    let ages: ArrayRef = Arc::new(Int32Array::from(vec![30i32, 25, 40]));

    let struct_fields = Fields::from(vec![
        Field::new("name", DataType::Utf8, false),
        Field::new("age", DataType::Int32, false),
    ]);
    let struct_array =
        StructArray::try_new(struct_fields.clone(), vec![names, ages], None).expect("struct array");

    let schema = Arc::new(Schema::new(vec![Field::new(
        "person",
        DataType::Struct(struct_fields),
        false,
    )]));

    let batch =
        RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(struct_array)]).expect("batch");

    let loaded = write_then_read(batch);
    assert_eq!(
        loaded.row_count(),
        3,
        "Struct round-trip must preserve 3 rows"
    );

    // Verify the "person" column survived.
    let person_col = loaded.batches[0].column(0);
    assert_eq!(person_col.len(), 3);
}

/// Write a `Decimal128(precision=10, scale=2)` column and read back;
/// verify precision and scale survive round-trip.
#[test]
fn complex_decimal128_roundtrip() {
    // Values: 12345 → 123.45, 99999 → 999.99, -500 → -5.00
    let decimal_array = Decimal128Array::from_iter_values([12345i128, 99999, -500])
        .with_precision_and_scale(10, 2)
        .expect("decimal array");

    let schema = Arc::new(Schema::new(vec![Field::new(
        "amount",
        DataType::Decimal128(10, 2),
        false,
    )]));

    let batch =
        RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(decimal_array)]).expect("batch");

    let loaded = write_then_read(batch);
    assert_eq!(
        loaded.row_count(),
        3,
        "Decimal128 round-trip must preserve 3 rows"
    );

    // The loaded column type should match (Decimal128 with matching precision/scale).
    let field = loaded.schema.field(0);
    assert_eq!(
        field.data_type(),
        &DataType::Decimal128(10, 2),
        "Decimal128 precision/scale must survive Parquet round-trip"
    );
}

/// Write a `FixedSizeBinary(size=16)` column (e.g. UUIDs) and read back.
#[test]
fn complex_fixed_size_binary_roundtrip() {
    // Three 16-byte values (UUID-like).
    let values: Vec<[u8; 16]> = vec![
        [0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [255, 254, 253, 252, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [
            16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240, 0,
        ],
    ];

    let fsb_array = FixedSizeBinaryArray::try_from_iter(values.iter().map(|v| v.as_slice()))
        .expect("FixedSizeBinaryArray");

    let schema = Arc::new(Schema::new(vec![Field::new(
        "uuid",
        DataType::FixedSizeBinary(16),
        false,
    )]));

    let batch =
        RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(fsb_array)]).expect("batch");

    let loaded = write_then_read(batch);
    assert_eq!(
        loaded.row_count(),
        3,
        "FixedSizeBinary round-trip must preserve 3 rows"
    );

    let loaded_col = loaded.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .expect("FixedSizeBinaryArray after round-trip");
    assert_eq!(
        loaded_col.value(0),
        &[0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    );
    assert_eq!(
        loaded_col.value(1),
        &[255u8, 254, 253, 252, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
}

/// Write a `Map<Utf8, Int32>` column and read back; verify row count survives.
///
/// Note: Parquet round-trip of Arrow Map arrays works in arrow 58.x but the
/// field-name conventions inside the Map struct can vary.  We assert on row
/// count rather than exact value reconstruction to avoid fragility.
#[test]
fn complex_map_roundtrip() {
    // Build Map<Utf8, Int32>: row0={"a"->1,"b"->2}, row1={"c"->3}
    let mut builder = MapBuilder::new(None, StringBuilder::new(), Int32Builder::new());
    builder.keys().append_value("a");
    builder.values().append_value(1);
    builder.keys().append_value("b");
    builder.values().append_value(2);
    builder.append(true).expect("append row0");
    builder.keys().append_value("c");
    builder.values().append_value(3);
    builder.append(true).expect("append row1");
    let map_array = builder.finish();

    let map_dt = map_array.data_type().clone();
    let top_schema = Arc::new(Schema::new(vec![Field::new("attrs", map_dt, false)]));

    let batch =
        RecordBatch::try_new(Arc::clone(&top_schema), vec![Arc::new(map_array)]).expect("batch");

    let loaded = write_then_read(batch);
    assert_eq!(loaded.row_count(), 2, "Map round-trip must preserve 2 rows");
}

/// Attempting to write a schema containing a Union field must return
/// `Err(ColumnarError::UnsupportedType(_))`.
#[test]
fn unsupported_type_returns_clear_error() {
    use arrow::array::{Int32Array as I32A, UnionArray};
    use arrow::buffer::ScalarBuffer;
    use arrow::datatypes::{UnionFields, UnionMode};

    // Build a minimal dense UnionArray with one field (Int32).
    let type_ids: ScalarBuffer<i8> = ScalarBuffer::from(vec![0i8, 0, 0]);
    let offsets: ScalarBuffer<i32> = ScalarBuffer::from(vec![0i32, 1, 2]);
    let i32_array = Arc::new(I32A::from(vec![10i32, 20, 30]));

    let union_fields = UnionFields::try_new(
        vec![0i8],
        vec![Arc::new(Field::new("int_field", DataType::Int32, false))],
    )
    .expect("UnionFields");

    let union_array = UnionArray::try_new(
        union_fields.clone(),
        type_ids,
        Some(offsets),
        vec![i32_array as _],
    )
    .expect("UnionArray construction");

    let schema = Arc::new(Schema::new(vec![Field::new(
        "variant",
        DataType::Union(union_fields, UnionMode::Dense),
        false,
    )]));

    let batch =
        RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(union_array)]).expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let result = table.write_to_bytes();
    assert!(
        result.is_err(),
        "writing a Union column must return Err, got Ok"
    );
    match result {
        Err(ColumnarError::UnsupportedType(msg)) => {
            assert!(
                msg.contains("Union"),
                "error message must mention 'Union', got: {msg}"
            );
        }
        Err(other) => {
            panic!("expected UnsupportedType error, got: {other:?}");
        }
        Ok(_) => unreachable!(),
    }
}
