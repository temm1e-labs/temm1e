//! Budget tracker -- tracks cumulative token usage and cost per process
//! lifetime, and enforces a configurable spend limit (0.0 = unlimited).

use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn};

/// Per-provider pricing (USD per 1M tokens).
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

/// Returns pricing for known providers/models (USD per 1M tokens).
/// Pricing last verified: March 2026.
pub fn get_pricing(model: &str) -> ModelPricing {
    let m = model.to_lowercase();
    match m.as_str() {
        // ── Anthropic ────────────────────────────────────────────
        _ if m.contains("opus")
            && (m.contains("4-6")
                || m.contains("4-5")
                || m.contains("4.5")
                || m.contains("4.6")) =>
        {
            ModelPricing {
                input_per_million: 5.0,
                output_per_million: 25.0,
            }
        }
        _ if m.contains("opus") => ModelPricing {
            input_per_million: 5.0,
            output_per_million: 25.0,
        },
        _ if m.contains("sonnet")
            && (m.contains("4-6")
                || m.contains("4-5")
                || m.contains("4.5")
                || m.contains("4.6")
                || m.contains("sonnet-4")) =>
        {
            ModelPricing {
                input_per_million: 3.0,
                output_per_million: 15.0,
            }
        }
        _ if m.contains("sonnet") => ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        _ if m.contains("haiku") && (m.contains("4-5") || m.contains("4.5")) => ModelPricing {
            input_per_million: 1.0,
            output_per_million: 5.0,
        },
        _ if m.contains("haiku") && (m.contains("3-5") || m.contains("3.5")) => ModelPricing {
            input_per_million: 0.80,
            output_per_million: 4.0,
        },
        _ if m.contains("haiku") => ModelPricing {
            input_per_million: 1.0,
            output_per_million: 5.0,
        },

        // ── OpenAI ───────────────────────────────────────────────
        _ if m.contains("gpt-5.2") || m.contains("gpt-5-2") => ModelPricing {
            input_per_million: 1.75,
            output_per_million: 14.0,
        },
        _ if m.contains("gpt-5") && !m.contains("gpt-5.2") && !m.contains("gpt-5-2") => {
            ModelPricing {
                input_per_million: 1.25,
                output_per_million: 10.0,
            }
        }
        _ if m.contains("gpt-4o-mini") => ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.60,
        },
        _ if m.contains("gpt-4o") => ModelPricing {
            input_per_million: 2.50,
            output_per_million: 10.0,
        },
        _ if m.contains("gpt-4") => ModelPricing {
            input_per_million: 2.50,
            output_per_million: 10.0,
        },
        _ if m == "o3" || m.starts_with("o3-") => ModelPricing {
            input_per_million: 2.0,
            output_per_million: 8.0,
        },
        _ if m == "o4-mini" || m.starts_with("o4-mini") => ModelPricing {
            input_per_million: 1.10,
            output_per_million: 4.40,
        },

        // ── Google Gemini ────────────────────────────────────────
        _ if m.contains("gemini-2.5-pro") || m.contains("gemini-2-5-pro") => ModelPricing {
            input_per_million: 1.25,
            output_per_million: 10.0,
        },
        _ if m.contains("gemini-3.1-flash-lite") || m.contains("gemini-3-1-flash-lite") => {
            ModelPricing {
                input_per_million: 0.075,
                output_per_million: 0.30,
            }
        }
        _ if m.contains("gemini-3-flash") || m.contains("gemini-3.1-pro") => ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.60,
        },
        _ if m.contains("gemini-2.5-flash") || m.contains("gemini-2-5-flash") => ModelPricing {
            input_per_million: 0.30,
            output_per_million: 2.50,
        },
        _ if m.contains("gemini-2.0-flash") || m.contains("gemini-2-0-flash") => ModelPricing {
            input_per_million: 0.10,
            output_per_million: 0.40,
        },
        _ if m.contains("gemini") => ModelPricing {
            input_per_million: 0.30,
            output_per_million: 2.50,
        },

        // ── xAI Grok ─────────────────────────────────────────────
        _ if m.contains("grok-4") && m.contains("fast") => ModelPricing {
            input_per_million: 0.20,
            output_per_million: 0.50,
        },
        _ if m.contains("grok-4-1") && !m.contains("fast") => ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        _ if m.contains("grok-4") => ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        _ if m.contains("grok-3") && m.contains("fast") => ModelPricing {
            input_per_million: 0.20,
            output_per_million: 0.50,
        },
        _ if m.contains("grok-3") => ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        _ if m.contains("grok") => ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },

        // ── MiniMax ──────────────────────────────────────────────
        _ if m.contains("minimax") || m.starts_with("m2") => ModelPricing {
            input_per_million: 0.30,
            output_per_million: 1.20,
        },

        // ── Ollama (subscription-based, no per-token cost) ───────
        _ if m.contains("llama")
            || m.contains("mistral")
            || m.contains("qwen")
            || m.contains("phi")
            || m.contains("codellama")
            || m.contains("glm") =>
        {
            ModelPricing {
                input_per_million: 0.0,
                output_per_million: 0.0,
            }
        }

        // ── Default: Sonnet-class pricing (conservative) ─────────
        _ => ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
    }
}

