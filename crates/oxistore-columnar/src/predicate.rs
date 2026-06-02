use arrow::array::{Array, Scalar as ArrowScalar};
/// Predicate AST and row-group pruning engine for Parquet column statistics.
///
/// This module implements a predicate algebra that can be evaluated against
/// Parquet row-group statistics to skip entire row groups that provably cannot
/// satisfy the predicate.  The engine is conservative: when uncertain (no
/// statistics, type mismatch, `Not`) it always keeps the row group.
///
/// # Design principles
///
/// - **Interval arithmetic only**: min/max statistics are used to determine
///   whether the row group _could_ contain a value satisfying the predicate.
/// - **No complement logic for `Not`**: negation of a range is not computed;
///   the engine always keeps groups guarded by `Not`.
/// - **Type-safe dispatch**: each `Scalar` variant maps to exactly one
///   `Statistics` variant; mismatches result in keeping the group.
use arrow::array::{BooleanArray, Float32Array, Float64Array, Int32Array, Int64Array, StringArray};
use arrow::compute::kernels::cmp as arrow_cmp;
use parquet::data_type::ByteArray;
use parquet::file::metadata::RowGroupMetaData;
use parquet::file::statistics::Statistics;
use parquet::schema::types::SchemaDescriptor;

use crate::ColumnarError;

/// A scalar value used in predicate comparisons.
///
/// `Float32` stores `f32` directly.  NaN is never equal to anything in
/// Parquet statistics so NaN predicates will always evaluate conservatively
/// (keep the row group).
#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    /// Boolean scalar.
    Bool(bool),
    /// 32-bit signed integer scalar.
    Int32(i32),
    /// 64-bit signed integer scalar.
    Int64(i64),
    /// 32-bit floating-point scalar.
    Float32(f32),
    /// 64-bit floating-point scalar.
    Float64(f64),
    /// Raw byte string scalar (compared lexicographically against Parquet
    /// `BYTE_ARRAY` statistics).
    Bytes(Vec<u8>),
    /// The SQL NULL scalar.  Any comparison involving Null keeps the group.
    Null,
}

/// A comparison operator used in leaf predicates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CmpOp {
    /// Equal (`=`).
    Eq,
    /// Not equal (`<>`).
    Ne,
    /// Strictly less than (`<`).
    Lt,
    /// Less than or equal (`<=`).
    Le,
    /// Strictly greater than (`>`).
    Gt,
    /// Greater than or equal (`>=`).
    Ge,
}

/// A predicate that can be pushed down to the Parquet row-group level.
///
/// Predicates are evaluated against column-chunk statistics (min / max /
/// null count) to determine whether a row group _might_ contain any rows
/// that satisfy the predicate.  Returning `false` from
/// [`Predicate::row_group_might_match`] means the row group can be skipped
/// entirely; returning `true` means the row group must be read (it may or
/// may not actually contain matching rows — further evaluation at the row
/// level is required).
#[derive(Clone, Debug)]
pub enum Predicate {
    /// A single column comparison: `column op value`.
    Cmp {
        /// Column name (must match a leaf column name in the Parquet schema).
        column: String,
        /// Comparison operator.
        op: CmpOp,
        /// Right-hand side scalar value.
        value: Scalar,
    },
    /// Logical conjunction: all sub-predicates must be satisfiable.
    And(Vec<Predicate>),
    /// Logical disjunction: at least one sub-predicate must be satisfiable.
    Or(Vec<Predicate>),
    /// Logical negation.  Conservative: always keeps the row group (no
    /// complement-range computation is performed).
    Not(Box<Predicate>),
    /// The tautology — always matches every row group.
    All,
    /// The contradiction — never matches any row group.  Useful as the
    /// identity element for `Or` construction.
    None,
}

