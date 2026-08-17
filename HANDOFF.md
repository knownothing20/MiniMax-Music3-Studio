# MiniMax Music3 Studio — full engineering handoff

Date: 2026-08-17

## Purpose and acceptance target

This fork is intended to preserve the useful desktop-studio workflow and visual
language of ACE-Step Studio while replacing ACE inference with **full-fidelity
MiniMax Music3**, using a native Windows stack:

```text
React ACE-derived UI -> Rust music-server (:8765) -> minimaxmusic.cpp mm-server (:8086)
Tauri desktop hosts and starts the native services
```

The product is not accepted merely because the UI renders or a job is queued.
Acceptance requires working user-visible functionality, verified native routes,
and a real installer/first-run/generation path.

## Original brief and decision history

The work began with a request to assess two paths: add MiniMax Music3 to
`timoncool/ACE-Step-Studio`, or fork the Studio for Music3. The chosen direction
was a dedicated fork because Music3 has a different inference graph and a
directly carried-over ACE backend would leave a large amount of misleading
model-specific UI. The non-negotiable product intent is nevertheless **an ACE
Studio successor**, not a bare demo: retain its useful layout, library, player,
creation workflow, model management, tools and desktop behavior where a real
Music3/native equivalent exists.

The user subsequently set these binding decisions:

1. Stack: native Windows desktop, Rust + Tauri + C++ inference; do not ship a
   ComfyUI/Python runtime as the local Music3 engine.
2. Preserve the useful ACE visual system and product workflows; remove or
   replace controls that have no actual backend rather than keeping fake UI.
3. Modes: local first and fastest; OpenRouter may provide cloud/hybrid music,
   LLM, ASR and images only from a dynamically verified catalog.
4. Models: no automatic download at startup. First run presents one deliberately
   selected recommended complete configuration; all other components are
   downloaded from Model Manager.
5. Quality: full original Music3 capability/quality is the default acceptance
   target. A Light quant is a speed/low-VRAM choice, never the claimed quality
   default on capable hardware.
6. Packaging: installer and update behavior must follow the proven Dub Studio
   pattern.

## Research and architecture record

### Inference candidates investigated

| Candidate | Finding | Decision |
| --- | --- | --- |
| Official MiniMax Music3 release | Defines the reference components and inference behavior. | Source of model contract and quality target. |
| Comfy-Org Music3 package | Provides official Comfy component files such as `dit_fp16`, `dit_int8_convrot`, pruned int8 text encoder and DAV VAE. | Useful compatibility/reference packaging; Comfy itself is not embedded in the desktop runtime. |
| `ServeurpersoCom/minimaxmusic.cpp` | Native C++ implementation with GGUF components, HTTP `mm-server`, quality/light/full variants, replay, batch and output support. | Selected local inference engine. Pinned at `9fdedf021049155b9ac194f88c1d014f4a572114`. |
| `audio.cpp` | Alternative Music3-capable path was catalogued as optional. | Kept optional; not installed or advertised as the primary runtime. |
| ACE Python/Gradio backend | Original Studio backend; exposes ACE-specific inference, LoRA, training and editing features. | Not used for local Music3 because it would violate the native stack and falsely imply Music3 support. |

### Quant/component research result

`minimaxmusic.cpp` expects a complete five-component set. A single DiT quant is
not a runnable Music3 install. Every valid profile contains:

```text
language model + RVQ depth decoder + condition encoder + DiT + vocoder
```

Profiles that were catalogued:

| Profile | Component level | Intended use |
| --- | --- | --- |
| Full Native | LM BF16 + depth BF16 + DiT F32 / matching full components | Full-fidelity target, about >=20 GB VRAM strict peak. |
| Q8 Quality | Q8 LM + Q8 depth + Q8 DiT / matching shared components | Quality quantized target, about >=10 GB strict peak. |
| Light | LM Q5_K_M + depth Q8 + DiT Q4_K_M + condition F32 + vocoder F32 | Low-VRAM/speed profile, about 7–9 GB tier. |

