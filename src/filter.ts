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

// mtcute reads a bare string as a username/phone; numeric (Bot-API marked) IDs
// such as -1001234567890 must be passed as numbers, otherwise it tries to
// resolve them as @usernames and fails with "Peer with username ... not found".
export function toInputPeer(peer: string): string | number {
  if (peer.startsWith('@')) return peer;
  const id = Number(peer);
  return Number.isNaN(id) ? peer : id;
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
