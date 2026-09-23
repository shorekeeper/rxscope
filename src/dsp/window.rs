//! Analysis window generation.
//!
//! All windows are the symmetric form, sampled over n minus one, which is what
//! a spectrum display wants: the periodic form is meant for overlap add
//! synthesis and biases the amplitude of a tone sitting on a bin centre.

use crate::config::settings::WindowFn;

/// Builds the coefficient table. Beta only matters for the Kaiser window.
pub fn build(kind: WindowFn, n: usize, beta: f32) -> Vec<f32> {
    let mut w = vec![0.0f32; n];
    if n == 0 {
        return w;
    }
    if n == 1 {
        w[0] = 1.0;
        return w;
    }

    let denom = (n - 1) as f64;
    let i0_beta = bessel_i0(beta as f64);

    for (i, slot) in w.iter_mut().enumerate() {
        let t = i as f64 / denom;
        let x = std::f64::consts::TAU * t;
        let value = match kind {
            WindowFn::Rectangular => 1.0,
            WindowFn::Hann => 0.5 - 0.5 * x.cos(),
            WindowFn::Hamming => 0.54 - 0.46 * x.cos(),
            WindowFn::Blackman => 0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos(),
            WindowFn::BlackmanHarris => {
                0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos()
            }
            WindowFn::Nuttall => {
                0.355768 - 0.487396 * x.cos() + 0.144232 * (2.0 * x).cos()
                    - 0.012604 * (3.0 * x).cos()
            }
            // Flat top trades resolution for amplitude accuracy, which is what
            // a calibration measurement needs.
            WindowFn::FlatTop => {
                0.21557895 - 0.41663158 * x.cos() + 0.277263158 * (2.0 * x).cos()
                    - 0.083578947 * (3.0 * x).cos()
                    + 0.006947368 * (4.0 * x).cos()
            }
            WindowFn::Kaiser => {
                let r = 2.0 * t - 1.0;
                let arg = (beta as f64) * (1.0 - r * r).max(0.0).sqrt();
                if i0_beta > 0.0 {
                    bessel_i0(arg) / i0_beta
                } else {
                    1.0
                }
            }
        };
        *slot = value as f32;
    }
    w
}

/// Sum of the coefficients. A windowed transform bin has to be divided by half
/// of this to read a full scale sine as zero decibels.
pub fn sum(w: &[f32]) -> f32 {
    w.iter().sum()
}

/// Modified Bessel function of the first kind, order zero, by its series. The
/// term ratio falls fast enough that fifty terms cover every beta the interface
/// allows.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0f64;
    let mut term = 1.0f64;
    let half = x / 2.0;
    for k in 1..50 {
        let f = half / k as f64;
        term *= f * f;
        sum += term;
        if term < sum * 1e-14 {
            break;
        }
    }
    sum
}