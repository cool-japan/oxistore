//! Integration tests: `EncryptedPooledStore` — cell-level AEAD encryption
//! layered over an `oxisql_pool::OxidbKvStore` (embedded/in-memory backend).
//!
//! ## Design
//!
//! `OxidbKvStore` is an async SQL-backed KV store using `&str` keys and
//! `String` values.  `EncryptedPooledStore` adapts this to support binary
//! byte-slice keys and values, adding per-value AEAD encryption:
//!
//! - Keys are hex-encoded before storage (binary → ASCII-safe text).
//! - Values are encrypted via `oxistore_encrypt::aead::encrypt_with_aead`,
//!   then base64-encoded for storage as `TEXT` in the SQL table.
//! - AAD is `derive_cell_id(key_bytes)` — the same BLAKE3-based binding used
//!   by `EncryptedKv`, preventing transplant attacks.
//!
//! ## What is tested
//!
//! 1. Round-trip: `put(key, plaintext)` then `get(key)` returns `plaintext`.
//! 2. Ciphertext-at-rest: the raw SQL value differs from plaintext.
//! 3. Multi-key isolation: two distinct keys produce independently encrypted values.
//! 4. Delete: `delete(key)` causes `get(key)` to return `None`.
//! 5. Auth failure: corrupting the stored ciphertext causes `get` to fail.

use std::sync::Arc;

use oxisql_pool::{embedded::EmbeddedPool, kv_store::OxidbKvStore, OxidbPool, PoolError};
use oxistore_encrypt::aead::{decrypt_with_aead, derive_cell_id, encrypt_with_aead, AeadKind};

// ── EncryptedPooledStore ──────────────────────────────────────────────────────

/// An async cell-level-encrypted wrapper over [`OxidbKvStore`].
///
/// Keys are stored as lowercase hex strings.  Values are encrypted with
/// XChaCha20-Poly1305 and base64-encoded for storage as UTF-8 `TEXT`.
///
/// The BLAKE3 cell ID of the raw key bytes is used as AAD, binding each
/// ciphertext to its storage location (transplant attack prevention).
struct EncryptedPooledStore {
    inner: OxidbKvStore,
    key: [u8; 32],
}

impl EncryptedPooledStore {
    /// Construct a new `EncryptedPooledStore` backed by `pool`.
    ///
    /// The pool must already be initialised (table created).
    fn new(pool: Arc<OxidbPool>, key: [u8; 32]) -> Self {
        Self {
            inner: OxidbKvStore::new(pool, Some("enc_kv")),
            key,
        }
    }

    /// Encrypt `value` under `key_bytes` and store it.
    ///
    /// The raw `key_bytes` are hex-encoded to obtain the SQL column key.
    async fn put(&self, key_bytes: &[u8], value: &[u8]) -> Result<(), PoolError> {
        let hex_key = encode_hex(key_bytes);
        let aad = derive_cell_id(key_bytes);
        let aead = AeadKind::XChaCha20Poly1305;
        let ct = encrypt_with_aead(&aead, &self.key, &aad, value)
            .map_err(|e| PoolError::Build(e.to_string()))?;
        let b64 = encode_base64(&ct);
        self.inner.set(&hex_key, &b64).await
    }

    /// Retrieve and decrypt the value stored under `key_bytes`.
    ///
    /// Returns `None` if the key is absent, or an error if authentication
    /// fails (tampered ciphertext or wrong encryption key).
    async fn get(&self, key_bytes: &[u8]) -> Result<Option<Vec<u8>>, PoolError> {
        let hex_key = encode_hex(key_bytes);
        let aad = derive_cell_id(key_bytes);
        let aead = AeadKind::XChaCha20Poly1305;
        match self.inner.get(&hex_key).await? {
            None => Ok(None),
            Some(b64) => {
                let ct = decode_base64(&b64)
                    .ok_or_else(|| PoolError::Build("base64 decode failed".into()))?;
                let pt = decrypt_with_aead(&aead, &self.key, &aad, &ct)
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(Some(pt))
            }
        }
    }

