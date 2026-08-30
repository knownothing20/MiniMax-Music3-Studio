import type { Song } from '../types';

const LEGACY_QUERY_KEYS = ['q', 'query', 'search'] as const;

export function readLibrarySearchQuery(search: string): string {
  const params = new URLSearchParams(search);
  for (const key of LEGACY_QUERY_KEYS) {
    const value = params.get(key)?.trim();
    if (value) return value;
  }
  return '';
}

export function resolveLegacySearchLocation(
  pathname: string,
  search: string,
): { query: string; redirectPath: string } | null {
  if (pathname !== '/search') return null;
  const query = readLibrarySearchQuery(search);
  return {
    query,
    redirectPath: query ? '/library?q=' + encodeURIComponent(query) : '/library',
  };
}

export function matchesLibrarySong(
  song: Pick<Song, 'title' | 'style' | 'lyrics' | 'tags'>,
  query: string,
): boolean {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return true;
  return [song.title, song.style, song.lyrics, ...(song.tags || [])]
    .some(value => value?.toLocaleLowerCase().includes(needle));
}
