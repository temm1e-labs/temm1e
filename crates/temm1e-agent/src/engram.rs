//! Engram Memory — scoring & lifecycle math for the permanent-memory tier.
//!
//! This module is the pure, deterministic core of **Engram**, the permanent
//! long-term memory layer that sits on top of λ-Memory. It contains **no LLM
//! calls and no I/O** — only the math that decides how durable a fact is and
//! which tier it belongs to. Full design: `tems_lab/ENGRAM_MEMORY.md`.
//!
//! Model (two-component memory, after Bjork & the FSRS scheduler):
//!   * `importance` (I) — *stored*; seeded from the first judgment, EMA-updated.
//!   * `i_eff`          — *derived lazily*: `I` annealed by time-since-use.
//!   * `tier`           — *derived* from `i_eff` with hysteresis.
//!
//! Guarantees that make it bulletproof / timeproof:
//!   * `I ∈ [0,5]` always (clipped) — no drift or saturation over years.
//!   * user-pinned ⇒ `i_eff = 5`, never anneals (sacrosanct).
//!   * the lazy anneal makes forgetting happen with **no LLM** — so demotion
//!     works even when the optional Curator is disabled.
//!   * clock skew (`now < last_accessed`) cannot inflate a score.

use serde::{Deserialize, Serialize};

/// The clamped importance range.
pub const IMPORTANCE_MIN: f32 = 0.0;
pub const IMPORTANCE_MAX: f32 = 5.0;

#[inline]
fn clip(x: f32) -> f32 {
    x.clamp(IMPORTANCE_MIN, IMPORTANCE_MAX)
}

/// Tunable parameters for the Engram scorer (defaults from the design doc).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EngramParams {
    /// EMA weight applied to each new judgment on update (clamped to `0..=1`).
    pub eta: f32,
    /// Promotion threshold: `i_eff ≥ theta_up` ⇒ Permanent.
    pub theta_up: f32,
    /// Demotion threshold: a Permanent fact with `i_eff ≤ theta_down` demotes.
    pub theta_down: f32,
    /// Anneal time constant in days (half-life ≈ `tau_days · ln 2`).
    pub tau_days: f32,
    /// Below this `i_eff` a fact is Archived (hash-only).
    pub archive_eps: f32,
}

impl Default for EngramParams {
    fn default() -> Self {
        Self {
            eta: 0.4,
            theta_up: 3.5,
            theta_down: 2.0,
            tau_days: 60.0,
            archive_eps: 0.05,
        }
    }
}

/// Memory tiers, derived from effective importance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    /// Always injected (capped at `P_max`); the Engram permanent tier.
    Permanent,
    /// The decaying λ tier — visible but not permanent.
    Active,
    /// Faded to a hash; recalled on demand.
    Archived,
}

/// Seed importance from the first judgment `importance_hat ∈ [0,5]`.
///
/// A confidently-judged fact (`importance_hat ≥ theta_up`) is permanent on its
/// first write — "stated once is enough", with no *user* repetition.
pub fn seed(importance_hat: f32) -> f32 {
    clip(importance_hat)
}

/// EMA update: blend the current importance with a new judgment. Smoothing
/// (plus hysteresis in [`tier`]) prevents turn-to-turn thrash.
pub fn ema_update(current: f32, importance_hat: f32, eta: f32) -> f32 {
    let eta = eta.clamp(0.0, 1.0);
    clip((1.0 - eta) * current + eta * importance_hat)
}

/// Effective importance with lazy time-anneal.
///
/// `now` / `last_accessed` are unix-epoch seconds. Clock skew is guarded with
/// `saturating_sub`, so `i_eff` is never inflated above the stored `I`.
/// User-pinned facts never anneal (`i_eff = IMPORTANCE_MAX`).
pub fn effective_importance(
    importance: f32,
    last_accessed: u64,
    now: u64,
    pinned_by_user: bool,
    tau_days: f32,
) -> f32 {
    if pinned_by_user {
        return IMPORTANCE_MAX;
    }
    let dt_days = (now.saturating_sub(last_accessed) as f32) / 86_400.0;
    let tau = tau_days.max(f32::EPSILON);
    clip(importance * (-dt_days / tau).exp())
}

