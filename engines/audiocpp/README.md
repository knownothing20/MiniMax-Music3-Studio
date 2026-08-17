# audio.cpp / MiniMax Music3 adapter

This directory defines the native `audio.cpp` adapter only. It does not vendor
an executable or model weights. Both are user-installed runtime data.

## Verified upstream baseline

| Item | Value |
| --- | --- |
| Engine repository | `https://github.com/0xShug0/audio.cpp` |
| Required branch | `preview/minimax-music-3` |
| Pinned commit | `aafb22aed103eba098a48e92b977e1c2dbfc8e13` |
| CLI executable | `audiocpp_cli` (`audiocpp_cli.exe` on Windows) |
| Model family | `minimax_music3` |
| Task and mode | `gen`, `offline` only |
| GGUF package | `audio-cpp/MiniMax-Music3-GGUF` at `ead13fe2b1ca3e8ca314d4bdfe1ff04c533f2b13` |

The branch is experimental. The adapter must reject another binary revision
until its CLI is inspected against this manifest.

## Invocation contract

The process runner builds this command from a validated request. Paths and text
are passed as individual process arguments; it must never invoke a shell.

```text
audiocpp_cli --task gen --family minimax_music3 --model <model-root>
  --backend <cpu|cuda|vulkan|metal|best> --mode offline --device <integer>
  --text <prompt> --out <temporary-output.wav>
  --request-option lyrics=<lyrics>
  [--request-option duration_sec=<float>]
  [--request-option num_inference_steps=<integer>]
  [--request-option guidance_scale=<float>]
  [--request-option ar_guidance_scale=<float>]
  [--request-option top_k=<integer>]
  [--request-option seed=<unsigned-integer>]
  [--session-option minimax_music3.language_model_gguf=<relative-gguf>]
  [--session-option minimax_music3.rvq_depth_decoder_gguf=<relative-gguf>]
  [--session-option minimax_music3.flow_transformer_gguf=<relative-gguf>]
  [--session-option minimax_music3.mem_saver=true|false]
  [--log]
```

`prompt` and `lyrics` are both required by the upstream session. `model-root`
is a directory, not a component GGUF path. Component override paths must be
relative to that root; upstream rejects absolute paths.

Only the options above are part of this adapter contract. In particular, this
integration does not claim support for reference audio, cover, repaint,
streaming output, structured generation progress, or a CLI cancellation flag.

## Studio request and result protocol

The Rust engine host owns job lifecycle; `audiocpp_cli` remains a child process.
The JSON shapes below are the adapter boundary, not an audio.cpp HTTP API.

```json
{
  "job_id": "uuid",
  "model_root": "C:/Users/user/AppData/Local/MiniMaxMusic3Studio/models/MiniMax-Music3-GGUF",
  "backend": "cuda",
  "device": 0,
  "prompt": "warm indie pop with live drums",
  "lyrics": "[verse] We walk into the morning light",
  "output_path": "C:/.../jobs/uuid/output.wav",
  "generation": {
    "duration_sec": 20.0,
    "num_inference_steps": 30,
    "guidance_scale": 1.7,
    "ar_guidance_scale": 1.5,
    "top_k": 50,
    "seed": 0
  },
  "profile_id": "balanced_q4_0"
}
```

The host validates non-empty text, a finite positive `duration_sec`, positive
steps and `top_k`, a non-negative seed, and that `output_path` is in its own
job directory before spawning the executable. It maps a profile only to the
three documented component override options in `engine-manifest.json`.

On exit code zero, the host validates that the temporary WAV exists and then
atomically publishes it to the project library. A non-zero exit, invalid output,
or a cancelled process produces no library asset.

## Progress and cancellation

audio.cpp at the pinned commit exposes `--log`, but no machine-readable
generation-progress callback or inference cancellation command. The
`AUDIOCPP_PROGRESS` line in upstream belongs only to its Python model installer;
it must not be treated as inference progress.

The host emits these stable events instead:

```json
{"job_id":"uuid","state":"queued","progress":0.0}
{"job_id":"uuid","state":"preparing","progress":null}
{"job_id":"uuid","state":"running","progress":null}
{"job_id":"uuid","state":"completed","progress":1.0,"output_path":".../output.wav"}
```

`progress: null` means indeterminate, never an invented percentage. Raw
`--log` lines may be retained for diagnostics but are not parsed into progress.

Cancellation is host-owned: when a pending cancellation is received, the Rust
host terminates the child process it created, waits for it to exit, emits
`{"state":"cancelled"}`, and never publishes its temporary output. This is
deliberately labelled `process_termination`, not cooperative model cancellation.
It is the only cancellation guarantee until an upstream revision adds a
documented inference cancellation API and the manifest is updated.

## Component compatibility and weight profiles

The package is componentized. A profile is valid only when it provides every
row in this matrix under one model root. The Studio must never expose an
individual GGUF as a selectable model.

| Component | Required | Runtime-supported files at the pinned source | User-selectable alone? |
| --- | --- | --- | --- |
| Language model | yes | `language_model_q4_0.gguf`, `language_model_q4_k.gguf`, `language_model_bf16.gguf` | No; profile-only. |
| RVQ depth decoder | yes | `rvq_depth_decoder_bf16.gguf`, `rvq_depth_decoder_q4_k.gguf` | No; profile-only. |
| Condition encoder | yes | `condition_encoder.gguf` | No. No override is exposed upstream. |
| Flow transformer | yes | `transformer_q4_0.gguf`, `transformer_q4_k.gguf`, `transformer_bf16.gguf` | No; profile-only. |
| Vocoder | yes | `vocoder.gguf` | No. No override is exposed upstream. |
| Config and tokenizer | yes | `config.json`, five `config/*.json` files, `tokenizer/tokenizer.json`, `tokenizer/tokenizer_config.json` | No. |

The pinned source explicitly exposes component override options only for the
language model, RVQ depth decoder, and flow transformer; it validates the
complete asset set before loading. It does not provide a route for using an
encoder, vocoder, or any single GGUF artifact by itself. Such partial packages
must be rejected at install time.

File sizes below are exact GGUF component bytes at the pinned package revision;
they exclude the small config/tokenizer files and are not VRAM estimates. The
upstream WebUI catalog declares 12 GiB as the minimum VRAM for its default
MiniMax-Music3 entry. Actual admission must use a benchmarked device profile,
not total GGUF bytes.

| Profile | Components | Download bytes | Purpose |
| --- | --- | ---: | --- |
| `balanced_q4_0` | LM Q4_0, depth BF16, transformer Q4_0, shared condition/vocoder | 9,012,701,792 | Upstream defaults; first supported profile. |
| `lm_quality_q4_k` | LM Q4_K, depth Q4_K, transformer Q4_K, shared condition/vocoder | 9,304,002,816 | Candidate only; needs audio-quality benchmark before UI exposure. |
| `quality_bf16` | LM BF16, depth BF16, transformer BF16, shared condition/vocoder | 28,506,140,416 | Candidate only; needs VRAM and output benchmark before UI exposure. |

See `engine-manifest.json` for the machine-readable component paths, exact
package commit, and profile eligibility.
