import type { ContentType } from './types.js';

interface VideoLike {
  type: 'video';
  isAnimation: boolean;
  isRound: boolean;
}

interface MediaLike {
  type: string;
}

interface MessageLike {
  media?: MediaLike | null;
  // Message text, or caption for media messages. Empty string when neither.
  text?: string;
}

export function matchesContentType(msg: MessageLike, types: ContentType[]): boolean {
  if (types.includes('all')) return true;

  const media = msg.media;
  if (!media) return types.includes('text');

  switch (media.type) {
    case 'photo':
      return types.includes('photo');

    case 'video': {
      const v = media as VideoLike;
      if (v.isAnimation) return types.includes('animation') || types.includes('video');
      if (v.isRound) return types.includes('video_note') || types.includes('video');
      return types.includes('video');
    }

    case 'audio':
      return types.includes('audio');

    case 'voice':
      return types.includes('voice');

    case 'sticker':
      return types.includes('sticker');

    case 'document':
      return types.includes('document');

    default:
      return false;
  }
}

// Decide whether a message's text passes a group's keyword filters. Matching is
// case-insensitive substring. An empty (or absent) list means "no constraint":
// no exclude list blocks nothing, no include list lets any text through.
// Exclude takes precedence — a message hit by both is dropped. A media-only
// message has empty text, so it can never satisfy a non-empty include list.
export function matchesKeywords(
  msg: MessageLike,
  include: string[] = [],
  exclude: string[] = [],
): boolean {
  if (include.length === 0 && exclude.length === 0) return true;

  const text = (msg.text ?? '').toLowerCase();

  if (exclude.some((kw) => text.includes(kw.toLowerCase()))) return false;
  if (include.length > 0) return include.some((kw) => text.includes(kw.toLowerCase()));
  return true;
}

// mtcute reads a bare string as a username/phone; numeric (Bot-API marked) IDs
// such as -1001234567890 must be passed as numbers, otherwise it tries to
// resolve them as @usernames and fails with "Peer with username ... not found".
export function toInputPeer(peer: string): string | number {
  if (peer.startsWith('@')) return peer;
  const id = Number(peer);
  return Number.isNaN(id) ? peer : id;
}

// Swap one stored peer identifier for another across a peer list, preserving
// order and dropping duplicates. Used when a basic group is upgraded to a
// supergroup: its id changes (e.g. -4187363166 → -1004187363166) and the group
// config must follow without introducing a duplicate target.
export function replacePeer(peers: string[], from: string, to: string): string[] {
  const out: string[] = [];
  for (const peer of peers) {
    const next = peer === from ? to : peer;
    if (!out.includes(next)) out.push(next);
  }
  return out;
}

export function matchesPeer(
  chat: { id: number | string; username?: string | null },
  peer: string,
): boolean {
  const clean = peer.trim().replace(/^@/, '').toLowerCase();

  if (chat.username?.toLowerCase() === clean) return true;

  const id = String(chat.id);
  if (id === clean) return true;
  // Telegram channel IDs are like -1001234567890; users often write just 1234567890.
  if (id.startsWith('-100') && id.slice(4) === clean) return true;

  return false;
}
