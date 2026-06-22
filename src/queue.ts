import type { RateLimitConfig } from './types.js';

type Task = () => Promise<void>;

export class RateLimitedQueue {
  private readonly queue: Task[] = [];
  private running = false;
  private readonly opts: RateLimitConfig;

  constructor(opts: RateLimitConfig) {
    this.opts = opts;
  }

  enqueue(task: Task): void {
    this.queue.push(task);
    if (!this.running) void this.drain();
  }

  get size(): number {
    return this.queue.length;
  }

  private async drain(): Promise<void> {
    this.running = true;
    while (this.queue.length > 0) {
      const task = this.queue.shift()!;
      await this.runWithRetry(task);
      if (this.queue.length > 0) {
        await this.sleep(this.opts.delayMs + Math.random() * this.opts.jitterMs);
      }
    }
    this.running = false;
  }

  private async runWithRetry(task: Task, attempt = 0): Promise<void> {
    try {
      await task();
    } catch (err: unknown) {
      if (isDeletedMessage(err)) {
        console.warn('[queue] Message was deleted before forwarding — skipping');
        return;
      }

      const waitSec = extractFloodWait(err);
      if (waitSec !== null && attempt < this.opts.maxRetries) {
        const waitMs = (waitSec + 5) * 1000;
        console.warn(
          `[queue] FloodWait ${waitSec}s — pausing ${waitSec + 5}s` +
            ` (attempt ${attempt + 1}/${this.opts.maxRetries})`,
        );
        await this.sleep(waitMs);
        return this.runWithRetry(task, attempt + 1);
      }

      console.error('[queue] Task failed:', err instanceof Error ? err.message : err);
    }
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }
}

function isDeletedMessage(err: unknown): boolean {
  if (!(err instanceof Error)) return false;
  const msg =
    ('errorMessage' in err ? (err as { errorMessage: string }).errorMessage : null) ?? err.message;
  return /MESSAGE_ID_INVALID|message.*not.*found|invalid.*message/i.test(msg);
}

function extractFloodWait(err: unknown): number | null {
  if (!(err instanceof Error)) return null;

  // mtcute raises RpcError with errorCode 420 and errorMessage like FLOOD_WAIT_60
  if ('errorCode' in err) {
    const code = (err as { errorCode: unknown }).errorCode;
    if (code === 420) {
      const msg = (err as { errorMessage?: string }).errorMessage ?? err.message;
      const m = msg.match(/FLOOD_WAIT[_\s]+(\d+)/i);
      return m ? Number(m[1]) : 60;
    }
  }

  if (err.message.includes('FLOOD_WAIT')) {
    const m = err.message.match(/(\d+)/);
    return m ? Number(m[1]) : 60;
  }

  return null;
}
