//! Property-based tests asserting AAD collision resistance for `CellId` and
//! `derive_cell_id`.
//!
//! Two independent security properties are verified:
//!
//! 1. **`derive_cell_id` collision resistance** — BLAKE3 of distinct byte
//!    sequences must produce distinct 32-byte cell IDs.  This is the actual
//!    AAD path used by `EncryptedKv::put` / `get`.
//!
//! 2. **`CellId::to_aad_bytes` injectivity** — the 20-byte little-endian
//!    packing of `(table_id, row_id, col_id)` must be injective over distinct
//!    triples.  Collisions here would allow ciphertext transplant attacks
//!    via the explicit `put_cell` / `get_cell` API.
//!
//! Both properties hold by construction, but the proptest confirms they hold
//! across a large random sample of inputs.

#![allow(unexpected_cfgs)]

use oxistore_encrypt::{derive_cell_id, CellId};
use proptest::prelude::*;

// ── Helper: generate an arbitrary CellId ─────────────────────────────────────

fn arb_cell_id() -> impl Strategy<Value = CellId> {
    (any::<u64>(), any::<u64>(), any::<u32>()).prop_map(|(table_id, row_id, col_id)| CellId {
        table_id,
        row_id,
        col_id,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(200))]

    /// `derive_cell_id` (BLAKE3) must produce distinct outputs for distinct
    /// raw key byte sequences.
    ///
    /// This covers the main `EncryptedKv::put` / `get` AAD path.
    #[test]
    fn derive_cell_id_distinct_for_distinct_keys(
        key1 in proptest::collection::vec(any::<u8>(), 0..64usize),
        key2 in proptest::collection::vec(any::<u8>(), 0..64usize),
    ) {
        prop_assume!(key1 != key2);
        let id1 = derive_cell_id(&key1);
        let id2 = derive_cell_id(&key2);
        prop_assert_ne!(
            id1, id2,
            "derive_cell_id collision: {:?} vs {:?}",
            key1,
            key2
        );
    }

    /// `CellId::to_aad_bytes` must be injective over distinct `(table_id,
    /// row_id, col_id)` triples.
    ///
    /// This covers the explicit `put_cell` / `get_cell` AAD path.
    #[test]
    fn cell_id_aad_bytes_injective(
        c1 in arb_cell_id(),
        c2 in arb_cell_id(),
    ) {
        prop_assume!(c1 != c2);
        let aad1 = c1.to_aad_bytes();
        let aad2 = c2.to_aad_bytes();
        prop_assert_ne!(
            aad1, aad2,
            "CellId::to_aad_bytes collision: {:?} vs {:?}",
            c1,
            c2
        );
    }
}

// ── Serde roundtrip (only compiled when the `serde` feature is enabled) ───────

#[cfg(feature = "serde")]
#[test]
fn cell_id_serde_roundtrip() {
    let id = CellId {
        table_id: 42,
        row_id: 9_999_999,
        col_id: 7,
    };
    let json = serde_json::to_string(&id).expect("serialize CellId");
    let back: CellId = serde_json::from_str(&json).expect("deserialize CellId");
    assert_eq!(id, back, "serde roundtrip must be lossless");
}
