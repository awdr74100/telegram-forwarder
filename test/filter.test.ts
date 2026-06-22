import { describe, expect, it } from 'vitest';

import { matchesContentType, matchesPeer, toInputPeer } from '../src/filter.js';

const makeMsg = (mediaType?: string, flags?: { isAnimation?: boolean; isRound?: boolean }) => ({
  media: mediaType
    ? {
        type: mediaType,
        isAnimation: flags?.isAnimation ?? false,
        isRound: flags?.isRound ?? false,
      }
    : null,
});

describe('matchesContentType', () => {
  it('forwards everything when type includes "all"', () => {
    expect(matchesContentType(makeMsg(), ['all'])).toBe(true);
    expect(matchesContentType(makeMsg('photo'), ['all'])).toBe(true);
    expect(matchesContentType(makeMsg('video'), ['all'])).toBe(true);
  });

  it('forwards text messages when no media', () => {
    expect(matchesContentType(makeMsg(), ['text'])).toBe(true);
    expect(matchesContentType(makeMsg(), ['photo'])).toBe(false);
    expect(matchesContentType(makeMsg(), ['all', 'photo'])).toBe(true);
  });

  it('does not forward text when media present but type not matched', () => {
    expect(matchesContentType(makeMsg('photo'), ['text'])).toBe(false);
  });

  it('matches basic media types correctly', () => {
    const cases: [string, string][] = [
      ['photo', 'photo'],
      ['video', 'video'],
      ['audio', 'audio'],
      ['voice', 'voice'],
      ['sticker', 'sticker'],
      ['document', 'document'],
    ];
    for (const [mediaType, contentType] of cases) {
      expect(matchesContentType(makeMsg(mediaType), [contentType as any])).toBe(true);
    }
  });

  it('matches animation (video.isAnimation=true) against "animation" filter', () => {
    const animMsg = makeMsg('video', { isAnimation: true });
    expect(matchesContentType(animMsg, ['animation'])).toBe(true);
    expect(matchesContentType(animMsg, ['video'])).toBe(true);
    expect(matchesContentType(animMsg, ['photo'])).toBe(false);
  });

  it('matches video_note (video.isRound=true) against "video_note" filter', () => {
    const roundMsg = makeMsg('video', { isRound: true });
    expect(matchesContentType(roundMsg, ['video_note'])).toBe(true);
    expect(matchesContentType(roundMsg, ['video'])).toBe(true);
    expect(matchesContentType(roundMsg, ['photo'])).toBe(false);
  });

  it('plain video does not match animation or video_note filters', () => {
    const plainVideo = makeMsg('video');
    expect(matchesContentType(plainVideo, ['animation'])).toBe(false);
    expect(matchesContentType(plainVideo, ['video_note'])).toBe(false);
    expect(matchesContentType(plainVideo, ['video'])).toBe(true);
  });

  it('handles multiple types', () => {
    expect(matchesContentType(makeMsg('photo'), ['photo', 'video'])).toBe(true);
    expect(matchesContentType(makeMsg('audio'), ['photo', 'video'])).toBe(false);
    expect(matchesContentType(makeMsg('audio'), ['audio', 'document'])).toBe(true);
  });

  it('returns false for unsupported media types', () => {
    expect(matchesContentType(makeMsg('location'), ['photo'])).toBe(false);
    expect(matchesContentType(makeMsg('poll'), ['text'])).toBe(false);
  });
});

describe('matchesPeer', () => {
  it('matches by username with @ prefix', () => {
    const chat = { id: 1234, username: 'mychannel' };
    expect(matchesPeer(chat, '@mychannel')).toBe(true);
    expect(matchesPeer(chat, 'mychannel')).toBe(true);
    expect(matchesPeer(chat, '@other')).toBe(false);
  });

  it('matches by exact ID', () => {
    const chat = { id: 99887766, username: null };
    expect(matchesPeer(chat, '99887766')).toBe(true);
    expect(matchesPeer(chat, '11111')).toBe(false);
  });

  it('matches channel negative ID and bare numeric part', () => {
    const chat = { id: -1001234567890, username: undefined };
    expect(matchesPeer(chat, '-1001234567890')).toBe(true);
    expect(matchesPeer(chat, '1234567890')).toBe(true);
  });

  it('is case-insensitive for usernames', () => {
    const chat = { id: 1, username: 'MyChannel' };
    expect(matchesPeer(chat, '@mychannel')).toBe(true);
    expect(matchesPeer(chat, 'MYCHANNEL')).toBe(true);
  });
});

describe('toInputPeer', () => {
  it('keeps usernames as @-prefixed strings', () => {
    expect(toInputPeer('@mychannel')).toBe('@mychannel');
  });

  it('converts a marked channel ID to a number', () => {
    expect(toInputPeer('-1001727806995')).toBe(-1001727806995);
  });

  it('converts a basic-group negative ID to a number', () => {
    expect(toInputPeer('-4187363166')).toBe(-4187363166);
  });

  it('converts a positive numeric ID to a number', () => {
    expect(toInputPeer('12345')).toBe(12345);
  });
});
