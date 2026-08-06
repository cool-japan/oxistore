//! Put/get/list against a real Google Cloud Storage bucket (or a
//! GCS-compatible emulator such as `fake-gcs-server`).
//!
//! This backend cannot be demonstrated against an embedded/offline server —
//! it signs a real RS256 JWT and exchanges it for an OAuth2 bearer token —
//! so this example reads its credentials from the environment and prints
//! setup instructions and exits cleanly (not an error) if they are not
//! configured, rather than failing the build or hanging.
//!
//! ```sh
//! # Real GCP project:
//! gcloud iam service-accounts keys create key.json \
//!     --iam-account=<name>@<project>.iam.gserviceaccount.com
//! export GOOGLE_APPLICATION_CREDENTIALS=$(pwd)/key.json
//! export OXISTORE_EXAMPLE_GCS_BUCKET=my-gcs-bucket
//!
//! cargo run -p oxistore-blob-gcs --example gcs_basics
//! ```
//!
//! To run against a local emulator (e.g. `fsouza/fake-gcs-server`) instead
//! of real GCS, point `OXISTORE_EXAMPLE_GCS_ENDPOINT` (and, if the emulator
//! also stubs OAuth2 token exchange, `OXISTORE_EXAMPLE_GCS_OAUTH_ENDPOINT`)
//! at it; `GOOGLE_APPLICATION_CREDENTIALS` must still point at *some*
//! service-account JSON (a self-signed one is fine — most emulators do not
//! verify the JWT signature against a real Google key).

use bytes::Bytes;
use oxistore_blob::BlobStore;
use oxistore_blob_gcs::{GcsBlobStore, GcsConfig, GcsServiceAccount};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Ok(credentials) = GcsServiceAccount::from_env() else {
        print_setup_instructions();
        return Ok(());
    };
    let Ok(bucket) = std::env::var("OXISTORE_EXAMPLE_GCS_BUCKET") else {
        print_setup_instructions();
        return Ok(());
    };

    let config = GcsConfig {
        bucket,
        credentials,
        timeout: std::time::Duration::from_secs(30),
        endpoint: std::env::var("OXISTORE_EXAMPLE_GCS_ENDPOINT").ok(),
        oauth_endpoint: std::env::var("OXISTORE_EXAMPLE_GCS_OAUTH_ENDPOINT").ok(),
    };
    let store = GcsBlobStore::new(config)?;

    let key = format!("oxistore-example/{}.txt", std::process::id());
    store
        .put(&key, Bytes::from("hello from oxistore-blob-gcs"))
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
    println!(
        "GOOGLE_APPLICATION_CREDENTIALS / OXISTORE_EXAMPLE_GCS_BUCKET are not both set -- \
         skipping the live round trip."
    );
    println!();
    println!("To run this example against a real GCP project:");
    println!("  gcloud iam service-accounts keys create key.json \\");
    println!("      --iam-account=<name>@<project>.iam.gserviceaccount.com");
    println!("  export GOOGLE_APPLICATION_CREDENTIALS=$(pwd)/key.json");
    println!("  export OXISTORE_EXAMPLE_GCS_BUCKET=my-gcs-bucket");
    println!("  cargo run -p oxistore-blob-gcs --example gcs_basics");
    println!();
    println!("To run against a local emulator instead, additionally set");
    println!("OXISTORE_EXAMPLE_GCS_ENDPOINT (and OXISTORE_EXAMPLE_GCS_OAUTH_ENDPOINT, if the");
    println!("emulator stubs OAuth2 token exchange too) to point at it.");
}
