//! Seeded randomness for the fake world: SplitMix64.
//!
//! The fake world must be reproducible: a seed reproduces a run exactly, and
//! 50 seeds give the 50-run samples the first-night skills are tested on.

#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        let sample = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        sample < p
    }

    /// A value in `low..=high`.
    pub fn range(&mut self, low: u32, high: u32) -> u32 {
        debug_assert!(low <= high);
        low + (self.next_u64() % u64::from(high - low + 1)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_reproducible() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn range_stays_in_bounds() {
        let mut rng = Rng::new(1);
        for _ in 0..1000 {
            let value = rng.range(3, 9);
            assert!((3..=9).contains(&value));
        }
    }
}
