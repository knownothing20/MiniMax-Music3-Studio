# Changelog

What changed, newest first. Dates are release dates; the studio is versioned by its
Windows build.

## Unreleased

### Added

- Added a fail-closed Rust contract client for OmniBridge MiniMax Music jobs.
  It persists intent before the one allowed submit, keeps task credentials out
  of public job JSON, resumes accepted work with GET-only polling, and verifies
  Artifact MIME, length, magic bytes, and SHA-256 before import.
- Added a read-only integration status endpoint and an example Profile v1 music
  binding. Neither claims that a `route:music:*` policy is published or that a
  paid provider call has been verified.
- The UI now treats `submission_unknown` as a terminal polling barrier: it keeps
  the original job ID visible and never creates an automatic replacement job.
- Added an in-memory Tauri-to-HTTP session bootstrap for every `/v1/*` route,
  explicit desktop/dev CORS origins, route-specific request limits, hardened
  image proxy validation, and secret redaction. Protected media is loaded with
  authenticated fetch and temporary Blob URLs; the token is never put in URLs
  or browser storage.
- External AudioMass editing now fails closed with a visible prompt for
  protected `/v1/*` media. A future short-lived, single-use media ticket is
  required before this path can be re-enabled without exposing the session
  secret in the editor URL.

## 2026-08-19 — 1.4.0

### Added