The first implementation incorrectly made Light the recommended default. This
was discovered by live QA and corrected in the model-manager/preset policy:
hardware selection now chooses Full at >=20 GB, Q8 at >=10 GB and Light only in
the low-VRAM tier. The existing local installation remains Light until a user
deliberately downloads Full/Q8.

### OpenRouter research/result

The intended integration is capability-based rather than a hard-coded provider
model list: fetch the catalog, then expose music/LLM/ASR/image choices only when
the provider declares the relevant modality. The native server contains this
catalog layer and tests that reject unknown/unsupported model capabilities.

At handoff there is no configured live OpenRouter key/catalog on this machine.
Therefore no cloud music, ASR, image, or hybrid run has been claimed successful.
The old ACE browser-local picker and Node routes must not be treated as proof of
OpenRouter support.

### Installer/updater research/result

Dub Studio was inspected as the reference implementation. Its relevant pattern
is: Tauri v2 updater driven by Rust, native update dialog, NSIS passive update,
portable detection, signed GitHub `latest.json`, and no hidden model download.
The release scripts/configuration in this repository were adjusted to follow
that pattern, including an early guard for absent signing material.

## Chronological engineering log

| Phase | What happened | Result / lesson |
| --- | --- | --- |
| Fork foundation | ACE visual app was retained while Rust workspace, engine contracts, native server and Tauri host were added. | Necessary base, but did not itself establish functional parity. |
| C++ runtime build | Pinned minimaxmusic.cpp was compiled for the local NVIDIA environment and launched as `mm-server` on `:8086`. | Live health and component discovery verified native Music3 rather than mock inference. |
| Model manager | Five-part downloadable profiles, checksums, resume, custom component selection and GPU presets were added. | First-run avoids automatic downloads; original Light recommendation was later found insufficient for the quality requirement. |
| First live smoke generation | A short/low-step result sounded poor. | This exposed ACE-derived defaults and proved that "it generated" is not quality acceptance. |
| Quality audit + rerun | Exact upstream defaults were used: 60 seconds, 30 steps, LM CFG 1.5, top-k 50, DiT CFG 1.7, WAV24. | Real 60-second native audio was produced, but perceptual quality still requires listening on Q8/Full. |
| UI conversion | A large native status banner, broken short-window layout, toast interception and legacy HUD were found in browser screenshots. | Banner was reduced, panel scrolling/sidebar constraints were changed, and false resource/backend indicators were removed. |
| Legacy API failure | UI showed "Connecting to server…" because AuthContext retried retired ACE Node service `:3001`, while Music3 native service was healthy. | Local desktop session fallback was added; native health is no longer masked by dead ACE auth. |
| Functional parity audit | Live probes showed many remaining ACE screens hit dead `/api/*` routes. | Native library/playlists/replay/likes/delete/import were moved to `/v1`; unsupported screens must not pretend to work. |
| Packaging audit | Tauri build paths were wrong; release flow incorrectly expected automatic updater metadata. | Build path fixed; desktop executable/server build; release script stages NSIS/portable/updater artifacts but cannot sign without credentials. |
| Git audit | Worktree contained pre-existing massive staged deletion of ACE source. | Native commits intentionally exclude it; recovery must be deliberate. |

## Commits in this handoff

- `1b70091 feat: establish native MiniMax Music3 Studio foundation`
- `42d4be4 feat: add native library audio import`

## Repository layout

| Path | Responsibility |
| --- | --- |
| `app/` | Main ACE-derived React UI served in development by Vite. |
| `crates/music-server/` | Native HTTP API, job queue, model manager, library, replay, OpenRouter capability layer. |
| `crates/music-engine/` | Engine contracts and mm-server client integration. |
| `desktop/src-tauri/` | Tauri host: starts the Rust service and bundled mm-server; updater dialog. |
| `scripts/build-minimax-runtime.ps1` | Builds the pinned C++ runtime. |
| `scripts/build-release.ps1` | Builds/stages signed installer, updater manifest and portable distribution. |
| `engines/minimaxmusic-cpp-source.json` | Pinned upstream runtime source. |

