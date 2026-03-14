# CLI Channel Setup

The CLI channel provides an interactive REPL for local development and testing. No tokens, accounts, or external services required.

## Usage

```bash
cargo build --release
./target/release/temm1e chat
```

This starts an interactive session in your terminal. Type messages and press Enter to send them to the agent.

## Commands

| Command | Description |
|---------|-------------|
| `/quit` or `/exit` | Exit the session |
| `/file <path>` | Attach a local file to your next message |

## File Transfer

- **Send files to the agent:** Use `/file path/to/file.txt` before your message
- **Receive files from the agent:** Files are saved to the workspace directory and the path is displayed
- Max file size: 100 MB

## Configuration

The CLI channel is always available (no feature flag needed). It uses the default provider and model from your config:

```toml
# temm1e.toml — no channel config needed for CLI
[provider]
name = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"
model = "claude-sonnet-4-6"
```

## When to Use

- Local development and testing
- Debugging agent behavior without a messaging app
- Scripting and automation (pipe input via stdin)
- Quick one-off interactions

## Security Notes

- The CLI channel allows all users (no allowlist enforcement)
- Only use in trusted local environments
- API keys are read from config/env, never prompted in the REPL
