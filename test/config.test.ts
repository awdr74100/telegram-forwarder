import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { clearSession, hasSession, isConfigured, loadConfig, saveConfig } from '../src/config.js';
import type { AppConfig } from '../src/types.js';

describe('session helpers', () => {
  let dir: string;
  let sessionPath: string;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'tgf-'));
    sessionPath = join(dir, 'session.sqlite');
  });

  afterEach(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  it('hasSession is false when no session files exist', () => {
    expect(hasSession(sessionPath)).toBe(false);
  });

  it('hasSession is true when only a WAL sidecar exists', () => {
    writeFileSync(`${sessionPath}-wal`, 'x');
    expect(hasSession(sessionPath)).toBe(true);
  });

  it('clearSession removes the db and its wal/shm sidecars', () => {
    writeFileSync(sessionPath, 'x');
    writeFileSync(`${sessionPath}-wal`, 'x');
    writeFileSync(`${sessionPath}-shm`, 'x');

    const removed = clearSession(sessionPath);

    expect(removed).toHaveLength(3);
    expect(existsSync(sessionPath)).toBe(false);
    expect(existsSync(`${sessionPath}-wal`)).toBe(false);
    expect(existsSync(`${sessionPath}-shm`)).toBe(false);
    expect(hasSession(sessionPath)).toBe(false);
  });

  it('clearSession returns an empty list when there is nothing to remove', () => {
    expect(clearSession(sessionPath)).toEqual([]);
  });
});

describe('loadConfig', () => {
  let dir: string;
  let configFile: string;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'tgf-'));
    configFile = join(dir, 'config.json');
  });

  afterEach(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  it('returns defaults when the config file does not exist', () => {
    const config = loadConfig(configFile);
    expect(config.apiId).toBe(0);
    expect(config.apiHash).toBe('');
    expect(config.groups).toEqual([]);
    expect(config.rateLimit).toEqual({ delayMs: 100, jitterMs: 50, maxRetries: 5 });
  });

  it('deep-merges a partial config, keeping unspecified nested defaults', () => {
    // Only one rateLimit field is persisted; the rest must survive via defu.
    writeFileSync(
      configFile,
      JSON.stringify({ apiId: 42, apiHash: 'abc', rateLimit: { delayMs: 999 } }),
    );

    const config = loadConfig(configFile);

    expect(config.apiId).toBe(42);
    expect(config.apiHash).toBe('abc');
    expect(config.rateLimit).toEqual({ delayMs: 999, jitterMs: 50, maxRetries: 5 });
  });

  it('falls back to defaults when the file is corrupt JSON', () => {
    writeFileSync(configFile, '{ not valid json');
    expect(loadConfig(configFile)).toMatchObject({ apiId: 0, apiHash: '' });
  });

  it('round-trips through saveConfig', () => {
    const original: AppConfig = {
      apiId: 7,
      apiHash: 'deadbeef',
      sessionPath: join(dir, 'session.sqlite'),
      groups: [],
      rateLimit: { delayMs: 200, jitterMs: 10, maxRetries: 2 },
    };

    saveConfig(original, configFile);

    expect(loadConfig(configFile)).toEqual(original);
  });

  it('saveConfig creates the parent directory if missing', () => {
    const nested = join(dir, 'a', 'b', 'config.json');
    saveConfig(loadConfig(nested), nested);
    expect(existsSync(nested)).toBe(true);
  });
});

describe('isConfigured', () => {
  const base: AppConfig = {
    apiId: 0,
    apiHash: '',
    sessionPath: '',
    groups: [],
    rateLimit: { delayMs: 0, jitterMs: 0, maxRetries: 0 },
  };

  it('is false when apiId is not set', () => {
    expect(isConfigured({ ...base, apiHash: 'abc' })).toBe(false);
  });

  it('is false when apiHash is empty', () => {
    expect(isConfigured({ ...base, apiId: 42 })).toBe(false);
  });

  it('is true when both apiId and apiHash are set', () => {
    expect(isConfigured({ ...base, apiId: 42, apiHash: 'abc' })).toBe(true);
  });
});
