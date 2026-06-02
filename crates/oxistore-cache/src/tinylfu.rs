/// Window TinyLFU (W-TinyLFU) cache implementation.
///
/// Implements the algorithm described in:
///   Einziger, G. & Friedman, R. (2017). TinyLFU: A Highly Efficient Cache
///   Admission Policy. ACM Trans. Storage 13(4).
///
/// ## Architecture
///
/// The cache is partitioned into two regions:
///
/// - **Window** (~1% of total capacity): a small LRU buffer.  New items enter
///   here; popular items are promoted to the main space.
/// - **Main** (~99% of total capacity): split between:
///   - **Protected** (80% of main): hot items that have been accessed at least
///     twice and promoted from probation.
///   - **Probation** (remaining main): items evicted from the window that
///     survived the admission gate.
///
/// ## Frequency Estimation
///
/// A [`CountMinSketch`] with a [`Doorkeeper`] bloom filter tracks access
/// frequency.  The doorkeeper prevents one-hit wonders from inflating counters.
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use hashlink::LinkedHashMap;

use crate::sketch::{CountMinSketch, Doorkeeper};
use crate::{Cache, CacheEntry};

// ---------------------------------------------------------------------------
// Key → bytes helper for the sketch
// ---------------------------------------------------------------------------

/// A `Hasher` that accumulates bytes via FNV-1a for use as sketch input.
struct KeyHasher {
    state: u64,
    seed: u64,
}

impl KeyHasher {
    fn new(seed: u64) -> Self {
        KeyHasher {
            state: 0xcbf29ce484222325u64 ^ seed,
            seed,
        }
    }
}

impl Hasher for KeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= b as u64;
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }

    fn finish(&self) -> u64 {
        self.state ^ self.seed
    }
}

/// Hash a key to a 16-byte representation for the sketch.
fn key_bytes<K: Hash>(key: &K) -> [u8; 16] {
    let mut h1 = KeyHasher::new(0xcbf29ce484222325);
    key.hash(&mut h1);
    let a = h1.finish();

    let mut h2 = KeyHasher::new(0x6c62272e07bb0142);
    key.hash(&mut h2);
    let b = h2.finish();

    let mut buf = [0u8; 16];
    buf[..8].copy_from_slice(&a.to_le_bytes());
    buf[8..].copy_from_slice(&b.to_le_bytes());
    buf
}

// ---------------------------------------------------------------------------
// XOR-shift RNG for tie-breaking
// ---------------------------------------------------------------------------

fn xorshift64(x: u64) -> u64 {
    let x = x ^ (x << 13);
    let x = x ^ (x >> 7);
    x ^ (x << 17)
}

// ---------------------------------------------------------------------------
// Cache segment tag
// ---------------------------------------------------------------------------

/// Identifies which segment a key currently resides in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheSegment {
    Window,
    Probation,
    Protected,
}

// ---------------------------------------------------------------------------
// WTinyLfuCache
// ---------------------------------------------------------------------------

/// Window TinyLFU cache with near-optimal hit rates on skewed workloads.
///
/// # Capacity split
///
/// ```text
/// window_cap    = max(1, total_cap / 100)
/// main_cap      = total_cap - window_cap
/// protected_cap = main_cap * 4 / 5
/// ```
///
/// # Type parameters
///
/// - `K`: key type — must be `Eq + Hash + Clone`.
/// - `V`: value type.
pub struct WTinyLfuCache<K, V> {
    total_cap: usize,
    window_cap: usize,
    main_cap: usize,
    protected_cap: usize,

    /// Window LRU segment.  Back = MRU, front = LRU (next candidate for eviction).
    window: LinkedHashMap<K, CacheEntry<V>, ahash::RandomState>,
    /// Protected segment (hot items).  Back = MRU, front = LRU.
    protected: LinkedHashMap<K, CacheEntry<V>, ahash::RandomState>,
    /// Probation segment.  Back = MRU, front = LRU (eviction candidate).
    probation: LinkedHashMap<K, CacheEntry<V>, ahash::RandomState>,

    /// Presence/routing index: maps each key to its current segment.
    segment_index: HashMap<K, CacheSegment, ahash::RandomState>,

    /// Count-Min Sketch for frequency estimation.
    sketch: CountMinSketch,
    /// Doorkeeper bloom filter.
    doorkeeper: Doorkeeper,

