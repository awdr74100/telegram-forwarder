import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

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

// `configFile` is injectable so tests can point at a temp file instead of the
// real ~/.telegram-forwarder/config.json.
export function loadConfig(configFile = CONFIG_FILE): AppConfig {
  if (!existsSync(configFile)) return structuredClone(DEFAULTS);
  try {
    const raw = JSON.parse(readFileSync(configFile, 'utf-8')) as Partial<AppConfig>;
    // defu deep-merges, so a partial stored config (e.g. only rateLimit.delayMs)
    // still inherits the remaining nested defaults instead of dropping them.
    return defu(raw, DEFAULTS);
  } catch {
    return structuredClone(DEFAULTS);
  }
}

export function saveConfig(config: AppConfig, configFile = CONFIG_FILE): void {
  mkdirSync(dirname(configFile), { recursive: true });
  writeFileSync(configFile, JSON.stringify(config, null, 2), 'utf-8');
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
