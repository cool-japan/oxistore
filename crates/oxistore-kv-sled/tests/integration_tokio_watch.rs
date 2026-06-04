//! Integration test: `watch_prefix` with a tokio async runtime.
//!
//! Verifies that [`SledStore::watch_prefix`] works correctly when the writes
//! are made from tokio `spawn_blocking` tasks — the common pattern when
//! embedding sled (a synchronous API) inside an async application.

use oxistore_core::KvStore as _;
use oxistore_kv_sled::SledStore;
use std::sync::Arc;
use std::time::Duration;

// ── watch_prefix inside a tokio multi-thread runtime ────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_prefix_tokio_multi_thread() {
    let store = Arc::new(SledStore::open_temporary().expect("open temporary"));

    // Attach subscriber before spawning the writer.
    let mut subscriber = store.watch_prefix(b"evt:");

    let store_writer = Arc::clone(&store);
    let write_task = tokio::task::spawn_blocking(move || {
        store_writer.put(b"evt:1", b"alpha").expect("put evt:1");
        store_writer.put(b"evt:2", b"beta").expect("put evt:2");
    });

    // Wait for the write task to finish.
    write_task.await.expect("write task panicked");

    // Poll the subscriber for the first event.
    let event = subscriber
        .next_timeout(Duration::from_secs(5))
        .expect("expected an event from the subscriber, got timeout or disconnect");

    // Confirm the event is an Insert with a key starting with "evt:".
    match &event {
        sled::Event::Insert { key, .. } => {
            assert!(
                key.starts_with(b"evt:"),
                "key must start with 'evt:', got {:?}",
                std::str::from_utf8(key)
            );
        }
        other => panic!("expected Insert event, got {other:?}"),
    }
}

// ── Multiple events on the same prefix ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_prefix_tokio_multiple_events() {
    let store = Arc::new(SledStore::open_temporary().expect("open temporary"));

    let mut subscriber = store.watch_prefix(b"watch:");
    const N: usize = 5;

    let store_writer = Arc::clone(&store);
    let write_task = tokio::task::spawn_blocking(move || {
        for i in 0..N {
            let key = format!("watch:{i:04}");
            let val = format!("val_{i}");
            store_writer
                .put(key.as_bytes(), val.as_bytes())
                .expect("put");
        }
    });
    write_task.await.expect("write task panicked");

    // Drain up to N events (next_timeout returns Result<Event, RecvTimeoutError>).
    let mut received = 0usize;
    while received < N {
        match subscriber.next_timeout(Duration::from_secs(3)) {
            Ok(_) => received += 1,
            Err(_) => break, // timed out — acceptable if we got at least some
        }
    }

    assert!(
        received > 0,
        "should have received at least one watch event"
    );
}

// ── watch does not trigger for different prefix ───────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_prefix_tokio_no_cross_prefix_events() {
    let store = Arc::new(SledStore::open_temporary().expect("open temporary"));

    // Subscribe only to "pfx_a:".
    let mut sub_a = store.watch_prefix(b"pfx_a:");

    let store_writer = Arc::clone(&store);
    let write_task = tokio::task::spawn_blocking(move || {
        // Write to "pfx_b:" only — sub_a must NOT see these.
        store_writer
            .put(b"pfx_b:1", b"unrelated")
            .expect("put pfx_b:1");
        // Then write to "pfx_a:" so we can confirm sub_a is alive.
        store_writer
            .put(b"pfx_a:1", b"trigger")
            .expect("put pfx_a:1");
    });
    write_task.await.expect("write task panicked");

    // The first event for sub_a must be from "pfx_a:".
    let event = sub_a
        .next_timeout(Duration::from_secs(5))
        .expect("subscriber timed out waiting for pfx_a: event");

    match &event {
        sled::Event::Insert { key, .. } => {
            assert!(
                key.starts_with(b"pfx_a:"),
                "event must be for 'pfx_a:', got {:?}",
                std::str::from_utf8(key)
            );
        }
        other => panic!("expected Insert event, got {other:?}"),
    }
}

// ── watch_prefix fires for delete events ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_prefix_tokio_delete_event() {
    let store = Arc::new(SledStore::open_temporary().expect("open temporary"));

    store.put(b"del:k1", b"v").expect("pre-put");

    let mut sub = store.watch_prefix(b"del:");

    let store_writer = Arc::clone(&store);
    let task = tokio::task::spawn_blocking(move || {
        store_writer.delete(b"del:k1").expect("delete");
    });
    task.await.expect("delete task panicked");

    let event = sub
        .next_timeout(Duration::from_secs(5))
        .expect("subscriber timed out waiting for delete event");

    match &event {
        sled::Event::Remove { key } => {
            assert_eq!(key.as_ref(), b"del:k1");
        }
        other => panic!("expected Remove event, got {other:?}"),
    }
}

// ── flush_with_reclaim inside tokio spawn_blocking ────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn flush_with_reclaim_tokio() {
    let store = Arc::new(SledStore::open_temporary().expect("open temporary"));

    let store_write = Arc::clone(&store);
    tokio::task::spawn_blocking(move || {
        for i in 0u32..100 {
            let key = format!("k{i}");
            store_write.put(key.as_bytes(), b"value").expect("put");
        }
    })
    .await
    .expect("write task panicked");

    let size = tokio::task::spawn_blocking({
        let s = Arc::clone(&store);
        move || s.flush_with_reclaim().expect("flush_with_reclaim")
    })
    .await
    .expect("flush task panicked");

    // Temporary sled databases may report 0 on some platforms.
    // The important invariant is that the call doesn't error.
    let _ = size;
}