/// Calculates USD cost for a given usage.
pub fn calculate_cost(input_tokens: u32, output_tokens: u32, pricing: &ModelPricing) -> f64 {
    let input_cost = (input_tokens as f64 / 1_000_000.0) * pricing.input_per_million;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * pricing.output_per_million;
    input_cost + output_cost
}

/// Thread-safe budget tracker that accumulates cost across a session.
/// Uses atomic u64 storing cost in micro-cents (1 USD = 100_000_000 units)
/// for lock-free operation.
pub struct BudgetTracker {
    /// Cumulative cost in micro-cents (1 USD = 100_000_000).
    cumulative_micro_cents: AtomicU64,
    /// Maximum spend in micro-cents (0 = unlimited).
    max_micro_cents: u64,
    /// Total input tokens consumed.
    total_input_tokens: AtomicU64,
    /// Total output tokens consumed.
    total_output_tokens: AtomicU64,
}

const MICRO_CENTS_PER_USD: f64 = 100_000_000.0;

impl BudgetTracker {
    /// Create a new tracker with a max spend in USD. 0.0 = unlimited.
    pub fn new(max_spend_usd: f64) -> Self {
        Self {
            cumulative_micro_cents: AtomicU64::new(0),
            max_micro_cents: (max_spend_usd.max(0.0) * MICRO_CENTS_PER_USD) as u64,
            total_input_tokens: AtomicU64::new(0),
            total_output_tokens: AtomicU64::new(0),
        }
    }

    /// Record usage from a completed API call. Returns the cost of this call in USD.
    pub fn record_usage(&self, input_tokens: u32, output_tokens: u32, cost_usd: f64) -> f64 {
        let micro_cents = (cost_usd.max(0.0) * MICRO_CENTS_PER_USD) as u64;
        self.cumulative_micro_cents
            .fetch_add(micro_cents, Ordering::Relaxed);
        self.total_input_tokens
            .fetch_add(input_tokens as u64, Ordering::Relaxed);
        self.total_output_tokens
            .fetch_add(output_tokens as u64, Ordering::Relaxed);

        let total = self.total_spend_usd();
        info!(
            call_cost_usd = format!("{:.6}", cost_usd),
            total_spend_usd = format!("{:.6}", total),
            budget_usd = format!("{:.2}", self.max_spend_usd()),
            input_tokens = input_tokens,
            output_tokens = output_tokens,
            "API cost recorded"
        );
        cost_usd
    }

    /// Check if the budget allows another API call. Returns Ok(()) or an error message.
    pub fn check_budget(&self) -> Result<(), String> {
        if self.max_micro_cents == 0 {
            return Ok(()); // Unlimited
        }
        let current = self.cumulative_micro_cents.load(Ordering::Relaxed);
        if current >= self.max_micro_cents {
            let spent = current as f64 / MICRO_CENTS_PER_USD;
            let limit = self.max_micro_cents as f64 / MICRO_CENTS_PER_USD;
            warn!(
                spent_usd = format!("{:.6}", spent),
                limit_usd = format!("{:.2}", limit),
                "Budget exceeded"
            );
            Err(format!(
                "Budget exceeded: ${:.4} spent of ${:.2} limit. \
                 Increase `max_spend_usd` in config or set to 0 for unlimited, then restart.",
                spent, limit
            ))
        } else {
            Ok(())
        }
    }

