# Changelog

What changed, newest first. Dates are release dates; the studio is versioned by its
Windows build.

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
