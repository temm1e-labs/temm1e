//! Thompson Sampling (Thompson, 1933).
//!
//! Multi-armed bandit strategy using Bayesian posterior sampling.
//! Each arm maintains a Beta(alpha, beta) posterior; at each step,
//! we sample from each posterior and select the arm with the highest sample.

use rand::Rng;
use serde::{Deserialize, Serialize};

/// Thompson sampler for K arms with Beta-Bernoulli model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThompsonSampler {
    /// (alpha, beta) parameters for each arm's Beta posterior.
    arms: Vec<(f64, f64)>,
}

impl ThompsonSampler {
    /// Create a new sampler with `k` arms, each with uniform Beta(1,1) prior.
    pub fn new(k: usize) -> Self {
        Self {
            arms: vec![(1.0, 1.0); k],
        }
    }

    /// Sample from each arm's posterior and return the index of the best arm.
    pub fn sample(&self) -> usize {
        let mut rng = rand::thread_rng();
        self.sample_with_rng(&mut rng)
    }

    /// Sample using a provided RNG (for testability).
    pub fn sample_with_rng<R: Rng>(&self, rng: &mut R) -> usize {
        let mut best_idx = 0;
        let mut best_val = f64::NEG_INFINITY;
        for (i, &(alpha, beta)) in self.arms.iter().enumerate() {
            let theta = sample_beta(rng, alpha, beta);
            if theta > best_val {
                best_val = theta;
                best_idx = i;
            }
        }
        best_idx
    }

    /// Update an arm's posterior after observing a reward.
    ///
    /// - `arm`: index of the arm
    /// - `reward`: true for success, false for failure
    pub fn update(&mut self, arm: usize, reward: bool) {
        if reward {
            self.arms[arm].0 += 1.0;
        } else {
            self.arms[arm].1 += 1.0;
        }
    }

    /// Expected value (posterior mean) for each arm: alpha / (alpha + beta).
    pub fn expected_values(&self) -> Vec<f64> {
        self.arms.iter().map(|&(a, b)| a / (a + b)).collect()
    }
}

/// Sample from Beta(alpha, beta) using the Gamma decomposition method.
///
/// Beta(a, b) = G1 / (G1 + G2) where G1 ~ Gamma(a, 1), G2 ~ Gamma(b, 1).
fn sample_beta<R: Rng>(rng: &mut R, alpha: f64, beta: f64) -> f64 {
    let g1 = sample_gamma(rng, alpha);
    let g2 = sample_gamma(rng, beta);
    if g1 + g2 == 0.0 {
        0.5
    } else {
        g1 / (g1 + g2)
    }
}

/// Sample from Gamma(shape, 1) using Marsaglia and Tsang's method (2000).
///
/// For shape >= 1, uses the direct method.
/// For shape < 1, uses the identity: if X ~ Gamma(shape+1, 1) and
/// U ~ Uniform(0,1), then X * U^(1/shape) ~ Gamma(shape, 1).
fn sample_gamma<R: Rng>(rng: &mut R, shape: f64) -> f64 {
    if shape < 1.0 {
        // Boost: Gamma(shape) = Gamma(shape+1) * U^(1/shape)
        let g = sample_gamma_ge1(rng, shape + 1.0);
        let u: f64 = rng.gen();
        return g * u.powf(1.0 / shape);
    }
    sample_gamma_ge1(rng, shape)
}

/// Marsaglia and Tsang's method for Gamma(shape, 1) with shape >= 1.
fn sample_gamma_ge1<R: Rng>(rng: &mut R, shape: f64) -> f64 {
    let d = shape - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();

    loop {
        // Generate a standard normal via Box-Muller.
        let (x, _) = box_muller(rng);
        let v = 1.0 + c * x;
        if v <= 0.0 {
            continue;
        }
        let v = v * v * v;
        let u: f64 = rng.gen();

        // Squeeze test.
        if u < 1.0 - 0.0331 * x * x * x * x {
            return d * v;
        }
        if u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
            return d * v;
        }
    }
}

/// Box-Muller transform: generate two independent standard normal samples
/// from two uniform samples.
fn box_muller<R: Rng>(rng: &mut R) -> (f64, f64) {
    let u1: f64 = rng.gen();
    let u2: f64 = rng.gen();
    let r = (-2.0 * u1.ln()).sqrt();
    let theta = 2.0 * std::f64::consts::PI * u2;
    (r * theta.cos(), r * theta.sin())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn seeded_rng() -> rand::rngs::StdRng {
        rand::rngs::StdRng::seed_from_u64(42)
    }

    #[test]
    fn uniform_priors_roughly_uniform() {
        let sampler = ThompsonSampler::new(3);
        let mut counts = [0u32; 3];
        let mut rng = seeded_rng();
        for _ in 0..3000 {
            let idx = sampler.sample_with_rng(&mut rng);
            counts[idx] += 1;
        }
        // Each arm should get roughly 1000 ± 200 selections.
        for &c in &counts {
            assert!(c > 600, "count {} too low for uniform", c);
            assert!(c < 1400, "count {} too high for uniform", c);
        }
    }

    #[test]
    fn rewarded_arm_selected_more() {
        let mut sampler = ThompsonSampler::new(3);
        // Heavily reward arm 1.
        for _ in 0..50 {
            sampler.update(1, true);
        }

        let mut counts = [0u32; 3];
        let mut rng = seeded_rng();
        for _ in 0..1000 {
            let idx = sampler.sample_with_rng(&mut rng);
            counts[idx] += 1;
        }
        // Arm 1 should dominate.
        assert!(
            counts[1] > counts[0] && counts[1] > counts[2],
            "rewarded arm should be selected most: {:?}",
            counts
        );
    }

    #[test]
    fn penalized_arm_selected_less() {
        let mut sampler = ThompsonSampler::new(3);
        // Penalize arm 0 heavily.
        for _ in 0..50 {
            sampler.update(0, false);
        }

        let mut counts = [0u32; 3];
        let mut rng = seeded_rng();
        for _ in 0..1000 {
            let idx = sampler.sample_with_rng(&mut rng);
            counts[idx] += 1;
        }
        // Arm 0 should be selected least.
        assert!(
            counts[0] < counts[1] && counts[0] < counts[2],
            "penalized arm should be selected least: {:?}",
            counts
        );
    }

    #[test]
    fn update_changes_expected_values() {
        let mut sampler = ThompsonSampler::new(2);
        let before = sampler.expected_values();
        assert!((before[0] - 0.5).abs() < 1e-12);
        assert!((before[1] - 0.5).abs() < 1e-12);

        sampler.update(0, true);
        let after = sampler.expected_values();
        // Arm 0: Beta(2,1) → mean = 2/3 ≈ 0.667
        assert!((after[0] - 2.0 / 3.0).abs() < 1e-12);
        // Arm 1: still Beta(1,1) → mean = 0.5
        assert!((after[1] - 0.5).abs() < 1e-12);
    }
}