## Actual local runtime state

The live native server was verified on this machine:

- Studio health: `GET http://127.0.0.1:8765/health` returns native runtime and
  reachable `minimaxmusic-cpp`.
- C++ engine: `GET http://127.0.0.1:8086/health` is healthy; `/props` reports
  pinned upstream commit `9fdedf021049155b9ac194f88c1d014f4a572114`.
- Installed component set is **Light**, not full fidelity:
  `LM Q5_K_M`, `depth Q8_0`, `condition F32`, `DiT Q4_K_M`, `vocoder F32`.
- Model root is `%LOCALAPPDATA%\MiniMax Music3 Studio\models\minimaxmusic-cpp`.

There is no mock or ACE fallback in the native Music3 synthesis route. If the
C++ engine is unreachable, the job is failed/not configured rather than silently
replaced with generated placeholder audio.

## Verified end-to-end facts

1. Direct `POST /v1/music/jobs` created a real local job with structured
   caption/sectioned lyrics and native Quality values.
2. The job completed and stored a 60.070023-second, stereo, 44.1 kHz, s24le
   WAV file. The media endpoint returned byte-identical audio.
3. Provenance showed `source=local_generation`,
   `engine=minimaxmusic-cpp`, profile selection, and replay `audio_codes`.
4. `cargo test -p music-server` passed with 34 tests.
5. `npm --prefix app run build` passed.
6. The Tauri desktop executable and `music-server.exe` build successfully.

Perceptual audio quality was **not** claimed by automation; it must be listened
to after switching from the currently installed Light set to Q8 or Full Native.

## Supported native API surface

| Capability | Route / status |
| --- | --- |
| Health/capabilities | `/health`, `/v1/capabilities` — implemented. |
| First-run/model manager | `/setup/*` — profiles, custom five-part sets, verified downloads, no automatic model download. |
| Text/caption + lyrics generation | `/v1/music/jobs` — implemented. |
| Job state/cancel | `/v1/music/jobs/*` — implemented. |
| Deterministic replay | `/v1/music/replay` — implemented using saved `audio_codes`. |
| Library/media/playlists | `/v1/library/*` — native storage, media serving, CRUD. |
| Import | `POST /v1/library/import` — MP3/WAV multipart import, atomic DB/media rollback, provenance `audio_import`. |
| OpenRouter | Dynamic native capability layer exists; it is **not verified live** without a configured key/catalog. |

## Music3 quality policy and parameters

The upstream `minimaxmusic.cpp` contract establishes these default Quality
generation values:

```text
duration=60 seconds, steps=30, lm_cfg=1.5, lm_top_k=50, dit_cfg=1.7
lm_batch_size=1, synth_batch_size=1, peak_clip=1, mp3_bitrate=128
```

Supported validated controls additionally include seed, LM seed, output
`mp3|wav16|wav24|wav32`, synth variations `1..9`, and deterministic replay.
Persist the full submitted `mm_request` with each generation; never retain only
the replay result.

Hardware policy is selection-only: it must never auto-download weights.

| VRAM | Selected profile | Notes |
| --- | --- | --- |
| >=20 GB | Full Native | BF16/BF16/F32, the full-fidelity target. |
| >=10 GB | Q8 Quality | Quality quantized profile. |
| 9 GB tier | Light | Explicit low-VRAM/speed option; never advertise as full quality. |

Current 24 GB RTX 4090 hardware therefore selects Full Native on a clean start,
but the user must deliberately download it in Model Manager. The old default
Light profile is installed locally and remains a speed fallback only.

## UI status

