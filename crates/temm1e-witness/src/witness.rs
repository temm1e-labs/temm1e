//! Witness runtime — dispatches predicate checks and renders verdicts.
//!
//! Phase 2 adds Tier 1 aspect verification via a `Provider` trait object
//! (single-model policy: same model the agent runs). Tier 1 calls use
//! clean-slate context (no conversation history), structured JSON output,
//! and a static cached system prompt. Tier 2 remains advisory-only.
//!
//! The Witness is the ONLY entity authorized to produce a `Verified` outcome.
//! It respects Law 5 (Narrative-Only FAIL) — it never mutates the file system,
//! git state, or processes. Its only output is a `Verdict` plus a rewritten
//! final-reply string.

use crate::config::WitnessStrictness;
use crate::error::WitnessError;
use crate::ledger::Ledger;
use crate::predicates::{check_tier0, CheckContext, PredicateCheckResult};
use crate::types::{
    Claim, Evidence, LedgerPayload, Oath, Predicate, PredicateResult, TierUsage, Verdict,
    VerdictOutcome,
};
use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;
use std::time::Instant;
use temm1e_core::traits::Provider;
use temm1e_core::types::message::{ChatMessage, CompletionRequest, MessageContent, Role};

/// Tier 1/2 LLM verifier output schema.
///
/// Tier 1 and Tier 2 verifiers must return exactly this JSON shape. Any
/// deviation is treated as `Inconclusive` with the raw response recorded
/// in the detail field.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LlmVerifierResponse {
    /// "pass" | "fail" — case-insensitive match
    pub verdict: String,
    /// One-sentence reason
    pub reason: String,
}

/// Abstract Tier 1 verifier — trait so tests can inject mocks.
#[async_trait]
pub trait Tier1Verifier: Send + Sync {
    async fn verify(
        &self,
        oath_goal: &str,
        predicate_rubric: &str,
        evidence: &str,
    ) -> Result<LlmVerifierResponse, WitnessError>;
}

/// Abstract Tier 2 adversarial auditor — same shape as Tier 1 but prompted
/// differently (assumes the claim is false until proven). Strictly advisory
/// per the research paper §5.2: a Tier 2 PASS never overrides a Tier 0 FAIL,
/// and Tier 2 only has authority when no Tier 0 check is present for a given
/// predicate.
#[async_trait]
pub trait Tier2Verifier: Send + Sync {
    async fn audit(
        &self,
        oath_goal: &str,
        predicate_rubric: &str,
        evidence: &str,
    ) -> Result<LlmVerifierResponse, WitnessError>;
}

/// Default Tier 1 verifier backed by a `Provider` + a model name.
///
/// Uses clean-slate context: no conversation history, no tool schema, no
/// prior reasoning. The Tier 1 verifier sees only the Oath goal, the
/// specific predicate rubric, and the evidence string.
pub struct ProviderTier1Verifier {
    provider: Arc<dyn Provider>,
    model: String,
}

impl ProviderTier1Verifier {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }
}

const TIER1_SYSTEM_PROMPT: &str = "You are a predicate verifier. Your job: given a \
single machine-checkable predicate, a piece of evidence, and a subtask goal, decide \
if the evidence satisfies the predicate.\n\n\
RULES:\n\
1. Reply ONLY with JSON: {\"verdict\": \"pass\" | \"fail\", \"reason\": \"brief explanation\"}\n\
2. Base your verdict ONLY on the evidence shown. Do not speculate about unseen state.\n\
3. If the evidence is insufficient to decide, reply {\"verdict\": \"fail\", \"reason\": \"insufficient evidence: ...\"}.\n\
4. Do not rewrite the predicate. Do not suggest improvements. Do not argue.\n\
5. Your verdict is binary. No \"partially\" or \"mostly\".";

#[async_trait]
impl Tier1Verifier for ProviderTier1Verifier {
    async fn verify(
        &self,
        oath_goal: &str,
        predicate_rubric: &str,
        evidence: &str,
    ) -> Result<LlmVerifierResponse, WitnessError> {
        let user_prompt = format!(
            "Oath goal: {}\n\nPredicate to verify: {}\n\nEvidence:\n{}\n\n\
             Does the evidence satisfy the predicate? Reply ONLY as JSON: \
             {{\"verdict\": \"pass\" | \"fail\", \"reason\": \"...\"}}",
            oath_goal, predicate_rubric, evidence
        );

        let req = CompletionRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text(user_prompt),
            }],
            tools: vec![],
            max_tokens: None,
            temperature: Some(0.0),
            system: Some(TIER1_SYSTEM_PROMPT.to_string()),
            system_volatile: None,
        };

        let resp = self
            .provider
            .complete(req)
            .await
            .map_err(|e| WitnessError::PredicateCheck(format!("tier1 call: {e}")))?;

        let text = extract_text(&resp.content);
        parse_tier1_response(&text)
    }
}

fn extract_text(content: &[temm1e_core::types::message::ContentPart]) -> String {
    let mut out = String::new();
    for part in content {
        if let temm1e_core::types::message::ContentPart::Text { text } = part {
            out.push_str(text);
        }
    }
    out
}

/// Default Tier 2 adversarial auditor backed by a `Provider` + a model name.
///
/// Like Tier 1, uses clean-slate context: no conversation history, no tools,
/// no prior reasoning. Unlike Tier 1, the system prompt instructs the model
/// to find the cheapest falsification — to assume the claim is false until
/// the evidence forces it to be true.
///
/// Per the paper, Tier 2 is strictly advisory. A Tier 2 PASS never overrides
/// a Tier 0 FAIL. The Witness runtime enforces this by wrapping Tier 2
/// predicate results as `advisory = true` in the aggregation step (unless
/// the predicate itself is marked non-advisory, which is discouraged).
pub struct ProviderTier2Verifier {
    provider: Arc<dyn Provider>,
    model: String,
}

impl ProviderTier2Verifier {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }
}

