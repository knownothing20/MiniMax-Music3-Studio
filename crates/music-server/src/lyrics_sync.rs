//! Karaoke timings for a finished track.
//!
//! The words are already known - Music3 sang the lyrics it was given - but the
//! *timings* are not, and the video studio's karaoke layer and the player both
//! need them. This module produces an LRC file for a track using whichever
//! recogniser the user picked; like every optional extra here it is off by
//! default and downloads nothing on its own.
//!
//! Three backends, all of them explicit choices:
//!
//!   * **Whisper** - whisper.cpp run as a sidecar. It writes LRC itself, and
//!     the CUDA build is preferred over the CPU one when both are installed.
//!   * **Parakeet** - the NVIDIA TDT model, the same one Dub Studio uses.
//!   * **OpenRouter** - a cloud model, billed to the user's own key, asked for
//!     verbose output because plain text carries no timings.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use parakeet_rs::Transcriber;

use crate::downloads::{Asset, AssetKind, Downloader};

/// Which recogniser produces the timings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsrProvider {
    #[default]
    None,
    Whisper,
    Parakeet,
    OpenRouter,
}

/// Persisted with the rest of the studio settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LyricsSyncConfig {
    /// The karaoke switch. Off means the buttons do not appear at all.
    pub enabled: bool,
    pub provider: AsrProvider,
    /// Which downloaded Whisper model to run.
    pub whisper_model: Option<String>,
    /// Which OpenRouter speech-to-text model to call.
    pub openrouter_model: Option<String>,
}

impl LyricsSyncConfig {
    pub fn available(&self) -> bool {
        self.enabled && self.provider != AsrProvider::None
    }
}

/// whisper.cpp is pinned to one release so a working setup keeps working.
const WHISPER_BUILD: &str = "v1.9.2";
/// The ONNX Runtime that Parakeet loads. Mixing versions deadlocks the loader,
/// so this is pinned exactly as Dub Studio pins it.
const ONNXRUNTIME_BUILD: &str = "v1.24.2";

pub const ASSETS: &[Asset] = &[
    Asset {
        id: "whisper-cuda",
        label: "whisper.cpp runtime (CUDA 12.4)",
        kind: AssetKind::Runtime,
        url: "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.2/whisper-cublas-12.4.0-bin-x64.zip",
        relative_path: "runtime/whisper-cuda.zip",
        bytes: 670_611_449,
        unzip_into: Some("whisper-cuda"),
        marker: "whisper-cli",
        vram_gb: None,
        note: "GPU build. Large, because it carries the CUDA libraries with it.",
    },
    Asset {
        id: "whisper-cpu",
        label: "whisper.cpp runtime (CPU)",
        kind: AssetKind::Runtime,
        url: "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.2/whisper-bin-x64.zip",
        relative_path: "runtime/whisper-cpu.zip",
        bytes: 8_194_445,
        unzip_into: Some("whisper-cpu"),
        marker: "whisper-cli",
        vram_gb: None,
        note: "Tiny download, works anywhere, slower on long tracks.",
    },
    Asset {
        id: "whisper-large-v3-turbo",
        label: "Whisper large-v3-turbo (q5_0)",
        kind: AssetKind::Model,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        relative_path: "models/ggml-large-v3-turbo-q5_0.bin",
        bytes: 574_041_195,
        unzip_into: None,
        marker: "",
        vram_gb: Some(2),
        note: "The accurate choice for sung lyrics.",
    },
    Asset {
        id: "whisper-base",
        label: "Whisper base",
        kind: AssetKind::Model,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        relative_path: "models/ggml-base.bin",
        bytes: 147_951_465,
        unzip_into: None,
        marker: "",
        vram_gb: Some(1),
        note: "Fast and small; misses words in dense mixes.",
    },
    Asset {
        id: "parakeet-tdt-int8",
        label: "Parakeet TDT 0.6B v3 (int8)",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.int8.onnx",
        relative_path: "models/parakeet/encoder-model.int8.onnx",
        bytes: 652_183_999,
        unzip_into: None,
        marker: "",
        vram_gb: Some(2),
        note: "The encoder; the decoder and vocabulary come with it.",
    },
    Asset {
        id: "parakeet-decoder",
        label: "Parakeet decoder",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/decoder_joint-model.int8.onnx",
        relative_path: "models/parakeet/decoder_joint-model.int8.onnx",
        bytes: 18_202_004,
        unzip_into: None,
        marker: "",
        vram_gb: None,
        note: "Required alongside the Parakeet encoder.",
    },
    Asset {
        id: "parakeet-features",
        label: "Parakeet feature extractor",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/nemo128.onnx",
        relative_path: "models/parakeet/nemo128.onnx",
        bytes: 139_764,
        unzip_into: None,
        marker: "",
        vram_gb: None,
        note: "The mel front end the encoder expects.",
    },
    Asset {
        id: "parakeet-vocab",
        label: "Parakeet vocabulary",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/vocab.txt",
        relative_path: "models/parakeet/vocab.txt",
        bytes: 93_939,
        unzip_into: None,
        marker: "",
        vram_gb: None,
        note: "Token table.",
    },
    Asset {
        id: "parakeet-config",
        label: "Parakeet configuration",
        kind: AssetKind::Model,
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/config.json",
        relative_path: "models/parakeet/config.json",
        bytes: 97,
        unzip_into: None,
        marker: "",
        vram_gb: None,
        note: "Token table.",
    },
    Asset {
        id: "onnxruntime",
        label: "ONNX Runtime 1.24.2",
        kind: AssetKind::Runtime,
        url: "https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-win-x64-1.24.2.zip",
        relative_path: "runtime/onnxruntime.zip",
        bytes: 74_075_355,
        unzip_into: Some("onnx"),
        marker: "onnxruntime.dll",
        vram_gb: None,
        note: "Parakeet runs on this; it is loaded at run time, not linked in.",
    },
];

