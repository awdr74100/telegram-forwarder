import type { Message } from '@mtcute/core';
import { Dispatcher } from '@mtcute/dispatcher';
import type { TelegramClient } from '@mtcute/node';

import { saveConfig } from './config.js';
import {
  matchesContentType,
  matchesKeywords,
  matchesPeer,
  replacePeer,
  toInputPeer,
} from './filter.js';
import { logger as defaultLogger, type Logger } from './logger.js';
import { type ForwardOutcome, RateLimitedQueue, SkippedForward } from './queue.js';
import type { AppConfig, ForwardGroup } from './types.js';

interface ForwarderStats {
  forwarded: number;
  skipped: number;
  failed: number;
}

interface ForwarderOptions {
  logger?: Logger;
  // When true, log every match but never actually forward — for verifying
  // filters safely without sending anything or risking FloodWait.
  dryRun?: boolean;
  // Persist config after an automatic target migration (basic group → supergroup
  // rewrites the stored target id). Injectable so tests don't touch the real
  // config file; defaults to saving ~/.telegram-forwarder/config.json.
  persistConfig?: (config: AppConfig) => void;
}

export class Forwarder {
  private readonly client: TelegramClient;
  private readonly config: AppConfig;
  private readonly groups: ForwardGroup[];
  private readonly queue: RateLimitedQueue;
  private readonly log: Logger;
  private readonly dryRun: boolean;
  private readonly persistConfig: (config: AppConfig) => void;
  // Targets retired this session: an invalid-peer error we could not recover via
  // migration. Skipped silently on subsequent messages so a permanently-broken
  // peer does not spam the log every time the source posts.
  private readonly deadTargets = new Set<string>();
  private dp: Dispatcher | null = null;
  private readonly stats: ForwarderStats = { forwarded: 0, skipped: 0, failed: 0 };

  // Bound once so it can be added and later removed from the connection emitter.
  private readonly handleConnectionState = (state: string): void => {
    if (state === 'connected') this.log.success('Connected to Telegram.');
    else if (state === 'offline') this.log.warn('Disconnected from Telegram — reconnecting…');
    else this.log.debug(`Connection state: ${state}`);
  };

  // Without an onError listener, mtcute prints every transport error as a scary
  // "[ERR] unhandled error". Registering this both silences that and lets us
  // route errors by severity. A dropped socket (e.g. the Mac sleeping / network
  // going away — ECONNRESET/ETIMEDOUT/EPIPE) is expected churn: mtcute reconnects
  // on its own, and onConnectionState already reports the offline→reconnect
  // transition, so we log it at debug. Anything else is a real error.
  private readonly handleClientError = (err: Error): void => {
    if (isTransientNetworkError(err)) {
      this.log.debug(`Network dropped (${err.message}) — mtcute will reconnect.`);
      return;
    }
    this.log.error(`Client error: ${err.message}`);
  };

  constructor(client: TelegramClient, config: AppConfig, opts: ForwarderOptions = {}) {
    const { logger = defaultLogger, dryRun = false, persistConfig = saveConfig } = opts;
    this.client = client;
    this.config = config;
    this.groups = config.groups.filter((g) => g.enabled);
    this.dryRun = dryRun;
    this.persistConfig = persistConfig;
    this.log = logger.withTag('forwarder');
    this.queue = new RateLimitedQueue(config.rateLimit, {
      logger,
      onOutcome: (outcome) => this.recordOutcome(outcome),
    });
  }

  // Pre-resolve every source/target peer before listening, so unreachable peers
  // surface as a startup warning instead of a lazy failure on the first forward.
  //
  // A peer stored as a numeric channel id (a private channel with no @username)
  // can only be resolved from a cached access hash. That cache is populated by a
  // full dialog scan — which `group add` runs, but a plain `start` does not — and
  // a "min" cache entry is backed by a message reference that goes stale between
  // runs. So a peer that resolved fine right after `group add` can fail to resolve
  // the next day. When that happens we warm the cache once with a dialog scan
  // (re-fetching fresh, non-min access hashes) and retry, mirroring `group add`.
  async checkPeers(): Promise<void> {
    const peers = new Set<string>();
    for (const group of this.groups) {
      for (const peer of group.sourcePeers) peers.add(peer);
      for (const peer of group.targetPeers) peers.add(peer);
    }

    // First pass straight from cache. A peer that only resolves through a fragile
    // message reference is treated as needing a refresh, not as already resolved.
    const stale = new Set<string>();
    for (const peer of peers) {
      if ((await this.resolveState(peer)) !== 'ok') stale.add(peer);
    }

    // Only pay for a dialog scan when something actually needs it.
    if (stale.size > 0) {
      this.log.info(`Warming peer cache via dialog sync for ${stale.size} peer(s)…`);
      await this.warmPeerCache();
    }

    let resolved = 0;
    for (const peer of peers) {
      // Healthy peers from the first pass are already good; only re-check the
      // ones we warmed. After warming, a peer still backed by a message reference
      // is accepted (it works for now) — only a hard failure warns.
      const state = stale.has(peer) ? await this.resolveState(peer) : 'ok';
      if (state === 'fail') {
        this.log.warn(`Could not resolve peer "${peer}" — forwards involving it may fail.`);
      } else {
        resolved++;
      }
    }
    this.log.info(`Resolved ${resolved}/${peers.size} peer(s).`);
  }

