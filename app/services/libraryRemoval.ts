import type { Song } from '../types';
import { deleteNativeSong } from './nativeLibrary';

type FetchLike = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export interface LibraryRemovalResult {
  succeeded: string[];
  failed: string[];
}

export interface LibraryRemovalDependencies {
  fetchImpl?: FetchLike;
  deleteSong?: (songId: string) => Promise<void>;
}

async function responseError(response: Response): Promise<string> {
  const body = await response.json().catch(() => ({})) as { error?: string; message?: string };
  return body.message || body.error || `Recovery-card dismissal failed (${response.status})`;
}

export async function dismissUnknownMusicJob(
  jobId: string,
  fetchImpl: FetchLike = window.fetch.bind(window),
): Promise<void> {
  const response = await fetchImpl(`/v1/music/jobs/${encodeURIComponent(jobId)}`, {
    method: 'DELETE',
  });
  if (!response.ok) throw new Error(await responseError(response));
}

export async function removeLibraryItems(
  songs: Song[],
  dependencies: LibraryRemovalDependencies = {},
): Promise<LibraryRemovalResult> {
  const fetchImpl = dependencies.fetchImpl ?? window.fetch.bind(window);
  const deleteSong = dependencies.deleteSong ?? deleteNativeSong;
  const succeeded: string[] = [];
  const failed: string[] = [];

  for (const song of songs) {
    try {
      if (song.submissionUnknown) {
        if (!song.jobId) throw new Error('Unknown submission has no durable job ID.');
        await dismissUnknownMusicJob(song.jobId, fetchImpl);
      } else {
        await deleteSong(song.id);
      }
      succeeded.push(song.id);
    } catch (error) {
      console.error('Failed to remove library item:', error);
      failed.push(song.id);
    }
  }

  return { succeeded, failed };
}
