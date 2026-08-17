# MiniMax Music3 Studio — engineering handoff

Date: 2026-08-17

## What this project is

A Windows desktop music studio built on **MiniMax Music3**, running natively:

```text
React UI  ─┐
           ├─ one executable: minimax-music3-studio-desktop.exe
Rust axum ─┘        │
                    └─ supervises minimaxmusic.cpp `mm-server` (C++/CUDA) on :8086
```

It is a fork of ACE-Step Studio that keeps the studio workflow — library,
player, playlists, search, setup flow, resource monitor — and replaces ACE
inference with Music3. The ACE sources are **not** in this repository; they
live in their own repository, and everything carried over was re-implemented
against the native `/v1` API.

There is no Python and no Node.js in the runtime, and no launcher script: the
release is a single `.exe`.

## Verified on this machine

Everything below was executed, not inferred.

| Check | Result |
| --- | --- |
| Local generation, Q8 Quality profile, 60 s / 30 steps / LM CFG 1.5 / top-k 50 / DiT CFG 1.7, seeds 424242 + 314159 | Completed. The engine log shows `CFG=1.50, top_k=50`, 1500 AR frames, 14 DiT windows × 30 steps, `[Done] 39.0 s total`. |
| Rendered file | 15,894,572-byte WAV, RIFF/WAVE, **stereo, 44.1 kHz, 24-bit, 60.07 s**, served byte-identical by `/v1/library/media/{id}`. |
| Provenance stored | `source=local_generation`, `engine_id=minimaxmusic-cpp`, `profile_id=quality-q8`, `dit_model=…transformer-Q8_0.gguf`, `lm_model=…language_model-Q8_0.gguf`, both seeds, 49,213 characters of `audio_codes`. |
| Deterministic replay | `POST /v1/music/replay` with `song_id` re-rendered the track from its saved codes (LM stage skipped) and imported the result. |
| Model download | Q8 Quality (12.8 GB, five components) downloaded, SHA-256 verified against the pinned HF revision, resumed after an interrupted run. |
| Engine ownership | The service starts `mm-server`; `/v1/engine/logs` returns its real log ring (registry load, prompt tokens, AR frames, DiT windows, VAE decode). |
| OpenRouter catalog | Live refresh returns 414 models: 414 text, 38 speech-to-text, 11 image, 2 music (`google/lyria-3-pro-preview`, `google/lyria-3-clip-preview`). No key needed for the catalog. |
| Desktop application | Built and launched; window opens, hosts the service in-process, starts the engine, survives restart. |
| Browser E2E | Every request the app issues is native (`/v1/*`, `/setup/*`, `/engine/*`, `/health`). Console is clean. **Zero** `/api/*` requests. |
| Engine flags reach the process | Setting `--keep-loaded` and `--max-batch 3` in Settings restarts the engine and the flags appear on its real command line; if an engine this service did not start holds the port, the request fails with that reason instead of a false success. |
| Component override | A render with a mixed set — Q8 language model with Q4 DiT — completed and stored those exact filenames in its provenance. |
| Model catalogue | All 13 GGUF entries compared against the pinned Hugging Face revision: filenames, byte sizes and SHA-256 (LFS oid) match exactly, nothing missing and nothing extra. |
| Engine defaults | Compared with upstream `request_init`: 60 s, 30 steps, seed -1, lm_seed -1, LM CFG 1.5, top-k 50, batches 1, DiT CFG 1.7, peak clip 10, MP3 128 kbps — the studio's quality baseline is identical. |
| Packaged application | The built desktop executable opens its window, hosts the service in-process, and its own first-run screen starts the engine — which is also the proof that the packaged UI reaches the service. |
| Localisation | Interface strings resolve through the catalogue in all five shipped languages; only technical identifiers (VRAM, GPU, DiT CFG, WAV/MP3, engine flag names) stay untranslated. |
| Tests | `cargo test --workspace` — 50 passing. `npm --prefix app run build` and `tsc --noEmit` clean. |

**Not verified:** perceptual audio quality has not been judged here — listen to
the Q8 track and compare it against Light yourself. Full Native (28.6 GB) was
not downloaded on this machine.

## What was actually wrong before, and what fixed it

The previous state built and passed tests, yet did not work. Running it
exposed the following, each fixed and committed separately:

1. **The desktop app crashed at startup.** The updater plugin was registered
   unconditionally but only configured in release builds, so it panicked with
   `PluginInitialization`. It is now registered only when the config declares it.
2. **The UI still talked to the retired ACE Node service.** On plain page load
   it issued `/api/auth/auto`, `/api/playlists`, `/api/reference-tracks` and
   `/api/generate/history` — all 500. The whole client for that service is gone.
3. **Light was advertised as the recommendation on a 24 GB card.** The
   recommendation is now derived from the machine: Full Native ≥ 20 GB, Q8
   Quality ≥ 10 GB, Light only in the low-VRAM tier.