const TIER2_SYSTEM_PROMPT: &str = "You are a skeptical adversarial auditor. Your job: \
find the cheapest way the claim could be false.\n\n\
Given a subtask goal, a predicate, and evidence, your task is to:\n\
1. Assume the claim is FALSE until the evidence forces you otherwise.\n\
2. Identify any plausible way the evidence could have been produced without \
the predicate holding (reward hacking, shortcut, fake file, etc.).\n\
3. If you can construct any plausible falsification scenario, reply FAIL.\n\
4. Your bias is toward finding failure. Do not be generous.\n\n\
RULES:\n\
1. Reply ONLY with JSON: {\"verdict\": \"pass\" | \"fail\", \"reason\": \"the cheapest falsification scenario\"}\n\
2. Your verdict is advisory — a stronger deterministic check may override you.\n\
3. Do not speculate about unseen state that you cannot check from the evidence.\n\
4. Do not rewrite the predicate. Do not suggest improvements. Do not argue.\n\
5. Binary verdict only. No \"partially\" or \"mostly\".";

#[async_trait]
impl Tier2Verifier for ProviderTier2Verifier {
    async fn audit(
        &self,
        oath_goal: &str,
        predicate_rubric: &str,
        evidence: &str,
    ) -> Result<LlmVerifierResponse, WitnessError> {
        let user_prompt = format!(
            "Oath goal: {}\n\nPredicate to audit: {}\n\nEvidence:\n{}\n\n\
             Find the cheapest way this claim could be false given only the evidence shown. \
             If you cannot find one, reply PASS. Otherwise FAIL with the falsification scenario. \
             Reply ONLY as JSON: {{\"verdict\": \"pass\" | \"fail\", \"reason\": \"...\"}}",
            oath_goal, predicate_rubric, evidence
        );

        let req = CompletionRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text(user_prompt),
            }],
            tools: vec![],
            max_tokens: None,
            temperature: Some(0.0),
            system: Some(TIER2_SYSTEM_PROMPT.to_string()),
            system_volatile: None,
        };

        let resp = self
            .provider
            .complete(req)
            .await
            .map_err(|e| WitnessError::PredicateCheck(format!("tier2 call: {e}")))?;

        let text = extract_text(&resp.content);
        // Tier 2 reuses the same JSON schema parser.
        parse_tier1_response(&text)
    }
}

/// Parse a Tier 1 / Tier 2 verifier response. Tolerates surrounding
/// markdown fences (```json) and trailing text.
pub fn parse_tier1_response(text: &str) -> Result<LlmVerifierResponse, WitnessError> {
    // Strip markdown code fences if present.
    let stripped = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Find the first JSON object in the response.
    let start = stripped.find('{');
    let end = stripped.rfind('}');
    let json_str = match (start, end) {
        (Some(s), Some(e)) if e >= s => &stripped[s..=e],
        _ => {
            return Err(WitnessError::PredicateCheck(format!(
                "tier1 response has no JSON object: {}",
                stripped
            )));
        }
    };

    serde_json::from_str::<LlmVerifierResponse>(json_str).map_err(WitnessError::Json)
}

/// The Witness: verifies sealed Oaths and records verdicts to the Ledger.
pub struct Witness {
    ledger: Arc<Ledger>,
    workspace_root: std::path::PathBuf,
    tier1: Option<Arc<dyn Tier1Verifier>>,
    tier2: Option<Arc<dyn Tier2Verifier>>,
}

