//! Tour of the [`BlobStore`] trait over [`LocalBlobStore`]: basic ops,
//! content-addressable storage (dedup), and default-method conveniences.
//!
//! ```sh
//! cargo run -p oxistore-blob --example blob_store_tour
//! ```

use bytes::Bytes;
use oxistore_blob::{BlobStore, LocalBlobStore};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("oxistore_blob_example_{}", std::process::id()));
    let store = LocalBlobStore::with_checksum_verification(&dir);

    // ── put / get / head / exists ─────────────────────────────────────────
    store
        .put("docs/readme.txt", Bytes::from("hello blob storage"))
        .await?;
    let data = store.get("docs/readme.txt").await?;
    println!(
        "get(docs/readme.txt) -> {:?}",
        String::from_utf8_lossy(&data)
    );

    let meta = store.head("docs/readme.txt").await?;
    println!(
        "head(docs/readme.txt) -> key={}, size={}",
        meta.key, meta.size
    );
    println!(
        "exists(docs/readme.txt) -> {}",
        store.exists("docs/readme.txt").await?
    );
    println!(
        "exists(docs/missing.txt) -> {}",
        store.exists("docs/missing.txt").await?
    );

    // ── list / list_meta (prefix-scoped) ──────────────────────────────────
    store
        .put("docs/changelog.txt", Bytes::from("v1: initial release"))
        .await?;
    store
        .put("images/logo.png", Bytes::from("not really a png"))
        .await?;
    let mut docs = store.list("docs/").await?;
    docs.sort();
    println!("list(docs/) -> {docs:?}");

    // ── copy / rename ──────────────────────────────────────────────────────
    store
        .copy("docs/readme.txt", "docs/readme-backup.txt")
        .await?;
    println!(
        "after copy, exists(readme-backup.txt) -> {}",
        store.exists("docs/readme-backup.txt").await?
    );
    store
        .rename("docs/readme-backup.txt", "docs/readme.bak")
        .await?;
    println!(
        "after rename, readme-backup.txt exists={}, readme.bak exists={}",
        store.exists("docs/readme-backup.txt").await?,
        store.exists("docs/readme.bak").await?
    );

    // ── put_if_absent: only writes if the key doesn't already exist ──────
    match store
        .put_if_absent("docs/readme.txt", Bytes::from("would overwrite"))
        .await
    {
        Err(oxistore_blob::BlobError::AlreadyExists(key)) => {
            println!("put_if_absent(existing key) -> rejected, {key} already exists")
        }
        other => println!("put_if_absent(existing key) -> unexpected: {other:?}"),
    }
    store
        .put_if_absent("docs/new.txt", Bytes::from("first write"))
        .await?;
    println!(
        "put_if_absent(new key) -> wrote {:?}",
        String::from_utf8_lossy(&store.get("docs/new.txt").await?)
    );

    // ── Content-addressable storage: identical content -> identical key ──
    let payload = Bytes::from("this content is deduplicated by its SHA-256 digest");
    let digest_a = store.put_cas(payload.clone()).await?;
    let digest_b = store.put_cas(payload.clone()).await?;
    println!(
        "put_cas twice with identical content -> same digest: {}",
        digest_a == digest_b
    );
    println!("digest (hex) -> {}", digest_a.to_hex());

    let fetched = store.get_cas(&digest_a).await?;
    assert_eq!(fetched, payload);
    println!("get_cas re-verifies the SHA-256 on read -> content matches");
    println!("exists_cas -> {}", store.exists_cas(&digest_a).await?);

    // ── Streaming put over AsyncRead, verified get back ───────────────────
    // `put_streaming` hashes the input incrementally as it reads (rather
    // than requiring the whole blob in memory up front) and stores it
    // content-addressed; `get_verified` is a `get_cas` alias that always
    // re-checks the SHA-256 on read.
    let stream_payload = b"streamed via AsyncRead, hashed incrementally".repeat(4);
    // `put_streaming` requires an owned, `'static` `AsyncRead` (it may read
    // from the source across await points beyond this function's stack
    // frame), so wrap the owned `Vec<u8>` in a `Cursor` rather than passing
    // a borrowed slice.
    let stream_digest = store
        .put_streaming(std::io::Cursor::new(stream_payload.clone()))
        .await?;
    let verified_bytes = store.get_verified(&stream_digest).await?;
    assert_eq!(verified_bytes.as_ref(), stream_payload.as_slice());
    println!(
        "put_streaming/get_verified round-trip -> {} bytes, digest={}",
        verified_bytes.len(),
        stream_digest.to_hex()
    );

    // ── delete_many / delete_prefix ────────────────────────────────────────
    store
        .delete_many(&["docs/changelog.txt", "docs/new.txt"])
        .await?;
    let removed = store.delete_prefix("images/").await?;
    println!("delete_prefix(images/) removed {removed} blob(s)");

    let mut remaining = store.list("").await?;
    remaining.sort();
    println!("remaining keys -> {remaining:?}");

    // Clean up the example's own temp directory.
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
