export type ContentType =
  | 'all'
  | 'text'
  | 'photo'
  | 'video'
  | 'animation'
  | 'video_note'
  | 'audio'
  | 'voice'
  | 'document'
  | 'sticker';

export interface ForwardGroup {
  id: string;
  name: string;
  sourcePeers: string[];
  targetPeers: string[];
  contentTypes: ContentType[];
  // Case-insensitive substring filters on the message text/caption. Optional so
  // configs written before this feature still load. Empty/absent = no filter;
  // exclude wins over include. See matchesKeywords in filter.ts.
  includeKeywords?: string[];
  excludeKeywords?: string[];
  noAuthor: boolean;
  enabled: boolean;
  createdAt: string;
}

export interface RateLimitConfig {
  delayMs: number;
  jitterMs: number;
  maxRetries: number;
}

export interface AppConfig {
  apiId: number;
  apiHash: string;
  sessionPath: string;
  groups: ForwardGroup[];
  rateLimit: RateLimitConfig;
}
