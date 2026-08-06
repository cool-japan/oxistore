//! Fuzz target for `EnvelopeCipher::decrypt` — the envelope wire-format
//! parser (`kek_version` header, `wrap_nonce`, `wrapped_dek`, `data_nonce`,
//! `data_ciphertext`; see `oxistore_encrypt::envelope` module docs for the
//! exact layout and `MIN_ENVELOPE_LEN`).
//!
//! `decrypt` is the first thing that runs on every byte sequence read back
//! out of a KV backend by `EncryptedKvEnvelope` / `EnvelopeTxn` /
//! `EnvelopeSnapshot` — i.e. it parses attacker-influenceable bytes any
//! time the underlying store has been tampered with, is corrupted, or (via
//! `EncryptedKvEnvelope::snapshot`/`transaction`) reflects data written by
//! a different, less-trusted component sharing the same store. The goal is
//! to prove it can never panic or read out of bounds on arbitrary input,
//! only ever return `Err(EncryptError)`.
//!
//! ```sh
//! cargo +nightly fuzz run envelope_decrypt fuzz/corpus/envelope_decrypt fuzz/seeds/envelope_decrypt
//! ```
//!
//! A small checked-in seed set of real, well-formed envelopes (including one
//! produced after a KEK rotation, so both `kek_version`s the fixed keyring
//! below knows about are represented) lives in `seeds/envelope_decrypt/`.
//! Pass **both** directories as shown above: the first positional argument
//! is where libFuzzer writes every newly-discovered input it keeps, so it
//! must be the gitignored `fuzz/corpus/<target>/` (create it once with
//! `mkdir -p` if it doesn't exist yet) — passing `seeds/` there instead
//! corrupts the checked-in seed set with generated garbage. Additional
//! positional arguments (`seeds/envelope_decrypt` here) are read-only inputs
//! merged in at startup, which is how the curated seeds get used without
//! being at risk of mutation.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxistore_encrypt::{EnvelopeCipher, Keyring};
use std::sync::LazyLock;

/// Fixed two-version keyring, built once per fuzzer process. Version 1 and
/// version 2 match how the files in `seeds/envelope_decrypt/` were produced
/// (see the crate's `envelope_encryption` example for the same
/// encrypt/rotate pattern) so a meaningful fraction of mutated inputs can
/// get past the `kek_version` lookup and into the AEAD-unwrap and
/// data-decrypt logic.
static CIPHER: LazyLock<EnvelopeCipher> = LazyLock::new(|| {
    let cipher = EnvelopeCipher::new(Keyring::new([0x01u8; 32]));
    // Ignore failure: RNG-independent, cannot fail in practice, and even if
    // it did the target remains valid (just with one fewer known version).
    let _ = cipher.add_kek_version([0x02u8; 32]);
    cipher
});

fuzz_target!(|data: &[u8]| {
    // The AAD is part of the trusted call site (the raw KV key) in real
    // usage, not attacker-controlled independently of `data` — fixing it
    // here keeps the fuzzer's entire input budget on the ciphertext framing
    // itself, which is the thing this target exists to stress.
    let _ = CIPHER.decrypt(data, b"");
});
