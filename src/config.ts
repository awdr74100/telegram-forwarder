import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

import { defu } from 'defu';

import type { AppConfig } from './types.js';

const CONFIG_DIR = join(homedir(), '.telegram-forwarder');
const CONFIG_FILE = join(CONFIG_DIR, 'config.json');

const DEFAULTS: AppConfig = {
  apiId: 0,
  apiHash: '',
  sessionPath: join(CONFIG_DIR, 'session.sqlite'),
  groups: [],
  rateLimit: {
    delayMs: 100,
    jitterMs: 50,
    maxRetries: 5,
  },
};

export function loadConfig(): AppConfig {
  if (!existsSync(CONFIG_FILE)) return structuredClone(DEFAULTS);
  try {
    const raw = JSON.parse(readFileSync(CONFIG_FILE, 'utf-8')) as Partial<AppConfig>;
    // defu deep-merges, so a partial stored config (e.g. only rateLimit.delayMs)
    // still inherits the remaining nested defaults instead of dropping them.
    return defu(raw, DEFAULTS);
  } catch {
    return structuredClone(DEFAULTS);
  }
}

export function saveConfig(config: AppConfig): void {
  mkdirSync(CONFIG_DIR, { recursive: true });
  writeFileSync(CONFIG_FILE, JSON.stringify(config, null, 2), 'utf-8');
}

export function isConfigured(config: AppConfig): boolean {
  return config.apiId > 0 && config.apiHash.length > 0;
}

// SQLite (WAL mode) stores the session across these sibling files.
const SESSION_SUFFIXES = ['', '-wal', '-shm', '-journal'];

export function hasSession(sessionPath: string): boolean {
  return SESSION_SUFFIXES.some((suffix) => existsSync(sessionPath + suffix));
}

export function clearSession(sessionPath: string): string[] {
  const removed: string[] = [];
  for (const suffix of SESSION_SUFFIXES) {
    const file = sessionPath + suffix;
    if (existsSync(file)) {
      rmSync(file);
      removed.push(file);
    }
  }
  return removed;
}

export function getConfigDir(): string {
  return CONFIG_DIR;
}
