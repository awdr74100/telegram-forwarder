import { appendFileSync } from 'node:fs';
import { inspect } from 'node:util';

import { createConsola, type ConsolaInstance, type LogObject } from 'consola';

// One shared, leveled logger for the long-running runtime (start / forwarder /
// queue). Interactive commands (init / group / reset) keep using
// @clack/prompts — consola is for streaming logs, clack for wizard-style
// prompts. nuxt/cli pairs the two libraries the same way.
export type Logger = ConsolaInstance;

export const logger: Logger = createConsola({
  formatOptions: { date: true, colors: true, compact: true },
});

// --verbose unlocks debug-level match tracing; --quiet drops everything below
// warnings. consola also honours the CONSOLA_LEVEL env var out of the box, so
// callers get an env override for free.
export function setVerbosity({ verbose, quiet }: { verbose?: boolean; quiet?: boolean }): void {
  if (quiet) logger.level = 1;
  else if (verbose) logger.level = 4;
}

// Mirror console output into an append-only file so a forwarder left running
// overnight leaves a trail to inspect. A failed write must never crash the run.
export function enableFileLogging(filePath: string): void {
  logger.addReporter({
    log(logObj: LogObject) {
      const time = logObj.date.toISOString();
      const tag = logObj.tag ? `[${logObj.tag}] ` : '';
      const body = logObj.args
        .map((arg) => (typeof arg === 'string' ? arg : inspect(arg)))
        .join(' ');
      try {
        appendFileSync(filePath, `${time} ${logObj.type.toUpperCase().padEnd(7)} ${tag}${body}\n`);
      } catch {
        // Logging must never take down the forwarder; swallow write failures.
      }
    },
  });
}
