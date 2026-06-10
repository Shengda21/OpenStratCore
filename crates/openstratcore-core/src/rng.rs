//! The ONLY source of randomness in the engine. Seedable and deterministic.

use rand::{Rng as _, SeedableRng};
use rand_pcg::Pcg64Mcg;

/// Deterministic RNG abstraction. Dice helpers map directly to the ruleset's d6 lookups.
pub trait Rng {
    /// Uniform integer in [0, n).
    fn next_u32_below(&mut self, n: u32) -> u32;

    /// One six-sided die, 1..=6.
    fn d6(&mut self) -> u32 {
        self.next_u32_below(6) + 1
    }

    /// Sum of `count` six-sided dice (the ruleset uses 1 or 2 dice for "随机数").
    fn roll_sum(&mut self, count: u32) -> u32 {
        (0..count).map(|_| self.d6()).sum()
    }
}

/// PCG-backed implementation. Construct from a u64 seed for reproducibility.
pub struct PcgRng {
    inner: Pcg64Mcg,
}

impl PcgRng {
    pub fn from_seed(seed: u64) -> Self {
        Self {
            inner: Pcg64Mcg::seed_from_u64(seed),
        }
    }
}

impl Rng for PcgRng {
    fn next_u32_below(&mut self, n: u32) -> u32 {
        debug_assert!(n > 0);
        self.inner.gen_range(0..n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = PcgRng::from_seed(42);
        let mut b = PcgRng::from_seed(42);
        for _ in 0..1000 {
            assert_eq!(a.roll_sum(2), b.roll_sum(2));
        }
    }

    #[test]
    fn dice_in_range() {
        let mut r = PcgRng::from_seed(7);
        for _ in 0..1000 {
            let d = r.d6();
            assert!((1..=6).contains(&d));
            let s = r.roll_sum(2);
            assert!((2..=12).contains(&s));
        }
    }
}
