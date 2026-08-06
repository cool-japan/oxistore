//! Fuzz target for `S3ErrorResponse::parse` — the S3 XML error-body parser.
//!
//! This parses the response body of every failed S3 request (any non-2xx
//! HTTP status from a real S3-compatible endpoint), so it consumes bytes
//! controlled by whatever server the crate's HTTP client is configured to
//! talk to — untrusted network input. The parser is event-based
//! (`quick_xml`, no DTD/entity expansion), but had no fuzz coverage before
//! this target; the goal is to prove arbitrary bytes only ever yield `None`
//! or a `Some(S3ErrorResponse)`, never a panic.
//!
//! ```sh
//! cargo +nightly fuzz run s3_error_xml fuzz/corpus/s3_error_xml fuzz/seeds/s3_error_xml
//! ```
//!
//! A small checked-in seed set of real S3 error bodies lives in
//! `seeds/s3_error_xml/`. Pass **both** directories as shown above: the
//! first positional argument is where libFuzzer writes every
//! newly-discovered input it keeps, so it must be the gitignored
//! `fuzz/corpus/s3_error_xml/` (create it once with `mkdir -p` if it
//! doesn't exist yet) — passing `seeds/` there instead corrupts the
//! checked-in seed set with generated garbage. The second, read-only
//! argument is how the curated seeds get used without risk of mutation.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxistore_blob_s3::S3ErrorResponse;

fuzz_target!(|data: &[u8]| {
    let _ = S3ErrorResponse::parse(data);
});
