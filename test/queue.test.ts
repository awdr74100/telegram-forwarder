import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Logger } from '../src/logger.js';
import { RateLimitedQueue } from '../src/queue.js';

const flush = (ms = 50) => new Promise<void>((r) => setTimeout(r, ms));

// A stand-in for the injected logger so tests assert on log calls without
// touching console or the real consola instance. withTag() returns the same
// object so `.withTag('queue')` inside the queue resolves to these spies.
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

const newQueue = (opts: ConstructorParameters<typeof RateLimitedQueue>[0], log = makeLogger()) => ({
  q: new RateLimitedQueue(opts, { logger: log as unknown as Logger }),
  log,
});

describe('RateLimitedQueue', () => {
  // FloodWait tests switch to fake timers; always hand back real ones so the
  // tests that rely on `flush` (real setTimeout) keep working.
  afterEach(() => {
    vi.useRealTimers();
  });

  it('executes tasks sequentially in order', async () => {
    const results: number[] = [];
    const { q } = newQueue({ delayMs: 0, jitterMs: 0, maxRetries: 0 });

    q.enqueue(async () => {
      results.push(1);
    });
    q.enqueue(async () => {
      results.push(2);
    });
    q.enqueue(async () => {
      results.push(3);
    });

    await flush();
    expect(results).toEqual([1, 2, 3]);
  });

  it('does not crash on task error, continues with next task', async () => {
    const results: number[] = [];
    const { q, log } = newQueue({ delayMs: 0, jitterMs: 0, maxRetries: 0 });

    q.enqueue(async () => {
      throw new Error('boom');
    });
    q.enqueue(async () => {
      results.push(42);
    });

    await flush();
    expect(results).toEqual([42]);
    expect(log.error).toHaveBeenCalled();
  });

  it('retries on FloodWait error and succeeds on second attempt', async () => {
    vi.useFakeTimers();
    let attempts = 0;
    // Buffer is (waitSec + 5) * 1000. Use waitSec = 0 so buffer = 5000ms,
    // fast-forwarded by the fake timers instead of waited out for real.
    const { q, log } = newQueue({ delayMs: 0, jitterMs: 0, maxRetries: 3 });

    const floodErr = Object.assign(new Error('FLOOD_WAIT 0'), {
      errorCode: 420,
      errorMessage: 'FLOOD_WAIT_0',
    });

    q.enqueue(async () => {
      attempts++;
      if (attempts < 2) throw floodErr;
    });

    // Run the first attempt, the 5s backoff, and the retry to completion.
    await vi.runAllTimersAsync();
    expect(attempts).toBe(2);
    expect(log.warn).toHaveBeenCalled();
  });

  it('gives up after maxRetries and logs error', async () => {
    vi.useFakeTimers();
    let attempts = 0;
    const { q, log } = newQueue({ delayMs: 0, jitterMs: 0, maxRetries: 1 });

    const floodErr = Object.assign(new Error('FLOOD_WAIT 0'), {
      errorCode: 420,
      errorMessage: 'FLOOD_WAIT_0',
    });

    q.enqueue(async () => {
      attempts++;
      throw floodErr;
    });

    await vi.runAllTimersAsync();
    // 1 initial + 1 retry (maxRetries=1) = 2 total attempts
    expect(attempts).toBe(2);
    expect(log.error).toHaveBeenCalled();
  });

  it('skips deleted messages without retry or error log', async () => {
    const results: number[] = [];
    const { q, log } = newQueue({ delayMs: 0, jitterMs: 0, maxRetries: 3 });

    const deletedErr = Object.assign(new Error('MESSAGE_ID_INVALID'), {
      errorCode: 400,
      errorMessage: 'MESSAGE_ID_INVALID',
    });

    q.enqueue(async () => {
      throw deletedErr;
    });
    q.enqueue(async () => {
      results.push(1);
    });

    await flush();
    // Deleted message: warn but don't error, don't retry, continue to next task
    expect(log.warn).toHaveBeenCalledWith(expect.stringContaining('deleted'));
    expect(log.error).not.toHaveBeenCalled();
    expect(results).toEqual([1]);
  });

  it('reports the terminal outcome of each task', async () => {
    const outcomes: string[] = [];
    const q = new RateLimitedQueue(
      { delayMs: 0, jitterMs: 0, maxRetries: 0 },
      {
        logger: makeLogger() as unknown as Logger,
        onOutcome: (outcome) => outcomes.push(outcome),
      },
    );

    q.enqueue(async () => {}, 'ok');
    q.enqueue(async () => {
      throw new Error('boom');
    }, 'bad');

    await flush();
    expect(outcomes).toEqual(['sent', 'failed']);
  });

  it('reports queue size', () => {
    const { q } = newQueue({ delayMs: 60_000, jitterMs: 0, maxRetries: 0 });
    // Enqueue 3; first is dequeued immediately, 2 remain
    q.enqueue(async () => {});
    q.enqueue(async () => {});
    q.enqueue(async () => {});
    expect(q.size).toBe(2);
  });
});