  // Resolve a peer from cache and classify the result:
  //   'ok'   — a stable input peer (real access hash, or resolved by username)
  //   'stale'— resolved only via a message reference (inputPeer*FromMessage),
  //            which can expire; worth refreshing with a dialog scan
  //   'fail' — could not be resolved at all
  private async resolveState(peer: string): Promise<'ok' | 'stale' | 'fail'> {
    try {
      const input = await this.client.resolvePeer(toInputPeer(peer));
      return input._.endsWith('FromMessage') ? 'stale' : 'ok';
    } catch {
      return 'fail';
    }
  }

  // Drain the full dialog list so every chat is re-fetched and re-cached with a
  // fresh, non-min access hash. This is the only way to recover a private peer
  // whose stored hash was lost or downgraded to a message reference — resolving
  // by id alone cannot. `archived: 'keep'` also covers archived chats, which the
  // default scan skips.
  private async warmPeerCache(): Promise<void> {
    try {
      let count = 0;
      // Iterating is the point: each dialog batch flows through mtcute's peer
      // cache; we don't need the Dialog objects themselves.
      for await (const _dialog of this.client.iterDialogs({ archived: 'keep' })) count++;
      this.log.debug(`Dialog sync cached ${count} dialog(s).`);
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      this.log.warn(`Dialog sync failed: ${reason} — some peers may not resolve.`);
    }
  }

  start(): void {
    if (this.groups.length === 0) {
      this.log.warn('No enabled groups — nothing to forward.');
      return;
    }

    this.client.onConnectionState.add(this.handleConnectionState);
    this.client.onError.add(this.handleClientError);

    const dp = Dispatcher.for(this.client);
    this.dp = dp;

    dp.onNewMessage(async (upd) => {
      const chat = upd.chat as { id: number; username?: string | null };

      for (const group of this.groups) {
        const isSourceMatch = group.sourcePeers.some((peer) => matchesPeer(chat, peer));
        if (!isSourceMatch) continue;

        const mediaType = upd.media?.type ?? 'text';
        if (!matchesContentType(upd, group.contentTypes)) {
          this.log.debug(
            `msg ${upd.id} from ${describeChat(chat)} — skipped: type '${mediaType}' ` +
              `not in [${group.contentTypes.join(', ')}] for "${group.name}"`,
          );
          continue;
        }

        if (!matchesKeywords(upd, group.includeKeywords, group.excludeKeywords)) {
          this.log.debug(
            `msg ${upd.id} from ${describeChat(chat)} — skipped: keyword filter for "${group.name}"`,
          );
          continue;
        }

        const msg = upd as unknown as Message;
        for (const targetPeer of group.targetPeers) {
          // A target retired earlier this session is skipped silently.
          if (this.deadTargets.has(targetPeer)) continue;

          const label = `[${group.name}] msg ${upd.id} (${mediaType}) → ${targetPeer}`;
          if (this.dryRun) {
            this.log.info(`[dry-run] would forward ${label}`);
            continue;
          }
          this.log.debug(`Enqueued ${label}`);
          this.queue.enqueue(() => this.forwardOne(msg, group, targetPeer), label);
        }
      }
    });

    this.log.info(`Dispatcher started. Monitoring ${this.groups.length} group(s).`);
  }