    /// Current total spend in USD.
    pub fn total_spend_usd(&self) -> f64 {
        self.cumulative_micro_cents.load(Ordering::Relaxed) as f64 / MICRO_CENTS_PER_USD
    }

    /// Max spend in USD.
    pub fn max_spend_usd(&self) -> f64 {
        self.max_micro_cents as f64 / MICRO_CENTS_PER_USD
    }

    /// Total tokens consumed.
    pub fn total_tokens(&self) -> (u64, u64) {
        (
            self.total_input_tokens.load(Ordering::Relaxed),
            self.total_output_tokens.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculate_cost_known_pricing() {
        let pricing = ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        };
        // 1M input + 1M output = $3 + $15 = $18
        let cost = calculate_cost(1_000_000, 1_000_000, &pricing);
        assert!((cost - 18.0).abs() < 1e-9);

        // 500 input + 1000 output
        let cost2 = calculate_cost(500, 1000, &pricing);
        let expected = (500.0 / 1_000_000.0) * 3.0 + (1000.0 / 1_000_000.0) * 15.0;
        assert!((cost2 - expected).abs() < 1e-12);
    }

    #[test]
    fn calculate_cost_zero_tokens() {
        let pricing = ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        };
        let cost = calculate_cost(0, 0, &pricing);
        assert!((cost).abs() < 1e-12);
    }

    #[test]
    fn budget_tracker_new_with_limit() {
        let tracker = BudgetTracker::new(5.0);
        assert!((tracker.max_spend_usd() - 5.0).abs() < 1e-6);
        assert!((tracker.total_spend_usd()).abs() < 1e-12);
        assert_eq!(tracker.total_tokens(), (0, 0));
    }

    #[test]
    fn budget_tracker_new_unlimited() {
        let tracker = BudgetTracker::new(0.0);
        assert!((tracker.max_spend_usd()).abs() < 1e-12);
        assert!(tracker.check_budget().is_ok());
    }

    #[test]
    fn budget_tracker_record_usage_accumulates() {
        let tracker = BudgetTracker::new(10.0);

        tracker.record_usage(1000, 500, 0.01);
        assert!((tracker.total_spend_usd() - 0.01).abs() < 1e-6);
        assert_eq!(tracker.total_tokens(), (1000, 500));

        tracker.record_usage(2000, 1000, 0.02);
        assert!((tracker.total_spend_usd() - 0.03).abs() < 1e-6);
        assert_eq!(tracker.total_tokens(), (3000, 1500));
    }

    #[test]
    fn budget_tracker_check_budget_within_limit() {
        let tracker = BudgetTracker::new(1.0);
        tracker.record_usage(1000, 500, 0.50);
        assert!(tracker.check_budget().is_ok());
    }

    #[test]
    fn budget_tracker_check_budget_exceeded() {
        let tracker = BudgetTracker::new(1.0);
        tracker.record_usage(100_000, 50_000, 0.60);
        tracker.record_usage(100_000, 50_000, 0.50);
        // Total = $1.10, limit = $1.00
        let result = tracker.check_budget();
        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        assert!(err_msg.contains("Budget exceeded"));
        assert!(err_msg.contains("$1.00"));
    }

    #[test]
    fn budget_tracker_check_budget_exactly_at_limit() {
        let tracker = BudgetTracker::new(1.0);
        tracker.record_usage(100_000, 50_000, 1.0);
        // Exactly at limit should trigger exceeded
        let result = tracker.check_budget();
        assert!(result.is_err());
    }

    #[test]
    fn budget_tracker_unlimited_never_exceeds() {
        let tracker = BudgetTracker::new(0.0);
        // Even with massive spend, unlimited should always pass
        tracker.record_usage(10_000_000, 5_000_000, 1000.0);
        assert!(tracker.check_budget().is_ok());
    }

    #[test]
    fn budget_tracker_record_returns_cost() {
        let tracker = BudgetTracker::new(10.0);
        let returned = tracker.record_usage(1000, 500, 0.042);
        assert!((returned - 0.042).abs() < 1e-12);
    }