impl Predicate {
    /// Evaluate this predicate against all rows in a [`arrow::record_batch::RecordBatch`],
    /// returning a [`BooleanArray`] mask.
    ///
    /// A `true` element in the mask means the corresponding row satisfies the
    /// predicate.  `Predicate::All` always returns all-true; `Predicate::None`
    /// always returns all-false.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError::SchemaMismatch`] if a column named in the
    /// predicate is missing from the batch, or if the scalar type is
    /// incompatible with the column type.  Returns [`ColumnarError::Arrow`] on
    /// Arrow compute errors.
    pub fn evaluate_batch(
        &self,
        batch: &arrow::record_batch::RecordBatch,
    ) -> Result<BooleanArray, ColumnarError> {
        match self {
            Predicate::All => Ok(BooleanArray::from(vec![true; batch.num_rows()])),
            Predicate::None => Ok(BooleanArray::from(vec![false; batch.num_rows()])),
            Predicate::Cmp { column, op, value } => {
                let col_idx = batch.schema().index_of(column).map_err(|_| {
                    ColumnarError::SchemaMismatch(format!("column '{}' not found in batch", column))
                })?;
                let col = batch.column(col_idx);
                evaluate_cmp_column(col.as_ref(), op, value)
            }
            Predicate::And(preds) => {
                let mut result = BooleanArray::from(vec![true; batch.num_rows()]);
                for pred in preds {
                    let mask = pred.evaluate_batch(batch)?;
                    result = arrow::compute::and(&result, &mask).map_err(ColumnarError::Arrow)?;
                }
                Ok(result)
            }
            Predicate::Or(preds) => {
                let mut result = BooleanArray::from(vec![false; batch.num_rows()]);
                for pred in preds {
                    let mask = pred.evaluate_batch(batch)?;
                    result = arrow::compute::or(&result, &mask).map_err(ColumnarError::Arrow)?;
                }
                Ok(result)
            }
            Predicate::Not(pred) => {
                let mask = pred.evaluate_batch(batch)?;
                arrow::compute::not(&mask).map_err(ColumnarError::Arrow)
            }
        }
    }

    /// Returns `true` if the predicate _might_ be satisfied by some row in
    /// this row group, or `false` if statistics _prove_ that no row can match.
    ///
    /// # Conservatism
    ///
    /// The engine always keeps the row group when:
    /// - The column is not found in the schema.
    /// - No statistics are present for the column chunk.
    /// - The `Scalar` type does not match the statistics type.
    /// - The predicate is `Not(_)`.
    /// - The `Scalar` is `Null`.
    pub fn row_group_might_match(&self, rg: &RowGroupMetaData, schema: &SchemaDescriptor) -> bool {
        match self {
            Predicate::All => true,
            Predicate::None => false,
            Predicate::Not(_) => true,
            Predicate::And(preds) => preds.iter().all(|p| p.row_group_might_match(rg, schema)),
            Predicate::Or(preds) => preds.iter().any(|p| p.row_group_might_match(rg, schema)),
            Predicate::Cmp { column, op, value } => cmp_might_match(rg, schema, column, op, value),
        }
    }
}