- **The studio can read audio, not only write it.** MiniMax published Music 3
  without the encoder that turns audio into the codes the model generates, so a
  finished track could never be handed back to it. The encoder was reconstructed
  by the community; it is exported to ONNX and published as
  [nerualdreming/open-rvq-encoder-minimax-music3-169m-v4-onnx](https://huggingface.co/nerualdreming/open-rvq-encoder-minimax-music3-169m-v4-onnx),
  verified against the PyTorch reference with every code identical. The path
  runs on parts already here: `neural-codec` from the engine turns audio into
  VAE latents, ONNX Runtime turns those into codes, and the engine renders from
  them. What that gives today is a re-render of a whole track through the model.
  Continuing past the end and repainting one section need the engine to accept
  codes as a prefix or a masked span, and its replay path takes neither.
- **Lighter quantisations for every role**, from a second community set: the
  language model down to Q4_K_M, Q4_K_S and Q3_K_M, the DiT to Q4_K_S and
  Q3_K_M, the depth decoder to Q4/Q5/Q6, plus MXFP4 and NVFP4 for all three -
  ggml declares both types and the CUDA backend carries kernels for them.
- **A Minimal profile** at about 6.4 GB, for 8 GB cards that were previously
  told to use the cloud, and Light drops from 8.8 GB to 7.7 GB.
- **Models you already have can be adopted** instead of downloaded again:
  a folder picker matches files by name, then by exact size, and hard-links
  them into place.
- **Six Whisper models and both Parakeet precisions**, matching Dub Studio's
  line-up, with the Gemma quantisations alongside them.

## 2026-08-19 — 1.3.5

### Fixed

- **An instrumental was sung.** The lyrics box keeps what was in it, so the
  words of the previous track went to the engine even with the instrumental
  switch on. An instrumental now submits no lyrics at all.

- **A clean installation never started.** The engine was launched once, at
  startup, and only if a complete set of weights was already on disk - so a
  first install downloaded its models, nothing started them, and the window
  waited on "loading the models into memory" until the studio was restarted by
  hand. The supervisor now watches: whenever the weights are there and nothing
  answers on the engine port, it brings the engine up. The same gap swallowed a
  crashed engine.
- **A 24 GB card was recommended a 26.6 GB set.** The tiers were round numbers;
  they are now the sets' own weights, so a 4090 is pointed at Quality Q8 and
  the full native set is only recommended from 30 GB.

### Changed

- **Six Whisper models instead of two** — tiny, base, small, medium, large-v3
  and large-v3-turbo, the same line-up Dub Studio offers.
- **The OpenRouter key and a self-hosted server address are typed where they
  are needed**, in the group that uses them, rather than on another page.
- **Every local engine is now a choice, not a list of files.** Karaoke offers
  Parakeet, Whisper or OpenRouter and what to run it on, and one button
  installs or removes the whole set - a runtime and five model files are one
  recogniser, not six decisions. The card is the default.
- Every model row states the same things in the same order - what it is, what
  it weighs, whether it is installed, which variant - with download and remove
  beside it, the way Dub Studio states it.
- The controls are shared components now. The settings panels had each drawn
  their own tabs, inputs and buttons, so the same decision looked like a
  different control depending on the page.
- A partial settings request no longer replaces the whole assistant
  configuration: sending a provider used to blank the model, the path and the
  reasoning effort.

## 2026-08-19 — 1.3.4

### Fixed

- **An installed studio kept its models on the system drive.** Portability was
  decided by a `portable.flag` file that only the portable archive ever
  contained, so installing into `F:\AI` still put twenty-five gigabytes of
  weights, the library and the media into `%LOCALAPPDATA%`. The studio now
  keeps its data beside its own executable whenever it can actually write
  there - it tries, rather than reasoning about permissions - and falls back to
  the profile only for a read-only location such as Program Files.
- **The engine started before its libraries had finished downloading.**
  Fetching cuBLAS only queued the download and returned at once, so the engine
  was launched into a folder that did not have it yet and failed exactly as if
  nothing had been fetched. The install now waits, and reports the failure if
  the files do not arrive.
- Automatic cover art was on by default, which does nothing without an
  OpenRouter key and costs money with one. It is off until switched on.
- The OpenRouter catalogue was requested every time the providers page opened,
  with or without a key, so people without one watched a spinner and then read
  an error they could do nothing about. It is only fetched once a key is saved.

## 2026-08-19 — 1.3.3

### Fixed

- **The installed executable had a different name from the portable one.** The
  installer wrote `minimax-music3-studio-desktop.exe`, after the Rust crate,
  while the portable build named the same binary `MiniMax-Music3-Studio.exe`:
  one studio under two names depending on how it arrived. Both are now
  `MiniMax-Music3-Studio.exe`, and the installer deletes the old name so an
  updated folder is not left with a dead 52 MB copy and a shortcut pointing at
  whichever was clicked first.

## 2026-08-19 — 1.3.2

### Fixed

- **The engine could not start on a machine without the CUDA Toolkit.**
  `mm-server.exe` loads `ggml.dll`, which loads `ggml-cuda.dll`, which imports
  `cublas64_13.dll` and through it `cublasLt64_13.dll` — all static imports,
  resolved by Windows before the engine's own code runs. Neither library was
  shipped, so the process died in the loader with "cublas64_13.dll was not
  found" and no fallback to the processor was possible. The studio now installs
  them itself, from NVIDIA's own redistributable archive, beside the engine
  binary where the loader looks first. A machine that already has them — a CUDA
  Toolkit on PATH — downloads nothing.
- **The Visual C++ runtime was missing the same way.** The engine and ggml
  import `MSVCP140.dll`, `VCRUNTIME140.dll`, `VCRUNTIME140_1.dll` and
  `VCOMP140.DLL`. When they are absent the studio downloads Microsoft's own
  redistributable and runs it, once, and only then.
- **The download buttons for the optional CUDA libraries did nothing.** The
  endpoint started the download inside a task that threw away its own result,
  then answered "started" regardless - so a refusal ("another download is
  already running") vanished into a successful reply, and no interface could
  have reported it. The refusal now reaches the caller, and the interface reads
  the reply instead of discarding it.
- All five interface languages declared `karaokeOff` twice, and the second
  silently replaced the first.
- **The writing assistant kept the graphics card after it had answered.** Gemma
  holds around five gigabytes; the engine then asked for eleven more and died
  on a 24 GB card, and the studio reported a queued job that never ran. The
  assistant is now unloaded as soon as it answers, and again before the engine
  starts — unless "keep models in VRAM between jobs" is on, which is exactly
  what that setting is for. It starts itself again on the next request.
- **A crashed engine is restarted instead of ending the request.** A job that
  found no engine used to come back as "mm-server is unavailable"; the
  supervisor now brings it back and sends the job again.
- **Running out of video memory says so.** The engine's own log is read when a
  job cannot be submitted, and a card that ran out of room is reported as
  that — instead of "download the five components", which pointed at models
  already on disk.

### Added

- The starting screen reports the library download with real percentages and
  gigabytes, and the "this is taking too long" warning no longer fires while
  half a gigabyte is on its way.
- A release now checks itself: a test walks the import tables of the built
  engine bundle and fails if it names a library that is neither beside it nor
  installed on first start. It found the Visual C++ runtime immediately.

### Changed

- The studio opens in its dark theme unless the user has chosen otherwise.
- The CUDA build carries PTX for the newest architecture as well, so a
  Blackwell variant without its own device code has something to compile from
  instead of failing at the first kernel launch — NVIDIA's own "Building for
  Maximum Compatibility" rule. ggml rewrites the flag to `120a-virtual`,
  because its Blackwell kernels use instructions that exist only there, so this
  does not reach past Blackwell.

## 2026-08-19 — 1.3.1

### Added

- **A portable copy keeps everything inside its own folder** — models, the
  library, media, logs, settings, temporary files and the WebView cache all sit
  beside the executable. Nothing is written into the user profile, so deleting
  the folder deletes the studio.
- **Downloads can be undone** — every ready-made set, every per-role
  quantisation and every optional model has a remove button beside the one that
  fetched it, and the panel shows the folder they live in with a button that
  opens it.
- **A local model fetches itself on first use** — choosing Parakeet or Whisper
  is the instruction to use it, so the first track that needs timings downloads
  the model, reporting real percentages, and then does the work.

### Changed

- The engine now dies with the studio however the studio ends — a job object
  ties the process tree together, so a force-closed window no longer leaves the
  engine holding the graphics card.
- The local writing assistant is constrained by a JSON schema at the sampling
  level, and its context doubled to 16384 tokens. It could previously answer
  with prose, with a fenced block, with a list where a string belonged, or run
  out of room mid-answer.
- Lyrics are written in the language of the request. The rule that keeps the
  caption English - the engine reads it - had been swallowing the song too.
- Cover art and cloud transcription stay silent without a key instead of
  reporting a failure nobody can act on.

### Fixed

- A settings page that changed one capability erased every other choice, which
  is how a studio with a downloaded engine started answering "the local music
  engine is not configured" and queueing jobs forever.
- The download panel sent `component_ids` while the service read `ids`, so
  pressing download on the 11.9 GB set fetched the 26.6 GB one. An empty
  request is now refused outright rather than falling back to a default.
- Removing weights left the download that would resume them in the studio's
  state, so deleted files came back by themselves on the next start.
- A job that names weights no longer on disk is refused with an explanation
  instead of being sent to the engine, which spent a minute loading nothing.
- The window recovers on its own when the service takes a moment longer to
  start, instead of leaving a browser connection error on screen for good.

## 2026-08-18

### Added

- **Stem separation** — a finished track is split into six tracks (drums, bass, other,
  vocals, guitar, piano) by HT-Demucs through ONNX Runtime. Runtime is chosen the same way
  as everywhere else: automatic, GPU or CPU. The model sits among the optional downloads
  and is fetched only on request. Controls live in Studio tools; the track menu opens a
  song there already selected.
- **Automatic cover art** — a cover is drawn as soon as a track finishes, if the switch is
  on, using the prompt the writing assistant produced along with the title and the length.
- **Cover prompt templates** with `{title}`, `{style}`, `{lyrics}`, `{excerpt}` and
  `{duration}`, and a default template that can be set in Settings.
- **ID3v2.4 tags on exported MP3s** — title, artist, album, genre, tempo, lyrics and the
  cover image.
- **A running account of background work** — covers and karaoke timings report themselves
  while they happen instead of finishing in silence, and the library rereads a track as
  soon as its work is done.

### Changed

- Covers are requested as a square 1024×1024. Image models answered in their own habit
  before, and a 1408×768 frame was cropped to a strip in every card.
- The image format is read from the picture's own bytes rather than from what was declared
  about it, and that type is what reaches the file name, the HTTP response and the tag
  inside the MP3.
- Track titles follow ACE-Step Studio's rule — a chorus line, then any sung line, then the
  description — and the description branch now names the genre instead of the caption's
  heading and measurements.
- Tracks are signed **MiniMax Music 3**, the same name that goes into the artist tag. The
  fork's web-era "Anonymous" is gone.
- Karaoke is refused for an instrumental before any recogniser is involved, on both the
  local and the cloud path, and the button is not offered for a track with no words. The
  failures now say what happened in the language of the interface.
- The writing assistant chooses the length of a track when the request does not, up to 360
  seconds, and fields the user filled in are handed to the model to build around.

### Fixed

- The MiniMax `music-caption-rewriter` skill reached neither the local nor the cloud model:
  the reference captions were selected and then never added to the prompt.
- Genre never made it into the tag, because only the caption's first phrase was examined
  and that phrase is always the heading.
- Downloads no longer re-request a package that finished but was not published, and an
  interrupted file is published atomically so a half-written DLL is never taken for an
  installed one.

## Earlier

The studio grew out of [ACE-Step Studio](https://github.com/timoncool/ACE-Step-Studio) and
was rebuilt around MiniMax Music3: a native Rust service inside a Tauri window, supervising
`minimaxmusic.cpp` — no Python and no Node.js in the shipped runtime.