impl Witness {
    pub fn new(ledger: Arc<Ledger>, workspace_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            ledger,
            workspace_root: workspace_root.into(),
            tier1: None,
            tier2: None,
        }
    }

    /// Attach a Tier 1 verifier. Without this, Tier 1 predicates
    /// return `Inconclusive`.
    pub fn with_tier1(mut self, tier1: Arc<dyn Tier1Verifier>) -> Self {
        self.tier1 = Some(tier1);
        self
    }

    /// Attach a Tier 2 adversarial auditor. Without this, Tier 2 predicates
    /// return `Inconclusive`. Tier 2 is strictly advisory regardless of
    /// whether this is attached — a Tier 2 PASS never overrides a Tier 0 FAIL.
    pub fn with_tier2(mut self, tier2: Arc<dyn Tier2Verifier>) -> Self {
        self.tier2 = Some(tier2);
        self
    }

    /// Attach a Tier 1 verifier backed by a Provider + model.
    pub fn with_provider(self, provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        self.with_tier1(Arc::new(ProviderTier1Verifier::new(provider, model)))
    }

    /// Attach both Tier 1 and Tier 2 verifiers backed by the same Provider +
    /// model — the common path under the single-model policy.
    pub fn with_tiered_provider(
        self,
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
    ) -> Self {
        let model = model.into();
        self.with_tier1(Arc::new(ProviderTier1Verifier::new(
            provider.clone(),
            model.clone(),
        )))
        .with_tier2(Arc::new(ProviderTier2Verifier::new(provider, model)))
    }

    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    /// Workspace root this Witness checks predicates against.
    pub fn workspace_root(&self) -> &std::path::Path {
        &self.workspace_root
    }

    /// Look up the most recent sealed Oath for a session from the Ledger.
    /// Used by the runtime gate to find the active Oath without external
    /// state tracking.
    pub async fn active_oath(&self, session_id: &str) -> Result<Option<Oath>, WitnessError> {
        let entries = self.ledger.read_session(session_id).await?;
        let latest = entries.iter().rev().find_map(|e| match &e.payload {
            LedgerPayload::OathSealed(oath) => Some(oath.clone()),
            _ => None,
        });
        Ok(latest)
    }

    /// Verify an Oath by running all its postcondition predicates. Records
    /// the verdict to the Ledger. Returns the full Verdict.
    ///
    /// Law 2: this is the only function that produces `Verified` outcomes.
    /// Law 5: never mutates files, git state, or processes.
    pub async fn verify_oath(&self, oath: &Oath) -> Result<Verdict, WitnessError> {
        if !oath.is_sealed() {
            return Err(WitnessError::NoSealedOath(oath.subtask_id.clone()));
        }

        let start = Instant::now();
        let ctx = CheckContext::new(&self.workspace_root);
        let mut per_predicate: Vec<PredicateResult> = Vec::new();
        let mut tier_usage = TierUsage::default();

        for predicate in &oath.postconditions {
            let tier = predicate.tier();
            let result = if tier == 0 {
                tier_usage.tier0_calls += 1;
                let r = check_tier0(predicate, &ctx).await?;
                tier_usage.tier0_latency_ms += r.latency_ms;
                r
            } else if tier == 1 {
                // Tier 1 — cheap aspect verifier via clean-slate LLM call.
                tier_usage.tier1_calls += 1;
                let t1_start = Instant::now();
                let r = self.dispatch_tier1(predicate, oath).await;
                let lat = t1_start.elapsed().as_millis() as u64;
                tier_usage.tier1_latency_ms += lat;
                let mut r = r;
                r.latency_ms = lat;
                r
            } else {
                // Tier 2 — adversarial auditor via clean-slate LLM call with
                // skeptical system prompt. Strictly advisory regardless of
                // the predicate's own `advisory` flag.
                tier_usage.tier2_calls += 1;
                let t2_start = Instant::now();
                let r = self.dispatch_tier2(predicate, oath).await;
                let lat = t2_start.elapsed().as_millis() as u64;
                tier_usage.tier2_latency_ms += lat;
                let mut r = r;
                r.latency_ms = lat;
                r
            };

            // Advisory rules:
            //   - AspectVerifier honors its own `advisory` flag.
            //   - AdversarialJudge (Tier 2) is ALWAYS advisory per the
            //     research paper §5.2: a Tier 2 PASS never overrides a Tier 0
            //     FAIL, and Tier 2 FAIL should not hard-fail the verdict by
            //     itself. Tier 2 is an auditor, not an authority.
            let advisory = matches!(
                predicate,
                Predicate::AspectVerifier { advisory: true, .. }
                    | Predicate::AdversarialJudge { .. }
            );

            per_predicate.push(PredicateResult {
                predicate: predicate.clone(),
                tier,
                outcome: result.outcome,
                detail: result.detail,
                advisory,
                latency_ms: result.latency_ms,
            });
        }

        let outcome = aggregate_outcome(&per_predicate);
        let reason = build_reason(&per_predicate);

        let verdict = Verdict {
            subtask_id: oath.subtask_id.clone(),
            rendered_at: Utc::now(),
            outcome,
            per_predicate,
            tier_usage,
            reason,
            cost_usd: 0.0,
            latency_ms: start.elapsed().as_millis() as u64,
        };

        // Write VerdictRendered to the Ledger.
        self.ledger
            .append(
                oath.session_id.clone(),
                Some(oath.subtask_id.clone()),
                Some(oath.root_goal_id.clone()),
                LedgerPayload::VerdictRendered(verdict.clone()),
                verdict.cost_usd,
                verdict.latency_ms,
            )
            .await?;

        Ok(verdict)
    }

    /// Dispatch a Tier 1 (AspectVerifier) predicate through the attached
    /// verifier. If no Tier 1 verifier is configured, returns
    /// `Inconclusive` with a "no verifier" reason.
    async fn dispatch_tier1(&self, predicate: &Predicate, oath: &Oath) -> PredicateCheckResult {
        let (rubric, _evidence_refs) = match predicate {
            Predicate::AspectVerifier {
                rubric,
                evidence_refs,
                ..
            } => (rubric.clone(), evidence_refs.clone()),
            _ => {
                return PredicateCheckResult {
                    outcome: VerdictOutcome::Inconclusive,
                    detail: "non-Tier1 predicate routed to dispatch_tier1".to_string(),
                    latency_ms: 0,
                };
            }
        };

        let tier1 = match self.tier1.as_ref() {
            Some(t) => t,
            None => {
                return PredicateCheckResult {
                    outcome: VerdictOutcome::Inconclusive,
                    detail: "no Tier 1 verifier configured — predicate advisory".to_string(),
                    latency_ms: 0,
                };
            }
        };

        // Phase 2 MVP: we do not yet fetch evidence by id from the ledger.
        // Evidence is presented as a best-effort summary of the oath goal
        // plus the workspace root. A richer evidence assembly is Phase 3.
        let evidence_summary = format!(
            "workspace_root: {}\nactive_oath_subtask: {}",
            self.workspace_root.display(),
            oath.subtask_id
        );

        match tier1.verify(&oath.goal, &rubric, &evidence_summary).await {
            Ok(resp) => {
                let outcome = match resp.verdict.to_lowercase().as_str() {
                    "pass" => VerdictOutcome::Pass,
                    "fail" => VerdictOutcome::Fail,
                    _ => VerdictOutcome::Inconclusive,
                };
                PredicateCheckResult {
                    outcome,
                    detail: format!("tier1: {}", resp.reason),
                    latency_ms: 0,
                }
            }
            Err(e) => PredicateCheckResult {
                outcome: VerdictOutcome::Inconclusive,
                detail: format!("tier1 error: {e}"),
                latency_ms: 0,
            },
        }
    }

    /// Dispatch a Tier 2 (AdversarialJudge) predicate through the attached
    /// auditor. If no Tier 2 auditor is configured, returns `Inconclusive`.
    /// Tier 2 remains advisory regardless of outcome.
    async fn dispatch_tier2(&self, predicate: &Predicate, oath: &Oath) -> PredicateCheckResult {
        let (rubric, _evidence_refs) = match predicate {
            Predicate::AdversarialJudge {
                rubric,
                evidence_refs,
                ..
            } => (rubric.clone(), evidence_refs.clone()),
            _ => {
                return PredicateCheckResult {
                    outcome: VerdictOutcome::Inconclusive,
                    detail: "non-Tier2 predicate routed to dispatch_tier2".to_string(),
                    latency_ms: 0,
                };
            }
        };

        let tier2 = match self.tier2.as_ref() {
            Some(t) => t,
            None => {
                return PredicateCheckResult {
                    outcome: VerdictOutcome::Inconclusive,
                    detail: "no Tier 2 auditor configured — predicate advisory".to_string(),
                    latency_ms: 0,
                };
            }
        };

        let evidence_summary = format!(
            "workspace_root: {}\nactive_oath_subtask: {}",
            self.workspace_root.display(),
            oath.subtask_id
        );

        match tier2.audit(&oath.goal, &rubric, &evidence_summary).await {
            Ok(resp) => {
                let outcome = match resp.verdict.to_lowercase().as_str() {
                    "pass" => VerdictOutcome::Pass,
                    "fail" => VerdictOutcome::Fail,
                    _ => VerdictOutcome::Inconclusive,
                };
                PredicateCheckResult {
                    outcome,
                    detail: format!("tier2 (advisory): {}", resp.reason),
                    latency_ms: 0,
                }
            }
            Err(e) => PredicateCheckResult {
                outcome: VerdictOutcome::Inconclusive,
                detail: format!("tier2 error: {e}"),
                latency_ms: 0,
            },
        }
    }

    /// Record a Claim to the Ledger without running predicates yet.
    pub async fn submit_claim(
        &self,
        claim: Claim,
        root_goal_id: String,
        session_id: String,
    ) -> Result<(), WitnessError> {
        self.ledger
            .append(
                session_id,
                Some(claim.subtask_id.clone()),
                Some(root_goal_id),
                LedgerPayload::ClaimSubmitted(claim),
                0.0,
                0,
            )
            .await?;
        Ok(())
    }

    /// Record Evidence produced during execution.
    pub async fn record_evidence(
        &self,
        evidence: Evidence,
        root_goal_id: String,
        session_id: String,
    ) -> Result<(), WitnessError> {
        self.ledger
            .append(
                session_id,
                Some(evidence.subtask_id.clone()),
                Some(root_goal_id),
                LedgerPayload::EvidenceProduced(evidence),
                0.0,
                0,
            )
            .await?;
        Ok(())
    }

    /// Compose the final reply to the user based on a verdict and the
    /// current agent-proposed reply. Honors Law 4 (loud failure) and Law 5
    /// (narrative-only — never modifies files).
    ///
    /// Strictness determines behavior:
    /// - Observe: return original reply unchanged (plus optional readout).
    /// - Warn: append a brief note if verdict is FAIL (plus optional readout).
    /// - Block: rewrite reply with honest failure description.
    /// - BlockWithRetry: same as Block (retry handled at runtime level).
    pub fn compose_final_reply(
        &self,
        agent_reply: &str,
        verdict: &Verdict,
        strictness: WitnessStrictness,
    ) -> String {
        self.compose_final_reply_ex(agent_reply, verdict, strictness, false)
    }

    /// Same as `compose_final_reply` but with an optional per-task readout
    /// suffix ("Witness: 4/5 PASS. Cost: $X. Latency: +Yms."). When
    /// `show_readout` is true, the readout is appended to the outgoing
    /// reply regardless of strictness — even Observe mode will show it
    /// so users can see the verdict without being blocked.
    pub fn compose_final_reply_ex(
        &self,
        agent_reply: &str,
        verdict: &Verdict,
        strictness: WitnessStrictness,
        show_readout: bool,
    ) -> String {
        let base = match strictness {
            WitnessStrictness::Observe => agent_reply.to_string(),
            WitnessStrictness::Warn => match verdict.outcome {
                VerdictOutcome::Pass => agent_reply.to_string(),
                VerdictOutcome::Fail | VerdictOutcome::Inconclusive => {
                    format!(
                        "{}\n\n---\n⚠ Witness: {}/{} predicates passed. {}",
                        agent_reply,
                        verdict.pass_count(),
                        verdict.total_count(),
                        verdict.reason,
                    )
                }
            },
            WitnessStrictness::Block | WitnessStrictness::BlockWithRetry => match verdict.outcome {
                VerdictOutcome::Pass => agent_reply.to_string(),
                VerdictOutcome::Fail => format_failed_reply(agent_reply, verdict),
                VerdictOutcome::Inconclusive => format_inconclusive_reply(agent_reply, verdict),
            },
        };
        if show_readout {
            format!("{}\n\n{}", base, format_readout(verdict))
        } else {
            base
        }
    }
}

