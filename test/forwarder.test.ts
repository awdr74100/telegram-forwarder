import type { TelegramClient } from '@mtcute/node';
import { describe, expect, it, vi } from 'vitest';

import { Forwarder } from '../src/forwarder.js';
import type { Logger } from '../src/logger.js';
import type { AppConfig, ForwardGroup } from '../src/types.js';

// The Dispatcher only needs these event hooks to bind/unbind, plus the
// connection-state emitter the forwarder subscribes to. This is an
// event-emitter stub, not a network mock — no Telegram RPC is ever exercised;
// we are testing our own group-filtering logic.
const makeFakeClient = () => ({
  onUpdate: {
    add: vi.fn<(handler: unknown) => void>(),
    remove: vi.fn<(handler: unknown) => void>(),
  },
  onRawUpdate: {
    add: vi.fn<(handler: unknown) => void>(),
    remove: vi.fn<(handler: unknown) => void>(),
  },
  onConnectionState: {
    add: vi.fn<(handler: unknown) => void>(),
    remove: vi.fn<(handler: unknown) => void>(),
  },
});

// A logger stub so tests assert on log calls without touching consola. withTag()
// returns the same object so both the forwarder and its queue resolve to these
// spies.
const makeLogger = () => {
  const log = {
    success: vi.fn<(...args: unknown[]) => void>(),
    warn: vi.fn<(...args: unknown[]) => void>(),
    error: vi.fn<(...args: unknown[]) => void>(),
    info: vi.fn<(...args: unknown[]) => void>(),
    debug: vi.fn<(...args: unknown[]) => void>(),
    withTag: () => log,
  };
  return log;
};

const makeGroup = (overrides: Partial<ForwardGroup> = {}): ForwardGroup => ({
  id: 'g1',
  name: 'Group',
  sourcePeers: ['@src'],
  targetPeers: ['@dst'],
  contentTypes: ['all'],
  noAuthor: false,
  enabled: true,
  createdAt: '2026-01-01T00:00:00.000Z',
  ...overrides,
});

const makeConfig = (groups: ForwardGroup[]): AppConfig => ({
  apiId: 1,
  apiHash: 'x',
  sessionPath: '',
  groups,
  rateLimit: { delayMs: 0, jitterMs: 0, maxRetries: 0 },
});

const makeForwarder = (config: AppConfig) => {
  const client = makeFakeClient();
  const log = makeLogger();
  const forwarder = new Forwarder(
    client as unknown as TelegramClient,
    config,
    log as unknown as Logger,
  );
  return { client, log, forwarder };
};

describe('Forwarder', () => {
  it('monitors only the enabled groups', () => {
    const { client, log, forwarder } = makeForwarder(
      makeConfig([
        makeGroup({ id: 'on', enabled: true }),
        makeGroup({ id: 'off', enabled: false }),
      ]),
    );

    forwarder.start();

    // Only the enabled group is counted, and the dispatcher is bound.
    expect(log.info).toHaveBeenCalledWith(expect.stringContaining('Monitoring 1 group(s)'));
    expect(client.onUpdate.add).toHaveBeenCalledTimes(1);

    forwarder.stop();
    expect(client.onUpdate.remove).toHaveBeenCalledTimes(1);
  });

  it('does nothing and binds no dispatcher when every group is disabled', () => {
    const { client, log, forwarder } = makeForwarder(makeConfig([makeGroup({ enabled: false })]));

    forwarder.start();

    expect(log.warn).toHaveBeenCalledWith(expect.stringContaining('No enabled groups'));
    expect(client.onUpdate.add).not.toHaveBeenCalled();
  });

  it('does nothing when there are no groups at all', () => {
    const { log, forwarder } = makeForwarder(makeConfig([]));

    forwarder.start();

    expect(log.warn).toHaveBeenCalledWith(expect.stringContaining('No enabled groups'));
    expect(forwarder.pending).toBe(0);
  });

  it('stop() is safe to call when start() bound nothing', () => {
    const { client, forwarder } = makeForwarder(makeConfig([]));

    forwarder.start();

    expect(() => forwarder.stop()).not.toThrow();
    expect(client.onUpdate.remove).not.toHaveBeenCalled();
  });
});
