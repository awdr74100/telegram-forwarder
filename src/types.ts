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
