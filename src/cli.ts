#!/usr/bin/env node
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { styleText } from 'node:util';

import {
  cancel,
  confirm,
  intro,
  isCancel,
  log,
  multiselect,
  outro,
  password,
  spinner,
  text,
} from '@clack/prompts';
import { defineCommand, runMain } from 'citty';

import { createClient } from './client.js';
import {
  clearSession,
  getConfigDir,
  hasSession,
  isConfigured,
  loadConfig,
  saveConfig,
} from './config.js';
import { Forwarder } from './forwarder.js';
import type { ContentType, ForwardGroup } from './types.js';

// Read version from package.json so it never drifts from the published package.
// Resolves to the package root in both `tsx src/cli.ts` and the bundled dist/cli.mjs.
const pkg = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf-8')) as {
  version: string;
};

const CONTENT_CHOICES: { label: string; value: ContentType }[] = [
  { label: 'All content', value: 'all' },
  { label: 'Text messages', value: 'text' },
  { label: 'Photos', value: 'photo' },
  { label: 'Videos', value: 'video' },
  { label: 'GIFs / Animations', value: 'animation' },
  { label: 'Video notes (circles)', value: 'video_note' },
  { label: 'Audio files', value: 'audio' },
  { label: 'Voice messages', value: 'voice' },
  { label: 'Documents / files', value: 'document' },
  { label: 'Stickers', value: 'sticker' },
];

interface ChannelChoice {
  // Full label shown in the picker, e.g. "My Channel (@mychannel)".
  name: string;
  // Identifier used as the stored value (@username or numeric id).
  value: string;
  // Short display name reused when summarizing a group.
  label: string;
}

// clack prompts resolve to a cancel symbol on Ctrl+C instead of throwing.
// Unwrap the value, or exit cleanly if the user cancelled.
function ensure<T>(value: T | symbol): T {
  if (isCancel(value)) {
    cancel('Cancelled.');
    process.exit(0);
  }
  return value;
}

// Connects and signs in. If the first connection hangs (usually a corrupt
// session), surface the `reset` escape hatch instead of spinning silently.
async function authenticate(client: ReturnType<typeof createClient>): Promise<void> {
  let settled = false;
  const hint = setTimeout(() => {
    if (settled) return;
    log.warn(
      'Still connecting to Telegram…\n' +
        'If this keeps hanging, your session may be corrupt. Stop (Ctrl+C) and run:\n' +
        styleText('cyan', '  telegram-forwarder reset'),
    );
  }, 15_000);

  try {
    await client.start({
      phone: async () => {
        settled = true;
        clearTimeout(hint);
        return ensure(
          await text({ message: 'Phone number (with country code, e.g. +886912345678):' }),
        );
      },
      code: async () => ensure(await text({ message: 'Verification code from Telegram:' })),
      password: async () =>
        ensure(await password({ message: '2FA password (leave blank if none):' })),
    });
  } finally {
    settled = true;
    clearTimeout(hint);
  }
}

async function fetchChannels(client: Parameters<typeof createClient>[0]): Promise<ChannelChoice[]> {
  const tg = createClient(client);
  await authenticate(tg);

  const s = spinner();
  s.start('Fetching your channels…');

  const choices: ChannelChoice[] = [];
  for await (const dialog of tg.iterDialogs()) {
    const peer = dialog.peer;
    if (peer.type !== 'chat') continue;
    const ct = peer.chatType;
    if (ct !== 'channel' && ct !== 'supergroup' && ct !== 'group' && ct !== 'gigagroup') continue;
    const identifier = peer.username ? `@${peer.username}` : String(peer.id);
    choices.push({
      name: `${peer.displayName} (${identifier})`,
      value: identifier,
      label: peer.displayName,
    });
  }

  await tg.destroy();
  s.stop(`Found ${choices.length} channel(s).`);
  return choices;
}

// ─── init ────────────────────────────────────────────────────────────────────

const init = defineCommand({
  meta: { name: 'init', description: 'Configure API credentials and authenticate your account' },
  async run() {
    const config = loadConfig();

    intro('Configure telegram-forwarder');

    const apiIdStr = ensure(
      await text({
        message: 'API ID (from https://my.telegram.org/apps):',
        initialValue: config.apiId > 0 ? String(config.apiId) : undefined,
        validate: (v) =>
          !isNaN(Number(v)) && Number(v) > 0 ? undefined : 'Must be a positive number',
      }),
    );

    const apiHash = ensure(
      await text({
        message: 'API Hash:',
        initialValue: config.apiHash || undefined,
        validate: (v) => (v && v.trim().length > 0 ? undefined : 'Required'),
      }),
    );

    config.apiId = Number(apiIdStr);
    config.apiHash = apiHash.trim();
    saveConfig(config);

    log.success('Credentials saved. Starting authentication…');

    const client = createClient(config);
    await authenticate(client);

    await client.destroy();
    log.success(`Authenticated! Session saved to: ${config.sessionPath}`);
    outro(`Next: add a group with ${styleText('cyan', 'telegram-forwarder group add')}`);
  },
});

