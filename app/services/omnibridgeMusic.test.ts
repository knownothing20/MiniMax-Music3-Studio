import { describe, expect, it, vi } from 'vitest';
import type { Music3Job } from '../types';
import {
  OMNIBRIDGE_CASE_STORAGE_KEY,
  OmniBridgeApiError,
  createOmniBridgeCaseIntent,
  getOmniBridgeMusicJob,
  listRecoverableOmniBridgeJobs,
  loadOmniBridgeCaseIntent,
  readOmniBridgeIntegrationStatus,
  sha256Digest,
  submitOmniBridgeMusicJobOnce,
  updateOmniBridgeCaseIntent,
  verifyImportedAudioArtifact,
} from './omnibridgeMusic';

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), { status, headers: { 'content-type': 'application/json' } });
}

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, value => value.toString(16).padStart(2, '0')).join('');
}

function job(status: Music3Job['status'] = 'queued'): Music3Job {
  return {
    id: 'omnibridge-api-case-fixed',
    engine_id: 'omnibridge',
    dispatch: 'omni_bridge',
    status,
    phase: status,
    message: status,
    caption: 'caption',
    lyrics: 'lyrics',
    duration_seconds: 30,
    generation_settings: {},
  };
}

class MemoryStorage {
  private values = new Map<string, string>();
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  setItem(key: string, value: string): void { this.values.set(key, value); }
  removeItem(key: string): void { this.values.delete(key); }
}

