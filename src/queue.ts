import { logger as defaultLogger, type Logger } from './logger.js';
import type { RateLimitConfig } from './types.js';

type Task = () => Promise<void>;

export type ForwardOutcome = 'sent' | 'skipped' | 'failed';

// Thrown by a task to tell the queue the forward was deliberately skipped rather
// than failed: the queue records it as 'skipped' and surfaces the message as a
// warning, not an error. Used when a target is retired mid-session (e.g. an
// unreachable peer that is not a recoverable migration).
export class SkippedForward extends Error {}

interface QueueDeps {
  logger?: Logger;
  // Called once per task with its terminal outcome so callers can keep stats.
  onOutcome?: (outcome: ForwardOutcome, label: string) => void;
}

interface QueueItem {
  task: Task;
  // Human-readable description used in log lines and outcome reporting.
  label: string;
}

export class RateLimitedQueue {
  private readonly queue: QueueItem[] = [];
  private running = false;
  private readonly opts: RateLimitConfig;
  private readonly log: Logger;
  private readonly onOutcome?: (outcome: ForwardOutcome, label: string) => void;

  constructor(opts: RateLimitConfig, deps: QueueDeps = {}) {
    this.opts = opts;
    this.log = (deps.logger ?? defaultLogger).withTag('queue');
    this.onOutcome = deps.onOutcome;
  }

  enqueue(task: Task, label = 'forward'): void {
    this.queue.push({ task, label });
    if (!this.running) void this.drain();
  }

  get size(): number {
    return this.queue.length;
  }

  private async drain(): Promise<void> {
    this.running = true;
    while (this.queue.length > 0) {
      const { task, label } = this.queue.shift()!;
      await this.runWithRetry(task, label);
      if (this.queue.length > 0) {
        await this.sleep(this.opts.delayMs + Math.random() * this.opts.jitterMs);
      }
    }
    this.running = false;
  }

  private async runWithRetry(task: Task, label: string, attempt = 0): Promise<void> {
    try {
      await task();
      this.log.success(label);
      this.onOutcome?.('sent', label);
    } catch (err: unknown) {
      if (err instanceof SkippedForward) {
        this.log.warn(err.message);
        this.onOutcome?.('skipped', label);
        return;
      }

      if (isDeletedMessage(err)) {
        this.log.warn(`Message was deleted before forwarding — skipping (${label})`);
        this.onOutcome?.('skipped', label);
        return;
      }

      const waitSec = extractFloodWait(err);
      if (waitSec !== null && attempt < this.opts.maxRetries) {
        const waitMs = (waitSec + 5) * 1000;
        this.log.warn(
          `FloodWait ${waitSec}s — pausing ${waitSec + 5}s` +
            ` (attempt ${attempt + 1}/${this.opts.maxRetries}) [${label}]`,
        );
        await this.sleep(waitMs);
        return this.runWithRetry(task, label, attempt + 1);
      }

      this.log.error(`Forward failed [${label}]:`, err instanceof Error ? err.message : err);
      this.onOutcome?.('failed', label);
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
