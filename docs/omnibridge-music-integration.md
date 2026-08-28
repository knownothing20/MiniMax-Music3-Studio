# OmniBridge Music integration

This is the first, no-provider-call slice of the remediation plan. It adds an
isolated Rust contract client and recovery store; it does not replace the
existing Local or direct OpenRouter execution paths yet.

## Ownership boundary

```text
music-maker -> OmniBridge -> selected deployment
                              |-> MiniMax/GMI cloud
                              `-> Compute Hub, once a Music Worker exists
```

music-maker never selects a Provider, reads a Provider key, or calls Compute
Hub directly. OmniBridge owns Route resolution, Provider credentials, durable
queue semantics, and the public ArtifactRef. Compute Hub remains a delegated
executor; the current Compute Hub repository has no Music Worker, so no local
music readiness is claimed by this slice.

## Runtime configuration

The temporary Rust adapter is enabled only when all four backend environment
variables are present:

```text
MUSIC_MAKER_OMNIBRIDGE_BASE_URL
MUSIC_MAKER_OMNIBRIDGE_GATEWAY_KEY
MUSIC_MAKER_OMNIBRIDGE_PLATFORM_ID
MUSIC_MAKER_OMNIBRIDGE_MUSIC_ROUTE
```

The Route must use `route:music:*`. There is intentionally no fallback Route,
deployment alias, or Provider model in source. The Gateway key and task token
are never serialized into public Studio API responses or Debug output.

`GET /v1/integrations/omnibridge` reports local configuration only. It does not
call a model and always reports real generation as unverified. A configured
Route is still not ready until OmniBridge has published and resolved it with
current `music_generation` evidence.

## Durable rules

1. Normalize the request and calculate its SHA-256 digest.
2. Persist the local intent and stable non-empty idempotency key.
3. Perform exactly one `POST /v1/jobs` with:
   - operation `audio.music.generate`;
   - kind `audio.music_generation`;
   - the configured `route:music:*` selector.
4. Persist `task_id` and the private `task_token` before polling.
5. Recover an accepted job with GET only. A transport-ambiguous submit becomes
   `submission_unknown` and is never replayed automatically.
6. Download only the metadata-only ArtifactRef and verify Content-Length, MIME,
   audio magic bytes, and SHA-256 before it can be imported.

The current GMI RequestQueue contract has no reliable accepted-task cancel, so
the adapter fails cancel closed and continues GET-only recovery.

## Known contract gaps

- OmniBridge currently accepts `task_id` in the real response while its OpenAPI
  describes `id`; the adapter accepts either until the contract is repaired.
- Profile v2 does not currently include Music, so the checked-in example uses
  Profile v1.
- `route:music:cloud` in the example is the target declaration, not evidence
  that the Route is published in the current runtime.
- The GMI implementation requires 1-3500 lyric characters while one document
  shows empty instrumental lyrics. Instrumental cloud generation therefore
  fails closed instead of inventing lyrics.
- The sidecar is an interim backend-only store. Production enablement still
  requires OS ACL/credential-store review plus the plan's session/CORS baseline.

## Verification policy

Unit tests are network-free and cover request serialization, response aliasing,
secret redaction, Artifact integrity, and sidecar conflict detection. Real
