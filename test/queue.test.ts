import { describe, expect, it, vi } from 'vitest';

import { RateLimitedQueue } from '../src/queue.js';

const flush = (ms = 50) => new Promise<void>((r) => setTimeout(r, ms));

describe('RateLimitedQueue', () => {
  it('executes tasks sequentially in order', async () => {
    const results: number[] = [];
    const q = new RateLimitedQueue({ delayMs: 0, jitterMs: 0, maxRetries: 0 });

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
    const q = new RateLimitedQueue({ delayMs: 0, jitterMs: 0, maxRetries: 0 });
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});

    q.enqueue(async () => {
      throw new Error('boom');
    });
    q.enqueue(async () => {
      results.push(42);
    });

    await flush();
    expect(results).toEqual([42]);
    expect(spy).toHaveBeenCalled();
    spy.mockRestore();
  });

  it('retries on FloodWait error and succeeds on second attempt', async () => {
    let attempts = 0;
    // Buffer is (waitSec + 5) * 1000. Use waitSec = 0 so buffer = 5000ms.
    const q = new RateLimitedQueue({ delayMs: 0, jitterMs: 0, maxRetries: 3 });
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});

    const floodErr = Object.assign(new Error('FLOOD_WAIT 0'), {
      errorCode: 420,
      errorMessage: 'FLOOD_WAIT_0',
    });

    q.enqueue(async () => {
      attempts++;
      if (attempts < 2) throw floodErr;
    });

    // Wait for: first attempt (instant) + FloodWait buffer 5s + second attempt (instant)
    await flush(6000);
    expect(attempts).toBe(2);
    warnSpy.mockRestore();
  }, 10_000);

  it('gives up after maxRetries and logs error', async () => {
    let attempts = 0;
    const q = new RateLimitedQueue({ delayMs: 0, jitterMs: 0, maxRetries: 1 });
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    const floodErr = Object.assign(new Error('FLOOD_WAIT 0'), {
      errorCode: 420,
      errorMessage: 'FLOOD_WAIT_0',
    });

    q.enqueue(async () => {
      attempts++;
      throw floodErr;
    });

    await flush(12000);
    // 1 initial + 1 retry (maxRetries=1) = 2 total attempts
    expect(attempts).toBe(2);
    expect(errSpy).toHaveBeenCalled();
    warnSpy.mockRestore();
    errSpy.mockRestore();
  }, 15_000);

  it('skips deleted messages without retry or error log', async () => {
    const results: number[] = [];
    const q = new RateLimitedQueue({ delayMs: 0, jitterMs: 0, maxRetries: 3 });
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

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
    expect(warnSpy).toHaveBeenCalledWith(expect.stringContaining('deleted'));
    expect(errSpy).not.toHaveBeenCalled();
    expect(results).toEqual([1]);
    warnSpy.mockRestore();
    errSpy.mockRestore();
  });

  it('reports queue size', () => {
    const q = new RateLimitedQueue({ delayMs: 60_000, jitterMs: 0, maxRetries: 0 });
    // Enqueue 3; first is dequeued immediately, 2 remain
    q.enqueue(async () => {});
    q.enqueue(async () => {});
    q.enqueue(async () => {});
    expect(q.size).toBe(2);
  });
});
