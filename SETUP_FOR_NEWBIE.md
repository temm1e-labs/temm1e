# HI!! Let's Set Me Up :3

Okay okay okay so YOU want to run me on your computer?? That's AWESOME. I'm going to walk you through every single step. Yes, every step. You don't need to know Rust, servers, or AI stuff. I'll explain as we go.

This is literally me helping you summon me into existence. Let's GO.

## What You'll Need

- A computer (macOS, Linux, or Windows with WSL)
- An internet connection
- A Telegram account (free)
- **One** of these (pick whichever you have):
  - A ChatGPT Plus or Pro subscription ($20/month) — easiest, no API key needed
  - An API key from any AI provider (Anthropic, OpenAI, Google, etc.)

That's it. That's the whole list. Let's begin.

## Step 1: Get Rust (It's What I'm Made Of!)

I'm written in Rust! Which means you need the Rust compiler to bring me to life. Don't worry, it's one command.

Open your terminal and run:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

It'll ask you a question. Press `1` for the default installation. Once it finishes:

```bash
source $HOME/.cargo/env
```

Did it work? Let's check!

```bash
rustc --version
# Should print something like: rustc 1.82.0 (...)
```

If you see a version number, NICE. Rust is ready.

> **Windows users:** You'll need [WSL2](https://learn.microsoft.com/en-us/windows/wsl/install) first. Run `wsl --install` in PowerShell, then do everything inside WSL from here on. It's basically Linux running inside Windows — very cool actually.

## Step 2: Install Chrome (Optional)

I have a browser tool! I can navigate websites, click buttons, take screenshots, fill out forms — the works. But I need Chrome or Chromium installed to do it.

- **macOS:** Chrome is probably already there. If not, grab it from google.com/chrome.
- **Linux:** `sudo apt install chromium-browser` (Ubuntu/Debian) or `sudo dnf install chromium` (Fedora).
- **Don't need the browser tool?** Skip this entirely. Everything else works without it.

## Step 3: Create a Telegram Bot

This is how you'll talk to me! It takes about 60 seconds and it's honestly kind of fun.

1. Open Telegram on your phone or desktop
2. Search for **@BotFather** and open a chat with it
3. Send `/newbot`
4. BotFather asks for a name — type anything (e.g., "My TEMM1E")
5. BotFather asks for a username — pick something ending in `bot` (e.g., `my_temm1e_bot`)
6. BotFather gives you a **bot token** — it looks like `7123456789:AAHx...`. **Copy this token and save it somewhere safe.**

> Your bot token is a secret. Anyone who has it can control your bot. Don't share it publicly. Treat it like a password.

## Step 4: Download and Build Me!

Here's where I start becoming real.

```bash
git clone https://github.com/nagisanzenin/temm1e.git
cd temm1e
cargo build --release
```

The first build takes 2-4 minutes because Rust is compiling around 300 dependencies. Go grab a drink or something. Subsequent builds are WAY faster.

When it finishes, your binary is at `./target/release/temm1e`. That's me. I'm in there.

## Step 5: Give Me a Brain

I need an AI provider to think with. Choose ONE option:

### Option A: Use Your ChatGPT Account (Easiest)

If you have ChatGPT Plus ($20/month) or ChatGPT Pro, this is the fastest path.

```bash
./target/release/temm1e auth login
```

A browser window opens. Log into your ChatGPT account. That's literally it.

You'll see:
```
Authenticated successfully!
Email:   you@gmail.com
Expires: 239h 59m
Model:   gpt-5.4 (default)
```

> **No browser on your server?** Use `temm1e auth login --headless` — it prints a URL you can open on any device (phone, laptop), then you paste the redirect URL back into the terminal. Clever, right?

### Option B: Use an API Key

If you have an API key from Anthropic, OpenAI, Google, or another provider — great. You'll paste it in Telegram after starting me up in the next step. No setup needed here.

Where to get API keys if you don't have one yet:
- **Anthropic (Claude):** https://console.anthropic.com/settings/keys — starts with `sk-ant-`
- **OpenAI (GPT):** https://platform.openai.com/api-keys — starts with `sk-`
- **Google (Gemini):** https://aistudio.google.com/apikey — starts with `AIzaSy`
- **xAI (Grok):** https://console.x.ai/ — starts with `xai-`

## Step 6: Wake Me Up

Set your Telegram bot token and start me:

```bash
export TELEGRAM_BOT_TOKEN="paste-your-bot-token-here"
./target/release/temm1e start
```

You should see logs showing the gateway starting and Telegram connecting. That's me booting up!

## Step 7: Talk to Me!

This is the BEST part.

1. Open Telegram
2. Find the bot you created in Step 3
3. Send any message

**If you used Option A (ChatGPT login):** I'm ready. Just start chatting!

**If you're using Option B (API key):** I'll send you a secure setup link. You have two choices:
- Click the link, paste your API key in the browser form (encrypted locally before sending)
- Or just paste your raw API key directly in the chat — I auto-detect the provider

After the key is validated, we're LIVE.

## Step 8: Try Me Out!

Send these to your bot:

- `Hello!` — basic chat
- `What files are in my home directory?` — I'll use my shell tool for this
- `Remember that my favorite color is blue` — I store this in memory
- `What's my favorite color?` — I remember things!
- `/model` — see what AI models are available

## Running Me in the Background

Once everything works, you can run me as a background daemon so I'm always on:

```bash
./target/release/temm1e start -d
```

I log to `~/.temm1e/temm1e.log`. When you need me to stop:

```bash
./target/release/temm1e stop
```

## Keeping Me Updated

When new versions come out:

```bash
./target/release/temm1e update
```

This pulls the latest code and rebuilds automatically. Fresh me!

## Troubleshooting

Oh no! Something went wrong? Let's figure it out together.

**"cargo: command not found"**
Rust isn't in your PATH yet. Run `source $HOME/.cargo/env` or restart your terminal. This is the most common hiccup and the easiest fix.

**Build fails with "linker not found"**
You need build tools installed. On Ubuntu/Debian: `sudo apt install build-essential`. On macOS: `xcode-select --install`. Then try `cargo build --release` again.

**Bot doesn't respond**
Okay don't panic. Let's check a few things:
- Is the token set? Run `echo $TELEGRAM_BOT_TOKEN` and make sure it prints something
- Check my logs: `tail -50 /tmp/temm1e.log` or `tail -50 ~/.temm1e/temm1e.log`
- Make sure you're messaging the right bot (easy mistake, no judgment)

**"OAuth token expired"**
Just re-authenticate: `./target/release/temm1e auth login`

Tokens expire after a while. This is normal.

**API key not accepted**
- Make sure you copied the FULL key (no extra spaces at the beginning or end)
- Check that your API key has credits/quota remaining
- Try pasting the raw key directly in chat instead of using the secure link

## What's Next

You did it!! I'm alive and talking to you. Here's where to go from here:

- Read about [what makes me different](README.md#temm1e-is-built-different) — the Finite Brain Model and Blueprint procedural memory
- Set up [Discord](docs/channels/discord.md) or [Slack](docs/channels/slack.md) channels
- Explore [MCP servers](README.md#self-extending-tool-system) — you can let me install my own tools :3
- Deploy on a VPS for 24/7 operation — see [SETUP_FOR_PROS.md](SETUP_FOR_PROS.md) for Docker and systemd guides