/// Aggregate per-predicate results into an overall verdict outcome.
///
/// Rules:
/// - Any non-advisory FAIL → FAIL.
/// - All PASS (ignoring advisory) → PASS.
/// - Otherwise → Inconclusive.
fn aggregate_outcome(per_predicate: &[PredicateResult]) -> VerdictOutcome {
    let mut has_fail = false;
    let mut has_inconclusive = false;
    for r in per_predicate {
        if r.advisory {
            continue;
        }
        match r.outcome {
            VerdictOutcome::Fail => has_fail = true,
            VerdictOutcome::Inconclusive => has_inconclusive = true,
            VerdictOutcome::Pass => {}
        }
    }
    if has_fail {
        VerdictOutcome::Fail
    } else if has_inconclusive {
        VerdictOutcome::Inconclusive
    } else {
        VerdictOutcome::Pass
    }
}

fn build_reason(per_predicate: &[PredicateResult]) -> String {
    let pass: u32 = per_predicate
        .iter()
        .filter(|r| r.outcome == VerdictOutcome::Pass && !r.advisory)
        .count() as u32;
    let fail: u32 = per_predicate
        .iter()
        .filter(|r| r.outcome == VerdictOutcome::Fail && !r.advisory)
        .count() as u32;
    let inc: u32 = per_predicate
        .iter()
        .filter(|r| r.outcome == VerdictOutcome::Inconclusive && !r.advisory)
        .count() as u32;
    format!(
        "{}/{} pass, {} fail, {} inconclusive",
        pass,
        per_predicate.len(),
        fail,
        inc
    )
}

