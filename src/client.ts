import { TelegramClient } from '@mtcute/node';

import type { AppConfig } from './types.js';

export function createClient(config: AppConfig): TelegramClient {
  return new TelegramClient({
    apiId: config.apiId,
    apiHash: config.apiHash,
    // Passing a string path → @mtcute/node automatically uses SQLite (better-sqlite3)
    storage: config.sessionPath,
  });
}
