//! Put/get/list against a real (or MinIO-compatible) S3 endpoint.
//!
//! This backend cannot be demonstrated against an embedded/offline server —
//! it is a real AWS SigV4 HTTP client — so this example reads its endpoint
//! and credentials from the environment and prints setup instructions and
//! exits cleanly (not an error) if they are not configured, rather than
//! failing the build or hanging.
//!
//! ```sh
//! # Start a local S3-compatible server, e.g. MinIO:
//! docker run -d -p 9000:9000 -e MINIO_ROOT_USER=minioadmin \
//!     -e MINIO_ROOT_PASSWORD=minioadmin minio/minio server /data
//!
//! export OXISTORE_EXAMPLE_S3_ENDPOINT=http://localhost:9000
//! export OXISTORE_EXAMPLE_S3_BUCKET=oxistore-example
//! export AWS_ACCESS_KEY_ID=minioadmin
//! export AWS_SECRET_ACCESS_KEY=minioadmin
//! # Create the bucket once, e.g.: aws --endpoint-url $OXISTORE_EXAMPLE_S3_ENDPOINT s3 mb s3://oxistore-example
//!
//! cargo run -p oxistore-blob-s3 --example s3_basics
//! ```
//!
//! Works unmodified against real AWS S3 too: set
//! `OXISTORE_EXAMPLE_S3_ENDPOINT=https://s3.<region>.amazonaws.com`,
//! `OXISTORE_EXAMPLE_S3_REGION`, and real IAM credentials.

use bytes::Bytes;
use oxistore_blob::BlobStore;
use oxistore_blob_s3::{S3BlobStoreBuilder, S3Credentials};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(endpoint) = std::env::var("OXISTORE_EXAMPLE_S3_ENDPOINT").ok() else {
        print_setup_instructions();
        return Ok(());
    };
    let bucket = std::env::var("OXISTORE_EXAMPLE_S3_BUCKET")
        .unwrap_or_else(|_| "oxistore-example".to_string());
    let region =
        std::env::var("OXISTORE_EXAMPLE_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());
    let credentials = S3Credentials::from_env().map_err(|e| {
        format!("OXISTORE_EXAMPLE_S3_ENDPOINT is set but credentials are missing: {e}")
    })?;

    let store = S3BlobStoreBuilder::new()
        .endpoint(&endpoint)
        .region(&region)
        .bucket(&bucket)
        .credentials(credentials)
        .path_style(true)
        .build()?;

    let key = format!("oxistore-example/{}.txt", std::process::id());
    store
        .put(&key, Bytes::from("hello from oxistore-blob-s3"))
        .await?;
    println!("put {key} -> ok");

    let data = store.get(&key).await?;
    println!("get {key} -> {:?}", String::from_utf8_lossy(&data));

    let meta = store.head(&key).await?;
    println!("head {key} -> size={}", meta.size);

    let listing = store.list("oxistore-example/").await?;
    println!("list(oxistore-example/) -> {listing:?}");

    store.delete(&key).await?;
    println!(
        "delete {key} -> ok, exists now = {}",
        store.exists(&key).await?
    );

    Ok(())
}

fn print_setup_instructions() {
    println!("OXISTORE_EXAMPLE_S3_ENDPOINT is not set -- skipping the live S3 round trip.");
    println!();
    println!("To run this example against a local S3-compatible server (MinIO):");
    println!("  docker run -d -p 9000:9000 -e MINIO_ROOT_USER=minioadmin \\");
    println!("      -e MINIO_ROOT_PASSWORD=minioadmin minio/minio server /data");
    println!("  export OXISTORE_EXAMPLE_S3_ENDPOINT=http://localhost:9000");
    println!("  export OXISTORE_EXAMPLE_S3_BUCKET=oxistore-example");
    println!("  export AWS_ACCESS_KEY_ID=minioadmin");
    println!("  export AWS_SECRET_ACCESS_KEY=minioadmin");
    println!("  aws --endpoint-url $OXISTORE_EXAMPLE_S3_ENDPOINT s3 mb s3://oxistore-example");
    println!("  cargo run -p oxistore-blob-s3 --example s3_basics");
    println!();
    println!("Or against real AWS S3: set OXISTORE_EXAMPLE_S3_ENDPOINT to");
    println!("https://s3.<region>.amazonaws.com, OXISTORE_EXAMPLE_S3_REGION, and real IAM keys.");
}
