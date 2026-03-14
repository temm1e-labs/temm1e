# OTK Secure Key Setup — Design Document

## Problem

TEMM1E users configure API keys by pasting them directly into messaging channels (Telegram, Discord, Slack). This exposes raw keys to:

1. **Messaging platform servers** — Telegram/Discord/Slack store message content; bot chats lack E2E encryption
2. **Chat history** — keys persist in scrollback until manually deleted
3. **Network observers** — if the channel transport isn't encrypted

The LLM provider never sees the key (onboarding intercepts before the agent loop), and keys are encrypted at rest in the vault. But the **channel transit** gap is real.

## Solution: One-Time Key (OTK) Encryption via URL Fragment

A static HTML page hosted on GitHub Pages acts as a client-side encryption bridge. The encryption key is passed in the URL fragment (`#`), which is never sent to any server. The user's browser encrypts the API key locally, and only an encrypted blob travels through the messaging channel.

## Architecture

```
┌──────────┐       ┌──────────────┐       ┌──────────────────┐
│  User    │       │  Messaging   │       │    TEMM1E       │
│  Phone   │       │  Platform    │       │  (user's server) │
└────┬─────┘       └──────┬───────┘       └────────┬─────────┘
     │                    │                        │
     │ 1. /addkey         │                        │
     │───────────────────→│───────────────────────→│
     │                    │                        │
     │                    │  2. Generate OTK       │
     │                    │     (random 256-bit)   │
     │                    │     Store in memory:   │
     │                    │     { chat_id → OTK }  │
     │                    │                        │
     │ 3. "Setup link:    │                        │
     │  ...github.io/     │                        │
     │  setup#OTK"        │                        │
     │←───────────────────│←───────────────────────│
     │                    │                        │
     │  Platform sees URL │                        │
     │  but #OTK fragment │                        │
     │  is STRIPPED by    │                        │
     │  HTTP spec — never │                        │
     │  stored on their   │                        │
     │  servers            │                        │
     │                    │                        │
     │ 4. User clicks link│                        │
     │                    │                        │
     │    ┌───────────────┴──────┐                 │
     │    │    GitHub Pages      │                 │
     │    │    (static CDN)      │                 │
     │    │                      │                 │
     │    │ Serves same HTML to  │                 │
     │    │ everyone. Sees only  │                 │
     │    │ GET /setup — never   │                 │
     │    │ the #fragment        │                 │
     │    └───────────────┬──────┘                 │
     │                    │                        │
     │ 5. Browser loads page                       │
     │    JS reads window.location.hash → OTK      │
     │                                             │
     │ 6. User pastes API key into form            │
     │                                             │
     │ 7. JS encrypts:                             │
     │    AES-256-GCM(key=OTK, plaintext=API_KEY)  │
     │    → "enc:v1:base64ciphertext"              │
     │                                             │
     │ 8. Page shows: "Paste this in chat"         │
     │                                             │
     │ 9. User pastes    │                         │
     │ "enc:v1:SGVsbG8..." │                       │
     │───────────────────→│────────────────────────→│
     │                    │                         │
     │  Platform sees only│   10. main.rs (Rust):   │
     │  encrypted blob —  │       detect "enc:v1:"  │
     │  useless without   │       lookup OTK by     │
     │  OTK               │         chat_id         │
     │                    │       AES-GCM decrypt   │
     │                    │       validate key      │
     │                    │       save to vault     │
     │                    │       delete OTK        │
     │                    │       hot-reload agent  │
     │                    │                         │
     │ 11. "API key       │                         │
     │  configured ✓"     │                         │
     │←───────────────────│←────────────────────────│
     │                    │                         │
     │                    │   LLM NEVER INVOLVED    │
```

## Security Properties

### What each party sees

| Party | Sees | Can recover API key? |
|-------|------|---------------------|
| Messaging platform servers | Link URL (fragment stripped) + `enc:v1:ciphertext` | No |
| GitHub Pages CDN | `GET /setup` (no fragment, no query params) | No |
| Network observers | TLS-encrypted HTTPS traffic | No |
| Chat history (if not deleted) | A link + encrypted blob; OTK expired/consumed | No |
| The LLM (Anthropic/OpenAI/etc) | Nothing — intercepted before agent loop | No |
| TEMM1E process (user's server) | Decrypted key in memory (required for API calls) | Yes — by design |
| User's browser (user's device) | OTK + raw key (user typed it) | Yes — it's their device |

### Why the URL fragment is safe