/// Derive the tier from effective importance, applying **hysteresis** relative
/// to the fact's `current` tier so scores hovering in `[theta_down, theta_up]`
/// don't flap. User pins are always Permanent.
pub fn tier(i_eff: f32, current: Tier, pinned_by_user: bool, p: &EngramParams) -> Tier {
    if pinned_by_user {
        return Tier::Permanent;
    }
    match current {
        // Sticky: stay Permanent until we fall to the (lower) demote threshold.
        Tier::Permanent => {
            if i_eff > p.theta_down {
                Tier::Permanent
            } else if i_eff < p.archive_eps {
                Tier::Archived
            } else {
                Tier::Active
            }
        }
        // Not yet permanent: promote only at the (higher) promote threshold.
        Tier::Active | Tier::Archived => {
            if i_eff >= p.theta_up {
                Tier::Permanent
            } else if i_eff < p.archive_eps {
                Tier::Archived
            } else {
                Tier::Active
            }
        }
    }
}

/// Whether a fact belongs in the always-on permanent block, derived from its
/// pin state and effective importance — no stored tier needed. User pins always
/// show; agent pins show until they anneal to the demote floor; an unpinned fact
/// shows only once it crosses the promote threshold.
pub fn is_visible_permanent(
    i_eff: f32,
    pinned_user: bool,
    pinned_agent: bool,
    p: &EngramParams,
) -> bool {
    pinned_user || (pinned_agent && i_eff > p.theta_down) || i_eff >= p.theta_up
}

