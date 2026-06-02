//! Count-Min Sketch with 4-bit packed counters and a Doorkeeper bloom filter.
//!
//! Used by W-TinyLFU as the frequency estimator.  The CMS uses 4-bit (nibble)
//! counters packed two-per-byte, with depth d=4 and width w=next_power_of_two(capacity).
//!
//! Frequency estimation uses double hashing (two independent FNV-1a passes) to
//! derive per-row column indices, avoiding the need for d independent hash functions.
//!
//! The Doorkeeper is a compact 4-hash bloom filter placed in front of the CMS.
//! An item's first occurrence only sets the bloom filter bits; only on subsequent
//! occurrences (when the bloom filter returns true) does the CMS counter increment.
//! This prevents one-hit wonders from polluting the CMS.

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------
/// Depth of the Count-Min Sketch (number of hash rows).
const DEPTH: usize = 4;

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf29ce484222325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x100000001b3;

// ---------------------------------------------------------------------------
// FNV-1a helpers
// ---------------------------------------------------------------------------

/// Compute FNV-1a 64-bit hash of `data` with a custom starting state.
fn fnv1a_64(data: &[u8], init: u64) -> u64 {
    let mut h = init;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Derive two independent 64-bit hashes (`h_a`, `h_b`) from a byte slice.
///
/// `h_a` uses the standard FNV offset basis; `h_b` uses a different seed so
/// the two values are uncorrelated.
pub(crate) fn double_hash(data: &[u8]) -> (u64, u64) {
    let h_a = fnv1a_64(data, FNV_OFFSET);
    // XOR with a distinct seed for the second pass.
    let h_b = fnv1a_64(data, FNV_OFFSET ^ 0x6c62272e07bb0142);
    (h_a, h_b)
}

/// Compute the column index for `row` given the two hash components and width.
///
/// Uses the double-hashing formula: `col = (h_a + row * h_b) % w`.
fn row_col(h_a: u64, h_b: u64, row: usize, w: usize) -> usize {
    h_a.wrapping_add((row as u64).wrapping_mul(h_b)) as usize % w
}

// ---------------------------------------------------------------------------
// Nibble helpers
// ---------------------------------------------------------------------------

/// Read the 4-bit value at `(row, col)` from the packed byte slice.
///
/// Layout: byte index = `row * w/2 + col/2`.
/// - col even → lower nibble (bits 3:0)
/// - col odd  → upper nibble (bits 7:4)
fn nibble_get(counters: &[u8], row: usize, col: usize, half_w: usize) -> u8 {
    let byte_idx = row * half_w + col / 2;
    let byte = counters[byte_idx];
    if col.is_multiple_of(2) {
        byte & 0x0F
    } else {
        (byte >> 4) & 0x0F
    }
}

/// Set the 4-bit value at `(row, col)` in the packed byte slice.
fn nibble_set(counters: &mut [u8], row: usize, col: usize, half_w: usize, val: u8) {
    let byte_idx = row * half_w + col / 2;
    if col.is_multiple_of(2) {
        counters[byte_idx] = (counters[byte_idx] & 0xF0) | (val & 0x0F);
    } else {
        counters[byte_idx] = (counters[byte_idx] & 0x0F) | ((val & 0x0F) << 4);
    }
}

// ---------------------------------------------------------------------------
// CountMinSketch
// ---------------------------------------------------------------------------

/// A Count-Min Sketch with 4-bit packed counters.
///
/// The sketch uses 4 rows and a width that is the next power of two at or above
/// `capacity`.  Counters saturate at 15 (no overflow beyond nibble range).
/// Periodic aging (right-shifting all counters by 1) prevents stale counts from
/// dominating.
pub struct CountMinSketch {
    /// Packed nibble storage.  Length = `DEPTH * w / 2`.
    counters: Vec<u8>,
    /// Width (number of columns, always a power of two).
    w: usize,
    /// Half-width (w / 2), used as row stride in the packed layout.
    half_w: usize,
    /// Number of increment calls since the last aging.
    additions: u64,
    /// Threshold at which aging is triggered (capacity × 10).
    sample_size: u64,
}

impl CountMinSketch {
    /// Create a new sketch sized for `capacity` items.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let w = capacity.next_power_of_two().max(1);
        let half_w = w / 2;
        let size = DEPTH * half_w;
        CountMinSketch {
            counters: vec![0u8; size.max(1)],
            w,
            half_w: half_w.max(1),
            additions: 0,
            sample_size: (capacity as u64) * 10,
        }
    }

    /// Increment the counter for `key_bytes`, then age if the sample threshold
    /// has been reached.
    pub fn increment(&mut self, key_bytes: &[u8]) {
        let (h_a, h_b) = double_hash(key_bytes);
        for row in 0..DEPTH {
            let col = row_col(h_a, h_b, row, self.w);
            let cur = nibble_get(&self.counters, row, col, self.half_w);
            if cur < 15 {
                nibble_set(&mut self.counters, row, col, self.half_w, cur + 1);
            }
        }
        self.additions += 1;
        if self.additions >= self.sample_size {
            self.age();
        }
    }

    /// Return the estimated frequency of `key_bytes` (minimum across all rows).
    #[must_use]
    pub fn estimate(&self, key_bytes: &[u8]) -> u64 {
        let (h_a, h_b) = double_hash(key_bytes);
        let mut min_val = u64::MAX;
        for row in 0..DEPTH {
            let col = row_col(h_a, h_b, row, self.w);
            let val = nibble_get(&self.counters, row, col, self.half_w) as u64;
            if val < min_val {
                min_val = val;
            }
        }
        min_val
    }

    /// Age all counters by right-shifting each nibble by 1.
    ///
    /// After aging, each counter is halved, preventing old access patterns from
    /// permanently dominating the estimator.
    pub fn age(&mut self) {
        for byte in self.counters.iter_mut() {
            // Upper nibble (bits 7:4): value = byte >> 4, halved = value >> 1,
            // placed back into upper position = (value >> 1) << 4.
            // Lower nibble (bits 3:0): value = byte & 0x0F, halved = value >> 1.
            let upper_nibble = ((*byte >> 4) >> 1) << 4;
            let lower_nibble = (*byte & 0x0F) >> 1;
            *byte = upper_nibble | lower_nibble;
        }
        self.additions = 0;
    }

    /// Reset the sketch to all zeros.
    pub fn clear(&mut self) {
        for b in self.counters.iter_mut() {
            *b = 0;
        }
        self.additions = 0;
    }
}

