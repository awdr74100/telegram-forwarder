# telegram-forwarder

[![npm version](https://img.shields.io/npm/v/telegram-forwarder?color=blue)](https://www.npmjs.com/package/telegram-forwarder)
[![CI](https://github.com/awdr74100/telegram-forwarder/actions/workflows/ci.yml/badge.svg)](https://github.com/awdr74100/telegram-forwarder/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)

A CLI tool that monitors Telegram channels and automatically forwards specific content to target channels.

- Pick source and target channels from a list — no manual ID entry
- Filter by content type — photos, videos, text, stickers, and more
- Filter by keyword — only forward (or skip) messages whose text/caption matches
- Handles Telegram FloodWait automatically with reactive backoff
- Dry-run mode shows what would be forwarded without sending anything
- Verbose mode traces why each message is or isn't forwarded; optional file logging
- Config and session persist across restarts in `~/.telegram-forwarder/`

> [!WARNING]
> This tool automates a **personal Telegram user account** (not a Bot API token). Automating user accounts can violate [Telegram's Terms of Service](https://telegram.org/tos) and may result in your account being limited or banned. Use it at your own risk — ideally on an account you can afford to lose. You are responsible for complying with Telegram's rules and any applicable laws.

## Prerequisites

### Get API Credentials

1. Go to [my.telegram.org/apps](https://my.telegram.org/apps)
2. Log in with your Telegram account
3. Create an App and note the **API ID** and **API Hash**

> This tool operates as your **user account**, not a Bot Token.

### Requirements

- Node.js 24+

## Installation

```bash
npm install -g telegram-forwarder
# or
pnpm add -g telegram-forwarder
```

## Quick Start

### 1. Initialize

```bash
telegram-forwarder init
```

You will be prompted for:

- API ID
- API Hash
- Phone number (with country code, e.g. `+1234567890`)
- Verification code sent by Telegram
- 2FA password (if enabled)

The session is saved to `~/.telegram-forwarder/session.sqlite` — you won't need to log in again on subsequent runs.

### 2. Add a Forwarding Group

```bash
telegram-forwarder group add
```

The CLI connects to Telegram and lists all channels you have joined. Everything is selected from lists — you never type a channel name or ID by hand. You then:

1. **Multi-select source channels** — the channels to monitor
2. **Multi-select target channels** — where to forward content
3. **Multi-select content types** — what to forward
4. **Include keywords** (optional) — only forward messages whose text/caption contains one of these
5. **Exclude keywords** (optional) — skip messages whose text/caption contains one of these
6. Choose whether to remove the "Forwarded from" attribution

Available content types: **All content**, **Text messages**, **Photos**, **Videos**, **GIFs / Animations**, **Video notes (circles)**, **Audio files**, **Voice messages**, **Documents / files**, and **Stickers**. Selecting **All content** forwards every message regardless of type.

Keyword filters are case-insensitive substring matches. Leave them blank for no filter. Exclude takes precedence — a message hit by both an include and an exclude keyword is skipped. Because the match is on text, a media-only message with no caption cannot satisfy an include keyword.

The group name is generated automatically from your selection.

Example flow:

```
Fetching your channels…

? Select source channels
  ❯ ◉ Breaking News (@breaking_news)
    ◯ Tech Daily (@tech_daily)
    ◉ Market Watch (@market_watch)

? Select target channels
  ❯ ◉ My Archive (@my_archive)
    ◯ Friends Group (@friends)

? What content to forward?
  ❯ ◉ Photos
    ◉ Videos
    ◯ Text messages

? Only forward messages containing these keywords (comma-separated, blank = no filter): breaking, urgent
? Skip messages containing these keywords (comma-separated, blank = none):
? Remove "Forwarded from" attribution? No

Group "Breaking News +1 → My Archive" added (ID: a1b2c3d4)
```

Each group forwards from **all selected sources** to **all selected targets** with the same content filter. You can create multiple groups for different filter combinations.

### 3. Start

```bash
telegram-forwarder start
```

The process runs continuously and monitors all enabled groups. Each forward is logged as it happens. Press `Ctrl+C` to stop. Add `--verbose` to also see why messages are skipped — see [`start`](#start) for all options.

---

## Commands

### `init`

Configure API credentials and authenticate. Only needs to run once.

```bash
telegram-forwarder init
```

### `group add`

Add a new forwarding group by selecting from your joined channels.

```bash
telegram-forwarder group add
```

### `group list`

List all groups and their current status.

```bash
telegram-forwarder group list
```

Example output:

```
✓ Breaking News +1 → My Archive a1b2c3d4
   Sources: @breaking_news, @market_watch
   Targets: @my_archive
   Types: photo, video
   Include keywords: breaking, urgent
   Remove attribution: no
```

### `group edit`

Edit an existing group. Pick a group from the list, then walk through the same
prompts as `group add` with the current values pre-filled — adjust sources,
targets, content types, keyword filters, or attribution and save.

```bash
telegram-forwarder group edit
```

### `group remove`

Delete groups permanently. Shows a checklist of your groups — select the ones to remove and confirm.

```bash
telegram-forwarder group remove
```

### `group toggle`

Pause or resume groups without deleting them. Shows a checklist where checked = enabled; adjust the checks and confirm.

```bash
telegram-forwarder group toggle
```

### `config`

View or update rate limit settings.

```bash
# Show current settings
telegram-forwarder config

# Update
telegram-forwarder config --delay 200 --jitter 100 --retries 3
```

| Option      | Default  | Description                                  |
| ----------- | -------- | -------------------------------------------- |
| `--delay`   | `100` ms | Minimum gap between forwards                 |
| `--jitter`  | `50` ms  | Random extra delay added on top of `--delay` |
| `--retries` | `5`      | Max retries when FloodWait is received       |

### `reset`

Clear the saved login session and force re-authentication. Use this if the client gets stuck while connecting (see [Troubleshooting](#troubleshooting)). Your API credentials are kept — only the session is removed.

```bash
telegram-forwarder reset
```

### `start`

Start monitoring and forwarding. The process runs continuously until you press `Ctrl+C`.

```bash
telegram-forwarder start

# Trace why each message is or isn't forwarded
telegram-forwarder start --verbose

# Also write logs to a file
telegram-forwarder start --log-file

# Preview matches without forwarding anything
telegram-forwarder start --dry-run
```

On startup it pre-resolves every source and target channel and warns about any it cannot reach, then logs each forward as it happens with a timestamp. On shutdown it prints a session summary (`forwarded`, `skipped`, `failed`) and reports any queued forwards dropped.

| Option          | Description                                                          |
| --------------- | -------------------------------------------------------------------- |
| `-v, --verbose` | Trace skip decisions (source/content-type mismatches) at debug level |
| `-q, --quiet`   | Only log warnings and errors                                         |
| `--log-file`    | Also append logs to `~/.telegram-forwarder/forwarder.log`            |
| `--dry-run`     | Log every matching message but never actually forward it             |

Use `--dry-run` to verify a new group's filters against live messages safely — it still connects and matches, but skips the send entirely.

The log level also honors the `CONSOLA_LEVEL` environment variable.

---

## Troubleshooting

### Stuck on connect with repeated `500: NEED_MEMBER_INVALID`

If a previous login was interrupted, the session database can be left in a bad state. Every later connection then fails on the first pre-auth call (`help.getConfig`) and the client retries forever, printing:

```
[WRN] [network] Telegram is having internal issues: 500:NEED_MEMBER_INVALID (help.getConfig), retrying in 1s
```

This is not a credentials or network problem. Clear the session and log in again:

```bash
telegram-forwarder reset
telegram-forwarder init
```

If it still fails immediately after a reset, double-check that your API ID and API Hash were copied from the **same app** on [my.telegram.org/apps](https://my.telegram.org/apps), and try a different network (some VPN/datacenter IPs are rejected by Telegram).

---

## Rate Limiting

Telegram returns a `FLOOD_WAIT` error when too many API calls are made in a short period.

This tool handles it in two ways:

1. **Proactive spacing** — a small delay (100 ms + jitter) is applied between consecutive forwards to stay well within limits
2. **Reactive backoff** — if `FLOOD_WAIT` is received, the queue pauses for `(wait_seconds + 5)` seconds before retrying

Messages deleted by the sender before they can be forwarded are skipped (and noted in the log).

> User accounts have significantly higher API limits than bots, so hitting FloodWait in normal usage is unlikely.

---

## File Locations

```
~/.telegram-forwarder/
├── config.json     ← API credentials, groups, and rate limit settings
├── session.sqlite  ← login session (do not delete)
└── forwarder.log   ← runtime log (only created when started with --log-file)
```

> [!IMPORTANT]
> `config.json` stores your **API ID and API Hash in plain text**, and `session.sqlite` grants **full access to your Telegram account**. Keep this directory private — never share it or commit it to version control.

---

## Development

```bash
# Clone and install
git clone https://github.com/awdr74100/telegram-forwarder.git
cd telegram-forwarder
pnpm install

# Run without building
pnpm dev <command>

# Run test
pnpm test

# Type check
pnpm typecheck

# Lint
pnpm lint
pnpm lint:fix

# Format
pnpm format
pnpm format:check

# Find unused exports / dependencies
pnpm knip
```

### Release

```bash
pnpm release   # bumps version, commits, creates git tag, and pushes
```

The GitHub Actions release workflow picks up the tag and publishes to npm via OIDC.

## License

[MIT](./LICENSE)
