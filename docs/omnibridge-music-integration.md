# OmniBridge Music integration

Music Maker consumes the public OmniBridge durable operation
`audio.music.generate`. It does not select a Provider, call Compute Hub
directly, or expose runtime paths and commands to callers.

## Ownership boundary

```text
Music Maker -> model_port / Project Profile -> OmniBridge -> selected deployment
                                                  |-> cloud deployment
                                                  `-> Compute Hub Music Worker
```

- Music Maker owns the caption, lyrics, product UI, job receipt and imported
  library record.
- OmniBridge owns Route candidates, Provider and Deployment configuration,
  credentials, upstream model ids and fallback policy.
- Compute Hub owns local Worker execution and its WAV Artifact.
- The browser never receives a Gateway key, Provider key, task token or private
  child-task id.

Other products such as Book Money should not call Music Maker's
session-protected `/v1/music/jobs` endpoint. They should define their own
Project Profile binding for `audio.music.generate` and use the shared
OmniBridge Project SDK. Music Maker is a reference consumer, not a shared
credential proxy.

## Public request contract

The request sent to OmniBridge is the strict
`omnibridge.music-generation-payload.v1` shape:

- `model`: a `route:music:*` selector resolved from the Project Profile;
- `prompt`: at most 2,000 UTF-16 code units;
- `lyrics`: 1-3,500 UTF-16 code units;
- `format`: `wav` by default, or `mp3`;
- `sample_rate`: one of 16000, 24000, 32000 or 44100;
- `bitrate`: one of 32000, 64000, 128000 or 256000.

Provider ids, URLs, model paths, commands and runtime overrides are not part of
this request. Music Maker currently sends 44.1 kHz and 256 kbps. The API case
uses WAV so the same public request can resolve to the local Compute Hub Music
Worker without a project-specific payload.

## Durable submit and recovery

1. Validate the request and calculate its SHA-256 digest.
2. Persist the local intent and stable idempotency key before any network POST.
3. Perform exactly one `POST /v1/jobs` with operation
   `audio.music.generate`, kind `audio.music_generation`, the Route selector,
   `Idempotency-Key` and `X-Platform-Id`.
4. Persist the returned task id and private task token immediately.
5. Poll only with `GET /v1/jobs/:taskId`.
6. If the POST outcome is ambiguous, record `submission_unknown`; never replay
   or switch candidates automatically.
7. Download the declared Artifact once the job succeeds, then verify MIME,
   Content-Length, declared byte count, audio magic and SHA-256 before import.

A process restart with an intent but no accepted handle becomes
`submission_unknown`. Only an explicit pre-submit rejection may remain
`not_submitted`; an accepted or unknown request has no automatic POST path.

## WAV receipt evidence

For WAV results, Music Maker additionally walks RIFF chunks and requires a
bounded PCM format with internally consistent channel, sample-rate, bit-depth,
block-align and byte-rate fields. The library record persists:

- `artifact_content_type`;
- `artifact_bytes`;
- `artifact_sha256`;
- `wav_channels`;
- `wav_sample_rate_hz`;
- `wav_bits_per_sample`;
- `wav_data_bytes`;
- `artifact_duration_seconds`.

The library then records `actual_duration_seconds` and
`duration_source=audio_file` from the imported audio itself. The caller's
requested duration is retained separately when it differs.

## Studio API

Music Maker's own desktop flow uses:

- `POST /v1/music/jobs` once with a stable `client_request_id`;
- `GET /v1/music/jobs/:job_id` for recovery;
- `GET /v1/music/jobs` to list locally recoverable jobs;
- `GET /v1/library/media/:song_id` for the imported audio.

These routes require the private Studio session header. They are intentionally
not a general cross-project API.

## Verification policy

Contract and integration tests are network-free and cover request
serialization, format allowlists, status recovery, task-token redaction,
Artifact integrity, WAV structure and duration evidence, sidecar conflict
detection, and ambiguous-submit no-replay behavior.

A readiness response proves only that the route and Worker can accept work.
A real generation canary requires a separate cost decision when the resolved
Route may use a paid deployment. Health checks never authorize a paid POST.