Per RFC 3986, the fragment identifier (`#` and everything after) is:
- **Never sent to the server** in HTTP requests
- **Not included** in the `Referer` header
- Processed entirely client-side by the browser
- Not logged by CDNs, proxies, or web servers

GitHub Pages receives `GET /setup` — it has no knowledge of the OTK.

### Why AES-256-GCM

- **Authenticated encryption** — if anyone tampers with the ciphertext, decryption fails (auth tag mismatch)
- **Built into every modern browser** via WebCrypto API — no external JS dependencies
- **Available in Rust** via the `aes-gcm` crate (or reuse existing `chacha20poly1305` patterns)
- **256-bit key** from the OTK provides full security margin

## OTK Lifecycle

```
/addkey command received
    ↓
Generate: crypto::random(32 bytes) → 256-bit OTK
    ↓
Store: HashMap<chat_id, SetupToken { otk, created_at }>
    ↓
Send link: "https://nagisanzenin.github.io/setup#{hex(otk)}"
    ↓
┌─── waiting (max 10 min) ───┐
│                             │
▼                             ▼
User pastes enc:v1:blob     Timer expires
    ↓                         ↓
Consume: remove from map    Cleanup: remove from map
    ↓                         ↓
Decrypt → validate → save   Link is dead
    ↓
Delete OTK from memory
    ↓
Gone forever
```

### Properties
- **One-time use**: consumed on first successful decryption, then deleted
- **Time-limited**: expires after 10 minutes regardless of use
- **Chat-scoped**: tied to a specific `chat_id`; cannot be used from a different conversation
- **Memory-only**: never written to disk; lost on process restart (user just types `/addkey` again)
- **No collision risk**: keyed by `chat_id` (platform-guaranteed unique), not by OTK value

## Command Interface

All commands are intercepted at the application layer in `main.rs` **before** the message reaches the agent. The LLM is never involved in credential operations.

| Command | Action | LLM involved? |
|---------|--------|---------------|
| `/addkey` | Generate OTK, send secure setup link | No |
| `/addkey unsafe` | Accept raw key paste in next message | No |
| `/keys` | List configured providers (names only, never keys) | No |
| `/removekey <provider>` | Remove a provider's credentials | No |
| `enc:v1:...` (auto-detected) | Decrypt and save key from OTK flow | No |
| Raw key paste (auto-detected) | Legacy flow — detect, save, delete message | No |
| "Switch to OpenAI" | Natural language model switching | Yes — but no keys involved |

## Message Handler Flow

```rust
// main.rs — message handler priority order

// 1. Commands — intercepted before agent
if msg.text.starts_with("/addkey") { ... return; }
if msg.text.starts_with("/keys") { ... return; }
if msg.text.starts_with("/removekey") { ... return; }

// 2. Pending raw key paste (from /addkey unsafe)
if pending_raw_key.contains(&msg.chat_id) { ... return; }

// 3. Encrypted blob from OTK flow
if msg.text.starts_with("enc:v1:") { ... return; }

// 4. Auto-detect raw keys in normal messages (backwards compat)
if let Some((provider, key)) = detect_api_key(&msg.text) { ... }

// 5. ONLY NOW does the message reach the agent
agent.process_message(msg).await;
```

## Hot-Reload Integration

After any key operation (add/remove/rotate), the agent is reloaded with the updated provider configuration:

```rust
// Save new credentials
save_credentials(provider, api_key, model, base_url).await?;

// Reload all active keys
let keys = load_active_provider_keys()?;

// Create new agent with updated provider
let new_agent = AgentRuntime::with_limits(
    provider, tools, memory, channels,
    max_tokens, max_rounds, max_spend_usd
);

// Atomic swap — zero downtime
*agent_state.write().await = Some(Arc::new(new_agent));

// User's next message uses the new provider
```

## GitHub Pages Setup

### Hosting

The static HTML page is hosted on GitHub Pages — a free CDN with global edge caching.

```bash
# Option A: gh-pages branch on the main repo
git checkout --orphan gh-pages
mkdir setup && cp setup.html setup/index.html
git add . && git commit -m "OTK setup page"
git push origin gh-pages
# → https://nagisanzenin.github.io/temm1e/setup

# Option B: Dedicated repo
# Create nagisanzenin/temm1e-setup, push index.html
# → https://nagisanzenin.github.io/temm1e-setup/
```

### Scale

The page is a single static HTML file (~5KB). GitHub Pages is backed by Fastly CDN.