/// Every Parakeet file, because the model is useless without all of them.
pub const PARAKEET_ASSET_IDS: [&str; 5] =
    ["parakeet-tdt-int8", "parakeet-decoder", "parakeet-features", "parakeet-vocab", "parakeet-config"];

pub fn asset(id: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.id == id)
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub enabled: bool,
    pub provider: AsrProvider,
    pub root: String,
    /// True when the selected provider can actually run right now.
    pub ready: bool,
    pub whisper_binary: Option<String>,
    pub whisper_model: Option<String>,
    pub openrouter_model: Option<String>,
    pub installed_models: Vec<String>,
    pub assets: Vec<crate::downloads::AssetStatus>,
    pub active_download: Option<crate::downloads::DownloadProgress>,
}

pub struct LyricsSync {
    downloader: Downloader,
}

impl LyricsSync {
    pub fn new(data_root: &Path) -> Self {
        Self { downloader: Downloader::new(data_root.join("karaoke")) }
    }

    pub fn downloader(&self) -> &Downloader {
        &self.downloader
    }

    /// The CUDA build first: on a machine that has one, the CPU build would be
    /// a silent downgrade.
    pub fn whisper_binary(&self) -> Option<PathBuf> {
        crate::downloads::locate_binary(self.downloader.root(), &["whisper-cuda", "whisper-cpu"], "whisper-cli")
    }

