# AGENTS.md

Guidance for anyone — human or agent — working on this repository.

## What this is

`tgfwd` mirrors Telegram messages from many source chats into many target chats.
It is built around one hostile case: **a publisher posts, then deletes one second
later**. Everything below follows from that.

## Non-negotiable design rules

1. **Capture before you decide.** `Snapshotter::capture` is synchronous and
   cannot fail. It runs before filtering, before routing, before any network
   call. Once a message is captured, deletion cannot take it away. Never move
   work ahead of capture, and never make capture `async`.

2. **Targets are independent.** Each target gets its own task and its own place
   in the pacer. One rate-limited chat must never delay the other nine. Do not
   introduce a shared serial queue.

3. **Failures degrade, they do not just retry.** See `engine/failure.rs`. A
   Telegram error means one of four things — wait, degrade, back off, or give up
   — and conflating them is the main way this class of tool loses messages.

4. **Nobody types a chat ID.** Everything selectable comes from the account's
   own dialog list (`telegram/dialogs.rs`). A mistyped ID fails silently at
   delivery time, which is the worst possible moment.

5. **Identifiers are for machines, not people.** A route's `id` exists so logs
   and shell scripts can name one. It is derived from the source chat by
   `auto_id`; the user is never asked to invent or recall it. Anywhere a route is
   offered for selection, show `describe_route` — what it moves — not the id. Do
   not add a prompt that asks the user to name something, and never
   ask anyone to type a value that already exists in the config or on Telegram:
   chat titles are full of emoji and symbols that cannot be typed from memory.

## Architecture

```
main.rs → cli.rs → commands/ → engine/
                             → telegram/       (all Telegram I/O)
                             → config/         (schema, validation)
                             → session.rs      (single-file Session impl)
                             → private_file.rs (owner-only writes)
                             → ui/             (theme, prompts, logger)
```

`private_file.rs` exists so everything this tool persists gets the same answer.
All three hold something other users of the machine should not read — the session
file *is* a login to the account, the configuration holds `api_hash`, and the
media cache holds the bodies of messages from chats the account can see. Having
one of them be careful while the others inherited the umask was a difference with
no reason behind it.

The cache needs `create` rather than `write` because `grammers` does the writing:
it opens the path with `File::create`, which keeps the mode of a file that is
already there, so creating it first is what decides that mode. Every entry point
unlinks before creating, because `OpenOptions::mode` applies only to a file it
creates and both configuration and session write through a fixed temporary name
that a killed process can leave behind.

`telegram/` never depends on `cli/`, `config/` or `ui/`. The login flow asks
questions through the `LoginPrompt` trait so it carries no terminal dependency.

### The delivery ladder (`engine/delivery.rs`)

| Rung | Cost | Survives |
|---|---|---|
| `Forward` | 1 RPC, no upload, keeps attribution | nothing extra |
| `Copy` | 1 RPC, no upload, reuses the file reference | source deletion, forward restrictions |
| `Rehost` | re-uploads snapshotted bytes | a dead file reference |

`DeliveryMode` picks which rungs exist. `Auto` uses all three, which is why it is
the default. Anything delivered below the top rung is counted as *rescued*.

**The payload shrinks as it descends.** Telegram answers a multi-message send
with one slot per message and puts `None` in the ones it refused, so a rung can
place part of an album and refuse the rest. Only the refused parts continue
downwards: re-sending the whole group would duplicate what already arrived, and
those duplicates would not be recognised by the `EchoGuard`. Whatever a rung does
place is remembered by the guard immediately, success or not.

### Session storage (`session.rs`)

`grammers-session` ships only a memory store and a SQLite one. This project
implements the `Session` trait over a single JSON file instead, so
`grammers-session` is built with `default-features = false` and `libsql` never
enters the dependency graph. **Do not re-enable `sqlite-storage`.**

Authorization keys are written through immediately; everything else is batched
behind a dirty flag and flushed by the engine's housekeeping tick.

**`cache_peer` must not block or await anything real.** `Engine::run` polls
`UpdateStream::next` inside a `tokio::select!`, so that future is dropped
whenever the housekeeping tick wins the race. It survives that today only
because of the one await in `UpdateStream::next_raw` that sits between taking a
batch off the message box and buffering it — `build_peer_map`, which reaches
`cache_peer` through the trait's default `cache_peers` — and that await completes
without ever yielding. Give `cache_peer` a real suspension point, or override
`cache_peers` with one, and updates that have already advanced the `pts` will
start disappearing at cancellation, with no gap left for `getDifference` to
recover.

## Traps discovered the hard way

