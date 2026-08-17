import { Song } from '../types';

interface NativeLibrarySong {
  id: string;
  title: string;
  audio_path?: string | null;
  caption: string;
  lyrics: string;
  metadata?: Record<string, unknown> | null;
  generation_settings?: Record<string, unknown> | null;
  engine_id: string;
  profile_id?: string | null;
  replay_request?: unknown | null;
  audio_codes?: unknown | null;
  created_at: string;
}

interface NativePlaylist {
  id: string;
  name: string;
  description: string | null;
  song_ids: string[];
  created_at: string;
  updated_at: string;
}

export interface NativeSongUpdate {
  title: string;
  audio_path?: string | null;
  caption?: string;
  lyrics?: string;
  metadata?: Record<string, unknown> | null;
  generation_settings?: Record<string, unknown> | null;
  engine_id?: string;
  profile_id?: string | null;
  replay_request?: Record<string, unknown> | null;
  audio_codes?: unknown;
  source?: string;
}

function nativeDate(value: string): Date {
  const epochSeconds = Number(value);
  return Number.isFinite(epochSeconds) && epochSeconds > 0 ? new Date(epochSeconds * 1000) : new Date(value);
}

function numberMetadata(metadata: Record<string, unknown> | null | undefined, key: string): number | undefined {
  const value = metadata?.[key];
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function stringMetadata(metadata: Record<string, unknown> | null | undefined, key: string): string | undefined {
  const value = metadata?.[key];
  return typeof value === 'string' ? value : undefined;
}

export function mapNativeLibrarySong(song: NativeLibrarySong): Song {
  const metadata = song.metadata ?? {};
  const tags = Array.isArray(metadata.tags) ? metadata.tags.filter((tag): tag is string => typeof tag === 'string') : [];

  return {
    id: song.id,
    title: song.title,
    lyrics: song.lyrics,
    style: song.caption,
    coverUrl: '',
    duration: (() => {
      const seconds = numberMetadata(metadata, 'duration_seconds') ?? numberMetadata(metadata, 'duration');
      return seconds && seconds > 0 ? `${Math.floor(seconds / 60)}:${String(Math.floor(seconds % 60)).padStart(2, '0')}` : '0:00';
    })(),
    createdAt: nativeDate(song.created_at),
    tags,
    audioUrl: song.audio_path ? `/v1/library/media/${encodeURIComponent(song.id)}` : undefined,
    isPublic: false,
    ditModel: song.engine_id,
    lmModel: song.profile_id || undefined,
    bpm: numberMetadata(metadata, 'bpm'),
    keyScale: stringMetadata(metadata, 'key_scale') ?? stringMetadata(metadata, 'keyScale'),
    timeSignature: stringMetadata(metadata, 'time_signature') ?? stringMetadata(metadata, 'timeSignature'),
    generationParams: song.generation_settings ?? undefined,
    nativeReplayAvailable: Boolean(song.replay_request && song.audio_codes),
  };
}

export async function loadNativeLibrarySongs(): Promise<Song[]> {
  const response = await fetch('/v1/library/songs');
  if (!response.ok) {
    throw new Error(`Native library request failed (${response.status})`);
  }
  const songs: NativeLibrarySong[] = await response.json();
  return songs.map(mapNativeLibrarySong);
}

export function mapNativePlaylist(playlist: NativePlaylist): import('../types').Playlist {
  return {
    id: playlist.id,
    name: playlist.name,
    description: playlist.description || undefined,
    songIds: playlist.song_ids,
    created_at: playlist.created_at,
    song_count: playlist.song_ids.length,
    isPublic: false,
  };
}

export async function loadNativePlaylists(): Promise<import('../types').Playlist[]> {
  const response = await fetch('/v1/library/playlists');
  if (!response.ok) throw new Error(`Native playlists request failed (${response.status})`);
  const playlists: NativePlaylist[] = await response.json();
  return playlists.map(mapNativePlaylist);
}

export async function getNativePlaylist(id: string): Promise<import('../types').Playlist | null> {
  const response = await fetch(`/v1/library/playlists/${encodeURIComponent(id)}`);
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(`Native playlist request failed (${response.status})`);
  return mapNativePlaylist(await response.json());
}

export async function createNativePlaylist(name: string, description: string, songIds: string[] = []): Promise<import('../types').Playlist> {
  const response = await fetch('/v1/library/playlists', {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, description: description || null, song_ids: songIds }),
  });
  if (!response.ok) throw new Error(`Native playlist creation failed (${response.status})`);
  return mapNativePlaylist(await response.json());
}

export async function updateNativePlaylist(id: string, playlist: import('../types').Playlist, songIds = playlist.songIds || []): Promise<import('../types').Playlist> {
  const response = await fetch(`/v1/library/playlists/${encodeURIComponent(id)}`, {
    method: 'PUT', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name: playlist.name, description: playlist.description || null, song_ids: songIds }),
  });
  if (!response.ok) throw new Error(`Native playlist update failed (${response.status})`);
  return mapNativePlaylist(await response.json());
}

export async function deleteNativePlaylist(id: string): Promise<void> {
  const response = await fetch(`/v1/library/playlists/${encodeURIComponent(id)}`, { method: 'DELETE' });
  if (!response.ok) throw new Error(`Native playlist deletion failed (${response.status})`);
}

export async function updateNativeSong(existing: Song, update: Partial<NativeSongUpdate>): Promise<Song> {
  const response = await fetch(`/v1/library/songs/${encodeURIComponent(existing.id)}`, {
    method: 'PUT', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      title: update.title ?? existing.title,
      audio_path: existing.audioUrl ? undefined : null,
      caption: update.caption ?? existing.style,
      lyrics: update.lyrics ?? existing.lyrics,
      metadata: update.metadata ?? { tags: existing.tags, duration: parseDuration(existing.duration), bpm: existing.bpm, keyScale: existing.keyScale, timeSignature: existing.timeSignature },
      generation_settings: update.generation_settings ?? existing.generationParams ?? {},
      engine_id: update.engine_id ?? existing.ditModel ?? 'minimaxmusic-cpp',
      profile_id: update.profile_id ?? existing.lmModel ?? null,
      replay_request: update.replay_request ?? null,
      audio_codes: update.audio_codes ?? null,
      source: update.source ?? 'local_generation',
    }),
  });
  if (!response.ok) throw new Error(`Native song update failed (${response.status})`);
  return mapNativeLibrarySong(await response.json());
}

export async function deleteNativeSong(id: string): Promise<void> {
  const response = await fetch(`/v1/library/songs/${encodeURIComponent(id)}`, { method: 'DELETE' });
  if (!response.ok) throw new Error(`Native song deletion failed (${response.status})`);
}

function parseDuration(value: string): number | undefined {
  const match = /^(\d+):(\d{2})$/.exec(value);
  return match ? Number(match[1]) * 60 + Number(match[2]) : undefined;
}