    pub fn installed_models(&self) -> Vec<&'static Asset> {
        ASSETS
            .iter()
            .filter(|asset| asset.kind == AssetKind::Model && self.downloader.is_installed(asset))
            .collect()
    }

    /// Parakeet needs every one of its files and the ONNX Runtime library.
    pub fn parakeet_ready(&self) -> bool {
        self.onnxruntime_library().is_some()
            && PARAKEET_ASSET_IDS
                .iter()
                .all(|id| asset(id).is_some_and(|asset| self.downloader.is_installed(asset)))
    }

    pub fn parakeet_dir(&self) -> PathBuf {
        self.downloader.root().join("models").join("parakeet")
    }

    /// `ort` loads this at run time; linking it would tie the build to one
    /// toolchain and one machine's libraries.
    pub fn onnxruntime_library(&self) -> Option<PathBuf> {
        let candidate = self.downloader.runtime_dir("onnx").join("onnxruntime.dll");
        candidate.is_file().then_some(candidate)
    }

    fn whisper_model_path(&self, config: &LyricsSyncConfig) -> Option<PathBuf> {
        let id = config.whisper_model.as_deref()?;
        let asset = asset(id)?;
        self.downloader.is_installed(asset).then(|| self.downloader.path_of(asset))
    }

    pub async fn status(&self, config: &LyricsSyncConfig) -> SyncStatus {
        let whisper_binary = self.whisper_binary();
        let ready = match config.provider {
            AsrProvider::None => false,
            AsrProvider::Whisper => whisper_binary.is_some() && self.whisper_model_path(config).is_some(),
            AsrProvider::Parakeet => self.parakeet_ready(),
            AsrProvider::OpenRouter => config.openrouter_model.as_deref().is_some_and(|model| !model.trim().is_empty()),
        };
        SyncStatus {
            enabled: config.enabled,
            provider: config.provider,
            root: self.downloader.root().display().to_string(),
            ready: config.enabled && ready,
            whisper_binary: whisper_binary.map(|path| path.display().to_string()),
            whisper_model: config.whisper_model.clone(),
            openrouter_model: config.openrouter_model.clone(),
            installed_models: self.installed_models().iter().map(|asset| asset.id.to_string()).collect(),
            assets: self.downloader.status_of(ASSETS),
            active_download: self.downloader.active().await,
        }
    }

    /// Runs Parakeet in this process and returns an LRC built from its word
    /// timings. Same stack Dub Studio uses: parakeet-rs over ONNX Runtime,
    /// loaded from the DLL beside the models rather than linked in.
    pub fn parakeet_words(&self, audio: &Path) -> Result<Vec<(f64, String)>> {
        let library = self
            .onnxruntime_library()
            .ok_or_else(|| anyhow!("the ONNX Runtime library is not installed"))?;
        if !self.parakeet_ready() {
            bail!("the Parakeet model is not fully downloaded");
        }
        // Must be set before anything touches `ort`, or it binds to whatever
        // onnxruntime.dll the system happens to have.
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe { std::env::set_var("ORT_DYLIB_PATH", &library) });

        let samples = crate::audio_pcm::decode_mono_16k(audio)
            .with_context(|| format!("decode {} for recognition", audio.display()))?;
        let mut model = parakeet_rs::ParakeetTDT::from_pretrained(&self.parakeet_dir(), None)
            .map_err(|error| anyhow!("load Parakeet: {error}"))?;
        let result = model
            .transcribe_samples(samples, 16_000, 1, Some(parakeet_rs::TimestampMode::Words))
            .map_err(|error| anyhow!("Parakeet transcription failed: {error}"))?;

        let words: Vec<(f64, String)> = result
            .tokens
            .into_iter()
            .filter_map(|token| {
                let text = token.text.trim().to_string();
                (!text.is_empty()).then_some((token.start as f64, text))
            })
            .collect();
        if words.is_empty() {
            bail!("the recogniser found no words to time");
        }
        Ok(words)
    }

    /// Runs whisper.cpp over one audio file and returns the words it heard.
    ///
    /// whisper.cpp writes LRC itself, but only line by line; asking for one
    /// token per line gives the word stream the aligner needs.
    pub fn whisper_words(&self, config: &LyricsSyncConfig, audio: &Path, language: Option<&str>, lyrics: &str) -> Result<Vec<(f64, String)>> {
        let binary = self.whisper_binary().ok_or_else(|| anyhow!("the Whisper runtime is not installed"))?;
        let model = self
            .whisper_model_path(config)
            .ok_or_else(|| anyhow!("no Whisper model is downloaded and selected"))?;

        let work = self.downloader.root().join("work");
        fs::create_dir_all(&work).with_context(|| format!("create {}", work.display()))?;
        let stem = work.join(format!("sync-{}", uuid::Uuid::now_v7()));
        let wav = stem.with_extension("wav");
        crate::audio_pcm::write_wav16k_mono(audio, &wav)
            .with_context(|| format!("decode {} for recognition", audio.display()))?;

        let mut command = Command::new(&binary);
        command
            .arg("--model")
            .arg(&model)
            .arg("--file")
            .arg(&wav)
            .arg("--output-lrc")
            .arg("--output-file")
            .arg(&stem)
            // One token per line: the aligner puts the written lyrics back on
            // top of these times, so what is wanted here is the finest
            // granularity whisper.cpp will emit.
            .arg("--max-len")
            .arg("1")
            .arg("--language")
            .arg(language.unwrap_or("auto"))
            // Biasing the decoder with the words that were actually sung is
            // what every lyric aligner does; without it a dense mix comes back
            // as "[Music]" and there is nothing to align to.
            .arg("--prompt")
            .arg(prompt_from(lyrics))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console(&mut command);

        let status = command.status().with_context(|| format!("run {}", binary.display()))?;
        let produced = stem.with_extension("lrc");
        let lrc = fs::read_to_string(&produced).ok();
        fs::remove_file(&wav).ok();
        fs::remove_file(&produced).ok();

        if !status.success() {
            bail!("whisper-cli exited with {status}");
        }
        let lrc = lrc.context("whisper-cli produced no LRC file")?;
        let words = words_from_lrc(&lrc);
        if words.is_empty() {
            bail!("the recogniser found no words to time");
        }
        Ok(words)
    }
}