4. **`POST /setup/download` hung the server.** It hashed every already-present
   GGUF on the request thread; resuming a 10 GB profile blocked past the client
   timeout. A completed `.part` also made the range request fail with 416.
5. **The dark theme was broken.** Surfaces used `dark:bg-suno-DEFAULT`, which is
   not a class Tailwind generates, so the setup screen and create panel rendered
   white inside a dark app. Tailwind also came from a CDN — fatal for a desktop
   app with no network. It is now a local build.
6. **The engine could only be started through a Tauri command**, so the studio
   failed in a browser with "cannot read properties of undefined (reading
   'invoke')". The service now owns the engine via `POST /engine/start`.
7. **The packaged service wrote its library into the process working directory.**
8. **Phantom providers.** The default configuration named local engines
   (`parakeet-tdt`, `local-llm`) that this build does not ship.
9. **The release was a launcher plus a second binary.** The service is now a
   library hosted inside the desktop executable.
10. **The application still wore Dub Studio's icon.**
11. **The packaged UI could not reach its own service.** Every request used a
    relative path, which in the desktop build resolves against Tauri's asset
    protocol; the first-run screen failed with `Unexpected token '<'`.
12. **The catalogue refresh was sent as a GET** to a POST-only route, so Studio
    tools reported cloud media as unavailable with a 405.
13. **Half the interface was untranslated**, mixing English chrome into a
    Russian UI.

## ACE → Music3 parity

| ACE feature | State in this fork |
| --- | --- |
| Create panel (simple / custom) | **Carried over.** Full engine contract: caption, lyrics, duration, steps, LM CFG, top-k, DiT CFG, peak clip, both seeds, songs (LM batch), variations per song, `mp3/wav16/wav24/wav32`, MP3 bitrate, and a per-request component override that can load any installed GGUF per role. "Reset" restores the engine's own defaults. |
| Engine launch flags | **New.** Settings exposes the real `mm-server` flags: `--keep-loaded`, `--max-batch`, `--max-seq`, `--no-fa`, `--no-batch-cfg`, `--clamp-fp16`. They are read once at startup, so saving restarts the engine, and the create panel clamps songs per request to `--max-batch`. |
| Deterministic re-render | **Extended.** The replay action opens a dialog prefilled from the track's own settings; steps, seed, DiT CFG and output format can change while the composition stays identical. |
| Generation queue, stages, cancel, reset | **Carried over**, driving `/v1/music/jobs`. |
| Progress | **Better than before.** ACE showed a fabricated percentage; the card now shows real progress parsed from the engine's AR-frame and DiT-window counters, plus a live engine-log panel. |
| Library, playlists, rename, delete, favourites | **Carried over** on `/v1/library`. Favourites are local — there is no social service to sync with. |
| Player, waveform, queue, shuffle, repeat | **Carried over** unchanged. |
| Search | **Re-aimed.** ACE searched a public feed; this searches the local library and playlists, keeping the genre palette as an authoring aid. |
| Import audio | **Carried over** — `POST /v1/library/import`, atomic with rollback. |
| Download / export track | **Carried over.** |
| Audio editor (AudioMass) | **Carried over** as local static assets; no backend needed. |
| Cover art | **Re-implemented.** Pollinations is gone with the ACE backend; covers are generated through catalog-verified OpenRouter image models or taken from a file, then stored with the track. |
| Caption / lyrics assistant | **Re-implemented** on `/v1/openrouter/completions`, enabled only when a catalog-verified text model is configured. |
| Resource monitor | **Restored and extended.** The first port dropped it. Now: GPU load, VRAM, temperature, power draw, RAM, CPU, engine process memory — all measured, with a dockable pop-out panel. |
| Model manager, GPU presets, first run | **Carried over and corrected** — five-component profiles, custom component builder, checksum-verified resumable downloads, hardware-aware recommendation, nothing downloaded automatically. |
| Settings | **Extended** into a real provider matrix: music, speech-to-text, assistant and cover art each independently Local or OpenRouter, cloud models listed only from the live catalog, API key held by the service. |
| Social profiles, comments, follows, public feed | **Removed.** A single-user desktop studio has no such service; the screens are gone rather than dead. |
| Video renderer | **Removed** — it required the ACE Node render service. See gaps. |
| Stem separation (Demucs) | **Removed** — it required the ACE Node service. See gaps. |
| LoRA training / adapters lab | **Removed** — ACE adapter formats do not apply to Music3. See gaps. |
| ACE-only conditioning: repaint, Flow Edit, DCW, audio2audio cover | **Removed** — no Music3 equivalent exists. |

## Known gaps, with the work they need

These are absent from the UI rather than present-but-dead:

* **Video renderer.** ACE rendered server-side. A native replacement is
  plausible with the `@ffmpeg/ffmpeg` dependency already in the app, or with an
  ffmpeg sidecar; it needs a render pipeline and a job contract.