// ---------------------------------------------------------------------------
// Doorkeeper (bloom filter)
// ---------------------------------------------------------------------------

/// A simple bloom filter placed in front of the Count-Min Sketch.
///
/// The doorkeeper prevents one-hit-wonder items from inflating CMS counters:
/// the first time a key is seen, only the bloom filter is updated; the CMS is
/// only incremented for keys that have been seen at least twice.
///
/// The filter uses 4 independent hash positions derived via the same
/// double-hashing scheme used by the CMS.
pub struct Doorkeeper {
    /// Bit array stored as `u64` words.
    bits: Vec<u64>,
    /// Total number of bits (`bits.len() * 64`).
    num_bits: usize,
}

impl Doorkeeper {
    /// Create a new `Doorkeeper` sized for `capacity` items.
    ///
    /// The bit array is `next_power_of_two(capacity * 8)` bits wide.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let num_bits = (capacity * 8).next_power_of_two().max(64);
        let num_words = num_bits / 64;
        Doorkeeper {
            bits: vec![0u64; num_words],
            num_bits,
        }
    }

    /// Set the bits for `key_bytes` and return whether they were ALL already set.
    ///
    /// - Returns `false` if this is the first time (at least one bit was unset).
    ///   The caller should NOT increment the CMS.
    /// - Returns `true` if all bits were already set (the key was seen before).
    ///   The caller SHOULD increment the CMS.
    pub fn put(&mut self, key_bytes: &[u8]) -> bool {
        let (h_a, h_b) = double_hash(key_bytes);
        let mut all_set = true;
        for row in 0..DEPTH {
            let pos = row_col(h_a, h_b, row, self.num_bits);
            let word = pos / 64;
            let bit = pos % 64;
            if self.bits[word] & (1u64 << bit) == 0 {
                all_set = false;
                self.bits[word] |= 1u64 << bit;
            }
        }
        all_set
    }

    /// Reset all bits to zero (used after CMS aging).
    pub fn clear(&mut self) {
        for w in self.bits.iter_mut() {
            *w = 0;
        }
    }
}