/// Groups a word stream into karaoke lines: a new line at a noticeable pause,
/// at sentence-ending punctuation, or once a line grows past comfortable
/// reading length. Ported from the segmentation Dub Studio uses for subtitles.
pub fn group_words(words: &[(f64, String)]) -> Vec<(f64, String)> {
    const PAUSE: f64 = 0.6;
    const MAX_CHARS: usize = 42;

    let mut lines: Vec<(f64, String)> = Vec::new();
    let mut start = 0.0;
    let mut current = String::new();
    let mut previous: Option<f64> = None;

    for (at, word) in words {
        let pause = previous.is_some_and(|last| at - last > PAUSE);
        let too_long = current.chars().count() + word.chars().count() + 1 > MAX_CHARS;
        if !current.is_empty() && (pause || too_long) {
            lines.push((start, std::mem::take(&mut current)));
        }
        if current.is_empty() {
            start = *at;
        } else {
            current.push(' ');
        }
        current.push_str(word);
        if word.ends_with(['.', '!', '?', '…']) {
            lines.push((start, std::mem::take(&mut current)));
        }
        previous = Some(*at);
    }
    if !current.is_empty() {
        lines.push((start, current));
    }
    merge_flashing_lines(lines)
}

/// ACE Step Studio merged LRC lines that begin less than two seconds apart so
/// they do not flash past unread. Karaoke here follows the same rule.
fn merge_flashing_lines(lines: Vec<(f64, String)>) -> Vec<(f64, String)> {
    const MIN_DISPLAY_SECONDS: f64 = 2.0;
    let mut merged: Vec<(f64, String)> = Vec::with_capacity(lines.len());
    for (start, text) in lines {
        match merged.last_mut() {
            Some((previous_start, previous_text)) if start - *previous_start < MIN_DISPLAY_SECONDS => {
                previous_text.push(' ');
                previous_text.push_str(&text);
            }
            _ => merged.push((start, text)),
        }
    }
    merged
}

/// The timed segments in an OpenAI-compatible verbose transcription. A model
/// that answered with plain text yields nothing, and the caller reports that
/// rather than inventing timings.
pub fn segments_from_verbose_json(body: &serde_json::Value) -> Vec<(f64, String)> {
    let mut words = Vec::new();
    if let Some(list) = body.get("words").and_then(|value| value.as_array()) {
        for entry in list {
            let (Some(start), Some(text)) = (entry.get("start").and_then(serde_json::Value::as_f64), entry.get("word").or_else(|| entry.get("text")).and_then(serde_json::Value::as_str)) else {
                continue;
            };
            words.push((start, text.trim().to_string()));
        }
    }
    if !words.is_empty() {
        return group_words(&words);
    }
    let mut segments = Vec::new();
    if let Some(list) = body.get("segments").and_then(|value| value.as_array()) {
        for entry in list {
            let (Some(start), Some(text)) = (entry.get("start").and_then(serde_json::Value::as_f64), entry.get("text").and_then(serde_json::Value::as_str)) else {
                continue;
            };
            segments.push((start, text.trim().to_string()));
        }
    }
    segments
}