- **`RpcError::name` has the number stripped out.** `FLOOD_WAIT_42` arrives as
  `name = "FLOOD_WAIT"`, `value = Some(42)`. Matching `is("FLOOD_WAIT_*")`
  silently never fires. There is a regression test for exactly this.
- **`RpcError::is` only understands a leading or trailing `*`.** A middle
  wildcard like `CHAT_SEND_*_FORBIDDEN` degrades into an exact comparison that
  never matches. Use the common prefix.
- **`forward_messages` and `send_album` can partially succeed**, returning `None`
  for the messages they refused. Treating that as success loses half an album;
  treating it as total failure duplicates the half that arrived. See the delivery
  ladder above for what is done instead.
- **`send_album` unwraps `InputMedia::media`.** `copy_media` sets it from
  `Media::to_raw_input_media`, which is `None` for a link preview — so filtering
  album members on "has media" rather than "converts to an input media" hands
  `grammers` a `None` to unwrap and takes the process down.
- **`Media` being `Some` does not mean there are bytes.** Polls, locations,
  contacts, dice and link previews are all modelled as media with no file
  location behind them. Asking to download one creates an empty file before the
  download reports failure, so `Snapshotter::capture` tests
  `to_raw_input_location` before queueing anything.
- **Album members are not buffered in posting order.** Each arrives on its own
  spawned task and they race for the buffer lock, so the group has to be sorted
  by message ID before delivery. Telegram numbers the members consecutively.
- **Config stores Bot API dialog IDs** (`-100…`), but every API call needs a
  `PeerRef` (ID *plus* the access hash bound to this account). The bridge is the
  session peer cache, warmed by `dialogs::fetch_all`. Calling it after login is
  not optional — `grammers` also needs it to resolve update gaps.
- **Values a hand-edited config can hold that nothing downstream can survive**
  are rejected by `Config::validate`, not handled at runtime: `max_in_flight = 0`
  gives the semaphore no permits and hangs every delivery *and* the shutdown that
  waits for them, and `max_attempts = 0` skips the retry loop body entirely.
- **The raw update channel is bounded and drops on overflow.** Since `grammers`
  0.11 the sender pool hands updates over through a channel of
  `updates_channel_capacity` and discards what does not fit, logging only a
  warning. The gap is recovered by `getDifference`, but anything deleted before
  that is lost, so `telegram/mod.rs` raises the library default of 100.

### Where files live

Paths come from the `directories` crate, which follows the XDG spec on Linux,
Apple's guidance on macOS and the Known Folder API on Windows. Do not replace
this with a `~/.tgfwd` dotdir: it would be wrong on Windows and would violate the
one part of this that has an actual written specification.

The real problem those paths create — being unguessable, and containing a space
on macOS — is solved by `tgfwd config path` and `tgfwd config edit`, not by
moving the files. `TGFWD_HOME` overrides everything and lays a whole profile out
under one root, which is what the tests and multi-account setups use.

## Conventions

- All code, comments and documentation are **English**. This is an open-source
  project.
- Comments explain *why*, not *what*. If a line needs a comment to say what it
  does, rename something instead.
- Tests live next to the code in `#[cfg(test)] mod tests`. Test names are
  sentences: `a_deleted_source_degrades_to_the_snapshot`.
- Prefer removing dead code over `#[allow(dead_code)]`.
- `unsafe` is forbidden crate-wide, including in tests. If a test seems to need
  it (e.g. `env::set_var`), restructure the code so it does not.

## Commands

```sh
just            # list the recipes
just check      # everything CI checks
just fix        # rewrite formatting
```

CI runs these same recipes rather than its own copy of the commands, so what
passes locally is what passes on push. Before the `justfile` existed the two had
already drifted: the documented lint did not fail on a warning while CI's did.

Individually: `just fmt`, `lint`, `test`, `unused`, `spell`, `workflows`, `audit`,
`build`. `audit` also runs weekly on its own, because an advisory published
against a crate already in the lock file turns a passing commit red without
anybody touching the repository.
The tools they need are `just`, `typos`, `actionlint`, `zizmor`, `cargo-shear`,
`cargo-deny`, `git-cliff`, `cargo-dist` and `cargo-release`; `just setup`
installs every one of them except `actionlint`, which is not a crate.

CI runs these on every push and pull request, with warnings promoted to errors
and `--locked` so a dependency bumped without committing `Cargo.lock` fails there
rather than drifting. The linting runs once, on Linux; the tests run on Linux,
macOS **and** Windows, because the session file's permissions, the config
directory layout and the editor `config edit` reaches for all differ there, and
compiling proves none of them still work.

`workflows`, `audit` and `release-check` are path-filtered into workflows of
their own rather than run on every push: each needs a tool the main job does not,
and each guards files that change rarely. `just check` runs the lot, which is why
it is the one command worth running before pushing.

