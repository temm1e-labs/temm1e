# Migration Guide: SkyClaw → TEMM1E

> **SkyClaw is now TEMM1E.** This change is permanent.
> This guide walks existing SkyClaw users through upgrading.

---

## Quick migration (most users)

If you're running SkyClaw locally with default paths, here's all you need:

### Step 1: Move your data directory

```bash
# Your data lives in ~/.skyclaw — move it to ~/.temm1e
mv ~/.skyclaw ~/.temm1e
```

This moves everything at once: your database, vault, credentials, custom tools, and skills.

### Step 2: Rename your config file

```bash
# In your project/workspace directory
mv skyclaw.toml temm1e.toml
```

Then edit `temm1e.toml` and rename the top-level section:

```toml
# Old
[skyclaw]
mode = "local"

# New
[temm1e]
mode = "local"
```

Everything else in the config stays the same — field names, values, env var references.

### Step 3: Rebuild the binary

```bash
cargo build --release --bin temm1e
```

### Step 4: Run it

```bash
# Old
skyclaw start
skyclaw chat

# New
temm1e start
temm1e chat
temm1e start --personality play   # NEW: PLAY mode
temm1e start --personality work   # NEW: WORK mode
temm1e start --personality pro    # NEW: PRO mode (professional, no emoticons)
```

That's it. Your conversation history, vault secrets, credentials, custom tools, and skills all carry over — the data formats haven't changed.

---

## Detailed file mapping

If you customized paths or need to verify everything moved correctly:

### Data directory (`~/.skyclaw` → `~/.temm1e`)

| Old path                           | New path                          | What it is                        |
|------------------------------------|-----------------------------------|-----------------------------------|
| `~/.skyclaw/memory.db`             | `~/.temm1e/memory.db`             | SQLite conversation history       |
| `~/.skyclaw/vault.enc`             | `~/.temm1e/vault.enc`             | Encrypted secrets (ChaCha20)      |
| `~/.skyclaw/vault.key`             | `~/.temm1e/vault.key`             | 32-byte encryption key            |
| `~/.skyclaw/credentials.toml`      | `~/.temm1e/credentials.toml`      | Provider API keys                 |
| `~/.skyclaw/config.toml`           | `~/.temm1e/config.toml`           | User config (if not using workspace toml) |
| `~/.skyclaw/agent-config.toml`     | `~/.temm1e/agent-config.toml`     | Agent config overrides            |
| `~/.skyclaw/allowlist.toml`        | `~/.temm1e/allowlist.toml`        | Admin user allowlist              |
| `~/.skyclaw/custom-tools/`         | `~/.temm1e/custom-tools/`         | User/agent-created script tools   |
| `~/.skyclaw/skills/`               | `~/.temm1e/skills/`               | Custom Markdown skills            |
| `~/.skyclaw/memory/`               | `~/.temm1e/memory/`               | Markdown memory files (if using Markdown backend) |
| `~/.skyclaw/workspace/`            | `~/.temm1e/workspace/`            | Heartbeat/scheduled task workspace |

### Files you can ignore (auto-regenerated)

- `~/.skyclaw/skyclaw.pid` → `~/.temm1e/temm1e.pid` (recreated on start)
- `~/.skyclaw/skyclaw.log` → `~/.temm1e/temm1e.log` (recreated on start)

### Critical files — do NOT lose these

| File                | Why                                                       |
|---------------------|-----------------------------------------------------------|
| `vault.key`         | Without it, your `vault.enc` secrets are unrecoverable    |
| `vault.enc`         | Your encrypted secrets — paired with `vault.key`          |
| `credentials.toml`  | Your provider API keys                                    |
| `memory.db`         | Your full conversation history                            |

---

## Docker users

### Volume mounts

```yaml
# Old
volumes:
  - skyclaw-data:/var/lib/skyclaw
  - ${HOME}/.skyclaw:/home/skyclaw/.skyclaw

# New
volumes:
  - temm1e-data:/var/lib/temm1e
  - ${HOME}/.temm1e:/home/temm1e/.temm1e
```

