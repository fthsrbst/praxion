//! A fast hasher for the internal visibility caches.
//!
//! The caches are keyed by block positions and asked once per candidate block,
//! which is thousands of lookups in a tick that spends its 3,000-ray budget.
//! The default `SipHash` costs several times more than the lookup it guards;
//! this is the multiply-and-rotate hash rustc uses for the same reason
//! (`rustc_hash`), not a new algorithm. It is not used for anything an
//! attacker controls the keys of in a way that could spread buckets badly: the
//! keys are block positions in the world we are looking at.

use std::hash::{BuildHasher, Hasher};

/// One multiply and rotate per machine word.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FxHasher {
    hash: u64,
}

impl FxHasher {
    const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(Self::SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_ne_bytes(word));
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    #[inline]
    fn write_i8(&mut self, value: i8) {
        self.add(u64::from(value as u8));
    }

    #[inline]
    fn write_i16(&mut self, value: i16) {
        self.add(u64::from(value as u16));
    }

    #[inline]
    fn write_i32(&mut self, value: i32) {
        self.add(u64::from(value as u32));
    }

    #[inline]
    fn write_i64(&mut self, value: i64) {
        self.add(value as u64);
    }

    #[inline]
    fn write_isize(&mut self, value: isize) {
        self.add(value as u64);
    }
}

/// Builds [`FxHasher`]s.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FxBuildHasher;

impl BuildHasher for FxBuildHasher {
    type Hasher = FxHasher;

    #[inline]
    fn build_hasher(&self) -> FxHasher {
        FxHasher::default()
    }
}

#[cfg(test)]
mod tests {
    use std::hash::{BuildHasher, Hash};

    use super::*;

    fn hash<T: Hash>(value: &T) -> u64 {
        FxBuildHasher.hash_one(value)
    }

    #[test]
    fn neighbouring_positions_hash_apart() {
        // Block positions differ in their low bits; the hash must spread them,
        // or every cache bucket would hold a run of neighbours.
        let mut buckets = std::collections::HashSet::new();
        for x in 0..64 {
            for z in 0..64 {
                buckets.insert(hash(&(x as i64, 0i64, z as i64)));
            }
        }
        assert_eq!(buckets.len(), 64 * 64, "the hash collides on small offsets");
    }
}
