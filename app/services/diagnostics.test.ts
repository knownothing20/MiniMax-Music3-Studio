import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Song } from '../types';
import { diagnosticDownloadPath, downloadSongDiagnostics } from './diagnostics';

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('song diagnostic download', () => {
  it('uses the active job id for queued or failed songs and clicks a ZIP download', async () => {
    const song = { id: 'temporary-row', jobId: 'job id/one' } as Song;
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      headers: new Headers({ 'content-disposition': 'attachment; filename="music-maker-diagnostics-jobidone.zip"' }),
      blob: async () => new Blob(['zip-bytes'], { type: 'application/zip' }),
    });
    vi.stubGlobal('fetch', fetchMock);
    const createObjectURL = vi.fn(() => 'blob:diagnostic');
    const revokeObjectURL = vi.fn();
    vi.stubGlobal('URL', Object.assign(URL, { createObjectURL, revokeObjectURL }));
    let downloaded = '';
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function () {
      downloaded = this.download;
    });

    expect(diagnosticDownloadPath(song)).toBe('/v1/library/songs/job%20id%2Fone/diagnostics');
    await downloadSongDiagnostics(song);

    expect(fetchMock).toHaveBeenCalledWith('/v1/library/songs/job%20id%2Fone/diagnostics');
    expect(downloaded).toBe('music-maker-diagnostics-jobidone.zip');
    expect(createObjectURL).toHaveBeenCalledOnce();
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:diagnostic');
  });

  it('surfaces the backend error without creating a download', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({
      ok: false,
      status: 404,
      headers: new Headers(),
      json: async () => ({ error: 'Song or music job not found' }),
    }));
    await expect(downloadSongDiagnostics({ id: 'missing' })).rejects.toThrow('Song or music job not found');
  });
});