  // Forward one message to one target, transparently following a basic-group →
  // supergroup upgrade. After a migration the old (chat-range) id is dead and
  // Telegram answers with CHANNEL_INVALID/PEER_ID_INVALID. We then probe the
  // target with getChat: a `migratedTo` means follow it (rewrite config + retry
  // on the new id); a still-healthy chat means the error was not about this
  // target (rethrow); an unreachable chat means retire it for this session.
  // Other errors (FloodWait, deleted message, transient) are rethrown so the
  // queue's own retry logic handles them.
  private async forwardOne(msg: Message, group: ForwardGroup, targetPeer: string): Promise<void> {
    try {
      await this.forward(msg, group, targetPeer);
    } catch (err) {
      if (!isInvalidTargetError(err)) throw err;

      const migration = await this.probeTarget(targetPeer);
      if (migration === 'healthy') throw err;
      if (migration === null) {
        this.deadTargets.add(targetPeer);
        throw new SkippedForward(
          `Target "${targetPeer}" is unreachable and not a known migration — ` +
            `retiring it this session. Run "group edit" to fix "${group.name}".`,
        );
      }

      group.targetPeers = replacePeer(group.targetPeers, targetPeer, migration);
      this.persistConfig(this.config);
      this.log.info(
        `Target "${targetPeer}" was upgraded to a supergroup — now forwarding ` +
          `"${group.name}" to "${migration}" (config updated).`,
      );
      await this.forward(msg, group, migration);
    }
  }

  private forward(msg: Message, group: ForwardGroup, targetPeer: string): Promise<unknown> {
    return this.client.forwardMessages({
      messages: [msg],
      toChatId: toInputPeer(targetPeer),
      noAuthor: group.noAuthor,
    });
  }

  // Classify a target that just failed with an invalid-peer error:
  //   string    — the new supergroup id it was migrated to (follow it)
  //   'healthy' — the target still resolves and was not migrated (error was elsewhere)
  //   null      — the target could not be fetched at all (retire it)
  private async probeTarget(targetPeer: string): Promise<string | 'healthy' | null> {
    try {
      const chat = await this.client.getChat(toInputPeer(targetPeer));
      return chat.migratedToId === null ? 'healthy' : String(chat.migratedToId);
    } catch {
      return null;
    }
  }

  // Number of forwards still queued (not yet sent). Useful when reporting
  // what gets dropped on shutdown.
  get pending(): number {
    return this.queue.size;
  }

  // One-line tally of what happened this session, printed on shutdown.
  summary(): string {
    const { forwarded, skipped, failed } = this.stats;
    return `forwarded ${forwarded}, skipped ${skipped}, failed ${failed}`;
  }

  stop(): void {
    this.client.onConnectionState.remove(this.handleConnectionState);
    this.client.onError.remove(this.handleClientError);
    // Detach handlers from the client — nulling the reference alone leaves the
    // dispatcher bound and still receiving updates.
    this.dp?.unbind();
    this.dp = null;
  }

  private recordOutcome(outcome: ForwardOutcome): void {
    if (outcome === 'sent') this.stats.forwarded++;
    else if (outcome === 'skipped') this.stats.skipped++;
    else this.stats.failed++;
  }
}

function describeChat(chat: { id: number; username?: string | null }): string {
  return chat.username ? `@${chat.username}` : String(chat.id);
}

// Socket-level errors that just mean the connection dropped and will be
// re-established — not something the user needs to act on. Matched by both the
// Node `code` (when present) and the message text, since mtcute may rewrap the
// original error and lose the `code` property.
const TRANSIENT_NETWORK_CODES = [
  'ECONNRESET',
  'ETIMEDOUT',
  'EPIPE',
  'ECONNABORTED',
  'ENETDOWN',
  'ENETUNREACH',
  'ENETRESET',
  'EHOSTUNREACH',
];

function isTransientNetworkError(err: Error): boolean {
  const code = (err as NodeJS.ErrnoException).code;
  if (code && TRANSIENT_NETWORK_CODES.includes(code)) return true;
  return TRANSIENT_NETWORK_CODES.some((c) => err.message.includes(c));
}

// Telegram errors meaning "the destination peer reference is no longer valid".
// For a forward target this is the signature of a basic group upgraded to a
// supergroup (its id changed) — or a chat we can no longer reach. Matched on
// both the RpcError `errorMessage` and a plain message string.
const INVALID_TARGET_ERRORS = ['CHANNEL_INVALID', 'PEER_ID_INVALID', 'CHAT_ID_INVALID'];

export function isInvalidTargetError(err: unknown): boolean {
  if (!(err instanceof Error)) return false;
  const msg =
    ('errorMessage' in err ? (err as { errorMessage?: string }).errorMessage : null) ?? err.message;
  return INVALID_TARGET_ERRORS.some((code) => msg.includes(code));
}