The recipes put the cargo bin directory on `PATH` themselves and choose the
separator by platform, so a Windows checkout needs nothing added to a profile.
There is no `cargo test --doc` step: this crate has no library target, and that
command errors out rather than finding nothing.

Exceptions to clippy's pedantic set live in `Cargo.toml` under `[lints.clippy]`,
each with a written justification. Add to that list only with a reason.

`clippy.toml` holds the rules the compiler cannot see, so that they fail a build
rather than only appearing in this file:

- **stdout belongs to `tgfwd config path`.** `println!` is disallowed, with one
  allowed use at that command. Anything else printed there lands inside
  `$(tgfwd config path)`.
- **`std::fs` is disallowed in favour of `fs_err`**, whose errors name the file
  they failed on. `session.rs` opts out where it needs the Unix `mode` extension.
- **A `DashMap` reference must not be held across an await.** Doing so deadlocks
  the next task to touch the same shard, and every caller of `Stats` is inside a
  delivery task that awaits the network. Verified to fire against a bound
  `Ref`; note it does not catch an `Option<Ref>`.

## What happens when the network drops

Traced rather than assumed, because the behaviour is not what it looks like from
here and the obvious test — pull the network and watch — appears to show nothing
working.

Dropping the link does not fail the socket. Nothing is being sent and nothing is
expected, so the connection simply goes quiet. `grammers` builds connections on
demand and drops failed ones from its pool, which means a brief outage is
absorbed silently and deliberately: a hiccup should not kill the process.

The recovery is driven by `NO_UPDATES_TIMEOUT`, fifteen minutes in
`grammers-session`. After that long without an update the message box asks for a
difference, that request fails while offline — the default retry policy gives up
after one attempt on an I/O error — and the error surfaces through
`UpdateStream::next`, ending the run with a non-zero exit for a supervisor to act
on. Once the link returns, the same difference mechanism replays what was missed,
so an outage costs latency rather than messages, except for anything deleted
while it lasted.

The gap this leaves is observability, not correctness: for up to fifteen minutes
a dead link is indistinguishable from a quiet channel, and the log shows
nothing amiss. Closing that would take an active heartbeat, which is a deliberate
addition of periodic traffic rather than a bug fix.

## Releasing

A pushed tag matching `v*` is the whole trigger. `dist` builds the five target
platforms, writes the installer scripts, checksums everything and creates the
GitHub Release, whose body is the matching section of `CHANGELOG.md`.

`just build` before any of it. It builds with `--profile dist`, which is what
`dist` uses and what `--release` is not: the first v0.1.0 tag failed on all five
platforms with `profile \`dist\` is not defined` after a green `just check`,
because nothing local had ever exercised the profile the release actually uses.

`cargo release` does the preparing, configured by `release.toml`: it runs
git-cliff to write the entry for the version, commits, and tags — locally.
`cargo release 0.1.0` is a dry run; `-x` performs it. The push is then two
commands, and deliberately not one: the branch first, and the tag only once CI
on it is green, because the tag is what starts the build of five platforms and
a public release.

Nothing is written to `CHANGELOG.md` ahead of that. The file holds no entry for
an unreleased version, so a version and a date in it always describe a release
that happened — the failure this replaced was a hand-written `0.1.0` section
carrying a date four days before anything shipped, with nothing in the
repository responsible for noticing.

The entry is generated from the commits since the last tag, which is what the
Conventional Commits convention is being enforced for. No category is skipped;
`cliff.toml` says why.

`.github/workflows/release.yml` is **generated** from `dist-workspace.toml` and
committed, so the pipeline keeps working whatever happens to the tool. Never
edit it by hand: change the config and run `just release-check`, which
regenerates it and fails if it was left stale. Forgetting is caught anyway —
`lint-workflows.yml` runs the same check whenever either file changes, and reads
the `dist` version out of `dist-workspace.toml` rather than repeating it.

That file is also the one place the repository's own standards are relaxed. It
is generated code holding write access to releases, so the exemptions in
`.github/zizmor.yml` and `.github/actionlint.yaml` name individual rules rather
than skipping the file — a new kind of finding still fails, which was verified
by planting one.

**Not published to crates.io.** `cargo install` would ask a user for a Rust
toolchain and a compile to deliver what the installer hands over in seconds.
`publish = false` in `Cargo.toml` makes an accidental publish fail; the
accompanying `[package.metadata.dist] dist = true` is what tells `dist` the
binary is still meant to ship, since it reads `publish = false` as "not for
distribution" otherwise.

## Not done yet

Homebrew, and any distribution channel beyond the GitHub Release.
