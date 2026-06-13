//! Distance measures between z-scored feature vectors.
//!
//! Cosine Delta (a.k.a. Würzburg Delta, Smith & Aldridge 2011; Evert et al.
//! 2017) is the default: it is the best-performing Delta variant in the
//! authorship-attribution literature. Classic Burrows Delta (mean absolute
//! z-difference, Burrows 2002) is reported alongside for interpretability.

/// 1 - cosine similarity of two vectors. Range [0, 2]; 0 = identical direction.
pub fn cosine_delta(a: &[f64], b: &[f64]) -> f64 {
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len().min(b.len()) {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 1.0;
    }
    1.0 - dot / (na.sqrt() * nb.sqrt())
}

/// Classic Burrows Delta: mean absolute difference of z-scores (Manhattan / n).
pub fn classic_delta(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return f64::INFINITY;
    }
    let s: f64 = (0..n).map(|i| (a[i] - b[i]).abs()).sum();
    s / n as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_is_zero() {
        let v = [1.0, 2.0, -3.0, 0.5];
        assert!(cosine_delta(&v, &v).abs() < 1e-12);
    }

    #[test]
    fn cosine_opposite_is_two() {
        assert!((cosine_delta(&[1.0, 0.0], &[-1.0, 0.0]) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn cosine_orthogonal_is_one() {
        assert!((cosine_delta(&[1.0, 0.0], &[0.0, 1.0]) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn classic_delta_is_mean_abs_zdiff() {
        // |0-1| + |0-2| + |0-3| = 6, / 3 = 2.0
        assert!((classic_delta(&[0.0, 0.0, 0.0], &[1.0, 2.0, 3.0]) - 2.0).abs() < 1e-12);
    }
}
