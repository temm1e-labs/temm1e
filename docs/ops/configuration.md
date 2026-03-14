# Operations Guide: Configuration

This guide covers the operational aspects of configuring TEMM1E: environment variables, secret management, configuration hierarchy, and common patterns for production deployments.

## Configuration Hierarchy

TEMM1E loads configuration from multiple sources. Each source overrides the previous:

```
1. Compiled defaults        (hardcoded in Rust Default implementations)
       |
2. /etc/temm1e/config.toml (system-level config, set during Docker build)
       |
3. ~/.temm1e/config.toml   (user-level config)
       |
4. ./config.toml            (workspace-level config)
       |
5. TEMM1E_* env vars       (environment variable overrides)
       |
6. CLI flags                (--mode, --config)
       |
7. vault:// resolution      (secrets fetched at runtime)
```

In Docker deployments, the typical sources are:
- `/etc/temm1e/default.toml` (baked into the image)
- A mounted `/etc/temm1e/config.toml` (deployment-specific)
- Environment variables (secrets and runtime overrides)

## Environment Variables

### TEMM1E_* Prefix Mapping

Any configuration key can be set via an environment variable using the `TEMM1E_` prefix. Nested keys use double underscores (`__`).

| Environment Variable | Config Key | Example |
|---------------------|------------|---------|
| `TEMM1E_MODE` | `temm1e.mode` | `cloud` |
| `TEMM1E_GATEWAY__HOST` | `gateway.host` | `0.0.0.0` |
| `TEMM1E_GATEWAY__PORT` | `gateway.port` | `443` |
| `TEMM1E_GATEWAY__TLS` | `gateway.tls` | `true` |
| `TEMM1E_PROVIDER__NAME` | `provider.name` | `anthropic` |
| `TEMM1E_PROVIDER__MODEL` | `provider.model` | `claude-sonnet-4-6` |
| `TEMM1E_MEMORY__BACKEND` | `memory.backend` | `postgres` |
| `TEMM1E_OBSERVABILITY__LOG_LEVEL` | `observability.log_level` | `debug` |
| `TEMM1E_OBSERVABILITY__OTEL_ENABLED` | `observability.otel_enabled` | `true` |

### ${ENV_VAR} Expansion in Config Files

Config values can reference environment variables using `${VAR_NAME}` syntax:

```toml
[provider]
name = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"

[channel.telegram]
token = "${TELEGRAM_BOT_TOKEN}"
```

At load time, `${ANTHROPIC_API_KEY}` is replaced with the value of the `ANTHROPIC_API_KEY` environment variable. If the variable is not set, TEMM1E reports a configuration error at startup.

### Common Environment Variables

These are the environment variables most frequently set in production:

| Variable | Required | Description |
|----------|----------|-------------|
| `ANTHROPIC_API_KEY` | If using Anthropic | Anthropic API key |
| `OPENAI_API_KEY` | If using OpenAI | OpenAI API key |
| `GOOGLE_API_KEY` | If using Google | Google Gemini API key |
| `TELEGRAM_BOT_TOKEN` | If using Telegram | Telegram bot token |
| `DISCORD_BOT_TOKEN` | If using Discord | Discord bot token |
| `SLACK_BOT_TOKEN` | If using Slack | Slack bot token |
| `WHATSAPP_API_TOKEN` | If using WhatsApp | WhatsApp Business API token |
| `DATABASE_URL` | If using PostgreSQL | PostgreSQL connection string |
| `RUST_LOG` | No | Log filter (overrides `observability.log_level`) |
| `TEMM1E_MODE` | No | Runtime mode override |

## vault:// URIs

The `vault://` URI scheme references secrets stored in the encrypted vault.

### How It Works

1. A config value is set to `vault://key-name`
2. At startup, the config loader detects `vault://` prefixes
3. The vault backend decrypts the named secret from `~/.temm1e/vault.enc`
4. The plaintext value replaces the `vault://` URI in memory
5. The vault file itself is never modified during resolution