| Users setting up | Bandwidth/day | GitHub Pages limit |
|-----------------|---------------|-------------------|
| 100/day | 500 KB | 100 GB/month |
| 10,000/day | 50 MB | 100 GB/month |
| 1,000,000/day | 5 GB | Would need own CDN |

Key setup is a **one-time event per user** (occasionally repeated for key rotation). Even with millions of TEMM1E deployments, the actual setup traffic is negligible.

### No server-side processing

GitHub Pages serves the same cached file to every request. It performs zero computation, stores zero state, and has zero knowledge of any OTK or API key.

## Channel Agnosticism

The scheme requires only two primitives from any messaging platform:
1. **Send text** (the link)
2. **Receive text** (the encrypted blob)

Every messaging platform supports this. No platform-specific APIs, webhooks, or integrations needed.

| Platform | Link with #fragment | Encrypted blob | Works? |
|----------|-------------------|----------------|--------|
| Telegram | ✓ | ✓ | Yes |
| Discord | ✓ | ✓ | Yes |
| Slack | ✓ | ✓ | Yes |
| WhatsApp | ✓ | ✓ | Yes |
| Signal | ✓ | ✓ | Yes |
| SMS/iMessage | ✓ (auto-linked) | ✓ | Yes |
| Email | ✓ | ✓ | Yes |
| IRC | ✓ (user copies URL) | ✓ | Yes |
| CLI | N/A (local) | N/A | Direct input |

Adding a new messaging channel to TEMM1E requires **zero changes** to the key setup flow.

## Fallback Modes

| User situation | Method | Security level |
|----------------|--------|---------------|
| Can open a browser | OTK secure flow | Key never in plaintext on any channel |
| Can't open browser / prefers speed | `/addkey unsafe` + paste | Key visible in chat briefly; auto-deleted |
| Config-savvy / CI/CD | `temm1e.toml` or env vars | Key never touches any channel |
| Existing users (backwards compat) | Auto-detect raw key in message | Same as current behavior |

## Implementation Scope (v1.5.0 — Implemented)

### New files
- `crates/temm1e-gateway/src/setup_tokens.rs` — OTK store with `SetupLinkGenerator` trait impl
- `crates/temm1e-core/src/traits/setup.rs` — `SetupLinkGenerator` trait (cross-crate OTK link generation)
- `crates/temm1e-tools/src/key_manage.rs` — `KeyManageTool` — agent generates OTK setup links in natural language
- `docs/setup/index.html` — static setup page for GitHub Pages (WebCrypto AES-256-GCM)

### Modified files
- `src/main.rs` — command interception (`/addkey`, `/keys`, `/removekey`), `enc:v1:` detection and decryption, `SecretCensorChannel` wrapper, `censor_secrets()` output filter, proactive onboarding with auto-generated setup links
- `crates/temm1e-tools/src/lib.rs` — `create_tools()` accepts `SetupLinkGenerator`, registers `KeyManageTool`
- `Cargo.toml` — added `aes-gcm`, `hex`, `async-trait` dependencies

### Security hardening
- System prompt `SECRET HANDLING` section: 3-environment model (user → claw → pc), one-way secret flow
- `SecretCensorChannel`: wraps all outbound channels, string-matches known API keys → `[REDACTED]`
- `censor_secrets()`: applied at agent reply path AND intermediate `send_message` tool calls

### Dependencies
- `aes-gcm` crate for server-side AES-256-GCM decryption
- `hex` crate for OTK hex encoding
- No new JS dependencies — WebCrypto API is built into browsers

### Test plan
- Unit tests: OTK generation, expiry, consumption, chat_id isolation
- Unit tests: AES-GCM encrypt/decrypt round-trip
- Unit tests: `enc:v1:` prefix detection in `detect_api_key()`
- Unit tests: command interception (`/addkey`, `/keys`, `/removekey`)
- Integration test: full flow — generate OTK → encrypt in simulated browser → paste blob → verify key saved
- Integration test: expired OTK rejection
- Integration test: wrong chat_id OTK rejection
- E2E test: via CLI chat — `/addkey` → verify link format → paste encrypted blob → verify provider configured

## Future Considerations

- **Custom domain**: Replace `nagisanzenin.github.io` with `setup.temm1e.dev` when the project warrants it
- **Telegram Mini Apps**: Native in-app WebView for seamless UX (Telegram-specific enhancement, not a replacement)
- **Key rotation alerts**: Notify users when keys approach expiry (for providers that support this)
- **Multi-key confirmation**: Show diff of provider changes before hot-reload ("You're adding OpenAI. Continue?")
