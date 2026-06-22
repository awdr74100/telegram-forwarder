import type { TelegramClient } from '@mtcute/node';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Forwarder } from '../src/forwarder.js';
import type { AppConfig, ForwardGroup } from '../src/types.js';

// The Dispatcher only needs these event hooks to bind/unbind. This is an
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
});

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

const logText = (spy: ReturnType<typeof vi.spyOn>) =>
  spy.mock.calls.map((args: unknown[]) => args.join(' ')).join('\n');

describe('Forwarder', () => {
  let logSpy: ReturnType<typeof vi.spyOn>;

  afterEach(() => {
    logSpy?.mockRestore();
  });

  it('monitors only the enabled groups', () => {
    logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    const client = makeFakeClient();
    const config = makeConfig([
      makeGroup({ id: 'on', enabled: true }),
      makeGroup({ id: 'off', enabled: false }),
    ]);

    const forwarder = new Forwarder(client as unknown as TelegramClient, config);
    forwarder.start();

    // Only the enabled group is counted, and the dispatcher is bound.
    expect(logText(logSpy)).toContain('Monitoring 1 group(s)');
    expect(client.onUpdate.add).toHaveBeenCalledTimes(1);

    forwarder.stop();
    expect(client.onUpdate.remove).toHaveBeenCalledTimes(1);
  });

  it('does nothing and binds no dispatcher when every group is disabled', () => {
    logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    const client = makeFakeClient();
    const config = makeConfig([makeGroup({ enabled: false })]);

    new Forwarder(client as unknown as TelegramClient, config).start();

    expect(logText(logSpy)).toContain('No enabled groups');
    expect(client.onUpdate.add).not.toHaveBeenCalled();
  });

  it('does nothing when there are no groups at all', () => {
    logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    const client = makeFakeClient();

    const forwarder = new Forwarder(client as unknown as TelegramClient, makeConfig([]));
    forwarder.start();

    expect(logText(logSpy)).toContain('No enabled groups');
    expect(forwarder.pending).toBe(0);
  });

  it('stop() is safe to call when start() bound nothing', () => {
    logSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
    const client = makeFakeClient();

    const forwarder = new Forwarder(client as unknown as TelegramClient, makeConfig([]));
    forwarder.start();

    expect(() => forwarder.stop()).not.toThrow();
    expect(client.onUpdate.remove).not.toHaveBeenCalled();
  });
});
