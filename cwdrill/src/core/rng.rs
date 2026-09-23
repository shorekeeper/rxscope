//! Pseudorandom source.
//!
//! Xorshift with a sixty four bit state. The whole of what a trainer needs a
//! generator for is choosing a character and perturbing a duration, and neither
//! is a cryptographic question; what both need is to be cheap enough to call a
//! few thousand times per second of material and reproducible enough that a
//! stated seed replays a session.
//!
//! The period is two to the sixty four less one, which at one draw per element
//! is longer than any operator will practise for.

/// Multiplier of the final scramble.
///
/// Xorshift alone passes the tests that matter here but leaves the low bits
/// weaker than the high ones, and the low bits are exactly what a small range
/// selection reads. One multiply removes the correlation, which is why the
/// range helper below takes the high bits rather than a remainder.
const SCRAMBLE: u64 = 0x2545_F491_4F6C_DD1D;

pub struct Rng(u64);

impl Rng {
    /// Seeds from a stated value. Nought is replaced, because the recurrence
    /// has a fixed point there and would produce nothing but nought.
    pub fn new(seed: u64) -> Rng {
        Rng(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    /// Seeds from the performance counter, for a session nobody asked to
    /// replay.
    pub fn from_clock() -> Rng {
        Rng::new(crate::core::time::ticks() as u64)
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(SCRAMBLE)
    }

    /// Uniform in the half open unit interval.
    ///
    /// Built from the top fifty three bits, which is the mantissa a double
    /// holds; taking fewer would quantize the result visibly on a fine jitter
    /// setting.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    #[inline]
    pub fn next_f32(&mut self) -> f32 {
        self.next_f64() as f32
    }

    /// Uniform in minus one to plus one.
    #[inline]
    pub fn symmetric(&mut self) -> f32 {
        (self.next_f64() * 2.0 - 1.0) as f32
    }

    /// Index below the bound, nought when the bound is nought.
    ///
    /// Multiply and shift rather than a remainder: a remainder biases towards
    /// the low end whenever the bound does not divide the word, and for a bound
    /// of twenty six that bias is measurable over a session.
    #[inline]
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        ((self.next_u64() as u128 * bound as u128) >> 64) as usize
    }

    /// Inclusive range.
    #[inline]
    pub fn between(&mut self, lo: usize, hi: usize) -> usize {
        if hi <= lo {
            return lo;
        }
        lo + self.below(hi - lo + 1)
    }

    /// True with the stated probability.
    #[inline]
    pub fn chance(&mut self, p: f32) -> bool {
        self.next_f32() < p
    }
}

impl Default for Rng {
    fn default() -> Rng {
        Rng::from_clock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stated_seed_replays() {
        // The one property a trainer needs from this: an operator reporting a
        // group that read badly can be given the same group back.
        let mut a = Rng::new(12345);
        let mut b = Rng::new(12345);
        for _ in 0..64 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn the_range_covers_its_bound_evenly() {
        // A remainder based selection leaves the last few indices short by a
        // measurable amount over this many draws, which over a session means
        // the last letters of the set are practised less.
        let mut rng = Rng::new(7);
        let mut counts = [0u32; 26];
        for _ in 0..26_000 {
            counts[rng.below(26)] += 1;
        }
        for (i, &c) in counts.iter().enumerate() {
            assert!(c > 800 && c < 1200, "index {} drawn {} times", i, c);
        }
    }

    #[test]
    fn the_unit_interval_stays_inside_itself() {
        let mut rng = Rng::new(99);
        for _ in 0..10_000 {
            let v = rng.next_f32();
            assert!((0.0..1.0).contains(&v));
            let s = rng.symmetric();
            assert!((-1.0..1.0).contains(&s));
        }
    }
}