fn format_failed_reply(_agent_reply: &str, verdict: &Verdict) -> String {
    let mut out = String::new();
    out.push_str("⚠ **Partial completion.**\n\n");
    out.push_str(&format!(
        "Witness verified {}/{} postconditions:\n\n",
        verdict.pass_count(),
        verdict.total_count()
    ));

    let passed: Vec<&PredicateResult> = verdict
        .per_predicate
        .iter()
        .filter(|r| r.outcome == VerdictOutcome::Pass)
        .collect();
    let failed: Vec<&PredicateResult> = verdict
        .per_predicate
        .iter()
        .filter(|r| r.outcome == VerdictOutcome::Fail)
        .collect();
    let incons: Vec<&PredicateResult> = verdict
        .per_predicate
        .iter()
        .filter(|r| r.outcome == VerdictOutcome::Inconclusive)
        .collect();

    if !passed.is_empty() {
        out.push_str("✓ Verified:\n");
        for r in passed {
            out.push_str(&format!("  • {}\n", r.detail));
        }
    }
    if !failed.is_empty() {
        out.push_str("\n✗ Could not verify:\n");
        for r in failed {
            out.push_str(&format!("  • {}\n", r.detail));
        }
    }
    if !incons.is_empty() {
        out.push_str("\n? Inconclusive:\n");
        for r in incons {
            out.push_str(&format!("  • {}\n", r.detail));
        }
    }

    out.push_str("\nThis work is incomplete according to the pre-committed contract. Files produced during this task have NOT been modified or rolled back — they remain in place for your review.");
    out
}

fn format_inconclusive_reply(agent_reply: &str, verdict: &Verdict) -> String {
    format!(
        "{}\n\n---\n⚠ Witness: verdict inconclusive. {}",
        agent_reply, verdict.reason
    )
}

/// Per-task Witness readout. One-line summary suitable for appending to
/// the agent's final reply. Example:
/// `Witness: 4/5 PASS (1 FAIL). Cost: $0.0123. Latency: +2ms. Tiers: T0×3 T1×1.`
pub fn format_readout(verdict: &Verdict) -> String {
    let mut tiers = String::new();
    if verdict.tier_usage.tier0_calls > 0 {
        tiers.push_str(&format!("T0×{}", verdict.tier_usage.tier0_calls));
    }
    if verdict.tier_usage.tier1_calls > 0 {
        if !tiers.is_empty() {
            tiers.push(' ');
        }
        tiers.push_str(&format!("T1×{}", verdict.tier_usage.tier1_calls));
    }
    if verdict.tier_usage.tier2_calls > 0 {
        if !tiers.is_empty() {
            tiers.push(' ');
        }
        tiers.push_str(&format!("T2×{}", verdict.tier_usage.tier2_calls));
    }
    let fail_suffix = if verdict.fail_count() > 0 {
        format!(" ({} FAIL)", verdict.fail_count())
    } else if verdict.inconclusive_count() > 0 {
        format!(" ({} INCONCLUSIVE)", verdict.inconclusive_count())
    } else {
        String::new()
    };
    format!(
        "─── Witness: {}/{} PASS{}. Cost: ${:.4}. Latency: +{}ms. Tiers: {}. ───",
        verdict.pass_count(),
        verdict.total_count(),
        fail_suffix,
        verdict.cost_usd,
        verdict.latency_ms,
        tiers,
    )
}

