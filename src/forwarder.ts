import type { Message } from '@mtcute/core';
import { Dispatcher } from '@mtcute/dispatcher';
import type { TelegramClient } from '@mtcute/node';

import { matchesContentType, matchesKeywords, matchesPeer, toInputPeer } from './filter.js';
import { logger as defaultLogger, type Logger } from './logger.js';
import { type ForwardOutcome, RateLimitedQueue } from './queue.js';
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
}

export class Forwarder {
  private readonly client: TelegramClient;
  private readonly groups: ForwardGroup[];
  private readonly queue: RateLimitedQueue;
  private readonly log: Logger;
  private readonly dryRun: boolean;
  private dp: Dispatcher | null = null;
  private readonly stats: ForwarderStats = { forwarded: 0, skipped: 0, failed: 0 };

  // Bound once so it can be added and later removed from the connection emitter.
  private readonly handleConnectionState = (state: string): void => {
    if (state === 'connected') this.log.success('Connected to Telegram.');
    else if (state === 'offline') this.log.warn('Disconnected from Telegram — reconnecting…');
    else this.log.debug(`Connection state: ${state}`);
  };

  constructor(client: TelegramClient, config: AppConfig, opts: ForwarderOptions = {}) {
    const { logger = defaultLogger, dryRun = false } = opts;
    this.client = client;
    this.groups = config.groups.filter((g) => g.enabled);
    this.dryRun = dryRun;
    this.log = logger.withTag('forwarder');
    this.queue = new RateLimitedQueue(config.rateLimit, {
      logger,
      onOutcome: (outcome) => this.recordOutcome(outcome),
    });
  }

  // Pre-resolve every source/target peer before listening, so unreachable peers
  // surface as a startup warning instead of a lazy failure on the first forward.
  async checkPeers(): Promise<void> {
    const peers = new Set<string>();
    for (const group of this.groups) {
      for (const peer of group.sourcePeers) peers.add(peer);
      for (const peer of group.targetPeers) peers.add(peer);
    }

    let resolved = 0;
    for (const peer of peers) {
      try {
        await this.client.resolvePeer(toInputPeer(peer));
        resolved++;
      } catch {
        this.log.warn(`Could not resolve peer "${peer}" — forwards involving it may fail.`);
      }
    }
    this.log.info(`Resolved ${resolved}/${peers.size} peer(s).`);
  }

  start(): void {
    if (this.groups.length === 0) {
      this.log.warn('No enabled groups — nothing to forward.');
      return;
    }

    this.client.onConnectionState.add(this.handleConnectionState);

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
          const label = `[${group.name}] msg ${upd.id} (${mediaType}) → ${targetPeer}`;
          if (this.dryRun) {
            this.log.info(`[dry-run] would forward ${label}`);
            continue;
          }
          this.log.debug(`Enqueued ${label}`);
          this.queue.enqueue(async () => {
            await this.client.forwardMessages({
              messages: [msg],
              toChatId: toInputPeer(targetPeer),
              noAuthor: group.noAuthor,
            });
          }, label);
        }
      }
    });

    this.log.info(`Dispatcher started. Monitoring ${this.groups.length} group(s).`);
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
