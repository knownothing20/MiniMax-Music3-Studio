import type { Music3Job } from '../types';

export const OMNIBRIDGE_CASE_SCHEMA = 'music-maker.omnibridge-api-case.v1';
export const OMNIBRIDGE_CASE_STORAGE_KEY = 'music3.omnibridge.api-case.v1';
const INTEGRATION_SCHEMAS = new Set([
  'music-maker.omnibridge-integration-status.v1',
  'music-maker.omnibridge-integration-status.v2',
]);
const MAX_AUDIO_BYTES = 128 * 1024 * 1024;

export type FetchLike = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export interface OmniBridgeIntegrationStatus {
  schema: string;
  configured: boolean;
  contractClient: string;
  executionTarget: string;
  musicRoute: string | null;
  operation: string | null;
  routeReadiness: string;
  realGenerationVerified: boolean;
  diagnosticStatus: string;
  contractStatus: string;
  contractVerified: boolean;
  routeResolutionVerified: boolean;
  providerResolutionVerified: boolean;
  error: string | null;
}

export type CaseSubmitOutcome = 'intent_persisted' | 'attempted' | 'accepted' | 'unknown' | 'rejected' | 'completed';

export interface OmniBridgeCaseIntent {
  schema: typeof OMNIBRIDGE_CASE_SCHEMA;
  clientRequestId: string;
  jobId: string;
  title: string;
  caption: string;
  lyrics: string;
  postAttempted: boolean;
  submitOutcome: CaseSubmitOutcome;
  createdAt: string;
  updatedAt: string;
}

export interface OmniBridgeCaseInput {
  title: string;
  caption: string;
  lyrics: string;
}

export interface VerifiedAudioArtifact {
  blob: Blob;
  songId: string;
  audioUrl: string;
  sha256: string;
  expectedSha256: string;
  bytes: number;
  contentType: string;
}

export class OmniBridgeApiError extends Error {
  readonly status: number | null;
  readonly responseKnown: boolean;

  constructor(message: string, options: { status?: number | null; responseKnown?: boolean } = {}) {
    super(message);
    this.name = 'OmniBridgeApiError';
    this.status = options.status ?? null;
    this.responseKnown = options.responseKnown ?? false;
  }
}

function objectValue(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new OmniBridgeApiError('The Studio returned an invalid JSON object.', { responseKnown: true });
  }
  return value as Record<string, unknown>;
}

async function jsonBody(response: Response): Promise<unknown> {
  try {
    return await response.json();
  } catch {
    throw new OmniBridgeApiError(`The Studio returned invalid JSON (${response.status}).`, {
      status: response.status,
      responseKnown: true,
    });
  }
}

function errorMessage(value: unknown, fallback: string): string {
  if (value && typeof value === 'object') {
    const body = value as Record<string, unknown>;
    for (const key of ['message', 'error']) {
      if (typeof body[key] === 'string' && body[key].trim()) return body[key].trim();
    }
  }
  return fallback;
}

function parseMusicJob(value: unknown): Music3Job {
  const body = objectValue(value);
  if (typeof body.id !== 'string' || !body.id.trim()) {
    throw new OmniBridgeApiError('Music job response has no recovery handle.', { responseKnown: true });
  }
  const statuses = ['queued', 'running', 'completed', 'failed', 'cancelled', 'unknown'];
  if (typeof body.status !== 'string' || !statuses.includes(body.status)) {
    throw new OmniBridgeApiError('Music job response has an unsupported status.', { responseKnown: true });
  }
  return body as unknown as Music3Job;
}

export async function readOmniBridgeIntegrationStatus(fetchImpl: FetchLike = window.fetch.bind(window)): Promise<OmniBridgeIntegrationStatus> {
  const response = await fetchImpl('/v1/integrations/omnibridge');
  const raw = await jsonBody(response);
  if (!response.ok) {
    throw new OmniBridgeApiError(errorMessage(raw, `OmniBridge status request failed (${response.status}).`), {
      status: response.status,
      responseKnown: true,
    });
  }
  const body = objectValue(raw);
  if (typeof body.schema !== 'string' || !INTEGRATION_SCHEMAS.has(body.schema) || typeof body.configured !== 'boolean') {
    throw new OmniBridgeApiError('OmniBridge integration status failed schema validation.', { responseKnown: true });
  }
  return {
    schema: body.schema,
    configured: body.configured,
    contractClient: typeof body.contract_client === 'string' ? body.contract_client : 'unknown',
    executionTarget: typeof body.execution_target === 'string' ? body.execution_target : 'unknown',
    musicRoute: typeof body.music_route === 'string' ? body.music_route : null,
    operation: typeof body.operation === 'string' ? body.operation : null,
    routeReadiness: typeof body.route_readiness === 'string' ? body.route_readiness : 'unknown',
    realGenerationVerified: body.real_generation_verified === true,
    diagnosticStatus: typeof body.diagnostic_status === 'string' ? body.diagnostic_status : 'unavailable',
    contractStatus: typeof body.contract_status === 'string' ? body.contract_status : 'unverified',
    contractVerified: body.contract_verified === true,
    routeResolutionVerified: body.route_resolution_verified === true,
    providerResolutionVerified: body.provider_resolution_verified === true,
    error: typeof body.error === 'string' && body.error.trim() ? body.error.trim() : null,
  };
}