The CreatePanel was rewritten to submit to the native `/v1` Music3 API and to
remove visible ACE controls with no corresponding Music3 contract: ACE model
switches, LoRA controls, repaint/flow/DCW, old reference uploads, fake batch
controls and Pollinations path.

Simple/manual/advanced workflows must expose only the verified Music3 contract:
caption, lyrics, templates, duration, steps, Quality/Quick, seed, LM CFG/top-k,
DiT CFG, output, and GPU/model profile status.

The rewritten source is valid UTF-8; earlier alleged mojibake was PowerShell
output corruption. Final visual click-through of the latest panel was not
completed because the in-app browser runtime became unavailable. Re-run visual
E2E before calling the UX finished.

The UI was also adjusted to stop falling back to absent ACE Node service `:3001`
for native-library operations. Local library, deletion, playlists, likes and
replay must remain on `/v1`. Dead ACE resource-monitor, backend-off indicator
and Pollinations row were removed/replaced with native status.

## Explicit parity gaps — do not misrepresent as complete

The following ACE features require real compatible native backends before they
can be reintroduced as active product features:

- Music3 LoRA training/loading and dataset workflow;
- audio-to-audio cover, repaint, Flow Edit, DCW and ACE-only conditioning;
- LRC alignment extraction;
- video studio/render pipeline;
- Demucs/separation and the audio editor;
- social profiles, comments, follows and public discovery;
- OpenRouter cloud music, ASR, image cover and hybrid flows until a dynamic
  catalog has been fetched and each capability has one successful real request.

Do not leave buttons that call legacy `/api/*` routes as if they work. Either
route a feature to a tested native implementation or mark it unavailable until
the service exists.

## Desktop installer and updater

`desktop/src-tauri/tauri.conf.json` now has corrected app build paths.
Tauri build produces `minimax-music3-studio-desktop.exe`; the release script
stages the desktop executable, `music-server.exe`, and the minimaxmusic.cpp
sidecar into portable/installer inputs. Models are deliberately excluded.

The updater follows the Dub Studio pattern: Rust-side check/dialog and signed
GitHub `latest.json` generated from the signed NSIS artifact. A real signed
NSIS/MSI/portable release was not emitted because this environment lacks:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
- updater public-key configuration

The release script guards these early and must not expose or generate secrets.
With signing available, acceptance still requires clean-machine testing:

```text
install -> first launch without weights -> choose/download full profile
-> engine starts -> generate/play/restart -> updater check -> portable check
```

## Commands for continuation

```powershell
# frontend
npm --prefix app run build

# server tests
cargo test -p music-server

# desktop verification
cargo check --manifest-path desktop/src-tauri/Cargo.toml
npm --prefix desktop run build

# native development endpoints
Invoke-WebRequest http://127.0.0.1:8765/health
Invoke-WebRequest http://127.0.0.1:8765/setup/status
Invoke-WebRequest http://127.0.0.1:8086/props
```

## Git safety — critical

The index contains a pre-existing massive staged deletion of the original ACE
tree (about 1,280 paths / 368k lines). It was intentionally excluded from
`1b70091` and `42d4be4`. Do **not** use `git reset --hard`, blindly commit those
deletions, or overwrite the tree. First inspect whether the source exists in
another worktree/history and restore the index deliberately while preserving the
native Studio commits.

## Recommended continuation order

1. Recover/secure the ACE source/index state before any broad cleanup.
2. Visual browser E2E of CreatePanel, native library/player/playlist/import.
3. Download Q8 or Full Native deliberately and compare actual audio against the
   Light baseline; validate full provenance/replay.
4. Implement a single native OpenRouter catalog/settings layer and test each
   enabled cloud capability with a real request.
5. Add compatible native services for the parity gaps one by one, keeping
   unsupported ACE screens absent rather than fake.
6. Build signed NSIS/MSI/portable and perform the clean-machine release path.
