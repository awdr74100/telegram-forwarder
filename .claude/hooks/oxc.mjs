#!/usr/bin/env node
// Claude Code PostToolUse hook: run the oxc toolchain on a file right after
// Claude edits or writes it.
//
// Flow:
//   1. oxlint --fix  -> auto-fix what it can, report what it can't
//   2. oxfmt --write -> apply the project's formatting rules
//   3. If unfixable lint errors remain (or a syntax error makes formatting
//      fail), exit with code 2 and write details to stderr so Claude can
//      follow up and fix them.
//
// Config files (.oxlintrc.json / .oxfmtrc.json) are auto-discovered by both
// tools, so they are not passed explicitly here.

import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// Project root: this file lives at <root>/.claude/hooks/oxc.mjs
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

// Only handle these extensions (the JS/TS family oxc supports).
const EXTENSIONS = new Set([
  '.ts',
  '.tsx',
  '.mts',
  '.cts',
  '.js',
  '.jsx',
  '.mjs',
  '.cjs',
]);

/** Read the hook payload (JSON) that Claude Code sends on stdin. */
async function readStdin() {
  let data = '';
  for await (const chunk of process.stdin) data += chunk;
  return data;
}

/** Prefer the project-local node_modules/.bin, fall back to PATH. */
function bin(name) {
  const local = join(root, 'node_modules', '.bin', name);
  return existsSync(local) ? local : name;
}

/** Run a command and return { code, stdout, stderr }; never throws on non-zero exit. */
function run(cmd, args) {
  try {
    const stdout = execFileSync(cmd, args, {
      cwd: root,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    return { code: 0, stdout, stderr: '' };
  } catch (err) {
    if (err.code === 'ENOENT') {
      return { code: 127, stdout: '', stderr: `command not found: ${cmd}` };
    }
    return {
      code: err.status ?? 1,
      stdout: err.stdout?.toString() ?? '',
      stderr: err.stderr?.toString() ?? '',
    };
  }
}

async function main() {
  let payload;
  try {
    payload = JSON.parse(await readStdin());
  } catch {
    process.exit(0); // No valid payload: exit quietly, don't block the flow.
  }

  const filePath = payload?.tool_input?.file_path;
  if (typeof filePath !== 'string' || filePath.length === 0) process.exit(0);

  // Skip non-JS/TS files, missing files, and files inside ignored directories.
  if (!EXTENSIONS.has(extname(filePath))) process.exit(0);
  if (!existsSync(filePath)) process.exit(0);
  if (/[/\\](node_modules|dist)[/\\]/.test(filePath)) process.exit(0);

  // 1) Lint with auto-fix; this also reports anything it cannot fix.
  const lint = run(bin('oxlint'), ['--fix', filePath]);
  // 2) Format last so the canonical layout always wins.
  const fmt = run(bin('oxfmt'), ['--write', '--no-error-on-unmatched-pattern', filePath]);

  const problems = [];

  // A non-zero oxlint exit means unfixable errors remain.
  if (lint.code !== 0 && lint.code !== 127) {
    const out = `${lint.stdout}${lint.stderr}`.trim();
    if (out) problems.push(`oxlint reported issues that need manual fixing:\n${out}`);
  }

  // oxfmt failure is usually a syntax error that prevents parsing.
  if (fmt.code !== 0 && fmt.code !== 127) {
    const out = `${fmt.stdout}${fmt.stderr}`.trim();
    if (out) problems.push(`oxfmt failed to format the file:\n${out}`);
  }

  if (problems.length > 0) {
    process.stderr.write(`[oxc hook] ${filePath}\n\n${problems.join('\n\n')}\n`);
    process.exit(2); // exit 2: feed stderr back to Claude so it can fix.
  }

  process.exit(0); // All clean: exit quietly.
}

main();