/// Compute the WitnessStrictness for a task given configuration and complexity.
pub fn resolve_strictness(
    config: &crate::config::WitnessConfig,
    is_complex: bool,
    is_standard: bool,
) -> WitnessStrictness {
    use crate::config::OverrideStrictness;
    match config.override_strictness {
        OverrideStrictness::Observe => WitnessStrictness::Observe,
        OverrideStrictness::Warn => WitnessStrictness::Warn,
        OverrideStrictness::Block => WitnessStrictness::Block,
        OverrideStrictness::Auto => {
            if is_complex {
                WitnessStrictness::Block
            } else if is_standard {
                WitnessStrictness::Warn
            } else {
                WitnessStrictness::Observe
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oath::seal_oath;
    use crate::types::Predicate;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tempfile::tempdir;

    async fn setup() -> (Witness, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let witness = Witness::new(ledger, dir.path().to_path_buf());
        (witness, dir)
    }

    /// A deterministic mock Tier 1 verifier that returns a canned response.
    struct MockTier1 {
        response: Mutex<LlmVerifierResponse>,
        calls: Mutex<u32>,
    }

    #[async_trait]
    impl Tier1Verifier for MockTier1 {
        async fn verify(
            &self,
            _oath_goal: &str,
            _predicate_rubric: &str,
            _evidence: &str,
        ) -> Result<LlmVerifierResponse, WitnessError> {
            *self.calls.lock().unwrap() += 1;
            Ok(self.response.lock().unwrap().clone())
        }
    }

    fn mock_tier1(verdict: &str, reason: &str) -> Arc<MockTier1> {
        Arc::new(MockTier1 {
            response: Mutex::new(LlmVerifierResponse {
                verdict: verdict.to_string(),
                reason: reason.to_string(),
            }),
            calls: Mutex::new(0),
        })
    }

    /// A deterministic mock Tier 2 adversarial auditor.
    struct MockTier2 {
        response: Mutex<LlmVerifierResponse>,
        calls: Mutex<u32>,
    }

    #[async_trait]
    impl Tier2Verifier for MockTier2 {
        async fn audit(
            &self,
            _oath_goal: &str,
            _predicate_rubric: &str,
            _evidence: &str,
        ) -> Result<LlmVerifierResponse, WitnessError> {
            *self.calls.lock().unwrap() += 1;
            Ok(self.response.lock().unwrap().clone())
        }
    }

    fn mock_tier2(verdict: &str, reason: &str) -> Arc<MockTier2> {
        Arc::new(MockTier2 {
            response: Mutex::new(LlmVerifierResponse {
                verdict: verdict.to_string(),
                reason: reason.to_string(),
            }),
            calls: Mutex::new(0),
        })
    }

    #[tokio::test]
    async fn verify_passing_oath_yields_pass_verdict() {
        let (witness, dir) = setup().await;
        let f = dir.path().join("hello.txt");
        tokio::fs::write(&f, "hi").await.unwrap();

        let oath = Oath::draft("st-1", "root-1", "sess-1", "touch a file")
            .with_postcondition(Predicate::FileExists { path: f });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();

        let verdict = witness.verify_oath(&sealed).await.unwrap();
        assert_eq!(verdict.outcome, VerdictOutcome::Pass);
        assert_eq!(verdict.pass_count(), 1);
        assert_eq!(verdict.fail_count(), 0);
    }

    #[tokio::test]
    async fn verify_failing_oath_yields_fail_verdict() {
        let (witness, dir) = setup().await;
        let missing = dir.path().join("nope.txt");

        let oath = Oath::draft("st-1", "root-1", "sess-1", "touch a file")
            .with_postcondition(Predicate::FileExists { path: missing });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();

        let verdict = witness.verify_oath(&sealed).await.unwrap();
        assert_eq!(verdict.outcome, VerdictOutcome::Fail);
        assert_eq!(verdict.fail_count(), 1);
    }

    #[tokio::test]
    async fn verify_unsealed_oath_errors() {
        let (witness, _dir) = setup().await;
        let oath = Oath::draft("st-1", "root-1", "sess-1", "x");
        let r = witness.verify_oath(&oath).await;
        assert!(matches!(r, Err(WitnessError::NoSealedOath(_))));
    }

    #[tokio::test]
    async fn verify_writes_verdict_to_ledger() {
        let (witness, dir) = setup().await;
        let f = dir.path().join("a.txt");
        tokio::fs::write(&f, "x").await.unwrap();

        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply with a file")
            .with_postcondition(Predicate::FileExists { path: f });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        witness.verify_oath(&sealed).await.unwrap();

        let entries = witness.ledger().read_session("sess-1").await.unwrap();
        assert!(entries
            .iter()
            .any(|e| matches!(e.payload, LedgerPayload::VerdictRendered(_))));
    }

    #[tokio::test]
    async fn compose_final_reply_block_rewrites_on_fail() {
        let (witness, dir) = setup().await;
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply with file").with_postcondition(
            Predicate::FileExists {
                path: dir.path().join("missing"),
            },
        );
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();

        let final_reply = witness.compose_final_reply("Done!", &verdict, WitnessStrictness::Block);
        assert!(final_reply.contains("Partial completion"));
        assert!(final_reply.contains("Could not verify"));
        assert!(!final_reply.contains("Done!"));
    }

    #[tokio::test]
    async fn compose_final_reply_observe_unchanged() {
        let (witness, dir) = setup().await;
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply").with_postcondition(
            Predicate::FileExists {
                path: dir.path().join("missing"),
            },
        );
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();

        let final_reply =
            witness.compose_final_reply("Done!", &verdict, WitnessStrictness::Observe);
        assert_eq!(final_reply, "Done!");
    }

    #[tokio::test]
    async fn compose_final_reply_warn_appends_note() {
        let (witness, dir) = setup().await;
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply").with_postcondition(
            Predicate::FileExists {
                path: dir.path().join("missing"),
            },
        );
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();

        let final_reply = witness.compose_final_reply("Done!", &verdict, WitnessStrictness::Warn);
        assert!(final_reply.starts_with("Done!"));
        assert!(final_reply.contains("Witness:"));
    }

    #[test]
    fn aggregate_all_pass() {
        let r = vec![
            PredicateResult {
                predicate: Predicate::FileExists {
                    path: PathBuf::from("/tmp/a"),
                },
                tier: 0,
                outcome: VerdictOutcome::Pass,
                detail: "".into(),
                advisory: false,
                latency_ms: 0,
            },
            PredicateResult {
                predicate: Predicate::FileExists {
                    path: PathBuf::from("/tmp/b"),
                },
                tier: 0,
                outcome: VerdictOutcome::Pass,
                detail: "".into(),
                advisory: false,
                latency_ms: 0,
            },
        ];
        assert_eq!(aggregate_outcome(&r), VerdictOutcome::Pass);
    }

    #[test]
    fn aggregate_one_fail_is_fail() {
        let r = vec![
            PredicateResult {
                predicate: Predicate::FileExists {
                    path: PathBuf::from("/tmp/a"),
                },
                tier: 0,
                outcome: VerdictOutcome::Pass,
                detail: "".into(),
                advisory: false,
                latency_ms: 0,
            },
            PredicateResult {
                predicate: Predicate::FileExists {
                    path: PathBuf::from("/tmp/b"),
                },
                tier: 0,
                outcome: VerdictOutcome::Fail,
                detail: "".into(),
                advisory: false,
                latency_ms: 0,
            },
        ];
        assert_eq!(aggregate_outcome(&r), VerdictOutcome::Fail);
    }

    #[test]
    fn advisory_fail_does_not_fail_overall() {
        let r = vec![
            PredicateResult {
                predicate: Predicate::FileExists {
                    path: PathBuf::from("/tmp/a"),
                },
                tier: 0,
                outcome: VerdictOutcome::Pass,
                detail: "".into(),
                advisory: false,
                latency_ms: 0,
            },
            PredicateResult {
                predicate: Predicate::AspectVerifier {
                    rubric: "is it good?".into(),
                    evidence_refs: vec![],
                    advisory: true,
                },
                tier: 1,
                outcome: VerdictOutcome::Fail,
                detail: "".into(),
                advisory: true,
                latency_ms: 0,
            },
        ];
        assert_eq!(aggregate_outcome(&r), VerdictOutcome::Pass);
    }

    #[tokio::test]
    async fn active_oath_returns_most_recent_sealed_oath() {
        let (witness, _dir) = setup().await;

        // Empty session → no oath.
        assert!(witness.active_oath("sess-x").await.unwrap().is_none());

        // Seal one.
        let oath_a = Oath::draft("st-a", "root-1", "sess-x", "do X").with_postcondition(
            Predicate::FileExists {
                path: PathBuf::from("/tmp/a"),
            },
        );
        let (sealed_a, _) = seal_oath(witness.ledger(), oath_a).await.unwrap();
        let got = witness.active_oath("sess-x").await.unwrap().unwrap();
        assert_eq!(got.subtask_id, sealed_a.subtask_id);

        // Seal a second — most recent wins.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let oath_b = Oath::draft("st-b", "root-1", "sess-x", "do Y").with_postcondition(
            Predicate::FileExists {
                path: PathBuf::from("/tmp/b"),
            },
        );
        let (sealed_b, _) = seal_oath(witness.ledger(), oath_b).await.unwrap();
        let got = witness.active_oath("sess-x").await.unwrap().unwrap();
        assert_eq!(got.subtask_id, sealed_b.subtask_id);
    }

    #[tokio::test]
    async fn tier1_verifier_routes_pass_verdict() {
        let dir = tempdir().unwrap();
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let mock = mock_tier1("pass", "looks fine");
        let witness = Witness::new(ledger, dir.path().to_path_buf()).with_tier1(mock.clone());

        // Build an oath with one Tier 0 predicate (satisfies Spec Reviewer
        // rigor) plus one Tier 1 AspectVerifier.
        let file = dir.path().join("a.txt");
        tokio::fs::write(&file, "hi").await.unwrap();
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply with file")
            .with_postcondition(Predicate::FileExists { path: file })
            .with_postcondition(Predicate::AspectVerifier {
                rubric: "is the reply clear?".to_string(),
                evidence_refs: vec![],
                advisory: false,
            });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();

        assert_eq!(verdict.outcome, VerdictOutcome::Pass);
        assert_eq!(verdict.tier_usage.tier1_calls, 1);
        assert_eq!(*mock.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn tier1_verifier_routes_fail_verdict() {
        let dir = tempdir().unwrap();
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let mock = mock_tier1("fail", "stub detected");
        let witness = Witness::new(ledger, dir.path().to_path_buf()).with_tier1(mock);

        let file = dir.path().join("a.txt");
        tokio::fs::write(&file, "hi").await.unwrap();
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply with file")
            .with_postcondition(Predicate::FileExists { path: file })
            .with_postcondition(Predicate::AspectVerifier {
                rubric: "is this real?".to_string(),
                evidence_refs: vec![],
                advisory: false,
            });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();

        assert_eq!(verdict.outcome, VerdictOutcome::Fail);
    }

    #[tokio::test]
    async fn tier1_advisory_fail_does_not_fail_overall() {
        let dir = tempdir().unwrap();
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let mock = mock_tier1("fail", "subjective objection");
        let witness = Witness::new(ledger, dir.path().to_path_buf()).with_tier1(mock);

        let file = dir.path().join("a.txt");
        tokio::fs::write(&file, "hi").await.unwrap();
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply")
            .with_postcondition(Predicate::FileExists { path: file })
            .with_postcondition(Predicate::AspectVerifier {
                rubric: "is this elegant?".to_string(),
                evidence_refs: vec![],
                advisory: true,
            });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();

        // Non-advisory Tier 0 FileExists passes; advisory Tier 1 FAIL does
        // not fail the overall verdict.
        assert_eq!(verdict.outcome, VerdictOutcome::Pass);
    }

    #[tokio::test]
    async fn tier1_without_verifier_is_inconclusive() {
        let (witness, dir) = setup().await;
        let file = dir.path().join("a.txt");
        tokio::fs::write(&file, "hi").await.unwrap();
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply")
            .with_postcondition(Predicate::FileExists { path: file })
            .with_postcondition(Predicate::AspectVerifier {
                rubric: "is this clear?".to_string(),
                evidence_refs: vec![],
                advisory: false,
            });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();

        // No Tier 1 attached → Tier 1 predicate returns Inconclusive → overall
        // outcome is Inconclusive (not Fail, because non-advisory inconclusive
        // on a non-Tier 0 predicate is not a hard fail).
        assert!(matches!(
            verdict.outcome,
            VerdictOutcome::Inconclusive | VerdictOutcome::Fail
        ));
    }

    #[test]
    fn parse_tier1_response_handles_plain_json() {
        let out = parse_tier1_response(r#"{"verdict": "pass", "reason": "ok"}"#).unwrap();
        assert_eq!(out.verdict, "pass");
    }

    #[test]
    fn parse_tier1_response_handles_markdown_fenced_json() {
        let out =
            parse_tier1_response("```json\n{\"verdict\": \"fail\", \"reason\": \"stub\"}\n```")
                .unwrap();
        assert_eq!(out.verdict, "fail");
    }

    #[test]
    fn parse_tier1_response_handles_trailing_prose() {
        let out = parse_tier1_response(
            r#"Here is my verdict: {"verdict": "pass", "reason": "good"} done."#,
        )
        .unwrap();
        assert_eq!(out.verdict, "pass");
    }

    #[test]
    fn parse_tier1_response_rejects_non_json() {
        let r = parse_tier1_response("I think it looks good.");
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn tier2_auditor_routes_pass_verdict_but_stays_advisory() {
        let dir = tempdir().unwrap();
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let mock = mock_tier2("pass", "no falsification found");
        let witness = Witness::new(ledger, dir.path().to_path_buf()).with_tier2(mock.clone());

        let file = dir.path().join("a.txt");
        tokio::fs::write(&file, "hi").await.unwrap();
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply with file")
            .with_postcondition(Predicate::FileExists { path: file })
            .with_postcondition(Predicate::AdversarialJudge {
                rubric: "could this be faked?".to_string(),
                evidence_refs: vec![],
                advisory: false, // Ignored — Tier 2 is always advisory
            });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();

        assert_eq!(verdict.outcome, VerdictOutcome::Pass);
        assert_eq!(verdict.tier_usage.tier2_calls, 1);
        assert_eq!(*mock.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn tier2_auditor_fail_does_not_fail_overall_when_tier0_passes() {
        let dir = tempdir().unwrap();
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let mock = mock_tier2("fail", "paranoid falsification");
        let witness = Witness::new(ledger, dir.path().to_path_buf()).with_tier2(mock);

        let file = dir.path().join("a.txt");
        tokio::fs::write(&file, "hi").await.unwrap();
        // Tier 0 passes cleanly; Tier 2 fails adversarially. Tier 2 is
        // advisory, so overall outcome is PASS.
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply")
            .with_postcondition(Predicate::FileExists { path: file })
            .with_postcondition(Predicate::AdversarialJudge {
                rubric: "could this be faked?".to_string(),
                evidence_refs: vec![],
                advisory: false, // Ignored
            });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();
        assert_eq!(
            verdict.outcome,
            VerdictOutcome::Pass,
            "Tier 2 FAIL must not override Tier 0 PASS"
        );
    }

    #[tokio::test]
    async fn tier2_auditor_cannot_override_tier0_fail() {
        let dir = tempdir().unwrap();
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let mock = mock_tier2("pass", "looks fine");
        let witness = Witness::new(ledger, dir.path().to_path_buf()).with_tier2(mock);

        // Tier 0 FileExists fails (file doesn't exist). Tier 2 says PASS.
        // Overall must be FAIL — Tier 0 is authoritative.
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply")
            .with_postcondition(Predicate::FileExists {
                path: dir.path().join("nope.txt"),
            })
            .with_postcondition(Predicate::AdversarialJudge {
                rubric: "x".to_string(),
                evidence_refs: vec![],
                advisory: false,
            });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();
        assert_eq!(
            verdict.outcome,
            VerdictOutcome::Fail,
            "Tier 2 PASS must never override Tier 0 FAIL"
        );
    }

    #[tokio::test]
    async fn tier2_without_auditor_is_inconclusive() {
        let (witness, dir) = setup().await;
        let file = dir.path().join("a.txt");
        tokio::fs::write(&file, "hi").await.unwrap();
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply")
            .with_postcondition(Predicate::FileExists { path: file })
            .with_postcondition(Predicate::AdversarialJudge {
                rubric: "x".to_string(),
                evidence_refs: vec![],
                advisory: false,
            });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();
        // Tier 2 always advisory → its Inconclusive doesn't fail overall.
        assert_eq!(verdict.outcome, VerdictOutcome::Pass);
        assert_eq!(verdict.tier_usage.tier2_calls, 1);
    }

    #[tokio::test]
    async fn with_tiered_provider_attaches_both_tiers() {
        // Smoke test that the convenience builder hooks both tiers. The
        // real Provider isn't exercised here — we just verify it compiles.
        // (Provider is constructed via a no-op mock from test-utils would
        // be nice, but for now we just call with_tier1 + with_tier2 directly
        // to verify both slots are populated.)
        let dir = tempdir().unwrap();
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let witness = Witness::new(ledger, dir.path().to_path_buf())
            .with_tier1(mock_tier1("pass", ""))
            .with_tier2(mock_tier2("pass", ""));
        assert!(witness.tier1.is_some());
        assert!(witness.tier2.is_some());
    }

    #[test]
    fn format_readout_shows_passcount_cost_latency_tiers() {
        let usage = TierUsage {
            tier0_calls: 3,
            tier1_calls: 1,
            ..Default::default()
        };
        let v = Verdict {
            subtask_id: "st-1".into(),
            rendered_at: Utc::now(),
            outcome: VerdictOutcome::Pass,
            per_predicate: vec![],
            tier_usage: usage,
            reason: "ok".into(),
            cost_usd: 0.0123,
            latency_ms: 2,
        };
        let s = format_readout(&v);
        assert!(s.contains("0/0 PASS"));
        assert!(s.contains("$0.0123"));
        assert!(s.contains("+2ms"));
        assert!(s.contains("T0×3"));
        assert!(s.contains("T1×1"));
    }

    #[tokio::test]
    async fn compose_final_reply_ex_appends_readout_in_observe() {
        let (witness, dir) = setup().await;
        let file = dir.path().join("a.txt");
        tokio::fs::write(&file, "hi").await.unwrap();
        let oath = Oath::draft("st-1", "root-1", "sess-1", "reply")
            .with_postcondition(Predicate::FileExists { path: file });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        let verdict = witness.verify_oath(&sealed).await.unwrap();

        let out =
            witness.compose_final_reply_ex("Done!", &verdict, WitnessStrictness::Observe, true);
        assert!(out.starts_with("Done!"));
        assert!(out.contains("Witness:"));
        assert!(out.contains("PASS"));
    }

    #[test]
    fn law5_no_destructive_api_in_witness_source() {
        // Read the witness.rs source and check for forbidden destructive API
        // patterns. Sentinels are built via concat!() so the literal strings
        // do not appear in the source file itself (which would cause this
        // test to falsely match against its own code).
        let src = include_str!("witness.rs");
        let sentinels: &[&str] = &[
            concat!("remove", "_file"),
            concat!("remove", "_dir"),
            concat!("git re", "set --hard"),
            concat!("Command::new(\"k", "ill\")"),
            concat!("rm ", "-rf"),
        ];
        for s in sentinels {
            assert!(
                !src.contains(s),
                "Law 5 violation: witness.rs contains destructive API pattern `{}`",
                s
            );
        }
    }
}
