# Telegram Channel Setup

## Prerequisites

- A Telegram account
- A Telegram Bot Token from [@BotFather](https://t.me/BotFather)

## Step 1: Create a Bot

1. Open Telegram and search for `@BotFather`
2. Send `/newbot`
3. Follow the prompts to name your bot
4. Copy the bot token (format: `1234567890:AAH...`)

## Step 2: Configure TEMM1E

Add to your `temm1e.toml`:

```toml
[channel.telegram]
enabled = true
token = "${TELEGRAM_BOT_TOKEN}"
allowlist = []           # Empty = auto-whitelist first user as admin
file_transfer = true     # Enable file sending/receiving
```

Set the environment variable:

```bash
export TELEGRAM_BOT_TOKEN="your-bot-token-here"
```

## Step 3: Build and Run

```bash
cargo build --release --features telegram
./target/release/temm1e start
```

## Step 4: Connect

1. Open your bot in Telegram (search for `@your_bot_name`)
2. Send any message — the first user is auto-whitelisted as admin
3. If no API key is configured, TEMM1E enters onboarding mode and asks you to paste one

## User Management

The first user to message the bot becomes the admin. Admin commands:

| Command | Description |
|---------|-------------|
| `/allow <user_id>` | Whitelist a user by their numeric Telegram ID |
| `/revoke <user_id>` | Remove a user from the whitelist |
| `/users` | List all whitelisted users |

The allowlist persists at `~/.temm1e/allowlist.toml`.

To find a user's numeric ID, have them message [@userinfobot](https://t.me/userinfobot).

## File Transfer

- **Receive:** Send documents, photos, audio, voice, or video to the bot
- **Send:** The agent can send files back (up to 50 MB per Telegram's limit)
- Files are saved to the agent's workspace directory

## Security Notes

- Empty `allowlist = []` auto-whitelists the first user only — subsequent users are denied until explicitly allowed
- User matching is by numeric ID only, never by username (usernames can change)
- Bot token is never logged at info level

## Troubleshooting

| Issue | Fix |
|-------|-----|
| `TELEGRAM_BOT_TOKEN not set` | Export the env var or set it in `.env` |
| Bot doesn't respond | Check that your user ID is in the allowlist |
| `polling error` in logs | Normal during network interruptions — auto-reconnects with exponential backoff |
| Files not downloading | Ensure `file_transfer = true` in config |
