# OpenAI OAuth for Codex — Research Document

**Branch:** `oauth_openai`
**Date:** 2026-03-11
**Status:** Research / Exploration
**Goal:** Enable TEMM1E users to authenticate with their ChatGPT Plus/Pro subscription via OAuth instead of API keys — same as OpenClaw does.

---

## Why This Matters

Currently TEMM1E requires users to have an OpenAI API key (pay-per-token billing). With Codex OAuth, users with a **ChatGPT Plus ($20/mo) or Pro ($200/mo) subscription** can use their subscription's included API access — no separate API billing needed. This is how OpenClaw works today.

---

## The OAuth Flow (Reverse-Engineered from OpenClaw + Codex CLI)

### Endpoints

| Endpoint | URL |
|----------|-----|
| **Authorization** | `https://auth.openai.com/oauth/authorize` |
| **Token exchange** | `https://auth.openai.com/oauth/token` |
| **API (Responses)** | `https://api.openai.com/v1/responses` |
| **API (Chat Completions)** | `https://api.openai.com/v1/chat/completions` |

### Client ID

```
app_EMoamEEZ73f0CkXaXp7hrann
```

This is a **public client ID** used by Codex CLI. OpenClaw, Roo Code, OpenCode, and other third-party tools all reuse this same client ID. There is no official OpenAI registration process for third-party OAuth clients yet — the community consensus is to reuse the Codex client ID.

> **Risk:** OpenAI could revoke or restrict this client ID at any time. No official guidance exists for third-party usage. See "Open Questions" below.

### PKCE Parameters

| Parameter | Value |
|-----------|-------|
| **Method** | S256 (SHA-256) |
| **Code verifier** | 32 random bytes → base64url encoded |
| **Code challenge** | SHA-256(verifier) → base64url encoded |

### OAuth Scopes

```
openid profile email offline_access
```

**Critical issue:** These identity-only scopes are what Codex CLI requests. However, for actual API access, the token also needs:
- `model.request` — permission to call models
- `api.responses.write` — permission to use the Responses API

