//! Smoke tests verifying that facade module re-exports compile and resolve correctly.
//!
//! These tests do not exercise runtime behaviour — they only confirm that the
//! public API surface is accessible through the `oxistore` facade crate.

#![forbid(unsafe_code)]

// ── compress module ───────────────────────────────────────────────────────────

/// Verify that the `compress` module re-exports `OxiArcCodec` and `CompressError`.
#[test]
#[cfg(feature = "compress")]
fn facade_compress_module_resolves() {
    use oxistore::compress::{CompressError, OxiArcCodec};
    // Type-name check: confirms both types are accessible and named as expected.
    let codec_name = std::any::type_name::<OxiArcCodec>();
    let error_name = std::any::type_name::<CompressError>();
    assert!(
        codec_name.contains("OxiArcCodec"),
        "OxiArcCodec must be accessible"
    );
    assert!(
        error_name.contains("CompressError"),
        "CompressError must be accessible"
    );
}

// ── encrypt module ────────────────────────────────────────────────────────────

/// Verify that the `encrypt` module re-exports core encryption types.
#[test]
#[cfg(feature = "encrypt")]
fn facade_encrypt_module_resolves() {
    use oxistore::encrypt::{EncryptError, EncryptedKv, Keyring, StaticKey};
    // Confirm each type is reachable through the facade by inspecting its name.
    let static_key_name = std::any::type_name::<StaticKey>();
    let keyring_name = std::any::type_name::<Keyring>();
    let error_name = std::any::type_name::<EncryptError>();
    assert!(
        static_key_name.contains("StaticKey"),
        "StaticKey must be accessible"
    );
    assert!(
        keyring_name.contains("Keyring"),
        "Keyring must be accessible"
    );
    assert!(
        error_name.contains("EncryptError"),
        "EncryptError must be accessible"
    );

    // EncryptedKv is generic; just assert the identifier compiles into scope.
    // (A zero-sized marker approach lets us check without full instantiation.)
    fn _takes_encrypted_kv<T, K, A>(_: &EncryptedKv<T, K, A>)
    where
        T: oxistore::KvStore,
        K: oxistore::encrypt::KeyProvider,
        A: oxistore::encrypt::Aead,
    {
    }
}

// ── serde-typed re-exports ────────────────────────────────────────────────────

/// Verify that the `serde-typed` feature re-exports `TypedKvStore`, `JsonCodec`,
/// `TypedCodec`, and `TypedKvError` at the crate root.
#[test]
#[cfg(feature = "serde-typed")]
fn facade_serde_typed_resolves() {
    use oxistore::{JsonCodec, TypedCodec, TypedKvError, TypedKvStore};

    let codec_name = std::any::type_name::<JsonCodec>();
    assert!(
        codec_name.contains("JsonCodec"),
        "JsonCodec must be accessible"
    );

    // TypedKvError is parameterised by the codec error type.
    // Use std::fmt::Error as a minimal concrete error type to avoid extra deps.
    let error_name = std::any::type_name::<TypedKvError<std::fmt::Error>>();
    assert!(
        error_name.contains("TypedKvError"),
        "TypedKvError must be accessible"
    );

    // TypedCodec is a trait — confirm it is reachable by naming it in a bound.
    fn _assert_codec_bound<C: TypedCodec>() {}

    // TypedKvStore is generic; confirm the identifier is in scope.
    fn _takes_typed_store<S, C>(_: &TypedKvStore<S, C>)
    where
        S: oxistore::KvStore,
        C: TypedCodec,
    {
    }
}