/// Evaluate a column-level comparison against each row, returning a boolean mask.
///
/// The column type and scalar type must match; a type mismatch returns
/// [`ColumnarError::SchemaMismatch`].
fn evaluate_cmp_column(
    col: &dyn Array,
    op: &CmpOp,
    value: &Scalar,
) -> Result<BooleanArray, ColumnarError> {
    match value {
        Scalar::Int32(v) => {
            let arr = col.as_any().downcast_ref::<Int32Array>().ok_or_else(|| {
                ColumnarError::SchemaMismatch("type mismatch: expected Int32".into())
            })?;
            let rhs = ArrowScalar::new(Int32Array::from(vec![*v]));
            apply_cmp_i32(arr, op, &rhs)
        }
        Scalar::Int64(v) => {
            let arr = col.as_any().downcast_ref::<Int64Array>().ok_or_else(|| {
                ColumnarError::SchemaMismatch("type mismatch: expected Int64".into())
            })?;
            let rhs = ArrowScalar::new(Int64Array::from(vec![*v]));
            apply_cmp_i64(arr, op, &rhs)
        }
        Scalar::Float32(v) => {
            let arr = col.as_any().downcast_ref::<Float32Array>().ok_or_else(|| {
                ColumnarError::SchemaMismatch("type mismatch: expected Float32".into())
            })?;
            let rhs = ArrowScalar::new(Float32Array::from(vec![*v]));
            apply_cmp_f32(arr, op, &rhs)
        }
        Scalar::Float64(v) => {
            let arr = col.as_any().downcast_ref::<Float64Array>().ok_or_else(|| {
                ColumnarError::SchemaMismatch("type mismatch: expected Float64".into())
            })?;
            let rhs = ArrowScalar::new(Float64Array::from(vec![*v]));
            apply_cmp_f64(arr, op, &rhs)
        }
        Scalar::Bytes(v) => {
            // StringArray is the Arrow Utf8 type; raw bytes must be valid UTF-8.
            let arr = col.as_any().downcast_ref::<StringArray>().ok_or_else(|| {
                ColumnarError::SchemaMismatch(
                    "type mismatch: expected Utf8 (StringArray) for Bytes comparison".into(),
                )
            })?;
            let s = std::str::from_utf8(v).map_err(|_| {
                ColumnarError::SchemaMismatch(
                    "Bytes scalar is not valid UTF-8; cannot compare against Utf8 column".into(),
                )
            })?;
            let rhs = ArrowScalar::new(StringArray::from(vec![s]));
            apply_cmp_str(arr, op, &rhs)
        }
        Scalar::Bool(_) | Scalar::Null => Err(ColumnarError::SchemaMismatch(format!(
            "unsupported scalar type for row-level filter: {value:?}",
        ))),
    }
}