### Usage

```toml
[provider]
api_key = "vault://anthropic-api-key"

[channel.telegram]
token = "vault://telegram-bot-token"

[memory]
connection_string = "vault://postgres-url"
```

### Storing Secrets in the Vault

Secrets are stored in the vault by sending them via a chat channel. The credential detector automatically identifies API key patterns and encrypts them.

Alternatively, the vault can be managed programmatically through the `Vault` trait.

### Vault File Location

| Mode | Vault File | Key File |
|------|-----------|----------|
| Local | `~/.temm1e/vault.enc` | `~/.temm1e/vault.key` |
| Docker | `/var/lib/temm1e/vault.enc` | `/var/lib/temm1e/vault.key` |

The vault key file must be protected. In Docker deployments, it lives on the persistent volume. For production, consider mounting it from a secrets manager.

### Encryption Details

- Algorithm: ChaCha20-Poly1305 AEAD
- Key derivation: from the vault key file
- Each secret is individually encrypted with a unique nonce
- Plaintext never touches disk

## Dual-Mode Configuration

### Cloud Mode

Recommended settings for production cloud deployments:

```toml
[temm1e]
mode = "cloud"

[gateway]
host = "0.0.0.0"
port = 443
tls = true
tls_cert = "/etc/temm1e/cert.pem"
tls_key = "/etc/temm1e/key.pem"

[memory]
backend = "postgres"
connection_string = "vault://postgres-url"

[security]
sandbox = "mandatory"
file_scanning = true
skill_signing = "required"
audit_log = true

[security.rate_limit]
requests_per_minute = 60

[observability]
log_level = "info"
otel_enabled = true
otel_endpoint = "http://otel-collector:4317"
```

Cloud mode automatically:
- Binds to all interfaces (`0.0.0.0`)
- Requires TLS
- Prefers PostgreSQL for memory
- Uses headless browser only

### Local Mode

Recommended settings for local development:

```toml
[temm1e]
mode = "local"

[gateway]
host = "127.0.0.1"
port = 8080

[memory]
backend = "sqlite"
path = "~/.temm1e/memory.db"

[observability]
log_level = "debug"
```

Local mode automatically:
- Binds to localhost only
- Does not require TLS
- Uses SQLite
- Supports headed browser (GUI mode)

### Auto Mode

When `mode = "auto"`, TEMM1E detects the environment:

1. Container runtime detected (/.dockerenv or cgroup)? -> cloud
2. Cloud metadata endpoint reachable (169.254.169.254)? -> cloud
3. Display server available ($DISPLAY or $WAYLAND_DISPLAY)? -> local with GUI
4. Otherwise -> local headless

## ZeroClaw Configuration Compatibility

TEMM1E reads ZeroClaw TOML configuration files. If a `config.toml` is detected as ZeroClaw format, it is automatically converted at load time.

Mapped sections:
- ZeroClaw provider configs -> TEMM1E `[provider]`
- ZeroClaw channel configs -> TEMM1E `[channel.*]`
- ZeroClaw memory configs -> TEMM1E `[memory]`
- ZeroClaw tunnel configs -> TEMM1E `[tunnel]`

Unsupported fields generate a warning in the logs.

## OpenClaw Configuration Compatibility

TEMM1E can also read OpenClaw YAML configuration files. If a `.yaml` or `.yml` config is detected, it is parsed and converted to the TEMM1E format.

```bash
# Migrate from OpenClaw
temm1e migrate --from openclaw /path/to/openclaw/workspace
```

## Configuration Validation

Validate your configuration before deploying:

```bash
temm1e config validate
```

This checks:
- Config file syntax (TOML/YAML parsing)
- Required fields are present when a section is enabled
- Environment variable references resolve
- vault:// URIs reference existing keys (if vault is accessible)
- Channel allowlists are non-empty when channels are enabled

To see the fully resolved configuration:

```bash
temm1e config show
```

This prints the merged configuration from all sources (with secrets redacted in the output).