// ─── group ───────────────────────────────────────────────────────────────────

const groupAdd = defineCommand({
  meta: { name: 'add', description: 'Add a new forwarding group by selecting from your channels' },
  async run() {
    const config = loadConfig();
    if (!isConfigured(config)) {
      log.error('Run "telegram-forwarder init" first.');
      process.exit(1);
    }

    intro('Add forwarding group');

    const channels = await fetchChannels(config);
    if (channels.length === 0) {
      cancel('No channels or groups found in your account.');
      process.exit(1);
    }

    const channelOptions = channels.map((c) => ({ value: c.value, label: c.name }));

    const sourcePeers = ensure(
      await multiselect({
        message: 'Select source channels (space to select, enter to confirm):',
        options: channelOptions,
        required: true,
      }),
    );

    const targetPeers = ensure(
      await multiselect({
        message: 'Select target channels:',
        options: channelOptions,
        required: true,
      }),
    );

    const contentTypes = ensure(
      await multiselect<ContentType>({
        message: 'What content to forward?',
        options: CONTENT_CHOICES,
        required: true,
      }),
    );

    const noAuthor = ensure(
      await confirm({
        message: 'Remove "Forwarded from" attribution?',
        initialValue: false,
      }),
    );

    const labelOf = new Map(channels.map((c) => [c.value, c.label]));
    const summarize = (peers: string[]): string => {
      const first = labelOf.get(peers[0]) ?? peers[0];
      return peers.length > 1 ? `${first} +${peers.length - 1}` : first;
    };
    const name = `${summarize(sourcePeers)} → ${summarize(targetPeers)}`;

    const newGroup: ForwardGroup = {
      id: randomUUID(),
      name,
      sourcePeers,
      targetPeers,
      contentTypes,
      noAuthor,
      enabled: true,
      createdAt: new Date().toISOString(),
    };

    config.groups.push(newGroup);
    saveConfig(config);
    outro(`Group "${newGroup.name}" added (ID: ${newGroup.id.slice(0, 8)})`);
  },
});

const groupList = defineCommand({
  meta: { name: 'list', alias: 'ls', description: 'List all forwarding groups' },
  run() {
    const { groups } = loadConfig();
    if (groups.length === 0) {
      log.info('No groups configured. Run "telegram-forwarder group add" to create one.');
      return;
    }
    for (const g of groups) {
      const status = g.enabled ? styleText('green', '✓') : styleText('red', '✗');
      const id = styleText('dim', g.id.slice(0, 8));
      console.log(`\n${status} ${styleText('bold', g.name)} ${id}`);
      console.log(`   Sources: ${styleText('cyan', g.sourcePeers.join(', '))}`);
      console.log(`   Targets: ${styleText('cyan', g.targetPeers.join(', '))}`);
      console.log(`   Types: ${g.contentTypes.join(', ')}`);
      console.log(`   Remove attribution: ${g.noAuthor ? 'yes' : 'no'}`);
    }
    console.log();
  },
});

const groupRemove = defineCommand({
  meta: {
    name: 'remove',
    alias: 'rm',
    description: 'Remove forwarding groups (select from a list)',
  },
  async run() {
    const config = loadConfig();
    if (config.groups.length === 0) {
      log.info('No groups configured.');
      return;
    }

    const ids = ensure(
      await multiselect({
        message: 'Select groups to remove (space to select, enter to confirm):',
        options: config.groups.map((g) => ({
          value: g.id,
          label: `${g.name}  (${g.contentTypes.join(', ')})`,
        })),
        required: false,
      }),
    );
    if (ids.length === 0) {
      log.info('Nothing selected.');
      return;
    }

    const ok = ensure(
      await confirm({ message: `Remove ${ids.length} group(s)?`, initialValue: false }),
    );
    if (!ok) return;

    config.groups = config.groups.filter((g) => !ids.includes(g.id));
    saveConfig(config);
    log.success(`Removed ${ids.length} group(s).`);
  },
});

