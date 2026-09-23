//! Complex forward transform.
//!
//! Iterative radix two decimation in time with precomputed bit reversal and
//! twiddle tables. Size is fixed at construction, which is what the spectrum
//! path needs: the table cost is paid once and the inner loop touches nothing
//! but two float arrays.
//!
//! A real input transform would halve the work, but the whole spectrum path
//! costs well under a millisecond per second of audio at any usable setting,
//! so the simpler formulation wins.

pub struct Fft {
    n: usize,
    /// Destination index of every input position after bit reversal.
    rev: Vec<u32>,
    /// Twiddle factors for half the transform, shared by every stage through
    /// the stride computed from the stage size.
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Fft {
    /// Size must be a power of two and at least two.
    pub fn new(n: usize) -> Fft {
        let n = n.max(2).next_power_of_two();
        let levels = n.trailing_zeros();

        let mut rev = vec![0u32; n];
        for (i, slot) in rev.iter_mut().enumerate() {
            *slot = (i as u32).reverse_bits() >> (32 - levels);
        }

        let half = n / 2;
        let mut cos = vec![0.0f32; half];
        let mut sin = vec![0.0f32; half];
        for k in 0..half {
            // Negative sign gives the forward transform.
            let a = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
            cos[k] = a.cos() as f32;
            sin[k] = a.sin() as f32;
        }

        Fft { n, rev, cos, sin }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// In place transform. Both arrays must hold exactly len samples.
    pub fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        debug_assert_eq!(re.len(), n);
        debug_assert_eq!(im.len(), n);

        // Permute into bit reversed order. Swapping only when the destination
        // is above the source visits every pair exactly once.
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }

        let mut size = 2usize;
        while size <= n {
            let half = size / 2;
            let stride = n / size;
            let mut base = 0usize;
            while base < n {
                let mut k = 0usize;
                for j in base..base + half {
                    let l = j + half;
                    let wr = self.cos[k];
                    let wi = self.sin[k];
                    let tr = re[l] * wr - im[l] * wi;
                    let ti = re[l] * wi + im[l] * wr;
                    re[l] = re[j] - tr;
                    im[l] = im[j] - ti;
                    re[j] += tr;
                    im[j] += ti;
                    k += stride;
                }
                base += size;
            }
            size <<= 1;
        }
    }
}