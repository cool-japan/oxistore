//! Put/get/list against a real (or Azurite-emulated) Azure Blob Storage
//! container.
//!
//! This backend cannot be demonstrated against an embedded/offline server —
//! it is a real HMAC-SHA256 Shared Key HTTP client — so this example reads
//! its endpoint and credentials from the environment and prints setup
//! instructions and exits cleanly (not an error) if they are not
//! configured, rather than failing the build or hanging.
//!
//! ```sh
//! # Start the Azurite emulator:
//! docker run -d -p 10000:10000 mcr.microsoft.com/azure-storage/azurite \
//!     azurite-blob --blobHost 0.0.0.0
//!
//! export AZURE_STORAGE_ACCOUNT=devstoreaccount1
//! # Azurite's well-known development account key:
//! export AZURE_STORAGE_KEY="Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=="
//! export OXISTORE_EXAMPLE_AZURE_ENDPOINT=http://127.0.0.1:10000/devstoreaccount1
//! export OXISTORE_EXAMPLE_AZURE_CONTAINER=oxistore-example
//! # Create the container once, e.g. via `az storage container create` or the Azurite REST API.
//!
//! cargo run -p oxistore-blob-azure --example azure_basics
//! ```
//!
//! Works unmodified against a real Azure Storage account too: omit
//! `OXISTORE_EXAMPLE_AZURE_ENDPOINT` and set real `AZURE_STORAGE_ACCOUNT` /
//! `AZURE_STORAGE_KEY` values — the endpoint then defaults to
//! `https://<account>.blob.core.windows.net`.

use bytes::Bytes;
use oxistore_blob::BlobStore;
use oxistore_blob_azure::{AzureBlobStore, AzureConfig, AzureCredentials};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Ok(credentials) = AzureCredentials::from_env() else {
        print_setup_instructions();
        return Ok(());
    };
    let container = std::env::var("OXISTORE_EXAMPLE_AZURE_CONTAINER")
        .unwrap_or_else(|_| "oxistore-example".to_string());

    let mut config = AzureConfig::new(credentials, container);
    if let Ok(endpoint) = std::env::var("OXISTORE_EXAMPLE_AZURE_ENDPOINT") {
        config.endpoint = Some(endpoint);
    }
    let store = AzureBlobStore::new(config)?;

    let key = format!("oxistore-example/{}.txt", std::process::id());
    store
        .put(&key, Bytes::from("hello from oxistore-blob-azure"))
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
        "AZURE_STORAGE_ACCOUNT / AZURE_STORAGE_KEY are not set -- skipping the live round trip."
    );
    println!();
    println!("To run this example against the Azurite emulator:");
    println!("  docker run -d -p 10000:10000 mcr.microsoft.com/azure-storage/azurite \\");
    println!("      azurite-blob --blobHost 0.0.0.0");
    println!("  export AZURE_STORAGE_ACCOUNT=devstoreaccount1");
    println!(
        "  export AZURE_STORAGE_KEY=\"Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==\""
    );
    println!("  export OXISTORE_EXAMPLE_AZURE_ENDPOINT=http://127.0.0.1:10000/devstoreaccount1");
    println!("  export OXISTORE_EXAMPLE_AZURE_CONTAINER=oxistore-example");
    println!("  cargo run -p oxistore-blob-azure --example azure_basics");
    println!();
    println!("Or against a real Azure Storage account: set real AZURE_STORAGE_ACCOUNT /");
    println!("AZURE_STORAGE_KEY values and leave OXISTORE_EXAMPLE_AZURE_ENDPOINT unset.");
}