OpenClaw hit this exact bug (issues #26801, #36660): OAuth succeeds but API calls fail with 403 because the token lacks API scopes. The fix is a **post-login scope probe** — validate the token can actually make API calls immediately after OAuth, fail early if not.

### Authorization URL (Full)

```
https://auth.openai.com/oauth/authorize
  ?client_id=app_EMoamEEZ73f0CkXaXp7hrann
  &redirect_uri=http://127.0.0.1:{PORT}/auth/callback
  &response_type=code
  &scope=openid+profile+email+offline_access
  &state={random_state}
  &code_challenge={challenge}
  &code_challenge_method=S256
  &id_token_add_organizations=true
  &codex_cli_simplified_flow=true
```

### Token Exchange (POST)

```
POST https://auth.openai.com/oauth/token
Content-Type: application/x-www-form-urlencoded

grant_type=authorization_code
&code={auth_code}
&redirect_uri=http://127.0.0.1:{PORT}/auth/callback
&client_id=app_EMoamEEZ73f0CkXaXp7hrann
&code_verifier={verifier}
```

**Response:**
```json
{
  "access_token": "eyJhb...",     // JWT
  "refresh_token": "ort_abc...",
  "id_token": "eyJhb...",         // JWT with email, org info
  "token_type": "Bearer",
  "expires_in": 3600
}
```

### Token Refresh

```
POST https://auth.openai.com/oauth/token
Content-Type: application/x-www-form-urlencoded

grant_type=refresh_token
&refresh_token={refresh_token}
&client_id=app_EMoamEEZ73f0CkXaXp7hrann
```

### Using the Token for API Calls

Replace the API key with the OAuth access token:

```
Authorization: Bearer {access_token}
```

The token is a JWT that OpenAI validates server-side.

### Which API Endpoint? Responses API (NOT Chat Completions)

**Critical finding:** Codex CLI deprecated `/v1/chat/completions` and fully removed it in February 2026. The **only supported wire protocol** is the Responses API (`/v1/responses`). The `wire_api` config defaults to `"responses"` and is the only accepted value.

Codex-specific models (e.g., `gpt-5.3-codex`) are **only available via the Responses API** — they don't work with `/v1/chat/completions` at all.

**Example API call with OAuth token:**
```bash
curl https://api.openai.com/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer eyJhb..." \
  -d '{
    "model": "gpt-5.3-codex",
    "input": "Explain Rust ownership in one sentence."
  }'
```

The Responses API has a different request/response shape than Chat Completions:
- **Input:** Uses `input` field (string or array of items) instead of `messages` array
- **Output:** Returns items instead of choices/messages
- **State:** Can manage conversation state server-side (no need to send full history)
- **Cost:** 40-80% better cache utilization than Chat Completions
- **Built-in tools:** Web search, file search, code interpreter, MCP

> **TEMM1E impact:** Our `OpenAICompatProvider` currently uses `/v1/chat/completions`. For OAuth/Codex models, we need a **separate Responses API adapter** or modify the provider to support both wire protocols. This reinforces the isolation requirement.

### Available Codex Models (as of March 2026)

| Model ID | Type | API | Notes |
|----------|------|-----|-------|
| `gpt-5.4` | Flagship | Responses | Powers Codex. Combines coding + reasoning. Recommended. |
| `gpt-5.4-pro` | Premium | Responses | API key only. Pro/Business/Enterprise plans. |
| `gpt-5.3-codex` | Coding | Responses **only** | Agentic coding. Supports reasoning effort (low/med/high/xhigh). |
| `gpt-5.3-codex-spark` | Fast | Responses only | Near-instant. **Pro users only.** |
| `gpt-5.2-codex` | Legacy | Responses | Previous coding model. |
| `gpt-5-codex` | Legacy | Responses | Alias, regularly updated snapshot. |
| `gpt-5-codex-mini` | Budget | Responses | Lower cost variant. |
| `gpt-5-mini` | Budget | Both | Cost-sensitive workloads. |

**Retired (Feb 2026):** `gpt-4o`, `gpt-4.1`, `gpt-4.1-mini`, `o4-mini`, `gpt-5` (Instant/Thinking).

ChatGPT Plus ($20/mo) gives access to `gpt-5.3` with 160 messages/3 hours, then downgrades to mini. ChatGPT Pro ($200/mo) gives unlimited + `gpt-5.3-codex-spark`.

---

## How OpenClaw Implements It

### File Structure

```
src/commands/openai-codex-oauth.ts          — Core OAuth flow (PKCE, browser, callback)
src/commands/auth-choice.apply.openai.ts    — Onboarding integration
src/commands/models/auth.ts                 — `openclaw models auth login --provider openai-codex`
src/agents/auth-profiles/oauth.ts           — Token refresh (refreshOAuthTokensForProfile)
src/agents/model-auth.ts                    — Token injection into API requests
```

### Credential Storage

```
~/.openclaw/credentials/oauth.json          — Raw OAuth tokens (import source)
~/.openclaw/agents/<agentId>/agent/auth-profiles.json  — Per-agent profiles
```

**Profile format:**
```json
{
  "openai-codex:user@email.com": {
    "type": "oauth",
    "access": "eyJhb...",
    "refresh": "ort_abc...",
    "expires": 1710180000,
    "email": "user@email.com",
    "accountId": "org-abc123"
  }
}
```

### Key Behaviors

1. **Auto-refresh:** Before each API call, checks `expires`. If within 5 minutes of expiry, refreshes under a file lock.
2. **Race condition handling:** Multiple agents sharing the same credential can cause `refresh_token_reused` errors (issue #26322). Fix: single-writer lock.
3. **Profile keying:** By email (`openai-codex:<email>`) not just `openai-codex:default` — supports multiple accounts.
4. **Default model:** `openai-codex/gpt-5.3-codex` (Codex-specific model variant).
5. **Local callback:** `http://127.0.0.1:1455/auth/callback` — binds a temporary HTTP server.
6. **Headless fallback:** If localhost binding fails, shows the auth URL and asks user to paste the redirect URL/code manually.

---

## Lessons from OpenClaw — Model Naming and Provider Isolation

OpenClaw made critical mistakes mixing OAuth and API key paths that we MUST avoid.

### OpenClaw Bug #30844: Hard-Routing Disaster

OpenClaw's `normalizeModelRef()` function unconditionally reroutes any model containing "codex" in its name to the `openai-codex` (OAuth) provider — even when the user configured an API key. Result: users with API keys who try `gpt-5.3-codex` get a JWT parsing error (`"Failed to extract accountId from token"`). The request fails in 23ms (before reaching the model) instead of the normal 3-5s.

**Root cause:** Model name contains provider routing hints. The name `gpt-5.3-codex` triggers OAuth path regardless of user intent.

### OpenClaw Bug #30533: Onboarding Confusion

OpenClaw's UI groups "OpenAI (API key)" and "OpenAI Codex (OAuth)" under a single "OpenAI" category. Users complete setup with only an API key, then fail when trying Codex models because the OAuth profile is missing.

**Root cause:** The two authentication methods are not visually or conceptually separated.

### Design Rules for TEMM1E (Learned from OpenClaw)

1. **Provider names MUST be distinct:** `openai` (API key) and `openai-codex` (OAuth) are separate providers with separate configs, separate credentials, separate model lists.

2. **Model names MUST NOT encode provider routing:** Never auto-route based on model name substrings. The user's config (`provider.name`) decides the auth path, not the model string.

3. **Model IDs are passed through verbatim:** TEMM1E sends whatever model string the user configured directly to the API. No normalization, no rewriting. `gpt-5.3-codex` is a valid model ID for both API key and OAuth paths — the difference is authentication, not the model name.

4. **Clean removal path:** All OAuth code must be behind a feature flag (`codex-oauth`) and in a separate crate or module. If OpenAI blocks third-party OAuth, we `cargo build` without the flag and everything works exactly as before.

---

## Proposed TEMM1E Implementation Plan

### Architecture: Isolation-First Design

```
crates/
  temm1e-codex-oauth/           ← NEW CRATE (behind feature flag "codex-oauth")
    src/
      lib.rs                     ← Public API: login(), refresh(), token_store()
      pkce.rs                    ← PKCE verifier/challenge generation
      callback_server.rs         ← Temporary axum server for OAuth redirect
      token_store.rs             ← Read/write ~/.temm1e/oauth.json
      responses_provider.rs      ← Provider trait impl using Responses API
```

**Why a separate crate, not a module in temm1e-providers:**
- **Clean removal:** `Cargo.toml` drops the dep, `#[cfg(feature = "codex-oauth")]` gates vanish, zero residue
- **Dependency isolation:** `jsonwebtoken` (if used) only pulled in when feature is enabled
- **No contamination:** `OpenAICompatProvider` stays unchanged — it's a different API shape entirely (Chat Completions vs Responses)
- **Build gating:** CI can test with and without `codex-oauth` to verify clean separation

### Phase 1: OAuth Flow

**New crate:** `crates/temm1e-codex-oauth/`

```rust
/// OAuth token set — stored in ~/.temm1e/oauth.json
#[derive(Serialize, Deserialize)]
pub struct CodexOAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,          // Unix timestamp
    pub email: String,
    pub account_id: String,
}

/// PKCE + OAuth flow
pub async fn login(headless: bool) -> Result<CodexOAuthTokens, Temm1eError> {
    // 1. Generate PKCE verifier (32 random bytes → base64url)
    // 2. Generate code_challenge = base64url(sha256(verifier))
    // 3. Generate random state
    // 4. Build authorize URL
    // 5. If headless: print URL, ask user to paste redirect URL
    //    If GUI: open browser + bind callback server on 127.0.0.1:{port}
    // 6. Wait for auth code
    // 7. POST to token endpoint with code + verifier
    // 8. Decode id_token JWT payload (base64, no signature verification)
    // 9. Extract email + accountId (org) from JWT claims
    // 10. Store tokens to ~/.temm1e/oauth.json
    // 11. Scope probe: make test API call to /v1/responses
    //     If 403 → fail with clear error about missing scopes
}
```

### Phase 2: Responses API Provider

**This is NOT a modification to `OpenAICompatProvider`.** It's a separate `Provider` trait implementation because the Responses API has a fundamentally different request/response shape.

```rust
/// Provider that uses OpenAI Responses API with OAuth tokens
pub struct CodexResponsesProvider {
    token_store: Arc<TokenStore>,  // Handles auto-refresh
    model: String,
    base_url: String,              // https://api.openai.com/v1
}

#[async_trait]
impl Provider for CodexResponsesProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        // 1. Get fresh access token (auto-refresh if within 5 min of expiry)
        // 2. Convert CompletionRequest messages → Responses API "input" format
        // 3. POST /v1/responses with Authorization: Bearer {jwt}
        // 4. Convert Responses API output → CompletionResponse
    }

    async fn stream(&self, request: CompletionRequest) -> Result<CompletionStream> {
        // Same but with stream=true, SSE parsing
    }
}
```

**Request translation (Chat Completions → Responses API):**
```
Chat Completions format:              Responses API format:
{                                     {
  "model": "gpt-5.3-codex",            "model": "gpt-5.3-codex",
  "messages": [                         "instructions": "You are...",
    {"role": "system", "content": ..},  "input": [
    {"role": "user", "content": ..},      {"role": "user", "content": ..},
    {"role": "assistant", "content": ..}  {"role": "assistant", "content": ..}
  ]                                     ]
}                                     }
```

Key differences:
- `system` message → `instructions` top-level field
- `messages` → `input` (items, not messages)
- Response: `output` array of items vs `choices[0].message`
- Tool calls: different schema (Responses API has built-in tool types)

### Phase 3: Token Management

**Storage:** `~/.temm1e/oauth.json` (separate from `credentials.toml`)
```json
{
  "access_token": "eyJhb...",
  "refresh_token": "ort_abc...",
  "expires_at": 1710180000,
  "email": "user@example.com",
  "account_id": "org-abc123"
}
```

**Auto-refresh logic:**
```rust
impl TokenStore {
    pub async fn get_access_token(&self) -> Result<String> {
        let tokens = self.tokens.lock().await;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

        if tokens.expires_at > now + 300 {  // 5 min buffer
            return Ok(tokens.access_token.clone());
        }

        // Refresh
        let new_tokens = refresh_token(&tokens.refresh_token).await?;
        self.save_to_disk(&new_tokens)?;
        Ok(new_tokens.access_token)
    }
}
```

**Race condition prevention:** Single `tokio::sync::Mutex` — only one refresh at a time. Callers waiting for the lock get the already-refreshed token.

### Phase 4: User Experience

**Config (`temm1e.toml`) — completely separate provider block:**
```toml
[provider]
name = "openai-codex"         # ← Distinct provider name
model = "gpt-5.3-codex"       # ← Model ID passed through verbatim
# No api_key needed — uses OAuth tokens from ~/.temm1e/oauth.json
```

vs the existing API key path (unchanged):
```toml
[provider]
name = "openai"
api_key = "${OPENAI_API_KEY}"
model = "gpt-5.2"
```

**Factory routing in `create_provider()`:**
```rust
match provider_name {
    "openai-codex" => {
        #[cfg(feature = "codex-oauth")]
        return Ok(Arc::new(CodexResponsesProvider::new(model, token_store)));
        #[cfg(not(feature = "codex-oauth"))]
        return Err(Temm1eError::Config(
            "OpenAI Codex OAuth requires the 'codex-oauth' feature. \
             Build with: cargo build --features codex-oauth".to_string()
        ));
    }
    "openai" | "gemini" | "grok" | ... => {
        // Existing OpenAICompatProvider path — completely untouched
    }
}
```

**CLI commands:**
```
temm1e auth login                    # Opens browser for OAuth (or headless flow)
temm1e auth status                   # Shows: email, account, token expiry
temm1e auth logout                   # Deletes ~/.temm1e/oauth.json
```

**Telegram (headless):**
```
User: /auth
Bot:  Click this link to authenticate with your ChatGPT account:
      https://auth.openai.com/oauth/authorize?client_id=...&redirect_uri=...

      After signing in, paste the URL you were redirected to here.

User: http://127.0.0.1:1455/auth/callback?code=abc123&state=xyz

Bot:  Authenticated! Connected as user@example.com.
      Model: gpt-5.3-codex (ChatGPT Plus subscription)
```

### Phase 5: Device Code Flow (Stretch)

For environments where neither browser redirect nor URL pasting works:
```
temm1e auth login --device-code
```
Uses OpenAI's device code flow (beta) — user gets a code, enters it at openai.com/device.

### Removal Procedure (If TOS Issues Arise)

If OpenAI blocks third-party OAuth usage:

1. `Cargo.toml`: Remove `codex-oauth` from default features
2. Users: Switch `provider.name` from `"openai-codex"` to `"openai"` + add API key
3. Code: All OAuth code is behind `#[cfg(feature = "codex-oauth")]` — compiles away to nothing
4. No migration needed for API key users — they were never affected
5. `crates/temm1e-codex-oauth/` can be deleted entirely with zero impact on other crates

---

## Open Questions

1. **Client ID legitimacy:** Is reusing `app_EMoamEEZ73f0CkXaXp7hrann` sanctioned by OpenAI? No official docs. Community consensus is "it works, everyone uses it" — OpenClaw, Roo Code, OpenCode, term-llm all do it. Risk: could be blocked. **Mitigation:** feature flag + clean removal path.

2. **Scope requirements:** Identity scopes (`openid profile email offline_access`) may not grant API access. OpenClaw fixed this with a post-login scope probe. **Action:** After login, immediately try a minimal `/v1/responses` call. If 403, fail with clear message.

3. **Model availability:** Codex-specific models (`gpt-5.3-codex`, `gpt-5.3-codex-spark`) are Responses API only. Standard models (`gpt-5.4`, `gpt-5-mini`) work with both APIs. OAuth tokens should work with all models the user's subscription includes. **Need to test:** does a Plus subscription token work with `gpt-5.4` or only Codex variants?

4. **Rate limits:** ChatGPT Plus = 160 messages/3hrs with gpt-5.3, then downgrades to mini. Pro = unlimited + spark. These are subscription-tier limits, not per-token billing. Need to handle rate limit responses gracefully (retry-after, downgrade model suggestion).

5. **Telegram challenge:** Our primary interface is Telegram — no browser on server. Headless flow: bot sends URL → user clicks in their browser → gets redirected to `127.0.0.1` (which won't exist on their machine) → user copies the URL and pastes it back. **Better approach:** Use a known redirect URI that displays the code, or device code flow.

6. **Token lifetime:** Access tokens expire in ~1 hour. Refresh tokens may be single-use (OpenClaw hit `refresh_token_reused` errors with concurrent refresh). **Fix:** Mutex-guarded single-writer refresh.

7. **Legal/TOS:** No clear answer. OpenAI hasn't blocked third-party usage of the Codex client ID despite widespread use. **Mitigation:** Feature flag makes it removable in one commit.

8. **Responses API adapter complexity:** The Responses API has a different request/response shape than Chat Completions. Need to translate TEMM1E's `CompletionRequest` (messages-based) to Responses API format (input/instructions-based). Tool call schemas also differ. This is the biggest implementation effort.

9. **Chat Completions deprecation:** Codex CLI fully deprecated `/v1/chat/completions` in Feb 2026. If OAuth tokens only work with the Responses API endpoint, we MUST implement the Responses API adapter — no shortcut of reusing `OpenAICompatProvider`.

---

## Dependencies (Rust Crates)

| Crate | Purpose |
|-------|---------|
| `sha2` | SHA-256 for PKCE code challenge (already in tree via other deps) |
| `base64` | base64url encoding for PKCE (already in dependencies) |
| `axum` | Temporary HTTP server for OAuth callback (already in dependencies) |
| `reqwest` | HTTP client for token exchange (already in dependencies) |
| `jsonwebtoken` | JWT decoding for id_token (new, but small) |
| `rand` | Random bytes for verifier + state (already in dependencies) |

All major dependencies already exist in the workspace. Only `jsonwebtoken` would be new, and it's optional (can decode JWT payload without verification using base64 decode).

---

## Competitive Analysis

| Feature | OpenClaw | Codex CLI | TEMM1E (Proposed) |
|---------|----------|-----------|-------------------|
| OAuth PKCE | ✓ | ✓ | Planned |
| Device code flow | ✗ | ✓ (beta) | Stretch goal |
| Headless paste flow | ✓ | ✓ | Planned (Telegram) |
| Token auto-refresh | ✓ | ✓ | Planned |
| Multi-account | ✓ (by email) | ✓ | Planned |
| Scope validation | ✓ (post-fix) | ✓ | Planned (probe) |
| API key fallback | ✓ | ✓ | ✓ (existing) |

---

## Sources

- [OpenAI Codex Auth Docs](https://developers.openai.com/codex/auth/)
- [OpenClaw OAuth Docs](https://docs.openclaw.ai/concepts/oauth)
- [OpenClaw PR #32065 — Codex OAuth built-in](https://github.com/openclaw/openclaw/pull/32065)
- [OpenClaw Issue #26801 — Missing OAuth scopes](https://github.com/openclaw/openclaw/issues/26801)
- [OpenClaw Issue #36660 — Token lacks api.responses.write](https://github.com/openclaw/openclaw/issues/36660)
- [OpenClaw Issue #26322 — Refresh token race condition](https://github.com/openclaw/openclaw/issues/26322)
- [OpenCode Issue #3281 — Codex OAuth implementation](https://github.com/anomalyco/opencode/issues/3281)
- [OpenAI Community — ClientID best practices](https://community.openai.com/t/best-practice-for-clientid-when-using-codex-oauth/1371778)
- [OpenClaw DeepWiki — Model Providers & Auth](https://deepwiki.com/openclaw/openclaw/3.3-model-providers-and-authentication)
- [opencode-openai-codex-auth plugin](https://github.com/numman-ali/opencode-openai-codex-auth)
- [Codex Issue #2798 — Remote/headless OAuth](https://github.com/openai/codex/issues/2798)
