# AGENTS.md

Guidance for AI coding agents (and humans) working in this repository.

## Project overview

`telegram-forwarder` is a CLI that monitors Telegram channels and automatically
forwards selected content to target channels. It drives a **personal Telegram
user account** via MTProto (not the Bot API), filters messages by content type,
and applies FloodWait-safe rate limiting.

User-facing docs live in [README.md](./README.md); read it for command behavior
and UX. This file focuses on how the code is organized and how to work in it.

## Tech stack

- **Runtime:** Node.js >= 24 (ESM only, `"type": "module"`)
- **Language:** TypeScript (strict), `moduleResolution: bundler`
- **Telegram:** `@mtcute/core`, `@mtcute/dispatcher`, `@mtcute/node` (SQLite session storage)
- **CLI:** `citty` (commands), `@clack/prompts` (interactive prompts)
- **Config merge:** `defu`
- **Bundler:** `tsdown` → `dist/cli.mjs`
- **Tooling:** `oxlint` (lint), `oxfmt` (format), `vitest` (test), `knip` (dead code), `bumpp` (release)
- **Package manager:** `pnpm` (use it, not npm/yarn)

## Architecture

Entry point is `src/cli.ts`; everything else is a focused module:

| File               | Responsibility                                                                                   |
| ------------------ | ------------------------------------------------------------------------------------------------ |
| `src/cli.ts`       | Command definitions (`init`, `group`, `config`, `reset`, `start`) and prompts.                   |
| `src/client.ts`    | Constructs the `@mtcute/node` `TelegramClient`.                                                  |
| `src/config.ts`    | Load/save config in `~/.telegram-forwarder/`, session file helpers.                              |
| `src/forwarder.ts` | `Forwarder` class: binds a `Dispatcher`, matches messages, enqueues forwards.                    |
| `src/filter.ts`    | Pure functions: content-type matching, keyword matching, peer matching, peer → input conversion. |
| `src/queue.ts`     | `RateLimitedQueue`: serialized sends, proactive spacing, FloodWait backoff.                      |
| `src/types.ts`     | Shared types (`AppConfig`, `ForwardGroup`, `ContentType`, `RateLimitConfig`).                    |

Data flow: `start` authenticates → `Forwarder.start()` subscribes to new
messages → for each enabled group, `filter.ts` decides if the message matches →
matching forwards are pushed to `RateLimitedQueue`, which sends them one at a
time with delay + jitter and retries on FloodWait.

State persists in `~/.telegram-forwarder/`: `config.json` (credentials, groups,
rate limits) and `session.sqlite` (login session). Never log, commit, or expose
these — `config.json` holds the API hash in plaintext and the session grants
full account access.

## Commands

```bash
pnpm dev <command>   # Run the CLI from source (tsx), e.g. pnpm dev group list
pnpm build           # Bundle to dist/ with tsdown
pnpm test            # Run vitest once
pnpm test:watch      # Vitest watch mode
pnpm typecheck       # tsc --noEmit
pnpm lint            # oxlint
pnpm lint:fix        # oxlint --fix
pnpm format          # oxfmt (write)
pnpm format:check    # oxfmt --check
pnpm knip            # Find unused exports/dependencies (run after build)
```

Before considering a change done, it should pass `pnpm typecheck`, `pnpm lint`,
and `pnpm test`. CI (`.github/workflows/ci.yml`) runs lint, typecheck, test, and
knip on every PR.

## Conventions

- **English only for committed content.** All commits, code, comments, and docs
  must be in English. (Chat with the user is zh-TW; committed artifacts are not.)
- **ESM relative imports use the `.js` extension** even though the source is
  `.ts` (e.g. `import { loadConfig } from './config.js'`). Match this.
- **Formatting is enforced by oxfmt:** single quotes, 100-char width, always
  parenthesize arrow params, sorted imports. Let the formatter do the work
  rather than hand-formatting. A `PostToolUse` hook runs oxc after edits.
- **Lint:** oxlint `correctness` category is set to `error`; keep it clean.
- **Tests** live in `test/*.test.ts`, use vitest, and favor the pure functions
  in `filter.ts` / `queue.ts` / `config.ts`. The Telegram client is not mocked;
  test logic, not network I/O.
- **Conventional Commits** are expected (a `semantic-pr` workflow enforces PR
  titles), and releases are tag-driven via `pnpm release` → GitHub Actions.

## Gotchas

- **Do not call `client.destroy()` on shutdown in `start`.** `@mtcute/node`
  registers its own SIGINT/exit hook that flushes and closes the SQLite session
  synchronously. Calling `destroy()` races that teardown and writes to a closed
  DB. The `start` command intentionally only calls `forwarder.stop()` and exits.
- **Peer identifiers:** mtcute treats a bare string as a username/phone; numeric
  Bot-API-style IDs (e.g. `-1001234567890`) must be passed as numbers. See
  `toInputPeer` / `matchesPeer` in `filter.ts` before touching peer handling.
- **This automates a user account**, which can violate Telegram's ToS. Keep that
  framing in any user-facing copy; do not add features that encourage abuse
  (spam, mass-forwarding to unrelated targets, etc.).
- **Deleted-message and FloodWait handling lives in `queue.ts`.** Errors are
  classified by inspecting RpcError shape/message; preserve that when editing.