    /// XOR-shift RNG state for admission tie-breaking.
    rng: u64,
}

impl<K, V> WTinyLfuCache<K, V>
where
    K: Eq + Hash + Clone,
{
    /// Create a new `WTinyLfuCache` with the given total capacity.
    ///
    /// Minimum capacity is 2.
    #[must_use]
    pub fn new(total_cap: usize) -> Self {
        let total_cap = total_cap.max(2);
        let window_cap = (total_cap / 100).max(1);
        let main_cap = total_cap - window_cap;
        let protected_cap = main_cap * 4 / 5;
        let rng = total_cap as u64 ^ 0xdeadbeef_cafebabe;

        WTinyLfuCache {
            total_cap,
            window_cap,
            main_cap,
            protected_cap,
            window: LinkedHashMap::with_hasher(ahash::RandomState::default()),
            protected: LinkedHashMap::with_hasher(ahash::RandomState::default()),
            probation: LinkedHashMap::with_hasher(ahash::RandomState::default()),
            segment_index: HashMap::with_hasher(ahash::RandomState::default()),
            sketch: CountMinSketch::new(total_cap),
            doorkeeper: Doorkeeper::new(total_cap),
            rng,
        }
    }

    // -----------------------------------------------------------------------
    // Sketch helpers
    // -----------------------------------------------------------------------

    /// Record an access in the doorkeeper + sketch.
    fn record_access(&mut self, key: &K) {
        let kb = key_bytes(key);
        let seen_before = self.doorkeeper.put(&kb);
        if seen_before {
            self.sketch.increment(&kb);
        }
    }

    /// Estimated access frequency for `key`.
    fn freq(&self, key: &K) -> u64 {
        let kb = key_bytes(key);
        self.sketch.estimate(&kb)
    }

    // -----------------------------------------------------------------------
    // TTL helpers
    // -----------------------------------------------------------------------

    /// Lazily remove `key` if its entry has expired.  Returns `true` if removed.
    fn remove_if_expired(&mut self, key: &K) -> bool {
        let seg = match self.segment_index.get(key).copied() {
            Some(s) => s,
            None => return false,
        };

        let expired = match seg {
            CacheSegment::Window => self
                .window
                .get(key)
                .map(|e| e.is_expired())
                .unwrap_or(false),
            CacheSegment::Probation => self
                .probation
                .get(key)
                .map(|e| e.is_expired())
                .unwrap_or(false),
            CacheSegment::Protected => self
                .protected
                .get(key)
                .map(|e| e.is_expired())
                .unwrap_or(false),
        };

        if expired {
            self.segment_index.remove(key);
            match seg {
                CacheSegment::Window => {
                    self.window.remove(key);
                }
                CacheSegment::Probation => {
                    self.probation.remove(key);
                }
                CacheSegment::Protected => {
                    self.protected.remove(key);
                }
            }
        }
        expired
    }

    // -----------------------------------------------------------------------
    // Segment promotion helpers
    // -----------------------------------------------------------------------

    /// Move `key` to the MRU tail of the window segment.
    fn promote_in_window(&mut self, key: &K) {
        if let Some(entry) = self.window.remove(key) {
            self.window.insert(key.clone(), entry);
        }
    }

    /// Move `key` to the MRU tail of the protected segment.
    fn promote_in_protected(&mut self, key: &K) {
        if let Some(entry) = self.protected.remove(key) {
            self.protected.insert(key.clone(), entry);
        }
    }

    /// Move `key` from probation → protected (MRU tail).
    /// If protected is now over capacity, demote its LRU front to probation MRU tail.
    fn promote_probation_to_protected(&mut self, key: &K) {
        if let Some(entry) = self.probation.remove(key) {
            self.segment_index
                .insert(key.clone(), CacheSegment::Protected);
            self.protected.insert(key.clone(), entry);

            // Demote protected tail to probation if over capacity.
            if self.protected.len() > self.protected_cap {
                if let Some((demoted_key, demoted_entry)) = self.protected.pop_front() {
                    self.segment_index
                        .insert(demoted_key.clone(), CacheSegment::Probation);
                    self.probation.insert(demoted_key, demoted_entry);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Window eviction + admission gate
    // -----------------------------------------------------------------------

    /// Evict the LRU item from the window, returning it (key + entry) if over cap.
    fn evict_from_window_if_over_cap(&mut self) -> Option<(K, CacheEntry<V>)> {
        if self.window.len() > self.window_cap {
            if let Some((k, entry)) = self.window.pop_front() {
                self.segment_index.remove(&k);
                return Some((k, entry));
            }
        }
        None
    }

    /// Run the admission gate for `window_candidate` (just evicted from window).
    ///
    /// If main space is not full, accept immediately (move to probation).
    /// Otherwise compare frequencies; higher-frequency item stays; ties use RNG.
    fn run_admission_gate(&mut self, candidate_key: K, candidate_entry: CacheEntry<V>) {
        let main_size = self.probation.len() + self.protected.len();
        if main_size < self.main_cap {
            // Main not full — accept unconditionally.
            self.segment_index
                .insert(candidate_key.clone(), CacheSegment::Probation);
            self.probation.insert(candidate_key, candidate_entry);
            return;
        }

        // Probation tail = main victim.
        let victim_key = match self.probation.front() {
            Some((k, _)) => k.clone(),
            None => {
                // Probation empty (all in protected) — discard candidate.
                return;
            }
        };

        let candidate_freq = self.freq(&candidate_key);
        let victim_freq = self.freq(&victim_key);

        let admit = if candidate_freq > victim_freq {
            true
        } else if candidate_freq == victim_freq {
            self.rng = xorshift64(self.rng);
            (self.rng & 0xFF) < 128
        } else {
            false
        };

        if admit {
            // Evict victim, admit candidate.
            self.probation.pop_front();
            self.segment_index.remove(&victim_key);

            self.segment_index
                .insert(candidate_key.clone(), CacheSegment::Probation);
            self.probation.insert(candidate_key, candidate_entry);
        }
        // else: candidate is discarded (not inserted anywhere).
    }

    // -----------------------------------------------------------------------
    // Core insert
    // -----------------------------------------------------------------------

    fn insert_entry(&mut self, key: K, entry: CacheEntry<V>) -> Option<V> {
        // Update existing key in-place.
        if let Some(seg) = self.segment_index.get(&key).copied() {
            self.record_access(&key);
            match seg {
                CacheSegment::Window => {
                    if let Some(existing) = self.window.get_mut(&key) {
                        existing.expires_at = entry.expires_at;
                        existing.value = entry.value;
                    }
                    self.promote_in_window(&key);
                }
                CacheSegment::Probation => {
                    if let Some(existing) = self.probation.get_mut(&key) {
                        existing.expires_at = entry.expires_at;
                        existing.value = entry.value;
                    }
                    self.promote_probation_to_protected(&key);
                }
                CacheSegment::Protected => {
                    if let Some(existing) = self.protected.get_mut(&key) {
                        existing.expires_at = entry.expires_at;
                        existing.value = entry.value;
                    }
                    self.promote_in_protected(&key);
                }
            }
            return None;
        }

        // New key.
        self.record_access(&key);
        self.segment_index.insert(key.clone(), CacheSegment::Window);
        self.window.insert(key.clone(), entry);

        // Drain window if over cap (run admission gate).
        if self.window.len() > self.window_cap {
            if let Some((candidate_key, candidate_entry)) = self.evict_from_window_if_over_cap() {
                self.run_admission_gate(candidate_key, candidate_entry);
            }
        }

        None
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Look up `key`, updating recency and frequency.
    ///
    /// Expired entries are lazily removed and treated as misses.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        if self.remove_if_expired(key) {
            return None;
        }

        let seg = self.segment_index.get(key).copied()?;

        self.record_access(key);

        match seg {
            CacheSegment::Window => {
                self.promote_in_window(key);
                self.window.get(key).map(|e| &e.value)
            }
            CacheSegment::Probation => {
                // Promote from probation to protected.
                self.promote_probation_to_protected(key);
                self.protected.get(key).map(|e| &e.value)
            }
            CacheSegment::Protected => {
                self.promote_in_protected(key);
                self.protected.get(key).map(|e| &e.value)
            }
        }
    }

    /// Insert or update `key` -> `value` without TTL.
    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        self.insert_entry(key, CacheEntry::new(value))
    }

    /// Insert or update `key` -> `value` with a TTL.
    pub fn put_with_ttl(&mut self, key: K, value: V, ttl: std::time::Duration) -> Option<V> {
        self.insert_entry(key, CacheEntry::with_ttl(value, ttl))
    }

    /// Total number of live entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.window.len() + self.protected.len() + self.probation.len()
    }

    /// Return `true` if the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total capacity.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.total_cap
    }

    /// Read a value without updating recency or frequency.
    ///
    /// Returns `None` if absent or expired.
    #[must_use]
    pub fn peek(&self, key: &K) -> Option<&V> {
        let seg = self.segment_index.get(key)?;
        let entry = match seg {
            CacheSegment::Window => self.window.get(key)?,
            CacheSegment::Probation => self.probation.get(key)?,
            CacheSegment::Protected => self.protected.get(key)?,
        };
        if entry.is_expired() {
            None
        } else {
            Some(&entry.value)
        }
    }

    /// Return `true` if `key` is present and not expired (no recency update).
    #[must_use]
    pub fn contains_key(&self, key: &K) -> bool {
        let seg = match self.segment_index.get(key) {
            Some(s) => s,
            None => return false,
        };
        let entry = match seg {
            CacheSegment::Window => self.window.get(key),
            CacheSegment::Probation => self.probation.get(key),
            CacheSegment::Protected => self.protected.get(key),
        };
        entry.map(|e| !e.is_expired()).unwrap_or(false)
    }

    /// Remove the entry for `key`, returning its value if present.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let seg = self.segment_index.remove(key)?;
        let entry = match seg {
            CacheSegment::Window => self.window.remove(key)?,
            CacheSegment::Probation => self.probation.remove(key)?,
            CacheSegment::Protected => self.protected.remove(key)?,
        };
        Some(entry.value)
    }

    /// Remove all entries from the cache.
    pub fn clear(&mut self) {
        self.window.clear();
        self.protected.clear();
        self.probation.clear();
        self.segment_index.clear();
        self.sketch.clear();
        self.doorkeeper.clear();
    }

    /// Dynamically resize the cache capacity.
    ///
    /// Recomputes segment sizes and evicts LRU entries as needed.
    pub fn resize(&mut self, new_cap: usize) {
        let new_cap = new_cap.max(2);
        self.total_cap = new_cap;
        self.window_cap = (new_cap / 100).max(1);
        self.main_cap = new_cap - self.window_cap;
        self.protected_cap = self.main_cap * 4 / 5;

        while self.window.len() > self.window_cap {
            if let Some((k, _)) = self.window.pop_front() {
                self.segment_index.remove(&k);
            }
        }
        while self.protected.len() > self.protected_cap {
            if let Some((k, _)) = self.protected.pop_front() {
                self.segment_index.remove(&k);
            }
        }
        let main_cap = self.main_cap;
        while self.probation.len() + self.protected.len() > main_cap {
            if let Some((k, _)) = self.probation.pop_front() {
                self.segment_index.remove(&k);
            }
        }
    }
}

impl<K, V> Cache<K, V> for WTinyLfuCache<K, V>
where
    K: Eq + Hash + Clone,
{
    fn get(&mut self, key: &K) -> Option<&V> {
        WTinyLfuCache::get(self, key)
    }

    fn put(&mut self, key: K, value: V) -> Option<V> {
        WTinyLfuCache::put(self, key, value)
    }

    fn put_with_ttl(&mut self, key: K, value: V, ttl: std::time::Duration) -> Option<V> {
        WTinyLfuCache::put_with_ttl(self, key, value, ttl)
    }

    fn len(&self) -> usize {
        WTinyLfuCache::len(self)
    }

    fn cap(&self) -> usize {
        WTinyLfuCache::cap(self)
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        WTinyLfuCache::remove(self, key)
    }

    fn clear(&mut self) {
        WTinyLfuCache::clear(self);
    }

    fn peek(&self, key: &K) -> Option<&V> {
        WTinyLfuCache::peek(self, key)
    }

    fn contains_key(&self, key: &K) -> bool {
        WTinyLfuCache::contains_key(self, key)
    }

    fn resize(&mut self, new_cap: usize) {
        WTinyLfuCache::resize(self, new_cap);
    }
}
