//! Tests for `From<CompressError> for StoreError` conversion.
//!
//! These tests do NOT require the `compress` feature — `CompressError` and
//! the `From` impl are always available.

use oxistore_compress::CompressError;
use oxistore_core::StoreError;

#[test]
fn from_compress_error_preserves_message() {
    let err = CompressError::Compress("DEFLATE encoder exploded".to_string());
    let store_err: StoreError = err.into();
    assert!(
        matches!(store_err, StoreError::Other(_)),
        "expected StoreError::Other, got {store_err:?}"
    );
    let display = store_err.to_string();
    assert!(
        display.contains("DEFLATE encoder exploded"),
        "original message not preserved: {display}"
    );
}

#[test]
fn from_compress_error_decompress_maps_to_other() {
    let err = CompressError::Decompress("corrupt stream".to_string());
    let store_err: StoreError = err.into();
    assert!(
        matches!(store_err, StoreError::Other(_)),
        "expected StoreError::Other, got {store_err:?}"
    );
    // The decompress message is also preserved.
    let display = store_err.to_string();
    assert!(
        display.contains("corrupt stream"),
        "decompress message not preserved: {display}"
    );
}

#[test]
fn from_compress_error_invalid_level_maps_to_other() {
    let err = CompressError::InvalidLevel(42);
    let store_err: StoreError = err.into();
    assert!(
        matches!(store_err, StoreError::Other(_)),
        "expected StoreError::Other, got {store_err:?}"
    );
    // The level number should appear in the message.
    let display = store_err.to_string();
    assert!(display.contains("42"), "level not in message: {display}");
}

#[test]
fn all_compress_error_variants_map_to_store_error_other() {
    // Every CompressError variant maps to StoreError::Other.
    let cases: Vec<(CompressError, &str)> = vec![
        (CompressError::Compress("enc fail".to_string()), "enc fail"),
        (
            CompressError::Decompress("dec fail".to_string()),
            "dec fail",
        ),
        (CompressError::InvalidLevel(255), "255"),
    ];
    for (err, expected_substr) in cases {
        let store_err: StoreError = err.into();
        assert!(
            matches!(store_err, StoreError::Other(_)),
            "expected StoreError::Other"
        );
        let display = store_err.to_string();
        assert!(
            display.contains(expected_substr),
            "expected '{expected_substr}' in '{display}'"
        );
    }
}