describe('OmniBridge Music API case client', () => {
  it('sanitizes the read-only integration status and never exposes connection details', async () => {
    const fetchImpl = vi.fn(async () => jsonResponse({
      schema: 'music-maker.omnibridge-integration-status.v2',
      configured: true,
      contract_client: 'temporary-rust-adapter',
      execution_target: 'omnibridge',
      music_route: 'route:music:cloud',
      operation: 'audio.music.generate',
      route_readiness: 'ready',
      real_generation_verified: false,
      diagnostic_status: 'ready',
      contract_status: 'verified',
      contract_verified: true,
      route_resolution_verified: true,
      provider_resolution_verified: true,
      base_url: 'http://secret.internal',
      platform_id: 'private-platform',
    }));

    const status = await readOmniBridgeIntegrationStatus(fetchImpl);

    expect(status).toMatchObject({
      configured: true,
      executionTarget: 'omnibridge',
      musicRoute: 'route:music:cloud',
      contractVerified: true,
      routeResolutionVerified: true,
      providerResolutionVerified: true,
    });
    expect(status).not.toHaveProperty('base_url');
    expect(status).not.toHaveProperty('platform_id');
  });

  it('performs exactly one POST with the stable client request id and has no retry path', async () => {
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => jsonResponse(job(), 202));

    const result = await submitOmniBridgeMusicJobOnce({ title: 'Case', caption: 'Synth pop', lyrics: '[Verse]\nHello' }, 'api-case-fixed', fetchImpl);

    expect(result.id).toBe('omnibridge-api-case-fixed');
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    const [url, init] = fetchImpl.mock.calls[0]!;
    expect(url).toBe('/v1/music/jobs');
    expect(init?.method).toBe('POST');
    expect(JSON.parse(String(init?.body))).toMatchObject({
      client_request_id: 'api-case-fixed',
      duration_seconds: 30,
      output_format: 'wav',
    });
  });

  it('marks transport failure as unknown instead of replaying POST', async () => {
    const fetchImpl = vi.fn(async () => { throw new TypeError('connection closed'); });

    await expect(submitOmniBridgeMusicJobOnce({ title: '', caption: 'c', lyrics: 'l' }, 'api-case-fixed', fetchImpl))
      .rejects.toMatchObject({ responseKnown: false });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it('treats invalid 2xx submit JSON as unknown but an explicit 4xx as rejected', async () => {
    const invalidAccepted = vi.fn(async () => new Response('not-json', { status: 202 }));
    await expect(submitOmniBridgeMusicJobOnce({ title: '', caption: 'c', lyrics: 'l' }, 'api-case-fixed', invalidAccepted))
      .rejects.toMatchObject({ responseKnown: false, status: 202 });
    expect(invalidAccepted).toHaveBeenCalledTimes(1);

    const rejected = vi.fn(async () => jsonResponse({ error: 'lyrics refused' }, 400));
    await expect(submitOmniBridgeMusicJobOnce({ title: '', caption: 'c', lyrics: 'l' }, 'api-case-fixed', rejected))
      .rejects.toMatchObject({ responseKnown: true, status: 400 });
    expect(rejected).toHaveBeenCalledTimes(1);
  });

  it('uses GET-only recovery and filters the list to OmniBridge jobs', async () => {
    const fetchImpl = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      expect(init?.method).not.toBe('POST');
      if (String(input) === '/v1/music/jobs') return jsonResponse([job(), { ...job(), id: 'local-job' }]);
      return jsonResponse(job('running'));
    });

    const recovered = await getOmniBridgeMusicJob('omnibridge-api-case-fixed', fetchImpl);
    const listed = await listRecoverableOmniBridgeJobs(fetchImpl);

    expect(recovered.status).toBe('running');
    expect(listed.map(value => value.id)).toEqual(['omnibridge-api-case-fixed']);
    expect(fetchImpl).toHaveBeenCalledTimes(2);
  });

  it('persists and reads back intent before the attempted marker is written', () => {
    const storage = new MemoryStorage();
    const intent = createOmniBridgeCaseIntent(
      { title: 'Case', caption: 'caption', lyrics: 'lyrics' },
      storage,
      () => '00000000-0000-4000-8000-000000000000',
      () => new Date('2026-08-28T00:00:00.000Z'),
    );

    expect(intent.postAttempted).toBe(false);
    expect(storage.getItem(OMNIBRIDGE_CASE_STORAGE_KEY)).toContain('intent_persisted');
    const attempted = updateOmniBridgeCaseIntent(intent, { postAttempted: true, submitOutcome: 'attempted' }, storage);
    expect(loadOmniBridgeCaseIntent(storage)).toEqual(attempted);
  });

  it('prefers WebCrypto SHA-256 when subtle is available', async () => {
    const subtleDigest = vi.fn(async (_algorithm: AlgorithmIdentifier, _data: BufferSource) => new Uint8Array(32).fill(0xab).buffer);
    const subtle = { digest: subtleDigest } as unknown as Pick<SubtleCrypto, 'digest'>;

    const digest = await sha256Digest(new Uint8Array([1, 2, 3]), subtle);

    expect(bytesToHex(digest)).toBe('ab'.repeat(32));
    expect(subtleDigest).toHaveBeenCalledTimes(1);
    expect(subtleDigest.mock.calls[0]?.[0]).toBe('SHA-256');
  });

  it('uses the pure SHA-256 fallback without subtle and still rejects mismatched Artifact evidence', async () => {
    const bytes = new TextEncoder().encode('abc');
    const expected = 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad';
    expect(bytesToHex(await sha256Digest(bytes, null))).toBe(expected);
    expect(bytesToHex(await sha256Digest(new Uint8Array(), null))).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    const boundary = new TextEncoder().encode('abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq');
    expect(bytesToHex(await sha256Digest(boundary, null))).toBe('248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1');

    const complete: Music3Job = {
      ...job('completed'),
      song: {
        id: 'fallback-song',
        audio_url: '/v1/library/media/fallback-song',
        song: { id: 'fallback-song', metadata: { artifact_sha256: expected } },
      },
    };
    const fetchImpl = vi.fn(async () => new Response(bytes, {
      status: 200,
      headers: { 'content-type': 'audio/mpeg', 'content-length': String(bytes.byteLength) },
    }));
    const fallbackDigest = (value: Uint8Array) => sha256Digest(value, null);

    await expect(verifyImportedAudioArtifact(complete, fetchImpl, fallbackDigest))
      .resolves.toMatchObject({ sha256: expected, expectedSha256: expected, bytes: 3 });

    if (complete.song?.song?.metadata) {
      complete.song.song.metadata.artifact_sha256 = '00'.repeat(32);
    }
    await expect(verifyImportedAudioArtifact(complete, fetchImpl, fallbackDigest))
      .rejects.toThrow('SHA-256');
  });

  it('verifies protected imported audio against the persisted Artifact SHA-256 evidence', async () => {
    const expected = 'ab'.repeat(32);
    const complete: Music3Job = {
      ...job('completed'),
      song: {
        id: 'song-1',
        audio_url: '/v1/library/media/song-1',
        song: { id: 'song-1', metadata: { artifact_sha256: expected } },
      },
    };
    const bytes = new Uint8Array([1, 2, 3, 4]);
    const fetchImpl = vi.fn(async () => new Response(bytes, {
      status: 200,
      headers: { 'content-type': 'audio/mpeg', 'content-length': String(bytes.byteLength) },
    }));
    const digest = vi.fn(async () => new Uint8Array(32).fill(0xab));

    const artifact = await verifyImportedAudioArtifact(complete, fetchImpl, digest);

    expect(artifact).toMatchObject({ songId: 'song-1', sha256: expected, expectedSha256: expected, bytes: 4, contentType: 'audio/mpeg' });
    expect(fetchImpl).toHaveBeenCalledWith('/v1/library/media/song-1');
  });

  it('fails closed when completed Artifact evidence is missing or mismatched', async () => {
    await expect(verifyImportedAudioArtifact(job('completed'), vi.fn())).rejects.toBeInstanceOf(OmniBridgeApiError);

    const complete: Music3Job = {
      ...job('completed'),
      song: {
        id: 'song-1',
        audio_url: '/v1/library/media/song-1',
        song: { id: 'song-1', metadata: { artifact_sha256: 'ab'.repeat(32) } },
      },
    };
    const fetchImpl = vi.fn(async () => new Response(new Uint8Array([1]), {
      status: 200,
      headers: { 'content-type': 'audio/mpeg', 'content-length': '1' },
    }));
    await expect(verifyImportedAudioArtifact(complete, fetchImpl, async () => new Uint8Array(32).fill(0xcd)))
      .rejects.toThrow('SHA-256');
  });

  it('accepts chunked audio without Content-Length and rejects a malformed length header', async () => {
    const expected = 'ab'.repeat(32);
    const complete: Music3Job = {
      ...job('completed'),
      song: {
        id: 'song-1',
        audio_url: '/v1/library/media/song-1',
        song: { id: 'song-1', metadata: { artifact_sha256: expected } },
      },
    };
    const bytes = new Uint8Array([1, 2]);
    const chunked = vi.fn(async () => new Response(bytes, { headers: { 'content-type': 'audio/mpeg' } }));
    await expect(verifyImportedAudioArtifact(complete, chunked, async () => new Uint8Array(32).fill(0xab)))
      .resolves.toMatchObject({ bytes: 2 });

    const malformed = vi.fn(async () => new Response(bytes, {
      headers: { 'content-type': 'audio/mpeg', 'content-length': 'two' },
    }));
    await expect(verifyImportedAudioArtifact(complete, malformed, async () => new Uint8Array(32).fill(0xab)))
      .rejects.toThrow('Content-Length');
  });
});
