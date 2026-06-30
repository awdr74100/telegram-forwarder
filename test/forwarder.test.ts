import type { TelegramClient } from '@mtcute/node';
import { describe, expect, it, vi } from 'vitest';

import { Forwarder, isInvalidTargetError } from '../src/forwarder.js';
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
  onError: {
    add: vi.fn<(handler: (err: Error) => void) => void>(),
    remove: vi.fn<(handler: (err: Error) => void) => void>(),
  },
  resolvePeer: vi.fn<(peer: string | number) => Promise<{ _: string }>>(),
  iterDialogs: vi.fn<() => AsyncIterable<unknown>>(),
});

// An async iterable that yields nothing — enough for warmPeerCache to drain it.
const emptyDialogs = (): AsyncIterable<unknown> => ({
  async *[Symbol.asyncIterator]() {},
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
  const forwarder = new Forwarder(client as unknown as TelegramClient, config, {
    logger: log as unknown as Logger,
  });
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

  it('registers an error handler that mutes network drops but surfaces real errors', () => {
    const { client, log, forwarder } = makeForwarder(makeConfig([makeGroup()]));

    forwarder.start();

    // The handler is registered so mtcute stops printing its own "unhandled error".
    expect(client.onError.add).toHaveBeenCalledTimes(1);
    const onError = client.onError.add.mock.calls[0]![0];

    // A dropped socket (Mac sleeping, etc.) is expected churn → debug, not error.
    const reset = Object.assign(new Error('read ECONNRESET'), { code: 'ECONNRESET' });
    onError(reset);
    expect(log.error).not.toHaveBeenCalled();
    expect(log.debug).toHaveBeenCalledWith(expect.stringContaining('mtcute will reconnect'));

    // Matched by message text too, even when the code property was stripped.
    onError(new Error('something ETIMEDOUT happened'));
    expect(log.error).not.toHaveBeenCalled();

    // Anything else is a genuine error and must be surfaced.
    onError(new Error('AUTH_KEY_UNREGISTERED'));
    expect(log.error).toHaveBeenCalledWith(expect.stringContaining('AUTH_KEY_UNREGISTERED'));

    forwarder.stop();
    expect(client.onError.remove).toHaveBeenCalledTimes(1);
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

describe('isInvalidTargetError', () => {
  it('matches invalid-peer RpcErrors by errorMessage', () => {
    const rpc = Object.assign(new Error('Telegram API error 400'), {
      errorMessage: 'CHANNEL_INVALID',
    });
    expect(isInvalidTargetError(rpc)).toBe(true);
  });

  it('matches the same codes in a plain message string', () => {
    expect(isInvalidTargetError(new Error('Telegram API error 400: PEER_ID_INVALID'))).toBe(true);
    expect(isInvalidTargetError(new Error('CHAT_ID_INVALID'))).toBe(true);
  });

  it('does not swallow FloodWait or deleted-message errors', () => {
    expect(isInvalidTargetError(new Error('FLOOD_WAIT_60'))).toBe(false);
    expect(isInvalidTargetError(new Error('MESSAGE_ID_INVALID'))).toBe(false);
  });

  it('ignores non-Error values', () => {
    expect(isInvalidTargetError('CHANNEL_INVALID')).toBe(false);
    expect(isInvalidTargetError(null)).toBe(false);
  });
});

describe('Forwarder.checkPeers', () => {
  it('does not scan dialogs when every peer resolves from cache', async () => {
    const { client, log, forwarder } = makeForwarder(
      makeConfig([makeGroup({ sourcePeers: ['@src'], targetPeers: ['-1001'] })]),
    );
    client.resolvePeer.mockResolvedValue({ _: 'inputPeerChannel' });

    await forwarder.checkPeers();

    expect(client.iterDialogs).not.toHaveBeenCalled();
    expect(log.warn).not.toHaveBeenCalled();
    expect(log.info).toHaveBeenCalledWith(expect.stringContaining('Resolved 2/2 peer(s)'));
  });

  it('warms the cache and recovers a peer that was missing on the first pass', async () => {
    const { client, log, forwarder } = makeForwarder(
      makeConfig([makeGroup({ sourcePeers: ['@src'], targetPeers: ['-1002'] })]),
    );
    client.iterDialogs.mockReturnValue(emptyDialogs());
    // @src resolves throughout; -1002 fails until the dialog scan warms it.
    client.resolvePeer.mockImplementation((peer) => {
      if (peer === '@src') return Promise.resolve({ _: 'inputPeerChannel' });
      return client.iterDialogs.mock.calls.length > 0
        ? Promise.resolve({ _: 'inputPeerChannel' })
        : Promise.reject(new Error('not in local cache'));
    });

    await forwarder.checkPeers();

    expect(client.iterDialogs).toHaveBeenCalledTimes(1);
    expect(log.warn).not.toHaveBeenCalled();
    expect(log.info).toHaveBeenCalledWith(expect.stringContaining('Resolved 2/2 peer(s)'));
  });

  it('treats a message-reference resolution as stale and refreshes it', async () => {
    const { client, log, forwarder } = makeForwarder(
      makeConfig([makeGroup({ sourcePeers: ['-1003'], targetPeers: ['-1003'] })]),
    );
    client.iterDialogs.mockReturnValue(emptyDialogs());
    client.resolvePeer.mockImplementation(() =>
      client.iterDialogs.mock.calls.length > 0
        ? Promise.resolve({ _: 'inputPeerChannel' })
        : Promise.resolve({ _: 'inputPeerChannelFromMessage' }),
    );

    await forwarder.checkPeers();

    expect(client.iterDialogs).toHaveBeenCalledTimes(1);
    expect(log.warn).not.toHaveBeenCalled();
    expect(log.info).toHaveBeenCalledWith(expect.stringContaining('Resolved 1/1 peer(s)'));
  });

  it('warns about a peer that stays unresolvable even after warming', async () => {
    const { client, log, forwarder } = makeForwarder(
      makeConfig([makeGroup({ sourcePeers: ['@src'], targetPeers: ['-1004'] })]),
    );
    client.iterDialogs.mockReturnValue(emptyDialogs());
    client.resolvePeer.mockImplementation((peer) => {
      if (peer === '@src') return Promise.resolve({ _: 'inputPeerChannel' });
      return Promise.reject(new Error('not in local cache'));
    });

    await forwarder.checkPeers();

    expect(client.iterDialogs).toHaveBeenCalledTimes(1);
    expect(log.warn).toHaveBeenCalledWith(
      expect.stringContaining('Could not resolve peer "-1004"'),
    );
    expect(log.info).toHaveBeenCalledWith(expect.stringContaining('Resolved 1/2 peer(s)'));
  });

  it('still resolves peers when the dialog scan itself fails', async () => {
    const { client, log, forwarder } = makeForwarder(
      makeConfig([makeGroup({ sourcePeers: ['-1005'], targetPeers: ['-1005'] })]),
    );
    client.iterDialogs.mockImplementation(() => {
      throw new Error('FLOOD_WAIT');
    });
    client.resolvePeer.mockRejectedValue(new Error('not in local cache'));

    await forwarder.checkPeers();

    expect(log.warn).toHaveBeenCalledWith(expect.stringContaining('Dialog sync failed'));
    expect(log.warn).toHaveBeenCalledWith(
      expect.stringContaining('Could not resolve peer "-1005"'),
    );
  });
});
