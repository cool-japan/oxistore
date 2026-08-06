//! Fuzz target for `decrypt_cell` — the cell-level wire-format parser
//! (`nonce` ‖ `ciphertext+tag`; see `oxistore_encrypt::cell` module docs for
//! the exact layout and `MIN_CIPHERTEXT_LEN`).
//!
//! This is `oxistore-encrypt`'s *other* ciphertext framing (used by
//! `EncryptedKv<T, K, A>`, distinct from and wire-incompatible with the
//! envelope format covered by the `envelope_decrypt` target) — same
//! goal: arbitrary input must only ever produce `Err(EncryptError)`, never
//! panic or read out of bounds.
//!
//! ```sh
//! cargo +nightly fuzz run cell_decrypt fuzz/corpus/cell_decrypt fuzz/seeds/cell_decrypt
//! ```
//!
//! A small checked-in seed set lives in `seeds/cell_decrypt/`. Pass **both**
//! directories as shown above — see the longer explanation in
//! `envelope_decrypt.rs`'s doc comment for why the first argument must be
//! the gitignored `fuzz/corpus/<target>/` (create it once with `mkdir -p`
//! if it doesn't exist yet) and not `seeds/` directly.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxistore_encrypt::{decrypt_cell, CellId, StaticKey};
use std::sync::LazyLock;

/// Fixed 32-byte key, built once per fuzzer process.
static KEY: LazyLock<StaticKey> = LazyLock::new(|| StaticKey::new(vec![0x03u8; 32]));

/// Fixed cell id matching `seeds/cell_decrypt/seed_0` — the `CellId` is
/// AAD, so (as with `envelope_decrypt`'s fixed AAD) keeping it constant
/// puts the fuzzer's whole input budget on the ciphertext bytes.
const CELL: CellId = CellId {
    table_id: 0,
    row_id: 0,
    col_id: 0,
};

fuzz_target!(|data: &[u8]| {
    let _ = decrypt_cell(&*KEY, CELL, data);
});