    /// Delete the entry for `key_bytes`.  No-op if absent.
    async fn delete(&self, key_bytes: &[u8]) -> Result<(), PoolError> {
        let hex_key = encode_hex(key_bytes);
        self.inner.delete(&hex_key).await.map(|_| ())
    }

    /// Return the raw (encrypted, base64) value stored for `key_bytes`.
    ///
    /// Used in tests to verify ciphertext-at-rest behaviour.
    async fn get_raw(&self, key_bytes: &[u8]) -> Result<Option<String>, PoolError> {
        let hex_key = encode_hex(key_bytes);
        self.inner.get(&hex_key).await
    }
}

// ── Encoding helpers ──────────────────────────────────────────────────────────

/// Encode `bytes` as lowercase hex.
fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// Encode `bytes` as standard base64 (no padding stripped; pure ASCII).
fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let v = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((v >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((v >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((v >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(v & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Decode base64-encoded `s` back to bytes.
///
/// Uses the RFC 4648 base64 alphabet.  Returns `None` for invalid input.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    fn char_to_val(c: char) -> Option<u8> {
        match c {
            'A'..='Z' => Some(c as u8 - b'A'),
            'a'..='z' => Some(c as u8 - b'a' + 26),
            '0'..='9' => Some(c as u8 - b'0' + 52),
            '+' => Some(62),
            '/' => Some(63),
            _ => None,
        }
    }

    let chars: Vec<char> = s.chars().collect();
    if !chars.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(chars.len() / 4 * 3);
    for chunk in chars.chunks(4) {
        let c0 = char_to_val(chunk[0])? as u32;
        let c1 = char_to_val(chunk[1])? as u32;
        // byte 0: top 6 bits from c0 | bottom 2 bits = top 2 bits of c1
        let byte0 = ((c0 << 2) | (c1 >> 4)) as u8;
        out.push(byte0);
        if chunk[2] != '=' {
            let c2 = char_to_val(chunk[2])? as u32;
            // byte 1: bottom 4 bits of c1 | top 4 bits of c2
            let byte1 = (((c1 & 0xf) << 4) | (c2 >> 2)) as u8;
            out.push(byte1);
        }
        if chunk[3] != '=' {
            let c2 = char_to_val(chunk[2])? as u32;
            let c3 = char_to_val(chunk[3])? as u32;
            // byte 2: bottom 2 bits of c2 | all 6 bits of c3
            let byte2 = (((c2 & 0x3) << 6) | c3) as u8;
            out.push(byte2);
        }
    }
    Some(out)
}

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Create a fresh in-memory pool and initialise the `enc_kv` table.
async fn make_store(key: [u8; 32]) -> EncryptedPooledStore {
    let pool = EmbeddedPool::new();
    let oxidb_pool = Arc::new(OxidbPool::Embedded(pool));
    let store = EncryptedPooledStore::new(Arc::clone(&oxidb_pool), key);
    // Initialise the backing table.
    OxidbKvStore::new(Arc::clone(&oxidb_pool), Some("enc_kv"))
        .init()
        .await
        .expect("init enc_kv table");
    store
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Basic round-trip: `put` then `get` returns the original plaintext.
#[tokio::test]
async fn pooled_encrypt_basic_roundtrip() {
    let store = make_store([0x11u8; 32]).await;
    store.put(b"hello", b"world").await.expect("put");
    let got = store.get(b"hello").await.expect("get").expect("Some");
    assert_eq!(got, b"world");
}

/// Ciphertext-at-rest: the raw SQL value must not equal the plaintext.
#[tokio::test]
async fn pooled_ciphertext_at_rest() {
    let plaintext = b"secret_payload_do_not_store_plain";

    let store = make_store([0x22u8; 32]).await;
    store.put(b"secret_key", plaintext).await.expect("put");

    let raw = store
        .get_raw(b"secret_key")
        .await
        .expect("get_raw")
        .expect("Some raw");

    // The raw stored value must be base64 of nonce+ciphertext+tag — never plaintext.
    assert_ne!(
        raw.as_bytes(),
        plaintext,
        "plaintext found in raw SQL store — encryption not applied"
    );
    // The decoded ciphertext must be longer than the plaintext (nonce + tag overhead).
    let ct = decode_base64(&raw).expect("valid base64");
    assert!(
        ct.len() > plaintext.len(),
        "ciphertext ({}) not longer than plaintext ({}) — expected nonce+tag overhead",
        ct.len(),
        plaintext.len()
    );
}

/// Multi-key isolation: distinct keys produce independently encrypted values.
/// Swapping the stored ciphertexts would cause authentication failure.
#[tokio::test]
async fn pooled_multi_key_isolation() {
    let store = make_store([0x33u8; 32]).await;

    store.put(b"key_a", b"value_alpha").await.expect("put a");
    store.put(b"key_b", b"value_beta").await.expect("put b");

    let raw_a = store.get_raw(b"key_a").await.expect("raw_a").expect("Some");
    let raw_b = store.get_raw(b"key_b").await.expect("raw_b").expect("Some");
    assert_ne!(
        raw_a, raw_b,
        "distinct keys must yield distinct ciphertexts"
    );

    let dec_a = store.get(b"key_a").await.expect("get a").expect("Some a");
    let dec_b = store.get(b"key_b").await.expect("get b").expect("Some b");
    assert_eq!(dec_a, b"value_alpha");
    assert_eq!(dec_b, b"value_beta");
}

/// Absent key returns `None` without error.
#[tokio::test]
async fn pooled_absent_key_returns_none() {
    let store = make_store([0x44u8; 32]).await;
    let got = store.get(b"not_stored").await.expect("get absent");
    assert!(got.is_none(), "absent key must return None");
}

/// Delete removes the entry; subsequent get returns None.
#[tokio::test]
async fn pooled_delete_removes_entry() {
    let store = make_store([0x55u8; 32]).await;

    store.put(b"del_key", b"del_val").await.expect("put");
    let before = store.get(b"del_key").await.expect("get before delete");
    assert!(before.is_some(), "entry must exist before delete");

    store.delete(b"del_key").await.expect("delete");
    let after = store.get(b"del_key").await.expect("get after delete");
    assert!(after.is_none(), "entry must be absent after delete");
}

/// Authentication failure: corrupting the stored ciphertext causes `get` to error.
#[tokio::test]
async fn pooled_authentication_failure_on_tampered_ciphertext() {
    let pool = EmbeddedPool::new();
    let oxidb_pool = Arc::new(OxidbPool::Embedded(pool));
    let store = EncryptedPooledStore::new(Arc::clone(&oxidb_pool), [0x66u8; 32]);
    OxidbKvStore::new(Arc::clone(&oxidb_pool), Some("enc_kv"))
        .init()
        .await
        .expect("init");

    store
        .put(b"tamper_key", b"tamper_value")
        .await
        .expect("put");

    // Retrieve the raw ciphertext, corrupt one byte, re-encode, write back.
    let raw = store
        .get_raw(b"tamper_key")
        .await
        .expect("get_raw")
        .expect("Some raw");
    let mut ct = decode_base64(&raw).expect("decode");
    // Flip a byte in the ciphertext body (after the 24-byte nonce).
    let tamper_idx = 25.min(ct.len() - 1);
    ct[tamper_idx] ^= 0xFF;
    let tampered_b64 = encode_base64(&ct);

    // Write the tampered value directly via the inner unencrypted store.
    let hex_key = encode_hex(b"tamper_key");
    let inner = OxidbKvStore::new(Arc::clone(&oxidb_pool), Some("enc_kv"));
    inner
        .set(&hex_key, &tampered_b64)
        .await
        .expect("write tampered");

    // Decryption must fail with an authentication error.
    let result = store.get(b"tamper_key").await;
    assert!(
        result.is_err(),
        "tampered ciphertext must cause an authentication error"
    );
}

/// Empty-value round-trip: zero-length plaintext encrypts and decrypts correctly.
#[tokio::test]
async fn pooled_empty_value_roundtrip() {
    let store = make_store([0x77u8; 32]).await;
    store.put(b"empty_key", b"").await.expect("put empty value");
    let got = store
        .get(b"empty_key")
        .await
        .expect("get empty")
        .expect("Some");
    assert_eq!(got, b"", "empty value must round-trip correctly");
}
