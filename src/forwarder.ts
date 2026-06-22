import { styleText } from 'node:util';

import type { Message } from '@mtcute/core';
import { Dispatcher } from '@mtcute/dispatcher';
import type { TelegramClient } from '@mtcute/node';

import { matchesContentType, matchesPeer, toInputPeer } from './filter.js';
import { RateLimitedQueue } from './queue.js';
import type { AppConfig, ForwardGroup } from './types.js';

export class Forwarder {
  private readonly client: TelegramClient;
  private readonly groups: ForwardGroup[];
  private readonly queue: RateLimitedQueue;
  private dp: Dispatcher | null = null;

  constructor(client: TelegramClient, config: AppConfig) {
    this.client = client;
    this.groups = config.groups.filter((g) => g.enabled);
    this.queue = new RateLimitedQueue(config.rateLimit);
  }

  start(): void {
    if (this.groups.length === 0) {
      console.log(styleText('yellow', 'No enabled groups — nothing to forward.'));
      return;
    }

    const dp = Dispatcher.for(this.client);
    this.dp = dp;

    dp.onNewMessage(async (upd) => {
      const chat = upd.chat as { id: number; username?: string | null };

      for (const group of this.groups) {
        const isSourceMatch = group.sourcePeers.some((peer) => matchesPeer(chat, peer));
        if (!isSourceMatch) continue;
        if (!matchesContentType(upd, group.contentTypes)) continue;

        const msg = upd as unknown as Message;
        const mediaType = upd.media?.type ?? 'text';

        for (const targetPeer of group.targetPeers) {
          this.queue.enqueue(async () => {
            await this.client.forwardMessages({
              messages: [msg],
              toChatId: toInputPeer(targetPeer),
              noAuthor: group.noAuthor,
            });
            console.log(
              styleText('dim', `[${formatTimestamp()}]`) +
                ' ' +
                styleText('green', '✓') +
                ` [${group.name}] msg ${upd.id} (${mediaType}) → ${targetPeer}`,
            );
          });
        }
      }
    });

    console.log(
      styleText('blue', `Dispatcher started. Monitoring ${this.groups.length} group(s).`),
    );
  }

  // Number of forwards still queued (not yet sent). Useful when reporting
  // what gets dropped on shutdown.
  get pending(): number {
    return this.queue.size;
  }

  stop(): void {
    // Detach handlers from the client — nulling the reference alone leaves the
    // dispatcher bound and still receiving updates.
    this.dp?.unbind();
    this.dp = null;
  }
}

// Local time in a readable "YYYY-MM-DD HH:mm:ss" format for log lines.
function formatTimestamp(date = new Date()): string {
  const pad = (n: number): string => String(n).padStart(2, '0');
  const ymd = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
  const hms = `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  return `${ymd} ${hms}`;
}
