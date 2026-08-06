//! Fuzz target for `parse_list_response` — the Azure Blob Storage
//! `ListBlobs` (`EnumerationResults`) XML response parser.
//!
//! This parses the response body of every successful
//! `?restype=container&comp=list` request against a real (or
//! Azurite-emulated) Azure Blob Storage account, so it consumes bytes
//! controlled by whatever server the crate's HTTP client is configured to
//! talk to — untrusted network input, and this crate's only XML parser
//! (mirroring `oxistore-blob-s3`'s `s3_error_xml` target for the sibling
//! S3 backend). The parser is event-based (`quick_xml`, no DTD/entity
//! expansion), but had no fuzz coverage before this target; the goal is to
//! prove arbitrary bytes only ever yield `Ok(..)` or `Err(BlobError)`,
//! never a panic.
//!
//! ```sh
//! cargo +nightly fuzz run azure_list_response_xml fuzz/corpus/azure_list_response_xml fuzz/seeds/azure_list_response_xml
//! ```
//!
//! A small checked-in seed set of real Azure `EnumerationResults` bodies
//! (a basic two-blob listing, a paginated response with a `NextMarker`,
//! and an empty container) lives in `seeds/azure_list_response_xml/`. Pass
//! **both** directories as shown above — see the longer explanation in
//! `oxistore-encrypt/fuzz/fuzz_targets/envelope_decrypt.rs`'s doc comment
//! for why the first argument must be the gitignored
//! `fuzz/corpus/azure_list_response_xml/` (create it once with `mkdir -p`
//! if it doesn't exist yet) and not `seeds/` directly.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxistore_blob_azure::parse_list_response;

fuzz_target!(|data: &[u8]| {
    let _ = parse_list_response(data);
});
