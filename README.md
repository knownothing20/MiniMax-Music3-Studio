# MiniMax Music3 Studio

A Windows desktop studio for **MiniMax Music3**. Write a caption and lyrics, generate a
full-length track on your own GPU, and keep everything — audio, settings and the exact
request that produced it — in a local library.

One executable. No Python, no Node.js, no launcher script, nothing phoning home unless you
ask it to.

## What you can do

- **Generate music locally** with the complete Music3 component set: caption, lyrics,
  duration, DiT steps, LM CFG and top-k, DiT CFG, peak clip, separate DiT and LM seeds,
  several songs per prompt and several variations per song, MP3 or 16/24/32-bit WAV.
- **Reproduce any track exactly.** Every generation stores its request and its audio codes,
  so a track can be re-rendered deterministically, or re-rendered with different steps,
  seed or output format.
- **Manage your library** — search, playlists, favourites, rename, import your own audio,
  export tracks, edit audio in the built-in editor.
- **Watch what generation costs** — live GPU load, VRAM, temperature, power draw, RAM and
  engine memory, in the sidebar or in a pop-out panel.
- **Add cloud capabilities when you want them.** Speech-to-text, a caption/lyrics
  assistant, cover art and cloud music can each independently use OpenRouter, chosen from
  the live model catalog. Local stays the default.
- **Choose your own quality/VRAM trade-off** in the model manager. Nothing downloads by
  itself.

## Models

A runnable Music3 installation is always five components: language model, RVQ depth
decoder, condition encoder, DiT and vocoder.

| Your GPU | Recommended profile | Download |
| --- | --- | --- |
| 20 GB VRAM and above | Full Native — BF16 / BF16 / F32 | 28.6 GB |
| 10 GB and above | Q8 Quality | 12.8 GB |
| 9 GB tier | Light — for speed and low VRAM | 8.8 GB |

The studio detects your GPU and preselects the profile it can actually run, but the
download is always your decision. Every component is checksum-verified against a pinned
Hugging Face revision and downloads resume where they stopped.

## Architecture

```text
React UI ─┐
          ├─ MiniMax Music3 Studio.exe   (window + native service)
Rust axum ┘        │
                   └─ minimaxmusic.cpp `mm-server`  (C++/CUDA, GGUF)
```

The service is compiled into the desktop binary and supervises the C++ engine. Cloud
capabilities go through a capability catalog fetched live from OpenRouter, so the studio
never offers a model that does not declare the modality it would be used for.

Music generation, speech-to-text, the assistant and cover art are configured
independently, which makes fully local, fully cloud and hybrid setups possible without
changing anything about how projects are stored.

## Building from source

```powershell
npm --prefix app install
npm --prefix desktop install
npm --prefix desktop run build      # the studio executable
cargo test --workspace
```

Developing the UI against a running service:

```powershell
cargo run -p music-server           # service on 127.0.0.1:8765
npm --prefix app run dev            # UI on 127.0.0.1:3000
```

### Native engine

`scripts/build-minimax-runtime.ps1` builds the pinned `minimaxmusic.cpp` runtime. Releases
always stage a universal CUDA build; a card-specific build is for local testing only:

```powershell
scripts\build-minimax-runtime.ps1 -OutputDirectory .\build\engine -RuntimeBackend cuda -CudaArchitecture native
```

### Release

`scripts/build-release.ps1 -Version X.Y.Z` produces NSIS and MSI installers, a portable
archive and a signed `latest.json` for the in-app updater. It requires
`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` and
`TAURI_UPDATER_PUBKEY`, and refuses to run without them. Model weights are never included
in an installer.

## Licensing

This studio is a fork of ACE-Step Studio with ACE inference replaced by MiniMax Music3;
the ACE sources live in their own repository. MiniMax Music3 weights are governed by their
own community license — commercial use must display the MiniMax-Music3 name and implement
the safeguards that license requires.

Engineering detail, verified behaviour and the current gap list live in
[HANDOFF.md](HANDOFF.md).