### Container paths

| Old                    | New                    |
|------------------------|------------------------|
| `/app/skyclaw`         | `/app/temm1e`          |
| `/var/lib/skyclaw`     | `/var/lib/temm1e`      |
| `/home/skyclaw/.skyclaw` | `/home/temm1e/.temm1e` |

If using named volumes, create the new volume and copy data:

```bash
docker volume create temm1e-data
docker run --rm \
  -v skyclaw-data:/from \
  -v temm1e-data:/to \
  alpine sh -c "cp -a /from/. /to/"
```

---

## Systemd users

### Service file

```bash
# Old: /etc/systemd/system/skyclaw.service
# New: /etc/systemd/system/temm1e.service

sudo systemctl stop skyclaw
sudo systemctl disable skyclaw

# Update paths in the service file:
#   ExecStart=/usr/local/bin/temm1e start
#   WorkingDirectory=/var/lib/temm1e
#   EnvironmentFile=/etc/temm1e/env
#   User=temm1e
#   Group=temm1e

sudo systemctl daemon-reload
sudo systemctl enable temm1e
sudo systemctl start temm1e
```

### System config

```bash
sudo mv /etc/skyclaw /etc/temm1e
sudo mv /var/lib/skyclaw /var/lib/temm1e
```

---

## Library/crate consumers

If you depend on skyclaw as a Rust library:

### Cargo.toml

```toml
# Old
[dependencies]
skyclaw-core = { git = "https://github.com/nagisanzenin/skyclaw" }
skyclaw-agent = { git = "https://github.com/nagisanzenin/skyclaw" }

# New
[dependencies]
temm1e-core = { git = "https://github.com/nagisanzenin/temm1e" }
temm1e-agent = { git = "https://github.com/nagisanzenin/temm1e" }
```

### Rust imports

```rust
// Old
use skyclaw_core::traits::Provider;
use skyclaw_core::types::error::SkyclawError;
use skyclaw_agent::runtime::AgentRuntime;

// New
use temm1e_core::traits::Provider;
use temm1e_core::types::error::Temm1eError;
use temm1e_agent::runtime::AgentRuntime;
```

### Error type

`SkyclawError` → `Temm1eError`. All variants remain the same.

---

## Config file reference

The config schema is unchanged except for the top-level section name. Config is searched in this order:

1. `/etc/temm1e/config.toml` (system)
2. `~/.temm1e/config.toml` (user)
3. `./config.toml` (workspace)
4. `./temm1e.toml` (workspace)

Environment variables still use `${VAR}` expansion syntax. No env var prefixes changed — `ANTHROPIC_API_KEY`, `TELEGRAM_BOT_TOKEN`, etc. are all the same.

---

## What's new in TEMM1E (beyond the rename)

The rebrand shipped with new features:

- **PLAY/WORK/PRO personality modes** — `temm1e start --personality play|work|pro`
- **`mode_switch` agent tool** — Tem can switch modes at runtime (play, work, pro)
- **Soul-injected system prompts** — Tem's character is baked into every LLM call
- **Vision browser** (v2.6.0) — screenshot + visual understanding tools

---

## Troubleshooting

**"config file not found"** — Make sure you renamed `skyclaw.toml` → `temm1e.toml` and the `[skyclaw]` section to `[temm1e]`.

**"database not found"** — Check that `~/.temm1e/memory.db` exists. If you forgot to move the data dir: `mv ~/.skyclaw ~/.temm1e`

**"vault key error"** — The `vault.key` file must be exactly 32 bytes with 0600 permissions. Verify: `ls -la ~/.temm1e/vault.key && wc -c ~/.temm1e/vault.key`

**"permission denied on vault.key"** — Fix permissions: `chmod 600 ~/.temm1e/vault.key`

**Custom tools missing** — Check that `~/.temm1e/custom-tools/` has your `.json` + script files.

---

## Is this permanent?

**Yes.** TEMM1E has its own identity — soul document, design brief, pixel art, voice guardrails, and legal distinction from existing IP. This is not a rename that will change again. SkyClaw is retired.