/// Puts the track's own lyrics on the recogniser's clock.
///
/// The words are already known - the model sang what it was given - so the
/// recogniser is used for *timing only*, which is what every karaoke aligner
/// worth the name does. Its text is mistrusted: sung vocals are mis-heard
/// constantly, and printing that back as lyrics is how karaoke ends up
/// showing nonsense. Each written line claims the earliest recognised word
/// that resembles it, scanning forward so a repeated chorus consumes its
/// occurrences in order; lines nobody could place are filled in between their
/// neighbours rather than dropped.
pub fn align_lyrics(words: &[(f64, String)], lyrics: &str) -> Vec<(f64, String)> {
    let lines: Vec<&str> = lyrics
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !(line.starts_with('[') && line.ends_with(']')))
        .collect();
    if lines.is_empty() || words.is_empty() {
        return Vec::new();
    }

    let heard: Vec<(f64, String)> = words.iter().map(|(at, word)| (*at, normalise(word))).collect();
    let mut placed: Vec<Option<f64>> = vec![None; lines.len()];
    let mut cursor = 0usize;

    for (index, line) in lines.iter().enumerate() {
        let expected: Vec<String> = line.split_whitespace().map(|word| normalise(word)).filter(|word| !word.is_empty()).collect();
        if expected.is_empty() {
            continue;
        }
        let written = expected.concat();
        let mut best: Option<(f64, usize, f64)> = None;
        for start in cursor..heard.len() {
            // A little slack either side: the recogniser splits words
            // differently from the page.
            let window = (start + expected.len() + 2).min(heard.len());
            let spoken: String = heard[start..window].iter().map(|(_, word)| word.as_str()).collect();
            let score = similarity(&written, &spoken);
            if score > best.map(|(value, _, _)| value).unwrap_or(0.0) {
                best = Some((score, start, heard[start].0));
            }
            // A line rarely starts far past where the previous one ended.
            if start > cursor + 40 {
                break;
            }
        }
        if let Some((score, start, at)) = best {
            if score >= 0.45 {
                placed[index] = Some(at);
                cursor = (start + expected.len()).min(heard.len().saturating_sub(1));
            }
        }
    }

    interpolate(&lines, &mut placed, heard.first().map(|(at, _)| *at).unwrap_or(0.0), heard.last().map(|(at, _)| *at).unwrap_or(0.0));
    lines
        .iter()
        .zip(placed)
        .filter_map(|(line, at)| at.map(|at| (at, (*line).to_string())))
        .collect()
}

/// Lines the recogniser could not place are spread evenly between the ones it
/// could, so a karaoke file has no silent holes.
fn interpolate(lines: &[&str], placed: &mut [Option<f64>], first: f64, last: f64) {
    let mut index = 0;
    while index < lines.len() {
        if placed[index].is_some() {
            index += 1;
            continue;
        }
        let gap_start = index;
        while index < lines.len() && placed[index].is_none() {
            index += 1;
        }
        let before = gap_start.checked_sub(1).and_then(|previous| placed[previous]).unwrap_or(first);
        let after = placed.get(index).copied().flatten().unwrap_or(last.max(before));
        let steps = (index - gap_start + 1) as f64;
        for (offset, slot) in placed[gap_start..index].iter_mut().enumerate() {
            *slot = Some(before + (after - before) * ((offset + 1) as f64 / steps));
        }
    }
}

