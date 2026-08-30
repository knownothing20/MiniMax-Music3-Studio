import { describe, expect, it, vi } from 'vitest';
import type { Song } from '../types';
import { removeLibraryItems } from './libraryRemoval';

function song(id: string, patch: Partial<Song> = {}): Song {
  return {
    id,
    title: id,
    lyrics: '',
    style: '',
    coverUrl: '',
    duration: '--:--',
    createdAt: new Date('2026-08-30T00:00:00Z'),
    tags: [],
    ...patch,
  };
}

describe('library removal routing', () => {
  it('dismisses unknown recovery cards locally and deletes real songs normally', async () => {
    const fetchImpl = vi.fn(async () => new Response(null, { status: 204 }));
    const deleteSong = vi.fn(async () => undefined);
    const result = await removeLibraryItems([
      song('recovered-card', {
        jobId: 'omnibridge-temp_123',
        submissionUnknown: true,
      }),
      song('library-song'),
    ], { fetchImpl, deleteSong });

    expect(result).toEqual({
      succeeded: ['recovered-card', 'library-song'],
      failed: [],
    });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(fetchImpl).toHaveBeenCalledWith('/v1/music/jobs/omnibridge-temp_123', {
      method: 'DELETE',
    });
    expect(deleteSong).toHaveBeenCalledTimes(1);
    expect(deleteSong).toHaveBeenCalledWith('library-song');
  });

  it('reports accurate partial counts without falling back to song DELETE', async () => {
    const fetchImpl = vi.fn(async () => new Response(JSON.stringify({
      error: 'not dismissible',
    }), {
      status: 409,
      headers: { 'Content-Type': 'application/json' },
    }));
    const deleteSong = vi.fn(async () => undefined);
    const result = await removeLibraryItems([
      song('recovered-card', {
        jobId: 'omnibridge-temp_456',
        submissionUnknown: true,
      }),
    ], { fetchImpl, deleteSong });

    expect(result).toEqual({
      succeeded: [],
      failed: ['recovered-card'],
    });
    expect(deleteSong).not.toHaveBeenCalled();
  });
});