* **Stem separation.** Needs a native separator sidecar (BSRoformer.cpp is the
  model Dub Studio uses) plus a job contract.
* **Music3 adapter training.** Upstream `minimaxmusic.cpp` has a `training/`
  tree and a hiddens route; exposing it needs a dataset workflow and a training
  job contract.
* **LRC / timed lyrics.** Needs word timings; OpenRouter transcription returns
  text, so this needs either a timestamp-capable model or a local aligner.
* **OpenRouter paid requests are unverified.** The catalog is verified live, and
  every request shape matches OpenRouter's documented contract (`/api/v1/images`
  with `{model, prompt}`; chat completions with `modalities: ["text","audio"]`,
  `audio.format`, `stream: true`, audio arriving as `delta.audio.data`). No
  music, transcription, image or assistant request has been executed, because
  each one is billed and no key was authorised for this work. To verify: add a
  key in Settings → Providers, pick a model per capability, run one request each.

## Repository layout

| Path | Responsibility |
| --- | --- |
| `app/` | React studio UI (Vite, local Tailwind build). |
| `crates/music-server/` | The service: HTTP API, jobs, model manager, library, replay, resources, credentials, OpenRouter capability layer. A library crate — the desktop binary hosts it. |
| `crates/music-engine/` | `mm-server` supervisor and engine contracts. |
| `crates/music-core/` | Capability and provider types. |
| `desktop/src-tauri/` | The desktop application: window, in-process service, updater. |
| `engines/minimaxmusic-cpp-source.json` | Pinned upstream engine (`9fdedf021049155b9ac194f88c1d014f4a572114`). |
| `scripts/build-minimax-runtime.ps1` | Builds the pinned C++ runtime (CUDA or Vulkan). |
| `scripts/build-release.ps1` | Signed installer, portable archive and updater manifest. |

## Native API

| Capability | Route |
| --- | --- |
| Health, capabilities, configuration | `/health`, `/v1/capabilities`, `/v1/configuration` |
| Machine resources | `/v1/system/resources` |
| Engine control and logs | `/engine/start`, `/engine/presets`, `/engine/preset`, `/v1/engine/logs` |
| First run and model manager | `/setup/status`, `/setup/catalog`, `/setup/download`, `/setup/cancel` |
| Generation, status, cancel, replay | `/v1/music/jobs`, `/v1/music/jobs/{id}`, `/v1/music/replay` |
| Library, media, covers, playlists, import | `/v1/library/*` |
| OpenRouter | `/v1/openrouter/settings`, `/catalog`, `/catalog/refresh`, `/completions`, `/transcriptions`, `/covers` |

## Model policy

Five components are always required — language model, RVQ depth decoder,
condition encoder, DiT, vocoder. A single quant is not an installation.

| VRAM | Recommended profile | Download |
| --- | --- | --- |
| ≥ 20 GB | Full Native (BF16 / BF16 / F32) | 28.6 GB |
| ≥ 10 GB | Q8 Quality | 12.8 GB |
| 9 GB tier | Light (speed / low VRAM) | 8.8 GB |

Nothing is ever downloaded automatically. The recommendation is a selection
only; the user starts the download.

## Packaging

`npm --prefix desktop run build` produces the single executable. The release
script builds the pinned C++ runtime, bundles it as `resources/minimaxmusic-cpp`
next to the application, and emits NSIS, MSI, a portable archive with a
`portable.flag`, and a `latest.json` built from the real signed artifact.
Model weights are never in the installer.

**External blocker:** no signing material is available in this environment, so
no signed installer or updater manifest was produced. The script fails fast and
says which variable is missing (verified: it stops on
`TAURI_SIGNING_PRIVATE_KEY`). With `TAURI_SIGNING_PRIVATE_KEY`,
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` and `TAURI_UPDATER_PUBKEY` set, the
remaining acceptance path is a clean machine:

```text
install → first launch with no weights → choose and download a profile
→ engine starts → generate → play → restart → updater check → portable check
```

## Commands

```powershell
cargo test --workspace
npm --prefix app run build
npm --prefix desktop run build          # the single executable
npm --prefix app run dev                # UI against a running service

Invoke-WebRequest http://127.0.0.1:8765/health
Invoke-WebRequest http://127.0.0.1:8765/setup/status
Invoke-WebRequest http://127.0.0.1:8765/v1/system/resources
```

Useful overrides: `MINIMAX_MM_SERVER_BIN`, `MINIMAX_MUSIC_MODELS_ROOT`,
`MINIMAX_STUDIO_DATA_ROOT`, `MINIMAX_STUDIO_PORT`, `OPENROUTER_API_KEY`.

## Suggested next steps

1. Download Full Native and compare it against Q8 by ear; keep whichever the
   quality bar requires as the shipped recommendation.
2. Add a key and execute one real request per cloud capability.
3. Produce a signed release and run the clean-machine path above.
4. Close the gaps in the order they matter to you — video, stems, training, LRC.