export async function submitOmniBridgeMusicJobOnce(
  input: OmniBridgeCaseInput,
  clientRequestId: string,
  fetchImpl: FetchLike = window.fetch.bind(window),
): Promise<Music3Job> {
  let response: Response;
  try {
    response = await fetchImpl('/v1/music/jobs', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        client_request_id: clientRequestId,
        title: input.title.trim() || undefined,
        caption: input.caption.trim(),
        lyrics: input.lyrics.replace(/\r\n?/g, '\n').trim(),
        duration_seconds: 30,
        output_format: 'wav',
      }),
    });
  } catch (error) {
    throw new OmniBridgeApiError(error instanceof Error ? error.message : 'Music submission outcome is unknown.');
  }
  let raw: unknown;
  try {
    raw = await response.json();
  } catch {
    throw new OmniBridgeApiError(`Music submission returned invalid JSON (${response.status}); its outcome is unknown.`, {
      status: response.status,
      responseKnown: response.status >= 400 && response.status < 500,
    });
  }
  if (!response.ok) {
    throw new OmniBridgeApiError(errorMessage(raw, `Music submission failed (${response.status}).`), {
      status: response.status,
      responseKnown: response.status >= 400 && response.status < 500,
    });
  }
  try {
    return parseMusicJob(raw);
  } catch (error) {
    throw new OmniBridgeApiError(error instanceof Error ? error.message : 'Music submission response has no recovery handle; its outcome is unknown.', {
      status: response.status,
      responseKnown: false,
    });
  }
}

export async function getOmniBridgeMusicJob(jobId: string, fetchImpl: FetchLike = window.fetch.bind(window)): Promise<Music3Job> {
  const response = await fetchImpl(`/v1/music/jobs/${encodeURIComponent(jobId)}`);
  const raw = await jsonBody(response);
  if (!response.ok) {
    throw new OmniBridgeApiError(errorMessage(raw, `Music job query failed (${response.status}).`), {
      status: response.status,
      responseKnown: true,
    });
  }
  return parseMusicJob(raw);
}

export async function listRecoverableOmniBridgeJobs(fetchImpl: FetchLike = window.fetch.bind(window)): Promise<Music3Job[]> {
  const response = await fetchImpl('/v1/music/jobs');
  const raw = await jsonBody(response);
  if (!response.ok) {
    throw new OmniBridgeApiError(errorMessage(raw, `Music job list failed (${response.status}).`), {
      status: response.status,
      responseKnown: true,
    });
  }
  if (!Array.isArray(raw)) throw new OmniBridgeApiError('Music job list is not an array.', { responseKnown: true });
  return raw.map(parseMusicJob).filter(job => job.id.startsWith('omnibridge-'));
}

function safeClientRequestId(value: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9._:-]{7,95}$/.test(value);
}

function validIntent(value: unknown): value is OmniBridgeCaseIntent {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const record = value as Partial<OmniBridgeCaseIntent>;
  return record.schema === OMNIBRIDGE_CASE_SCHEMA
    && typeof record.clientRequestId === 'string'
    && safeClientRequestId(record.clientRequestId)
    && record.jobId === `omnibridge-${record.clientRequestId}`
    && typeof record.title === 'string'
    && typeof record.caption === 'string'
    && typeof record.lyrics === 'string'
    && typeof record.postAttempted === 'boolean'
    && ['intent_persisted', 'attempted', 'accepted', 'unknown', 'rejected', 'completed'].includes(record.submitOutcome || '')
    && typeof record.createdAt === 'string'
    && typeof record.updatedAt === 'string';
}

