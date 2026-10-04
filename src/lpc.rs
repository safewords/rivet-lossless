//! Linear prediction analysis shared by the FLAC and ALAC encoders.
//!
//! The textbook autocorrelation method: window the block, take its
//! autocorrelation, and solve the normal equations with the Levinson-Durbin
//! recursion, which yields the predictor of every order up to the maximum in
//! one pass along with each order's prediction error. The coefficients are
//! then quantised to the integer precision the bitstream carries.

/// A Tukey (tapered cosine) window of `n` points whose tapers take `p` of
/// its length in total (`p = 0` is rectangular, `p = 1` is Hann). Tapering
/// the block edges keeps the discontinuity at the block boundary out of the
/// autocorrelation.
pub(crate) fn tukey(n: usize, p: f64) -> Vec<f64> {
    let mut w = vec![1.0; n];
    if n < 2 || p <= 0.0 {
        return w;
    }
    let taper = ((p * (n - 1) as f64) / 2.0).floor() as usize;
    if taper == 0 {
        return w;
    }
    for i in 0..taper {
        let v = 0.5 - 0.5 * (std::f64::consts::PI * i as f64 / taper as f64).cos();
        w[i] = v;
        w[n - 1 - i] = v;
    }
    w
}

/// The autocorrelation of `x · window` at lags `0..=max_lag`.
///
/// Every lag is summed in the same order, sample 0 first, starting from
/// `-0.0` (what `Iterator::sum` starts from), with separate multiplies and
/// adds, so the result is the same to the bit on every CPU and every code
/// path; the vector paths only compute several lags side by side.
pub(crate) fn autocorrelation(x: &[i64], window: &[f64], max_lag: usize) -> Vec<f64> {
    let xw: Vec<f64> = x.iter().zip(window).map(|(&s, &w)| s as f64 * w).collect();
    let mut r = vec![0.0; max_lag + 1];
    if max_lag < AUTOC_LANES {
        autocorrelation_lanes(&xw, &mut r);
    } else {
        autocorrelation_scalar(&xw, &mut r);
    }
    r
}

/// Lags computed side by side: enough for FLAC's order 32.
const AUTOC_LANES: usize = 33;

/// The autocorrelation one lag at a time: the definition.
fn autocorrelation_scalar(xw: &[f64], r: &mut [f64]) {
    for (lag, r) in r.iter_mut().enumerate() {
        *r = if lag >= xw.len() {
            0.0
        } else {
            xw[lag..].iter().zip(xw).map(|(a, b)| a * b).sum()
        };
    }
}

crate::simd::multiversion! {
/// Every lag at once, one sample at a time: `acc[lag] += xw[i + lag] ·
/// xw[i]`, which keeps each lag's own order of summation and vectorises
/// across the lags. Lags whose sum has ended (`i + lag` past the end) drop
/// out in the tail loop, which is the definition's own.
fn autocorrelation_lanes(xw: &[f64], r: &mut [f64]) {
    let lags = r.len();
    let n = xw.len();
    // Up to 8 lags in a narrower accumulator, so the common orders do not
    // carry the work of 33.
    if lags <= 8 {
        autocorrelation_block::<8>(xw, r);
    } else if lags <= 16 {
        autocorrelation_block::<16>(xw, r);
    } else {
        autocorrelation_block::<AUTOC_LANES>(xw, r);
    }
    for (lag, r) in r.iter_mut().enumerate() {
        if lag >= n {
            *r = 0.0;
        }
    }
}
}

#[inline(always)]
fn autocorrelation_block<const L: usize>(xw: &[f64], r: &mut [f64]) {
    let lags = r.len();
    let n = xw.len();
    let mut acc = [-0.0f64; L];
    // Every lag of the block is in range for these samples.
    let body = n.saturating_sub(L - 1);
    for i in 0..body {
        let x = xw[i];
        let ahead: &[f64; L] = xw[i..i + L].try_into().expect("L samples");
        for l in 0..L {
            acc[l] += ahead[l] * x;
        }
    }
    for i in body..n {
        let x = xw[i];
        for l in 0..lags.min(n - i) {
            acc[l] += xw[i + l] * x;
        }
    }
    r.copy_from_slice(&acc[..lags]);
}

