# TEMM1E — Setup for Pros

You know what you're doing. Here's what you need.

## Requirements

- Rust 1.82+ (or Docker)
- Chrome/Chromium (optional, for browser tool)
- Telegram bot token via [@BotFather](https://t.me/BotFather)

## Build

```bash
git clone https://github.com/nagisanzenin/temm1e.git && cd temm1e
cargo build --release   # ~2.5min cold, 9.6 MB binary
```

## Authentication — Pick Your Poison

### Codex OAuth (ChatGPT Plus/Pro)

```bash
temm1e auth login                    # browser flow
temm1e auth login --headless         # headless (paste redirect URL)
temm1e auth login --output ./o.json  # export token for containers
temm1e auth status                   # check expiry
```

Tokens last ~10 days. Stored at `~/.temm1e/oauth.json`. Auto-detected at startup.

### API Key

No auth flow needed. Start the bot, paste any supported key in Telegram. Auto-detected:

| Prefix | Provider |
|--------|----------|
| `sk-ant-` | Anthropic |
| `sk-` | OpenAI |
| `AIzaSy` | Gemini |
| `xai-` | Grok |
| `sk-or-` | OpenRouter |

Or use the OTK secure setup link (AES-256-GCM encrypted client-side).

## Run

```bash
export TELEGRAM_BOT_TOKEN="..."
temm1e start                          # foreground
temm1e start -d                       # daemon (logs: ~/.temm1e/temm1e.log)
temm1e start -d --log /var/log/sk.log # custom log path
temm1e stop                           # graceful shutdown
```

## Configuration

Config file: `temm1e.toml` (project root) or `~/.temm1e/temm1e.toml`.

```toml
[provider]
name = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"
model = "claude-sonnet-4-6"

[agent]
max_spend_usd = 5.0   # 0.0 = unlimited (default)

[channel.telegram]
enabled = true
token = "${TELEGRAM_BOT_TOKEN}"
allowlist = []         # empty = auto-whitelist first user
file_transfer = true

[memory]
backend = "sqlite"

[security]
sandbox = "mandatory"
```

Environment variables expand via `${VAR}` syntax. Full schema: `crates/temm1e-core/src/types/config.rs`.

## Docker

```bash
# Authenticate on host
temm1e auth login --output ./oauth.json

# Or set API key as env var
echo "ANTHROPIC_API_KEY=sk-ant-..." > .env
```

```yaml
# docker-compose.yml
services:
  temm1e:
    build: .
    environment:
      - TELEGRAM_BOT_TOKEN=${TELEGRAM_BOT_TOKEN}
    volumes:
      - ./oauth.json:/root/.temm1e/oauth.json      # Codex OAuth
      - ./temm1e.toml:/root/.temm1e/temm1e.toml   # config
      - temm1e-data:/root/.temm1e                   # persistent state
    restart: unless-stopped

volumes:
  temm1e-data:
```

`TELEGRAM_BOT_TOKEN` env var auto-injects into Telegram config. No need to duplicate it in `temm1e.toml`.

## VPS Deployment (systemd)

```bash
# Build on server (or cross-compile and scp the binary)
cargo build --release
sudo cp target/release/temm1e /usr/local/bin/

# Create systemd service
sudo tee /etc/systemd/system/temm1e.service << 'EOF'
[Unit]
Description=TEMM1E AI Agent
After=network.target

[Service]
Type=simple
User=temm1e
Environment=TELEGRAM_BOT_TOKEN=your-token
Environment=ANTHROPIC_API_KEY=your-key
ExecStart=/usr/local/bin/temm1e start
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now temm1e
journalctl -u temm1e -f  # tail logs
```

Minimum VPS: 512 MB RAM, 1 vCPU. Idles at 15 MB RSS. >:3

## Multi-Provider — Switch Live

Swap providers mid-conversation with `/model`:

```
/model                    # list available models
/model gpt-5.4           # switch to GPT-5.4
/model claude-sonnet-4-6 # switch to Claude
```

Or just say "Switch to GPT-5.2" — natural language works too.

Credentials stored at `~/.temm1e/credentials.toml`. The agent reads and edits this file itself.

## MCP Servers — Extend at Runtime

```
/mcp add fetch npx -y @modelcontextprotocol/server-fetch
/mcp add github npx -y @modelcontextprotocol/server-github
/mcp                    # list connected servers
/mcp remove fetch       # disconnect
```

The agent also self-extends — it searches the 14-server built-in registry by capability when it needs something it doesn't have.

Config: `~/.temm1e/mcp.toml`

## Key Paths

| Path | Purpose |
|------|---------|
| `~/.temm1e/` | Home directory (all persistent state) |
| `~/.temm1e/credentials.toml` | Provider API keys (encrypted) |
| `~/.temm1e/oauth.json` | Codex OAuth tokens |
| `~/.temm1e/memory.db` | SQLite memory backend |
| `~/.temm1e/allowlist.toml` | User whitelist |
| `~/.temm1e/custom-tools/` | Agent-authored script tools |
| `~/.temm1e/mcp.toml` | MCP server configuration |
| `~/.temm1e/temm1e.log` | Daemon log (with `-d`) |

## Updating

```bash
temm1e update   # git pull + cargo build --release
# or manually:
git pull && cargo build --release
```

## Compilation Gates

All four pass before anything touches main. No exceptions.

```bash
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo test --workspace    # 1,378 tests, 0 failures
```

## Design Documents

| Document | Topic |
|----------|-------|
| [COGNITIVE_ARCHITECTURE.md](docs/design/COGNITIVE_ARCHITECTURE.md) | The Finite Brain Model — context as working memory |
| [BLUEPRINT_SYSTEM.md](docs/design/BLUEPRINT_SYSTEM.md) | Blueprint procedural memory vision |
| [BLUEPRINT_MATCHING_V2.md](docs/design/BLUEPRINT_MATCHING_V2.md) | Zero-extra-LLM-call matching architecture |
| [BLUEPRINT_IMPLEMENTATION.md](docs/design/BLUEPRINT_IMPLEMENTATION.md) | Step-by-step implementation plan |
| [OTK_SECURE_KEY_SETUP.md](docs/OTK_SECURE_KEY_SETUP.md) | AES-256-GCM encrypted onboarding |
| [BENCHMARK_REPORT.md](docs/benchmarks/BENCHMARK_REPORT.md) | Performance benchmarks vs OpenClaw/ZeroClaw |