fn normalise(word: &str) -> String {
    word.chars().filter(|character| character.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// How much of the written line the recogniser heard, compared character by
/// character rather than word by word.
///
/// Sung vocals come back mangled - "on the glass" as "arms the grass" - and a
/// word-level comparison scores that as a miss even though the line is plainly
/// the right one. Comparing characters in order tolerates the mangling, which
/// is what character-level aligners do for exactly this reason.
fn similarity(expected: &str, heard: &str) -> f64 {
    if expected.is_empty() || heard.is_empty() {
        return 0.0;
    }
    let left: Vec<char> = expected.chars().collect();
    let right: Vec<char> = heard.chars().collect();
    let mut previous = vec![0usize; right.len() + 1];
    let mut current = vec![0usize; right.len() + 1];
    for l in 0..left.len() {
        for r in 0..right.len() {
            current[r + 1] = if left[l] == right[r] { previous[r] + 1 } else { current[r].max(previous[r + 1]) };
        }
        std::mem::swap(&mut previous, &mut current);
        current.iter_mut().for_each(|value| *value = 0);
    }
    previous[right.len()] as f64 / left.len() as f64
}

/// Turns timed segments into an LRC body. Used by the providers that return
/// structured timings rather than a file.
pub fn lrc_from_segments(segments: &[(f64, String)]) -> String {
    let mut out = String::new();
    for (start, text) in segments {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let total = start.max(0.0);
        let minutes = (total as u64) / 60;
        let seconds = (total as u64) % 60;
        let hundredths = ((total - total.floor()) * 100.0).round() as u64;
        out.push_str(&format!("[{minutes:02}:{seconds:02}.{hundredths:02}]{text}\n"));
    }
    out
}

/// The opening lines of the lyrics, as a decoding hint. Whisper's prompt is
/// bounded, and the first lines are enough to tell it this is singing, in this
/// language, about these words.
fn prompt_from(lyrics: &str) -> String {
    let mut prompt = String::new();
    for line in lyrics.lines().map(str::trim).filter(|line| !line.is_empty() && !(line.starts_with('[') && line.ends_with(']'))) {
        if prompt.chars().count() + line.chars().count() > 400 {
            break;
        }
        if !prompt.is_empty() {
            prompt.push(' ');
        }
        prompt.push_str(line);
    }
    prompt
}

/// Reads a whisper.cpp LRC back as a word stream.
pub fn words_from_lrc(lrc: &str) -> Vec<(f64, String)> {
    let mut words = Vec::new();
    for line in lrc.lines() {
        let Some(rest) = line.strip_prefix('[') else { continue };
        let Some((stamp, text)) = rest.split_once(']') else { continue };
        let Some((minutes, seconds)) = stamp.split_once(':') else { continue };
        let (Ok(minutes), Ok(seconds)) = (minutes.trim().parse::<f64>(), seconds.trim().parse::<f64>()) else {
            continue;
        };
        let text = text.trim();
        if !text.is_empty() {
            words.push((minutes * 60.0 + seconds, text.to_string()));
        }
    }
    words
}

/// The timestamps in an LRC body, in seconds. Also the emptiness check: a file
/// with no timestamps is not karaoke, however much text it contains.
pub fn parse_lrc_times(lrc: &str) -> Vec<f64> {
    let mut times = Vec::new();
    for line in lrc.lines() {
        let Some(rest) = line.strip_prefix('[') else { continue };
        let Some((stamp, _)) = rest.split_once(']') else { continue };
        let Some((minutes, seconds)) = stamp.split_once(':') else { continue };
        let (Ok(minutes), Ok(seconds)) = (minutes.trim().parse::<f64>(), seconds.trim().parse::<f64>()) else {
            continue;
        };
        times.push(minutes * 60.0 + seconds);
    }
    times
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_become_a_playable_lrc_body() {
        let lrc = lrc_from_segments(&[
            (0.0, "neon on the glass".into()),
            (12.5, "driving home".into()),
            (75.25, "  ".into()),
            (81.5, "the engine dies".into()),
        ]);
        assert_eq!(lrc, "[00:00.00]neon on the glass\n[00:12.50]driving home\n[01:21.50]the engine dies\n");
        assert_eq!(parse_lrc_times(&lrc), vec![0.0, 12.5, 81.5]);
    }

    #[test]
    fn words_become_lines_at_pauses_and_never_flash_past() {
        let words = vec![
            (0.0, "neon".into()),
            (0.4, "on".into()),
            (0.8, "the".into()),
            (1.2, "glass".into()),
            // A long pause would start a new line, but it lands inside the two
            // second window ACE used, so it joins the line before it.
            (2.9, "driving".into()),
            (3.3, "home".into()),
            (12.0, "the".into()),
            (12.4, "engine".into()),
            (12.9, "dies".into()),
        ];
        let lines = group_words(&words);
        assert_eq!(
            lines,
            vec![
                (0.0, "neon on the glass".to_string()),
                (2.9, "driving home".to_string()),
                (12.0, "the engine dies".to_string()),
            ]
        );
    }

    #[test]
    fn a_verbose_transcription_yields_timed_lines_and_plain_text_yields_none() {
        let verbose = serde_json::json!({
            "text": "neon on the glass",
            "segments": [{ "start": 1.5, "text": " neon on the glass" }, { "start": 9.0, "text": "driving home" }]
        });
        assert_eq!(
            segments_from_verbose_json(&verbose),
            vec![(1.5, "neon on the glass".to_string()), (9.0, "driving home".to_string())]
        );
        assert!(segments_from_verbose_json(&serde_json::json!({ "text": "no timings here" })).is_empty());
    }

    #[test]
    fn the_written_lyrics_are_kept_and_only_the_timing_is_borrowed() {
        // What the recogniser heard: two words wrong, as sung vocals go.
        let heard = vec![
            (1.0, "neon".into()),
            (1.4, "arms".into()),   // "on" misheard
            (1.8, "the".into()),
            (2.2, "grass".into()),  // "glass" misheard
            (9.0, "driving".into()),
            (9.6, "home".into()),
        ];
        let lyrics = "[verse]
Neon on the glass
Driving home
";
        let lines = align_lyrics(&heard, lyrics);

        // The user's words, on the recogniser's clock - never the mishearing.
        assert_eq!(lines, vec![(1.0, "Neon on the glass".to_string()), (9.0, "Driving home".to_string())]);
    }

    #[test]
    fn a_line_nobody_could_place_is_filled_in_between_its_neighbours() {
        let heard = vec![(0.0, "first".into()), (10.0, "third".into())];
        let lines = align_lyrics(&heard, "First
Something entirely unheard
Third");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].0, 0.0);
        assert!(lines[1].0 > 0.0 && lines[1].0 < 10.0, "the middle line got {}", lines[1].0);
        assert_eq!(lines[2].0, 10.0);
    }

    #[test]
    fn text_without_timestamps_is_not_karaoke() {
        assert!(parse_lrc_times("just some lyrics\nand another line").is_empty());
    }

    #[test]
    fn the_whisper_runtime_is_pinned_and_every_asset_is_distinct() {
        for entry in ASSETS {
            assert!(entry.bytes > 0, "{} has no size", entry.id);
            assert!(entry.url.starts_with("https://"), "{} is not fetched over https", entry.id);
            assert_eq!(ASSETS.iter().filter(|other| other.id == entry.id).count(), 1);
            if entry.kind == AssetKind::Runtime {
                // Every runtime is pinned to an exact release - whisper.cpp to
                // its tag, ONNX Runtime to its version - so a working setup
                // keeps working.
                let pinned = entry.url.contains(WHISPER_BUILD) || entry.url.contains(ONNXRUNTIME_BUILD);
                assert!(pinned, "{} is not pinned to a release", entry.id);
                assert!(!entry.marker.is_empty(), "{} has no proof of extraction", entry.id);
            }
        }
    }

    #[test]
    fn karaoke_stays_off_until_a_provider_is_chosen() {
        let mut config = LyricsSyncConfig::default();
        assert!(!config.available());
        config.enabled = true;
        assert!(!config.available());
        config.provider = AsrProvider::Whisper;
        assert!(config.available());
    }
}