/// Levinson-Durbin: the predictor of every order `1..=max_order` for the
/// autocorrelation `r`. `coefs[k - 1]` is the order-`k` predictor, whose
/// element `j` weighs the sample `j + 1` back; `errors[k - 1]` is its
/// residual energy. Stops early (returning fewer orders) when the
/// recursion goes unstable — the signal is fully predicted, or silent.
pub(crate) fn levinson(r: &[f64], max_order: usize) -> (Vec<Vec<f64>>, Vec<f64>) {
    let mut coefs = Vec::with_capacity(max_order);
    let mut errors = Vec::with_capacity(max_order);
    if r.is_empty() || r[0] <= 0.0 {
        return (coefs, errors);
    }
    let mut a: Vec<f64> = Vec::with_capacity(max_order);
    let mut err = r[0];
    for k in 1..=max_order.min(r.len() - 1) {
        let mut acc = r[k];
        for j in 0..k - 1 {
            acc -= a[j] * r[k - 1 - j];
        }
        let reflection = acc / err;
        if !reflection.is_finite() {
            break;
        }
        let prev = a.clone();
        a.push(reflection);
        for j in 0..k - 1 {
            a[j] = prev[j] - reflection * prev[k - 2 - j];
        }
        err *= 1.0 - reflection * reflection;
        coefs.push(a.clone());
        errors.push(err.max(0.0));
        if err <= 0.0 {
            break;
        }
    }
    (coefs, errors)
}

/// `coefs` quantised to signed `precision`-bit integers with a right shift:
/// `c ≈ q / 2^shift`. The shift is the largest in `0..=max_shift` that
/// keeps every coefficient in range. The rounding error of each coefficient
/// is carried into the next, so the quantised filter's response stays close
/// to the real one.
pub(crate) fn quantize(coefs: &[f64], precision: u32, max_shift: i32) -> (Vec<i32>, i32) {
    let limit = (1i64 << (precision - 1)) - 1;
    let cmax = coefs.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    let shift = if cmax <= 0.0 {
        0
    } else {
        // cmax · 2^shift ≤ limit.
        let log2 = cmax.log2().floor() as i32 + 1;
        (precision as i32 - 1 - log2).clamp(0, max_shift)
    };
    let scale = f64::from(1u32 << shift);
    let mut carry = 0.0;
    let q = coefs
        .iter()
        .map(|&c| {
            let want = c * scale + carry;
            let got = (want.round() as i64).clamp(-limit - 1, limit);
            carry = want - got as f64;
            got as i32
        })
        .collect();
    (q, shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levinson_recovers_an_ar2_process() {
        // x[n] = 1.6 x[n-1] - 0.8 x[n-2] + noise.
        let mut x = vec![0i64; 4096];
        let mut seed = 1u32;
        let (mut a, mut b) = (0.0f64, 0.0f64);
        for s in x.iter_mut() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (f64::from(seed >> 8) / f64::from(1u32 << 24) - 0.5) * 200.0;
            let v = 1.6 * a - 0.8 * b + noise;
            b = a;
            a = v;
            *s = v.round() as i64;
        }
        let r = autocorrelation(&x, &vec![1.0; x.len()], 2);
        let (coefs, errors) = levinson(&r, 2);
        assert!((coefs[1][0] - 1.6).abs() < 0.05, "{:?}", coefs[1]);
        assert!((coefs[1][1] + 0.8).abs() < 0.05, "{:?}", coefs[1]);
        assert!(errors[1] < errors[0]);
    }

    #[test]
    fn quantisation_keeps_the_precision() {
        let (q, shift) = quantize(&[1.9, -0.95, 0.01], 12, 15);
        assert_eq!(shift, 10);
        assert!(q.iter().all(|&c| (-2048..2048).contains(&c)), "{q:?}");
        assert!((f64::from(q[0]) / 1024.0 - 1.9).abs() < 1e-3);
        let (q, shift) = quantize(&[0.0, 0.0], 12, 15);
        assert_eq!((q, shift), (vec![0, 0], 0));
    }

    #[test]
    fn autocorrelation_is_the_definition_to_the_bit() {
        let mut seed = 5u32;
        for n in [0usize, 1, 2, 7, 8, 9, 15, 16, 17, 32, 33, 34, 100, 4096] {
            let x: Vec<i64> = (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    i64::from(seed as i32 >> 8)
                })
                .collect();
            let w = tukey(n, 0.5);
            let xw: Vec<f64> = x.iter().zip(&w).map(|(&s, &w)| s as f64 * w).collect();
            for max_lag in [0usize, 1, 7, 8, 12, 15, 16, 31, 32] {
                let got = autocorrelation(&x, &w, max_lag);
                let mut want = vec![0.0; max_lag + 1];
                autocorrelation_scalar(&xw, &mut want);
                let bits = |v: &[f64]| v.iter().map(|f| f.to_bits()).collect::<Vec<_>>();
                assert_eq!(bits(&got), bits(&want), "n {n} lag {max_lag}");
            }
        }
    }

    #[test]
    fn a_tukey_window_tapers_only_the_edges() {
        let w = tukey(100, 0.5);
        assert_eq!(w[0], 0.0);
        assert_eq!(w[50], 1.0);
        assert!(w[10] > 0.0 && w[10] < 1.0);
        assert_eq!(w[99], 0.0);
    }
}