/// Greedy budget packer for the Permanent block under the `P_max` cap.
///
/// Given items as `(i_eff, token_cost)`, returns the indices to render, highest
/// `i_eff` first, until `budget` tokens are exhausted. Stable order of the
/// returned indices follows descending `i_eff`.
pub fn pack_by_budget(items: &[(f32, usize)], budget: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..items.len()).collect();
    idx.sort_by(|&a, &b| {
        items[b]
            .0
            .partial_cmp(&items[a].0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut used = 0usize;
    let mut out = Vec::new();
    for i in idx {
        let cost = items[i].1;
        if used + cost <= budget {
            used += cost;
            out.push(i);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: EngramParams = EngramParams {
        eta: 0.4,
        theta_up: 3.5,
        theta_down: 2.0,
        tau_days: 60.0,
        archive_eps: 0.05,
    };
    const DAY: u64 = 86_400;

    #[test]
    fn seed_clamps() {
        assert_eq!(seed(4.0), 4.0);
        assert_eq!(seed(9.0), 5.0);
        assert_eq!(seed(-1.0), 0.0);
    }

    #[test]
    fn ema_moves_toward_target_and_clamps() {
        let a = ema_update(0.0, 5.0, 0.4); // 2.0
        assert!((a - 2.0).abs() < 1e-5);
        let b = ema_update(a, 5.0, 0.4); // 0.6*2 + 0.4*5 = 3.2
        assert!((b - 3.2).abs() < 1e-5);
        assert_eq!(ema_update(5.0, 10.0, 1.0), 5.0); // clamps to max
        assert_eq!(ema_update(0.0, 4.0, 0.0), 0.0); // eta=0 ⇒ unchanged
    }

    #[test]
    fn effective_no_anneal_at_zero_dt() {
        let now = 1_000_000u64;
        assert!((effective_importance(4.0, now, now, false, 60.0) - 4.0).abs() < 1e-4);
    }

    #[test]
    fn effective_pin_is_max() {
        assert_eq!(effective_importance(0.1, 0, 10_000_000, true, 60.0), 5.0);
    }

    #[test]
    fn effective_anneals_over_time() {
        let now = 100 * DAY;
        // after tau days the factor is 1/e ≈ 0.368
        let v = effective_importance(4.0, now - 60 * DAY, now, false, 60.0);
        assert!((v - 4.0 * std::f32::consts::E.recip()).abs() < 0.05, "v={v}");
    }

    #[test]
    fn effective_clock_skew_guarded() {
        // now < last_accessed must not inflate the score
        let v = effective_importance(3.0, 1000, 500, false, 60.0);
        assert!((v - 3.0).abs() < 1e-4);
    }

    #[test]
    fn effective_tau_zero_does_not_panic() {
        // tau=0 is guarded; result is finite (anneals hard, but no NaN/inf misuse)
        let v = effective_importance(3.0, 0, DAY, false, 0.0);
        assert!(v.is_finite());
    }

    #[test]
    fn tier_promotes_at_theta_up() {
        assert_eq!(tier(3.6, Tier::Active, false, &P), Tier::Permanent);
        assert_eq!(tier(3.4, Tier::Active, false, &P), Tier::Active);
    }

    #[test]
    fn tier_hysteresis_is_sticky() {
        // a Permanent fact in the band stays Permanent
        assert_eq!(tier(3.0, Tier::Permanent, false, &P), Tier::Permanent);
        // an Active fact in the band stays Active (didn't reach promote)
        assert_eq!(tier(3.0, Tier::Active, false, &P), Tier::Active);
        // Permanent demotes only at/below theta_down
        assert_eq!(tier(1.9, Tier::Permanent, false, &P), Tier::Active);
    }

    #[test]
    fn tier_archives_below_eps() {
        assert_eq!(tier(0.01, Tier::Active, false, &P), Tier::Archived);
        assert_eq!(tier(0.01, Tier::Permanent, false, &P), Tier::Archived);
    }

    #[test]
    fn tier_pin_always_permanent() {
        assert_eq!(tier(0.0, Tier::Archived, true, &P), Tier::Permanent);
    }

    #[test]
    fn visible_permanent_rules() {
        assert!(is_visible_permanent(0.0, true, false, &P)); // user pin: always
        assert!(is_visible_permanent(3.0, false, true, &P)); // agent pin above demote floor
        assert!(!is_visible_permanent(1.5, false, true, &P)); // agent pin annealed below floor
        assert!(is_visible_permanent(3.6, false, false, &P)); // unpinned + hot
        assert!(!is_visible_permanent(3.0, false, false, &P)); // unpinned in-band: hidden
    }

    #[test]
    fn pack_respects_budget_highest_first() {
        let items = [(1.0, 50), (5.0, 40), (3.0, 40)];
        // highest i_eff first: idx1 (5.0,40) + idx2 (3.0,40) = 80 ⇒ fits; idx0 excluded
        assert_eq!(pack_by_budget(&items, 80), vec![1, 2]);
    }

    #[test]
    fn pack_empty_and_zero_budget() {
        assert!(pack_by_budget(&[], 100).is_empty());
        assert!(pack_by_budget(&[(5.0, 10)], 0).is_empty());
    }

    #[test]
    fn lifecycle_confident_promote_then_anneal_demote() {
        // a confidently-judged fact promotes on first write (no repetition)
        let i = seed(4.0);
        let t0 = 1_000 * DAY;
        let promoted = tier(
            effective_importance(i, t0, t0, false, P.tau_days),
            Tier::Active,
            false,
            &P,
        );
        assert_eq!(promoted, Tier::Permanent);
        // 90 days of disuse anneals it below theta_down ⇒ auto-demote, no LLM
        let later = t0 + 90 * DAY;
        let i_eff = effective_importance(i, t0, later, false, P.tau_days);
        assert_eq!(tier(i_eff, Tier::Permanent, false, &P), Tier::Active, "i_eff={i_eff}");
    }
}
