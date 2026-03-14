# API Reference: Configuration

TEMM1E uses TOML as its native configuration format. ZeroClaw TOML and OpenClaw YAML configs are detected and converted automatically at load time.

## Configuration Resolution Order

Sources are loaded in this order. Later sources override earlier ones:

1. **Compiled defaults** -- hardcoded in Rust structs via `Default` implementations
2. **System config** -- `/etc/temm1e/config.toml`
3. **User config** -- `~/.temm1e/config.toml`
4. **Workspace config** -- `./config.toml` in the current directory
5. **Environment variables** -- `TEMM1E_*` prefix, mapped to config keys
6. **CLI flags** -- `--mode`, `--config`, etc.
7. **vault:// URIs** -- resolved from the encrypted vault at runtime

## Sections

### [temm1e]

Top-level runtime settings.

```toml
[temm1e]
mode = "auto"              # "cloud" | "local" | "auto"
tenant_isolation = false   # Enable multi-tenant isolation (future)
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `mode` | string | `"auto"` | Runtime mode. `cloud` binds to 0.0.0.0, requires TLS, uses PostgreSQL/KMS defaults. `local` binds to 127.0.0.1, uses SQLite/local vault defaults. `auto` detects the environment. |
| `tenant_isolation` | bool | `false` | When true, each user gets an isolated workspace. Single-tenant for v0.1. |

### [gateway]

HTTP/WebSocket gateway server settings.

```toml
[gateway]
host = "127.0.0.1"
port = 8080
tls = false
# tls_cert = "/etc/temm1e/cert.pem"
# tls_key = "/etc/temm1e/key.pem"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `host` | string | `"127.0.0.1"` | Bind address. Cloud mode overrides to `"0.0.0.0"`. |
| `port` | u16 | `8080` | Bind port. |
| `tls` | bool | `false` | Enable TLS (rustls). Required in cloud mode. |
| `tls_cert` | string? | null | Path to TLS certificate file (PEM). |
| `tls_key` | string? | null | Path to TLS private key file (PEM). |

### [provider]

AI model provider configuration.

```toml
[provider]
name = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"
model = "claude-sonnet-4-6"
# base_url = "https://api.openai.com/v1"   # for openai-compatible
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `name` | string? | null | Provider backend: `"anthropic"`, `"openai-compatible"`, `"google"`, `"mistral"`, `"groq"` |
| `api_key` | string? | null | API key. Supports `${ENV_VAR}` expansion and `vault://key-name` URIs. |
| `model` | string? | null | Model identifier (e.g., `"claude-sonnet-4-6"`, `"gpt-4"`, `"gemini-pro"`). |
| `base_url` | string? | null | Custom API endpoint. Required for OpenAI-compatible backends (Ollama, vLLM, etc.). |

#### Provider examples

**Anthropic:**
```toml
[provider]
name = "anthropic"
api_key = "vault://anthropic-key"
model = "claude-sonnet-4-6"
```

**OpenAI-compatible (local Ollama):**
```toml
[provider]
name = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "llama3.1"
```

**Google Gemini:**
```toml
[provider]
name = "google"
api_key = "${GOOGLE_API_KEY}"
model = "gemini-pro"
```

### [memory]

Memory backend for conversation history and long-term memory.

```toml
[memory]
backend = "sqlite"
# path = "~/.temm1e/memory.db"
# connection_string = "postgres://user:pass@localhost/temm1e"

[memory.search]
vector_weight = 0.7
keyword_weight = 0.3
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `backend` | string | `"sqlite"` | Backend: `"sqlite"`, `"postgres"`, `"markdown"` |
| `path` | string? | null | File path for SQLite database or Markdown directory |
| `connection_string` | string? | null | PostgreSQL connection string |
| `search.vector_weight` | f32 | `0.7` | Weight for vector similarity in hybrid search (0.0-1.0) |
| `search.keyword_weight` | f32 | `0.3` | Weight for keyword matching in hybrid search (0.0-1.0) |

### [vault]

Encrypted secrets management.

```toml
[vault]
backend = "local-chacha20"
# key_file = "~/.temm1e/vault.key"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `backend` | string | `"local-chacha20"` | Vault backend. `"local-chacha20"` uses ChaCha20-Poly1305 AEAD. |
| `key_file` | string? | null | Path to vault encryption key. If not set, defaults to `~/.temm1e/vault.key`. |

### [filestore]

File storage backends for received and generated files.

```toml
[filestore]
backend = "local"
# path = "~/.temm1e/files"
# bucket = "my-temm1e-bucket"
# region = "us-east-1"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `backend` | string | `"local"` | Storage backend: `"local"` (filesystem) or `"s3"` (S3/R2/GCS compatible) |
| `path` | string? | null | Local filesystem path for file storage |
| `bucket` | string? | null | S3 bucket name |
| `region` | string? | null | S3 region |

### [security]

Security policy settings. All policies are enforced by default.

```toml
[security]
sandbox = "mandatory"
file_scanning = true
skill_signing = "required"
audit_log = true

# [security.rate_limit]
# requests_per_minute = 60
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `sandbox` | string | `"mandatory"` | Sandbox mode. Always `"mandatory"` -- cannot be disabled. |
| `file_scanning` | bool | `true` | Scan uploaded files for embedded secrets and API keys. |
| `skill_signing` | string | `"required"` | Require Ed25519 signatures on skills. |
| `audit_log` | bool | `true` | Log all tool executions, vault access, and file transfers. |
| `rate_limit.requests_per_minute` | u32? | null | Per-user rate limit. Null means no limit. |

