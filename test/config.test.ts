import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { clearSession, hasSession } from '../src/config.js';

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
