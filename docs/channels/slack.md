# Slack Channel Setup

## Prerequisites

- A Slack workspace where you have admin permissions
- A Slack Bot Token from the [Slack API Portal](https://api.slack.com/apps)

## Step 1: Create a Slack App

1. Go to [api.slack.com/apps](https://api.slack.com/apps) and click **Create New App**
2. Choose **From scratch**, name it, and select your workspace
3. Go to **OAuth & Permissions** in the left sidebar
4. Under **Bot Token Scopes**, add:
   - `channels:history` — read messages in public channels
   - `channels:read` — list public channels
   - `chat:write` — send messages
   - `files:read` — download files shared in channels
   - `files:write` — upload files to channels
   - `users:read` — look up user info
5. Click **Install to Workspace** at the top
6. Copy the **Bot User OAuth Token** (starts with `xoxb-`)

## Step 2: Invite the Bot to a Channel

In Slack, go to the channel where you want the bot and type:

```
/invite @YourBotName
```

## Step 3: Configure TEMM1E

Add to your `temm1e.toml`:

```toml
[channel.slack]
enabled = true
token = "${SLACK_BOT_TOKEN}"
allowlist = []           # Empty = auto-whitelist first user as admin
file_transfer = true     # Enable file sending/receiving
```

Set the environment variable:

```bash
export SLACK_BOT_TOKEN="xoxb-your-bot-token-here"
```

## Step 4: Build and Run

```bash
cargo build --release --features slack
./target/release/temm1e start
```

## Step 5: Connect

TEMM1E polls Slack for new messages (no webhook server needed). Send a message in any channel the bot is invited to, and it will respond.

The first user to interact is auto-whitelisted as admin.

## User Management

Admin commands (send in any channel the bot is in):

| Command | Description |
|---------|-------------|
| `/allow <user_id>` | Whitelist a user by their Slack user ID |
| `/revoke <user_id>` | Remove a user from the whitelist |
| `/users` | List all whitelisted users |

The allowlist persists at `~/.temm1e/slack_allowlist.toml`.

To find a user's Slack ID: click their profile picture, then click the `...` menu and select **Copy member ID** (format: `U12345678`).

## File Transfer

- **Receive:** Share files in the channel — the bot downloads them using authenticated Slack API
- **Send:** The agent can upload files to the channel (up to 100 MB)
- Files are saved to the agent's workspace directory
- Filenames are sanitized to prevent path traversal

## Message Splitting

Slack has a 4000-character message limit. TEMM1E automatically splits long responses at natural boundaries (newlines, then spaces).

## How It Works

TEMM1E uses **polling** (not Socket Mode or webhooks):

1. Polls `conversations.list` to discover channels the bot is in
2. Polls `conversations.history` for new messages in each channel
3. Posts replies via `chat.postMessage`

This means no public URL or webhook server is needed — the bot works behind firewalls and NAT.

## Required Bot Token Scopes

| Scope | Why |
|-------|-----|
| `channels:history` | Read messages in public channels |
| `channels:read` | List channels the bot is in |
| `chat:write` | Send messages and replies |
| `files:read` | Download shared files |
| `files:write` | Upload files to channels |
| `users:read` | Look up user info |

## Security Notes

- Empty `allowlist = []` auto-whitelists the first user only
- Bot ignores its own messages (prevents loops)
- User matching is by Slack user ID only (format: `U...`)
- File downloads use authenticated Bearer token
- Bot token is never logged at info level

## Troubleshooting

| Issue | Fix |
|-------|-----|
| `SLACK_BOT_TOKEN not set` | Export the env var or set it in `.env` |
| Bot doesn't respond | Ensure the bot is invited to the channel (`/invite @bot`) |
| `auth.test failed` | Check that your token is valid and has the required scopes |
| Missing messages | Ensure `channels:history` scope is granted |
| File upload fails | Ensure `files:write` scope is granted |
| Rate limiting | TEMM1E includes 100ms delays between API calls; Slack free-tier has stricter limits |