### [heartbeat]

Periodic heartbeat checker. Reads a HEARTBEAT.md file from the workspace.

```toml
[heartbeat]
interval = "30m"
checklist = "HEARTBEAT.md"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `interval` | string | `"30m"` | Check interval (e.g., `"5m"`, `"1h"`, `"30m"`). |
| `checklist` | string | `"HEARTBEAT.md"` | Path to the heartbeat checklist file. |

### [cron]

Persistent cron job scheduler.

```toml
[cron]
storage = "sqlite"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `storage` | string | `"sqlite"` | Where to persist cron jobs. |

### [tools]

Enable or disable built-in tools.

```toml
[tools]
shell = true
browser = true
file = true
git = true
cron = true
http = true
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `shell` | bool | `true` | Shell command execution |
| `browser` | bool | `true` | Browser automation (requires Chrome/Chromium) |
| `file` | bool | `true` | File read/write/search operations |
| `git` | bool | `true` | Git operations |
| `cron` | bool | `true` | Cron job management |
| `http` | bool | `true` | HTTP request tool |

### [tunnel]

Optional tunnel for external access.

```toml
[tunnel]
provider = "cloudflare"
token = "${CLOUDFLARE_TUNNEL_TOKEN}"
# command = "cloudflared tunnel run"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `provider` | string | -- | Tunnel provider: `"cloudflare"`, `"ngrok"`, etc. |
| `token` | string? | null | Authentication token for the tunnel provider. |
| `command` | string? | null | Custom command to start the tunnel. |

### [observability]

Logging and metrics configuration.

```toml
[observability]
log_level = "info"
otel_enabled = false
# otel_endpoint = "http://localhost:4317"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `log_level` | string | `"info"` | Log level: `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"` |
| `otel_enabled` | bool | `false` | Enable OpenTelemetry export. |
| `otel_endpoint` | string? | null | OpenTelemetry collector gRPC endpoint. |

### [channel.\<name\>]

Per-channel configuration. The key is the channel name (e.g., `telegram`, `discord`, `slack`, `whatsapp`).

```toml
[channel.telegram]
enabled = true
token = "${TELEGRAM_BOT_TOKEN}"
allowlist = ["alice", "bob"]
file_transfer = true
# max_file_size = "50MB"
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Whether this channel is active. |
| `token` | string? | null | Bot/API token. Supports `${ENV_VAR}` and `vault://` URIs. |
| `allowlist` | string[] | `[]` | Allowed user IDs or usernames. Empty = deny all. |
| `file_transfer` | bool | `true` | Enable bi-directional file transfer. |
| `max_file_size` | string? | null | Override the default maximum file size for this channel. |

## Environment Variable Mapping

Any config value can be set via an environment variable using the `TEMM1E_` prefix:

```bash
TEMM1E_MODE=cloud                     # temm1e.mode
TEMM1E_GATEWAY__HOST=0.0.0.0          # gateway.host
TEMM1E_GATEWAY__PORT=443              # gateway.port
TEMM1E_PROVIDER__NAME=anthropic       # provider.name
TEMM1E_OBSERVABILITY__LOG_LEVEL=debug # observability.log_level
```

Double underscores (`__`) separate nested keys.

## vault:// URI Scheme

Any config value that accepts a string can reference a vault secret:

```toml
[provider]
api_key = "vault://anthropic-api-key"

[channel.telegram]
token = "vault://telegram-bot-token"
```

The vault resolver decrypts the named secret at runtime from the local vault file (`~/.temm1e/vault.enc`).

## Full Example

```toml
[temm1e]
mode = "cloud"

[gateway]
host = "0.0.0.0"
port = 443
tls = true
tls_cert = "/etc/temm1e/cert.pem"
tls_key = "/etc/temm1e/key.pem"

[provider]
name = "anthropic"
api_key = "vault://anthropic-key"
model = "claude-sonnet-4-6"

[memory]
backend = "postgres"
connection_string = "vault://postgres-connection-string"

[memory.search]
vector_weight = 0.7
keyword_weight = 0.3

[vault]
backend = "local-chacha20"
key_file = "/var/lib/temm1e/.vault.key"

[filestore]
backend = "s3"
bucket = "temm1e-files"
region = "us-east-1"

[security]
sandbox = "mandatory"
file_scanning = true
skill_signing = "required"
audit_log = true

[security.rate_limit]
requests_per_minute = 60

[heartbeat]
interval = "15m"
checklist = "HEARTBEAT.md"

[tools]
shell = true
browser = true
file = true
git = true
cron = true
http = true

[tunnel]
provider = "cloudflare"
token = "vault://cloudflare-tunnel-token"

[observability]
log_level = "info"
otel_enabled = true
otel_endpoint = "http://otel-collector:4317"

[channel.telegram]
enabled = true
token = "vault://telegram-token"
allowlist = ["admin_user"]
file_transfer = true

[channel.discord]
enabled = true
token = "vault://discord-token"
allowlist = ["123456789012345678"]
file_transfer = true

[channel.slack]
enabled = true
token = "vault://slack-token"
allowlist = ["U0123456789"]
file_transfer = true
```
