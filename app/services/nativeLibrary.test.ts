import { describe, expect, it } from 'vitest';
import { API_BASE } from './apiBase';
import { mapNativeLibrarySong } from './nativeLibrary';

describe('mapNativeLibrarySong', () => {
  it('uses the native media route only when the library record has audio', () => {
    const withAudio = mapNativeLibrarySong({
      id: 'song id/with space',
      title: 'Native track',
      audio_path: 'outputs/native-track.mp3',
      caption: 'warm synthpop',
      lyrics: 'one line',
      metadata: {},
      generation_settings: {},
      engine_id: 'native-engine',
      profile_id: 'recommended',
      replay_request: { caption: 'warm synthpop' },
      audio_codes: [1, 2, 3],
      created_at: '1',
    });
    const withoutAudio = mapNativeLibrarySong({
      id: 'silent-song',
      title: 'No audio yet',
      audio_path: null,
      caption: '',
      lyrics: '',
      metadata: {},
      generation_settings: {},
      engine_id: 'native-engine',
      created_at: '1',
    });

    expect(withAudio.audioUrl).toBe(`${API_BASE}/v1/library/media/song%20id%2Fwith%20space`);
    expect(withAudio.nativeReplayAvailable).toBe(true);
    expect(withoutAudio.audioUrl).toBeUndefined();
    expect(withoutAudio.nativeReplayAvailable).toBe(false);
  });
});
