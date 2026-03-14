# Discord Channel Setup

## Prerequisites

- A Discord account
- A Discord server where you have admin permissions
- A Discord Bot Token from the [Developer Portal](https://discord.com/developers/applications)

## Step 1: Create a Discord Application and Bot

1. Go to [Discord Developer Portal](https://discord.com/developers/applications)
2. Click **New Application** and give it a name
3. Go to **Bot** in the left sidebar
4. Click **Reset Token** and copy the bot token
5. Under **Privileged Gateway Intents**, enable:
   - **Message Content Intent** (required to read message text)
   - **Server Members Intent** (optional, for user lookup)

## Step 2: Invite the Bot to Your Server

1. Go to **OAuth2 > URL Generator** in the Developer Portal
2. Select scopes: `bot`
3. Select permissions: `Send Messages`, `Read Message History`, `Attach Files`, `Read Messages/View Channels`
4. Copy the generated URL and open it in your browser
5. Select your server and authorize

## Step 3: Configure TEMM1E

Add to your `temm1e.toml`:

```toml
[channel.discord]
enabled = true
token = "${DISCORD_BOT_TOKEN}"
allowlist = []           # Empty = auto-whitelist first user as admin
file_transfer = true     # Enable file sending/receiving
```

Set the environment variable:

```bash
export DISCORD_BOT_TOKEN="your-bot-token-here"
```

## Step 4: Build and Run

```bash
cargo build --release --features discord
./target/release/temm1e start
```

## Step 5: Connect

The bot responds to:

- **Direct messages** — message the bot directly
- **@mentions in servers** — mention `@YourBot` in any channel it has access to

The first user to interact is auto-whitelisted as admin.

## User Management

Admin commands (send as DM or @mention):

| Command | Description |
|---------|-------------|
| `/allow <user_id>` | Whitelist a user by their Discord snowflake ID |
| `/revoke <user_id>` | Remove a user from the whitelist |
| `/users` | List all whitelisted users |

The allowlist persists at `~/.temm1e/discord_allowlist.toml`.

To find a user's Discord ID: enable Developer Mode in Discord settings, then right-click a user and select **Copy User ID**.

## File Transfer

- **Receive:** Send file attachments in DMs or channels — the bot downloads them
- **Send:** The agent can send files back (up to 25 MB per Discord's non-Nitro limit)
- Files are saved to the agent's workspace directory

## Message Splitting

Discord has a 2000-character message limit. TEMM1E automatically splits long responses at natural boundaries (newlines, then spaces).

## Security Notes

- Empty `allowlist = []` auto-whitelists the first user only
- Bot ignores messages from other bots (prevents loops)
- User matching is by Discord snowflake ID only
- Bot token is never logged at info level

## Required Bot Intents

The bot needs these Gateway Intents enabled in the Developer Portal:

| Intent | Why |
|--------|-----|
| **Message Content** | Read message text (required) |
| **Guild Messages** | Receive messages in servers |
| **Direct Messages** | Receive DMs |

## Troubleshooting

| Issue | Fix |
|-------|-----|
| `DISCORD_BOT_TOKEN not set` | Export the env var or set it in `.env` |
| Bot doesn't respond in server | Make sure you @mention it, and that Message Content Intent is enabled |
| Bot doesn't respond to DMs | Ensure the bot has DM permissions and your ID is whitelisted |
| `Reconnecting...` in logs | Normal during Discord gateway disconnections — auto-reconnects |
| Files too large | Discord non-Nitro limit is 25 MB; use smaller files or a file hosting URL |