export function loadOmniBridgeCaseIntent(storage: Pick<Storage, 'getItem'> = window.localStorage): OmniBridgeCaseIntent | null {
  try {
    const raw = storage.getItem(OMNIBRIDGE_CASE_STORAGE_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    return validIntent(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

export function persistOmniBridgeCaseIntent(
  intent: OmniBridgeCaseIntent,
  storage: Pick<Storage, 'getItem' | 'setItem'> = window.localStorage,
): OmniBridgeCaseIntent {
  if (!validIntent(intent)) throw new OmniBridgeApiError('Refusing to persist an invalid API case intent.');
  storage.setItem(OMNIBRIDGE_CASE_STORAGE_KEY, JSON.stringify(intent));
  const readBack = loadOmniBridgeCaseIntent(storage);
  if (!readBack || readBack.clientRequestId !== intent.clientRequestId || readBack.postAttempted !== intent.postAttempted) {
    throw new OmniBridgeApiError('The API case intent could not be durably persisted; no POST was sent.');
  }
  return readBack;
}

export function createOmniBridgeCaseIntent(
  input: OmniBridgeCaseInput,
  storage: Pick<Storage, 'getItem' | 'setItem'> = window.localStorage,
  idFactory: () => string = defaultCaseId,
  now: () => Date = () => new Date(),
): OmniBridgeCaseIntent {
  const clientRequestId = `api-case-${idFactory()}`;
  if (!safeClientRequestId(clientRequestId)) throw new OmniBridgeApiError('Could not create a safe API case identifier.');
  const timestamp = now().toISOString();
  return persistOmniBridgeCaseIntent({
    schema: OMNIBRIDGE_CASE_SCHEMA,
    clientRequestId,
    jobId: `omnibridge-${clientRequestId}`,
    title: input.title.trim(),
    caption: input.caption.trim(),
    lyrics: input.lyrics.replace(/\r\n?/g, '\n').trim(),
    postAttempted: false,
    submitOutcome: 'intent_persisted',
    createdAt: timestamp,
    updatedAt: timestamp,
  }, storage);
}

function defaultCaseId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') return crypto.randomUUID();
  if (typeof crypto !== 'undefined' && typeof crypto.getRandomValues === 'function') {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    return Array.from(bytes, value => value.toString(16).padStart(2, '0')).join('');
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 14)}-${Math.random().toString(36).slice(2, 14)}`;
}

export function updateOmniBridgeCaseIntent(
  intent: OmniBridgeCaseIntent,
  patch: Partial<Pick<OmniBridgeCaseIntent, 'jobId' | 'postAttempted' | 'submitOutcome'>>,
  storage: Pick<Storage, 'getItem' | 'setItem'> = window.localStorage,
  now: () => Date = () => new Date(),
): OmniBridgeCaseIntent {
  return persistOmniBridgeCaseIntent({ ...intent, ...patch, updatedAt: now().toISOString() }, storage);
}

export function clearOmniBridgeCaseIntent(storage: Pick<Storage, 'removeItem'> = window.localStorage): void {
  storage.removeItem(OMNIBRIDGE_CASE_STORAGE_KEY);
}

function normalizedSha256(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  const normalized = value.trim().toLowerCase().replace(/^sha256:/, '');
  return /^[a-f0-9]{64}$/.test(normalized) ? normalized : null;
}

function completedSong(job: Music3Job) {
  return job.songs?.[0] ?? job.song;
}

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, value => value.toString(16).padStart(2, '0')).join('');
}

const SHA256_CONSTANTS = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

function rotateRight(value: number, count: number): number {
  return (value >>> count) | (value << (32 - count));
}

function sha256Fallback(bytes: Uint8Array): Uint8Array {
  const paddedLength = Math.ceil((bytes.byteLength + 9) / 64) * 64;
  const padded = new Uint8Array(paddedLength);
  padded.set(bytes);
  padded[bytes.byteLength] = 0x80;
  const bitLength = bytes.byteLength * 8;
  const paddedView = new DataView(padded.buffer);
  paddedView.setUint32(paddedLength - 8, Math.floor(bitLength / 0x1_0000_0000), false);
  paddedView.setUint32(paddedLength - 4, bitLength >>> 0, false);

  const state = new Uint32Array([
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
    0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ]);
  const words = new Uint32Array(64);

  for (let offset = 0; offset < paddedLength; offset += 64) {
    for (let index = 0; index < 16; index += 1) {
      words[index] = paddedView.getUint32(offset + index * 4, false);
    }
    for (let index = 16; index < 64; index += 1) {
      const previous15 = words[index - 15];
      const previous2 = words[index - 2];
      const sigma0 = rotateRight(previous15, 7) ^ rotateRight(previous15, 18) ^ (previous15 >>> 3);
      const sigma1 = rotateRight(previous2, 17) ^ rotateRight(previous2, 19) ^ (previous2 >>> 10);
      words[index] = (words[index - 16] + sigma0 + words[index - 7] + sigma1) >>> 0;
    }

    let [a, b, c, d, e, f, g, h] = state;
    for (let index = 0; index < 64; index += 1) {
      const sum1 = rotateRight(e, 6) ^ rotateRight(e, 11) ^ rotateRight(e, 25);
      const choice = (e & f) ^ (~e & g);
      const temporary1 = (h + sum1 + choice + SHA256_CONSTANTS[index] + words[index]) >>> 0;
      const sum0 = rotateRight(a, 2) ^ rotateRight(a, 13) ^ rotateRight(a, 22);
      const majority = (a & b) ^ (a & c) ^ (b & c);
      const temporary2 = (sum0 + majority) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d + temporary1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (temporary1 + temporary2) >>> 0;
    }
    state[0] = (state[0] + a) >>> 0;
    state[1] = (state[1] + b) >>> 0;
    state[2] = (state[2] + c) >>> 0;
    state[3] = (state[3] + d) >>> 0;
    state[4] = (state[4] + e) >>> 0;
    state[5] = (state[5] + f) >>> 0;
    state[6] = (state[6] + g) >>> 0;
    state[7] = (state[7] + h) >>> 0;
  }

  const digest = new Uint8Array(32);
  const digestView = new DataView(digest.buffer);
  state.forEach((value, index) => digestView.setUint32(index * 4, value, false));
  return digest;
}

export async function sha256Digest(
  bytes: Uint8Array,
  subtle: Pick<SubtleCrypto, 'digest'> | null | undefined = globalThis.crypto?.subtle,
): Promise<Uint8Array> {
  if (!subtle) return sha256Fallback(bytes);
  const input = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
  return new Uint8Array(await subtle.digest('SHA-256', input));
}

export async function verifyImportedAudioArtifact(
  job: Music3Job,
  fetchImpl: FetchLike = window.fetch.bind(window),
  digestImpl: (bytes: Uint8Array) => Promise<Uint8Array> = sha256Digest,
): Promise<VerifiedAudioArtifact> {
  if (job.status !== 'completed') throw new OmniBridgeApiError('Artifact verification requires a completed job.');
  const completed = completedSong(job);
  const metadata = completed?.song?.metadata;
  const expectedSha256 = normalizedSha256(metadata?.artifact_sha256);
  if (!completed?.id || !completed.audio_url || !expectedSha256) {
    throw new OmniBridgeApiError('Completed job is missing its imported song, audio URL, or Artifact SHA-256 evidence.', { responseKnown: true });
  }
  if (!completed.audio_url.startsWith('/v1/library/media/')) {
    throw new OmniBridgeApiError('Refusing to fetch an Artifact from outside the protected Studio library.', { responseKnown: true });
  }
  const response = await fetchImpl(completed.audio_url);
  if (!response.ok) {
    throw new OmniBridgeApiError(`Imported audio fetch failed (${response.status}).`, { status: response.status, responseKnown: true });
  }
  const lengthHeader = response.headers.get('content-length');
  let declaredLength: number | null = null;
  if (lengthHeader !== null) {
    if (!/^\d+$/.test(lengthHeader.trim())) {
      throw new OmniBridgeApiError('Imported audio returned an invalid Content-Length.', { responseKnown: true });
    }
    declaredLength = Number(lengthHeader);
    if (!Number.isSafeInteger(declaredLength) || declaredLength < 0) {
      throw new OmniBridgeApiError('Imported audio returned an invalid Content-Length.', { responseKnown: true });
    }
  }
  if (declaredLength !== null && declaredLength > MAX_AUDIO_BYTES) {
    throw new OmniBridgeApiError('Imported audio exceeds the API case verification limit.', { responseKnown: true });
  }
  const contentType = (response.headers.get('content-type') || '').split(';')[0].trim().toLowerCase();
  if (!contentType.startsWith('audio/')) {
    throw new OmniBridgeApiError('Imported Artifact did not return an audio MIME type.', { responseKnown: true });
  }
  const buffer = await response.arrayBuffer();
  if (buffer.byteLength === 0 || buffer.byteLength > MAX_AUDIO_BYTES) {
    throw new OmniBridgeApiError('Imported audio is empty or too large.', { responseKnown: true });
  }
  if (declaredLength !== null && declaredLength !== buffer.byteLength) {
    throw new OmniBridgeApiError('Imported audio byte length does not match Content-Length.', { responseKnown: true });
  }
  const bytes = new Uint8Array(buffer);
  const sha256 = bytesToHex(await digestImpl(bytes));
  if (sha256 !== expectedSha256) {
    throw new OmniBridgeApiError('Imported audio SHA-256 does not match the verified Artifact evidence.', { responseKnown: true });
  }
  return {
    blob: new Blob([bytes], { type: contentType }),
    songId: completed.id,
    audioUrl: completed.audio_url,
    sha256,
    expectedSha256,
    bytes: buffer.byteLength,
    contentType,
  };
}