    #[test]
    fn get_pricing_opus_4_6() {
        let pricing = get_pricing("claude-opus-4-6");
        assert!((pricing.input_per_million - 5.0).abs() < 1e-9);
        assert!((pricing.output_per_million - 25.0).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_sonnet_4_6() {
        let pricing = get_pricing("claude-sonnet-4-6");
        assert!((pricing.input_per_million - 3.0).abs() < 1e-9);
        assert!((pricing.output_per_million - 15.0).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_haiku_4_5() {
        let pricing = get_pricing("claude-haiku-4-5");
        assert!((pricing.input_per_million - 1.0).abs() < 1e-9);
        assert!((pricing.output_per_million - 5.0).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_haiku_3_5() {
        let pricing = get_pricing("claude-3-5-haiku");
        assert!((pricing.input_per_million - 0.80).abs() < 1e-9);
        assert!((pricing.output_per_million - 4.0).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_gpt5_2() {
        let pricing = get_pricing("gpt-5.2");
        assert!((pricing.input_per_million - 1.75).abs() < 1e-9);
        assert!((pricing.output_per_million - 14.0).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_gpt5() {
        let pricing = get_pricing("gpt-5");
        assert!((pricing.input_per_million - 1.25).abs() < 1e-9);
        assert!((pricing.output_per_million - 10.0).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_gpt4o() {
        let pricing = get_pricing("gpt-4o-2024-08");
        assert!((pricing.input_per_million - 2.50).abs() < 1e-9);
        assert!((pricing.output_per_million - 10.0).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_gpt4o_mini() {
        let pricing = get_pricing("gpt-4o-mini");
        assert!((pricing.input_per_million - 0.15).abs() < 1e-9);
        assert!((pricing.output_per_million - 0.60).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_o3() {
        let pricing = get_pricing("o3");
        assert!((pricing.input_per_million - 2.0).abs() < 1e-9);
        assert!((pricing.output_per_million - 8.0).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_o4_mini() {
        let pricing = get_pricing("o4-mini");
        assert!((pricing.input_per_million - 1.10).abs() < 1e-9);
        assert!((pricing.output_per_million - 4.40).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_gemini_2_5_pro() {
        let pricing = get_pricing("gemini-2.5-pro");
        assert!((pricing.input_per_million - 1.25).abs() < 1e-9);
        assert!((pricing.output_per_million - 10.0).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_gemini_2_5_flash() {
        let pricing = get_pricing("gemini-2.5-flash");
        assert!((pricing.input_per_million - 0.30).abs() < 1e-9);
        assert!((pricing.output_per_million - 2.50).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_gemini_2_0_flash() {
        let pricing = get_pricing("gemini-2.0-flash");
        assert!((pricing.input_per_million - 0.10).abs() < 1e-9);
        assert!((pricing.output_per_million - 0.40).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_grok4() {
        let pricing = get_pricing("grok-4");
        assert!((pricing.input_per_million - 3.0).abs() < 1e-9);
        assert!((pricing.output_per_million - 15.0).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_grok4_fast() {
        let pricing = get_pricing("grok-4-1-fast");
        assert!((pricing.input_per_million - 0.20).abs() < 1e-9);
        assert!((pricing.output_per_million - 0.50).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_minimax() {
        let pricing = get_pricing("m2.5");
        assert!((pricing.input_per_million - 0.30).abs() < 1e-9);
        assert!((pricing.output_per_million - 1.20).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_ollama_llama() {
        let pricing = get_pricing("llama3.3");
        assert!((pricing.input_per_million).abs() < 1e-9);
        assert!((pricing.output_per_million).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_ollama_glm() {
        let pricing = get_pricing("glm-5");
        assert!((pricing.input_per_million).abs() < 1e-9);
        assert!((pricing.output_per_million).abs() < 1e-9);
    }

    #[test]
    fn get_pricing_unknown_model_defaults() {
        let pricing = get_pricing("some-unknown-model-xyz");
        // Default is Sonnet-class
        assert!((pricing.input_per_million - 3.0).abs() < 1e-9);
        assert!((pricing.output_per_million - 15.0).abs() < 1e-9);
    }

    #[test]
    fn budget_tracker_multiple_small_calls() {
        let tracker = BudgetTracker::new(0.10);
        // Simulate 100 small calls at $0.001 each = $0.10 total
        for _ in 0..100 {
            tracker.record_usage(100, 50, 0.001);
        }
        // Should be at or slightly above the limit due to floating point
        assert!(tracker.check_budget().is_err());
    }
}