const groupToggle = defineCommand({
  meta: { name: 'toggle', description: 'Enable or disable groups (checked = enabled)' },
  async run() {
    const config = loadConfig();
    if (config.groups.length === 0) {
      log.info('No groups configured.');
      return;
    }

    const enabledIds = ensure(
      await multiselect({
        message: 'Checked = enabled, unchecked = disabled:',
        options: config.groups.map((g) => ({ value: g.id, label: g.name })),
        initialValues: config.groups.filter((g) => g.enabled).map((g) => g.id),
        required: false,
      }),
    );

    for (const g of config.groups) g.enabled = enabledIds.includes(g.id);
    saveConfig(config);
    log.success('Group status updated.');
  },
});

const group = defineCommand({
  meta: { name: 'group', description: 'Manage forwarding groups' },
  subCommands: { add: groupAdd, list: groupList, remove: groupRemove, toggle: groupToggle },
});

// ─── config ──────────────────────────────────────────────────────────────────

const configCmd = defineCommand({
  meta: { name: 'config', description: 'Show or update rate limit settings' },
  args: {
    delay: {
      type: 'string',
      alias: 'd',
      description: 'Base delay between forwards in milliseconds',
    },
    jitter: { type: 'string', alias: 'j', description: 'Random extra delay range in milliseconds' },
    retries: { type: 'string', alias: 'r', description: 'Max retries on FloodWait error' },
  },
  run({ args }) {
    const config = loadConfig();
    if (args.delay) config.rateLimit.delayMs = Number(args.delay);
    if (args.jitter) config.rateLimit.jitterMs = Number(args.jitter);
    if (args.retries) config.rateLimit.maxRetries = Number(args.retries);
    if (args.delay || args.jitter || args.retries) {
      saveConfig(config);
      log.success('Rate limit config updated.');
    }
    console.log('\nRate limit settings:');
    console.log(`  Base delay:  ${config.rateLimit.delayMs}ms`);
    console.log(`  Jitter:      0–${config.rateLimit.jitterMs}ms`);
    console.log(`  Max retries: ${config.rateLimit.maxRetries}`);
    console.log(`\nConfig directory: ${getConfigDir()}`);
  },
});

// ─── reset ───────────────────────────────────────────────────────────────────

const reset = defineCommand({
  meta: { name: 'reset', description: 'Clear the saved login session and force re-authentication' },
  async run() {
    const { sessionPath } = loadConfig();
    if (!hasSession(sessionPath)) {
      log.info('No session found — nothing to reset.');
      return;
    }

    const ok = ensure(
      await confirm({
        message:
          'This clears your login session and you will need to authenticate again. Continue?',
        initialValue: false,
      }),
    );
    if (!ok) return;

    const removed = clearSession(sessionPath);
    log.success(`Session cleared (${removed.length} file(s) removed).`);
    outro(`Run ${styleText('cyan', 'telegram-forwarder init')} to log in again.`);
  },
});

// ─── start ───────────────────────────────────────────────────────────────────

const start = defineCommand({
  meta: { name: 'start', description: 'Start monitoring and forwarding' },
  async run() {
    const config = loadConfig();

    if (!isConfigured(config)) {
      log.error('Run "telegram-forwarder init" first.');
      process.exit(1);
    }

    const activeGroups = config.groups.filter((g) => g.enabled);
    if (activeGroups.length === 0) {
      log.error('No enabled groups. Use "telegram-forwarder group add" to add one.');
      process.exit(1);
    }

    log.info(styleText('blue', 'Starting Telegram Forwarder…'));
    log.message(styleText('dim', `Active groups: ${activeGroups.map((g) => g.name).join(', ')}`));

    const client = createClient(config);

    await authenticate(client);

    const forwarder = new Forwarder(client, config);
    forwarder.start();

    // @mtcute/node registers its own exit hook that synchronously flushes and
    // closes the SQLite storage on SIGINT/exit. Calling client.destroy() here
    // would race that teardown and write to an already-closed DB (crash), so we
    // just stop dispatching and exit — mtcute persists the session on its way out.
    process.once('SIGINT', () => {
      const dropped = forwarder.pending;
      log.warn(
        dropped > 0 ? `Shutting down… dropping ${dropped} queued forward(s).` : 'Shutting down…',
      );
      forwarder.stop();
      process.exit(0);
    });

    log.success('Listening for messages. Ctrl+C to stop.');
  },
});

// ─── main ──────────────────────────────────────────────────────────────────��─

const main = defineCommand({
  meta: {
    name: 'telegram-forwarder',
    version: pkg.version,
    description: 'Monitor Telegram channels and forward specific content',
  },
  subCommands: { init, group, config: configCmd, reset, start },
});

runMain(main);