/// Apply a comparison op to an Int32 column and a scalar Datum.
fn apply_cmp_i32(
    col: &Int32Array,
    op: &CmpOp,
    rhs: &ArrowScalar<Int32Array>,
) -> Result<BooleanArray, ColumnarError> {
    match op {
        CmpOp::Eq => arrow_cmp::eq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Ne => arrow_cmp::neq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Lt => arrow_cmp::lt(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Le => arrow_cmp::lt_eq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Gt => arrow_cmp::gt(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Ge => arrow_cmp::gt_eq(col, rhs).map_err(ColumnarError::Arrow),
    }
}

/// Apply a comparison op to an Int64 column and a scalar Datum.
fn apply_cmp_i64(
    col: &Int64Array,
    op: &CmpOp,
    rhs: &ArrowScalar<Int64Array>,
) -> Result<BooleanArray, ColumnarError> {
    match op {
        CmpOp::Eq => arrow_cmp::eq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Ne => arrow_cmp::neq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Lt => arrow_cmp::lt(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Le => arrow_cmp::lt_eq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Gt => arrow_cmp::gt(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Ge => arrow_cmp::gt_eq(col, rhs).map_err(ColumnarError::Arrow),
    }
}

/// Apply a comparison op to a Float32 column and a scalar Datum.
fn apply_cmp_f32(
    col: &Float32Array,
    op: &CmpOp,
    rhs: &ArrowScalar<Float32Array>,
) -> Result<BooleanArray, ColumnarError> {
    match op {
        CmpOp::Eq => arrow_cmp::eq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Ne => arrow_cmp::neq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Lt => arrow_cmp::lt(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Le => arrow_cmp::lt_eq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Gt => arrow_cmp::gt(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Ge => arrow_cmp::gt_eq(col, rhs).map_err(ColumnarError::Arrow),
    }
}

/// Apply a comparison op to a Float64 column and a scalar Datum.
fn apply_cmp_f64(
    col: &Float64Array,
    op: &CmpOp,
    rhs: &ArrowScalar<Float64Array>,
) -> Result<BooleanArray, ColumnarError> {
    match op {
        CmpOp::Eq => arrow_cmp::eq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Ne => arrow_cmp::neq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Lt => arrow_cmp::lt(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Le => arrow_cmp::lt_eq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Gt => arrow_cmp::gt(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Ge => arrow_cmp::gt_eq(col, rhs).map_err(ColumnarError::Arrow),
    }
}

/// Apply a comparison op to a StringArray column and a scalar Datum.
fn apply_cmp_str(
    col: &StringArray,
    op: &CmpOp,
    rhs: &ArrowScalar<StringArray>,
) -> Result<BooleanArray, ColumnarError> {
    match op {
        CmpOp::Eq => arrow_cmp::eq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Ne => arrow_cmp::neq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Lt => arrow_cmp::lt(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Le => arrow_cmp::lt_eq(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Gt => arrow_cmp::gt(col, rhs).map_err(ColumnarError::Arrow),
        CmpOp::Ge => arrow_cmp::gt_eq(col, rhs).map_err(ColumnarError::Arrow),
    }
}

/// Evaluate a leaf comparison predicate against row-group statistics.
///
/// Returns `true` (keep) when uncertain; `false` (skip) only when statistics
/// prove impossibility.
fn cmp_might_match(
    rg: &RowGroupMetaData,
    schema: &SchemaDescriptor,
    column: &str,
    op: &CmpOp,
    value: &Scalar,
) -> bool {
    // Conservative: Null rhs never prunes.
    if matches!(value, Scalar::Null) {
        return true;
    }

    // Find the leaf column index by name.
    let col_idx = match schema.columns().iter().position(|c| c.name() == column) {
        Some(idx) => idx,
        Option::None => return true, // unknown column — keep
    };

    // Bounds-check against the row group (schemas may differ).
    if col_idx >= rg.num_columns() {
        return true;
    }

    // Get column chunk statistics.
    let stats = match rg.column(col_idx).statistics() {
        Some(s) => s,
        Option::None => return true, // no statistics — keep
    };

    // Dispatch to type-specific evaluation.
    evaluate_cmp(stats, op, value)
}

/// Dispatch a comparison to the correct typed statistics handler.
fn evaluate_cmp(stats: &Statistics, op: &CmpOp, value: &Scalar) -> bool {
    match (stats, value) {
        (Statistics::Boolean(s), Scalar::Bool(v)) => {
            let (min, max) = match (s.min_opt(), s.max_opt()) {
                (Some(mn), Some(mx)) => (*mn, *mx),
                _ => return true,
            };
            apply_bool_op(op, min, max, *v)
        }
        (Statistics::Int32(s), Scalar::Int32(v)) => {
            let (min, max) = match (s.min_opt(), s.max_opt()) {
                (Some(mn), Some(mx)) => (*mn, *mx),
                _ => return true,
            };
            apply_ord_op(op, min, max, *v, s.null_count_opt())
        }
        (Statistics::Int64(s), Scalar::Int64(v)) => {
            let (min, max) = match (s.min_opt(), s.max_opt()) {
                (Some(mn), Some(mx)) => (*mn, *mx),
                _ => return true,
            };
            apply_ord_op(op, min, max, *v, s.null_count_opt())
        }
        (Statistics::Float(s), Scalar::Float32(v)) => {
            let (min, max) = match (s.min_opt(), s.max_opt()) {
                (Some(mn), Some(mx)) => (*mn, *mx),
                _ => return true,
            };
            apply_float_op_f32(op, min, max, *v, s.null_count_opt())
        }
        (Statistics::Double(s), Scalar::Float64(v)) => {
            let (min, max) = match (s.min_opt(), s.max_opt()) {
                (Some(mn), Some(mx)) => (*mn, *mx),
                _ => return true,
            };
            apply_float_op_f64(op, min, max, *v, s.null_count_opt())
        }
        (Statistics::ByteArray(s), Scalar::Bytes(v)) => {
            let (min, max) = match (s.min_opt(), s.max_opt()) {
                (Some(mn), Some(mx)) => (mn, mx),
                _ => return true,
            };
            apply_bytes_op(op, min, max, v.as_slice(), s.null_count_opt())
        }
        // Type mismatch or unsupported combination — keep conservatively.
        _ => true,
    }
}

/// Apply a comparison operator to boolean min/max statistics.
///
/// Booleans have only two possible values so the interval is just the pair
/// `(min, max)`.  Note: `Ne` on booleans is always kept conservatively
/// because we do not have null-count available in this path.
fn apply_bool_op(op: &CmpOp, min: bool, max: bool, v: bool) -> bool {
    match op {
        // min <= v && v <= max for booleans, rewritten to avoid bool_comparison:
        // v must be at least min (i.e., !min || v) and at most max (i.e., !v || max)
        CmpOp::Eq => (!min || v) && (!v || max),
        // Conservatively keep — we have no null-count available here.
        CmpOp::Ne => true,
        // min < v  for booleans means min=false and v=true.
        CmpOp::Lt => !min && v,
        // min <= v means v=true OR min=false.
        CmpOp::Le => !min || v,
        // max > v  for booleans means max=true and v=false.
        CmpOp::Gt => max && !v,
        // max >= v means max=true OR v=false.
        CmpOp::Ge => max || !v,
    }
}

/// Apply a comparison operator to ordered (Ord) min/max statistics.
///
/// Generic over any `PartialOrd + PartialEq` value type (i32, i64).
fn apply_ord_op<T: PartialOrd + PartialEq>(
    op: &CmpOp,
    min: T,
    max: T,
    v: T,
    null_count: Option<u64>,
) -> bool {
    match op {
        CmpOp::Eq => min <= v && v <= max,
        CmpOp::Ne => {
            // Skip ONLY if every value in the chunk equals v AND there are no nulls.
            let all_same = min == v && max == v;
            let no_nulls = null_count == Some(0);
            !(all_same && no_nulls)
        }
        CmpOp::Lt => min < v,
        CmpOp::Le => min <= v,
        CmpOp::Gt => max > v,
        CmpOp::Ge => max >= v,
    }
}

/// Apply a comparison operator to f32 min/max statistics.
///
/// NaN comparisons always keep the row group (NaN is not ordered).
fn apply_float_op_f32(op: &CmpOp, min: f32, max: f32, v: f32, null_count: Option<u64>) -> bool {
    // NaN predicates are not prunable — keep.
    if v.is_nan() || min.is_nan() || max.is_nan() {
        return true;
    }
    match op {
        CmpOp::Eq => min <= v && v <= max,
        CmpOp::Ne => {
            let all_same = (min - v).abs() < f32::EPSILON && (max - v).abs() < f32::EPSILON;
            let no_nulls = null_count == Some(0);
            !(all_same && no_nulls)
        }
        CmpOp::Lt => min < v,
        CmpOp::Le => min <= v,
        CmpOp::Gt => max > v,
        CmpOp::Ge => max >= v,
    }
}

/// Apply a comparison operator to f64 min/max statistics.
fn apply_float_op_f64(op: &CmpOp, min: f64, max: f64, v: f64, null_count: Option<u64>) -> bool {
    if v.is_nan() || min.is_nan() || max.is_nan() {
        return true;
    }
    match op {
        CmpOp::Eq => min <= v && v <= max,
        CmpOp::Ne => {
            let all_same = (min - v).abs() < f64::EPSILON && (max - v).abs() < f64::EPSILON;
            let no_nulls = null_count == Some(0);
            !(all_same && no_nulls)
        }
        CmpOp::Lt => min < v,
        CmpOp::Le => min <= v,
        CmpOp::Gt => max > v,
        CmpOp::Ge => max >= v,
    }
}

/// Apply a comparison operator to ByteArray min/max statistics (lexicographic).
fn apply_bytes_op(
    op: &CmpOp,
    min: &ByteArray,
    max: &ByteArray,
    v: &[u8],
    null_count: Option<u64>,
) -> bool {
    let min_data = min.data();
    let max_data = max.data();
    match op {
        CmpOp::Eq => min_data <= v && v <= max_data,
        CmpOp::Ne => {
            let all_same = min_data == v && max_data == v;
            let no_nulls = null_count == Some(0);
            !(all_same && no_nulls)
        }
        CmpOp::Lt => min_data < v,
        CmpOp::Le => min_data <= v,
        CmpOp::Gt => max_data > v,
        CmpOp::Ge => max_data >= v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_all_and_none() {
        // Trivial predicates need no schema — just verify compile + logic.
        // We can't easily construct RowGroupMetaData in unit tests without a
        // full builder chain, so we test the leaf logic via evaluate_cmp.
        let stats = Statistics::int32(Some(10), Some(20), None, Some(0), false);
        // Eq: value in range
        assert!(evaluate_cmp(&stats, &CmpOp::Eq, &Scalar::Int32(15)));
        // Eq: value below range
        assert!(!evaluate_cmp(&stats, &CmpOp::Eq, &Scalar::Int32(5)));
        // Gt: max > v
        assert!(evaluate_cmp(&stats, &CmpOp::Gt, &Scalar::Int32(15)));
        // Gt: max not > v
        assert!(!evaluate_cmp(&stats, &CmpOp::Gt, &Scalar::Int32(25)));
        // Lt: min < v
        assert!(evaluate_cmp(&stats, &CmpOp::Lt, &Scalar::Int32(15)));
        // Lt: min not < v
        assert!(!evaluate_cmp(&stats, &CmpOp::Lt, &Scalar::Int32(5)));
    }

    #[test]
    fn ne_pruning_requires_no_nulls() {
        let stats_no_nulls = Statistics::int32(Some(10), Some(10), None, Some(0), false);
        // All values equal 10, no nulls → prune when Ne 10
        assert!(!evaluate_cmp(
            &stats_no_nulls,
            &CmpOp::Ne,
            &Scalar::Int32(10)
        ));
        // Keep when Ne 5 (10 != 5 is possible)
        assert!(evaluate_cmp(&stats_no_nulls, &CmpOp::Ne, &Scalar::Int32(5)));

        let stats_with_nulls = Statistics::int32(Some(10), Some(10), None, Some(1), false);
        // Same range but has nulls → keep (Ne might succeed where null is present)
        assert!(evaluate_cmp(
            &stats_with_nulls,
            &CmpOp::Ne,
            &Scalar::Int32(10)
        ));
    }

    #[test]
    fn type_mismatch_keeps_group() {
        let stats = Statistics::int32(Some(10), Some(20), None, Some(0), false);
        // Int64 scalar against Int32 statistics → type mismatch → keep
        assert!(evaluate_cmp(&stats, &CmpOp::Eq, &Scalar::Int64(15)));
    }

    #[test]
    fn null_scalar_keeps_group() {
        // Null scalar never prunes — test via a fake stats set.
        let stats = Statistics::int32(Some(10), Some(20), None, Some(0), false);
        assert!(evaluate_cmp(&stats, &CmpOp::Eq, &Scalar::Null));
    }

    #[test]
    fn ge_boundary_keeps_group_at_max() {
        // max = 100, query Ge 100 → 100 >= 100 → keep
        let stats = Statistics::int64(Some(1), Some(100), None, Some(0), false);
        assert!(evaluate_cmp(&stats, &CmpOp::Ge, &Scalar::Int64(100)));
        // Ge 101 → max 100 < 101 → prune
        assert!(!evaluate_cmp(&stats, &CmpOp::Ge, &Scalar::Int64(101)));
    }
}
