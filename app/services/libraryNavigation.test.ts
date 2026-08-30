import { describe, expect, it } from 'vitest';
import {
  matchesLibrarySong,
  readLibrarySearchQuery,
  resolveLegacySearchLocation,
} from './libraryNavigation';

const song = {
  title: 'Night Train',
  style: 'Global Metadata: synthwave',
  lyrics: '[chorus]\n回到城市',
  tags: ['retro', 'dreamy'],
};

describe('legacy search navigation', () => {
  it('redirects /search to the library and preserves common legacy query keys', () => {
    expect(resolveLegacySearchLocation('/search', '?q=night')).toEqual({
      query: 'night',
      redirectPath: '/library?q=night',
    });
    expect(resolveLegacySearchLocation('/search', '?query=%E5%A4%9C%E8%BD%A6')).toEqual({
      query: '夜车',
      redirectPath: '/library?q=%E5%A4%9C%E8%BD%A6',
    });
    expect(resolveLegacySearchLocation('/library', '?q=night')).toBeNull();
    expect(readLibrarySearchQuery('?search=retro')).toBe('retro');
  });

  it('keeps library search across title, structured description, lyrics, and tags', () => {
    expect(matchesLibrarySong(song, 'train')).toBe(true);
    expect(matchesLibrarySong(song, 'synthwave')).toBe(true);
    expect(matchesLibrarySong(song, '城市')).toBe(true);
    expect(matchesLibrarySong(song, 'dreamy')).toBe(true);
    expect(matchesLibrarySong(song, 'classical')).toBe(false);
  });
});
