//! Handing a finished track back to the model.
//!
//! Music3 writes music but cannot read it: MiniMax published the model without
//! the encoder that turns audio into the codes it generates. The encoder was
//! reconstructed by the community and exported to ONNX, so the whole path runs
//! on parts the studio already has:
//!
//! ```text
//! track.mp3
//!   -> neural-codec   audio -> VAE latents            (ships with the engine)
//!   -> this module    latents -> 8 codes per frame    (ONNX Runtime)
//!   -> mm-server      codes -> audio                  (the audio_codes field)
//! ```
//!
//! What the engine does with those codes, read from its own replay path rather
//! than assumed: `audio_codes` replaces the autoregressive stage entirely. The
//! language model is fed the codes teacher-forced and the track is rendered
//! from them - a re-render of the whole piece. The caption and the lyrics still
//! build the prompt the codes are replayed through, so the music is the one
//! that was read and the sound is the one that was asked for. Continuing past
//! the end and repainting a section are not in the engine: they need the codes
//! to be a prefix or a masked span, and the replay path takes neither.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::downloads::{Asset, AssetKind, Downloader};

/// The shape of the encoder's world, taken from the reference implementation
/// rather than derived: `training/dataset.py` in the engine's repository.
///
/// None of this is a ratio anyone can guess. The latent timeline is stitched
/// out of the DiT's overlapping windows, so a frame's latents are not at
/// `frame * 86.13 / 25` - they are wherever the stitch put them. The encoder
/// sees the track through a fixed window of 128 frames padded to 448 latents.
/// Feeding it a whole track with an evenly spaced pooling matrix, which is what
/// this did first, asks a question the model was never trained to answer.
const LATENT_CHANNELS: usize = 128;
const FRAMES_PER_WIN: usize = 128;
const LATENT_WINDOW_MAX: usize = 448;
/// Latents per frame, exactly: (44100 / 512) / 25.
const RATIO_NUM: i64 = 441;
const RATIO_DEN: i64 = 128;
/// The DiT denoises 200 frames at a time and hops 100; the stitched timeline
/// advances 345 latents per hop.
const CHUNK_FRAMES: i64 = 200;
const CHUNK_HOP: i64 = 100;
const HOP_LATENTS: i64 = 345;
/// The first frame a window other than the first one owns.
const OWNED_FROM: i64 = 25;
/// Codes per frame: one semantic, seven acoustic.
const CODES_PER_FRAME: usize = 8;

pub const ENCODER: Asset = Asset {
    id: "rvq-encoder",
    label: "RVQ encoder (audio in)",
    kind: AssetKind::Model,
    url: "https://huggingface.co/nerualdreming/open-rvq-encoder-minimax-music3-169m-v4-onnx/resolve/89544ca58ac8ce90bde5f33012d2a6cf8f4349ed/rvq-encoder-169m-v4.onnx",
    relative_path: "models/rvq-encoder/rvq-encoder-169m-v4.onnx",
    bytes: 676_327_026,
    unzip_into: None,
    marker: "",
    pick: &[],
    vram_gb: Some(2),
    note: "Lets the studio read a finished track and render it again.",
};

/// Reads audio into the codes the engine renders from.
pub struct AudioInput {
    downloader: Downloader,
    /// `neural-codec.exe`, built and shipped alongside the engine.
    codec: PathBuf,
    /// The vocoder's weights. The codec is the engine's own tool and reads
    /// audio with the same VAE the model was trained against, so it has to be
    /// handed those weights - they are already on disk as part of the set.
    vae: Option<PathBuf>,
}

impl AudioInput {
    pub fn new(data_root: &Path, engine_bundle: &Path) -> Self {
        let name = if cfg!(windows) { "neural-codec.exe" } else { "neural-codec" };
        let vae = data_root.join("models").join("minimaxmusic-cpp").join("MiniMax-Music3-vocoder-F32.gguf");
        Self {
            downloader: Downloader::new(data_root.join("audio-input")),
            codec: engine_bundle.join(name),
            vae: vae.is_file().then_some(vae),
        }
    }

    pub fn downloader(&self) -> &Downloader {
        &self.downloader
    }

    /// Whether both halves are present: the codec beside the engine, and the
    /// encoder on disk.
    pub fn is_ready(&self) -> bool {
        self.codec.is_file() && self.downloader.is_installed(&ENCODER)
    }

    pub fn encoder_path(&self) -> PathBuf {
        self.downloader.root().join(ENCODER.relative_path)
    }

    pub async fn install(&self) -> Result<()> {
        self.downloader.install_all("audio-input", &[&ENCODER]).await
    }

    /// A finished track as the codes the engine renders from.
    ///
    /// The reading follows `training/eval-e2e.py`: whole windows of 128 frames,
    /// then one last window shifted back to end on the final frame, keeping
    /// only the frames the whole ones did not cover.
    pub fn codes_of(&self, audio: &Path, on_gpu: bool) -> Result<String> {
        let encoder = self.encoder_path();
        if !encoder.is_file() {
            bail!("the audio encoder is not downloaded yet");
        }
        let work = self.downloader.root().join("work");
        std::fs::create_dir_all(&work).with_context(|| format!("create {}", work.display()))?;
        let latents = work.join(format!("input-{}.vae", uuid::Uuid::now_v7()));
        let read = self.read_track(&encoder, audio, &latents, on_gpu);
        std::fs::remove_file(&latents).ok();
        read
    }

    fn read_track(&self, encoder: &Path, audio: &Path, latents_path: &Path, on_gpu: bool) -> Result<String> {
        self.write_latents(audio, latents_path)?;
        let latent_frames = std::fs::metadata(latents_path)?.len() as usize / (LATENT_CHANNELS * 4);
        let (frames, starts) = track_frames(latent_frames);
        if frames < FRAMES_PER_WIN {
            bail!("{} is too short to read: the encoder sees {FRAMES_PER_WIN} frames at a time", audio.display());
        }

        let mut builder = ort::session::Session::builder().context("prepare an ONNX session")?;
        if on_gpu {
            // `error_on_failure`, because a provider that registers and then
            // quietly runs on the processor looks like a slow encoder.
            match builder.clone().with_execution_providers([ort::ep::CUDA::default().build().error_on_failure()]) {
                Ok(with_cuda) => builder = with_cuda,
                Err(error) => eprintln!("the card provider did not register, reading on the processor: {error}"),
            }
        }
        let mut session = builder
            .commit_from_file(encoder)
            .with_context(|| format!("load the audio encoder {}", encoder.display()))?;

        let mut codes: Vec<i64> = Vec::with_capacity(frames * CODES_PER_FRAME);
        let mut covered = 0usize;
        while covered + FRAMES_PER_WIN <= frames {
            codes.extend(predict_window(&mut session, latents_path, &starts, covered)?);
            covered += FRAMES_PER_WIN;
        }
        if covered < frames {
            // The tail, from a window that ends on the last frame: only the
            // part the whole windows did not reach is kept.
            let window = predict_window(&mut session, latents_path, &starts, frames - FRAMES_PER_WIN)?;
            let keep = frames - covered;
            codes.extend_from_slice(&window[(FRAMES_PER_WIN - keep) * CODES_PER_FRAME..]);
        }

        // The engine's replay wants a warm-up frame that renders no audio. It
        // is a copy of the first predicted frame, so every code the render is
        // built from was predicted - the track's true first frame, which the
        // encoder never saw, used to stand here and leaked into the result.
        let mut stream = Vec::with_capacity(codes.len() + CODES_PER_FRAME);
        stream.extend_from_slice(&codes[..CODES_PER_FRAME]);
        stream.extend_from_slice(&codes);
        Ok(codes_to_request(&stream))
    }

    /// Turns a finished track into VAE latents with the engine's own codec, so
    /// they are exactly what the model's decoder was trained against.
    fn write_latents(&self, audio: &Path, destination: &Path) -> Result<()> {
        if !self.codec.is_file() {
            bail!("neural-codec is missing from the engine bundle: {}", self.codec.display());
        }
        // The codec's interface, read from the binary rather than assumed.
        let vae = self
            .vae
            .as_ref()
            .context("the vocoder weights are not installed; the codec cannot read audio without them")?;
        let output = std::process::Command::new(&self.codec)
            .arg("--vae")
            .arg(vae)
            .arg("--encode")
            .arg("-i")
            .arg(audio)
            .arg("-o")
            .arg(destination)
            .output()
            .with_context(|| format!("run {}", self.codec.display()))?;
        if !output.status.success() {
            bail!("neural-codec failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(())
    }
}

/// Where every frame boundary sits on the stitched latent timeline.
///
/// A frame belongs to the DiT window that owns it, and its latent start is that
/// window's stitch offset plus the ceiling boundary of the local frame under
/// the window's own ratio. Ported from `frame_latent_starts` in the engine's
/// `training/dataset.py`.
fn frame_latent_starts(frames: usize) -> Vec<i64> {
    let frames = frames as i64;
    let windows = std::cmp::max(1, (frames - 1) / CHUNK_HOP);
    (0..=frames)
        .map(|t| {
            let k = (t - OWNED_FROM).div_euclid(CHUNK_HOP).clamp(0, windows - 1);
            let tau = t - k * CHUNK_HOP;
            let span = std::cmp::min(CHUNK_FRAMES, frames - k * CHUNK_HOP);
            let latents = span * RATIO_NUM / RATIO_DEN;
            k * HOP_LATENTS + (tau * latents + span - 1) / span
        })
        .collect()
}

/// The longest track whose stitched window starts stay inside the latent file.
fn track_frames(latent_frames: usize) -> (usize, Vec<i64>) {
    let mut frames = latent_frames * FRAMES_PER_WIN / RATIO_NUM as usize;
    while frames > 0 {
        let starts = frame_latent_starts(frames);
        if starts[frames] <= latent_frames as i64 {
            return (frames, starts);
        }
        frames -= 1;
    }
    (0, vec![0])
}

/// One window of 128 frames: the latents it covers, padded to the width the
/// encoder takes, and the pooling matrix that averages them onto the frames.
fn predict_window(session: &mut ort::session::Session, latents_path: &Path, starts: &[i64], t0: usize) -> Result<Vec<i64>> {
    use std::io::{Read, Seek, SeekFrom};

    let bounds: Vec<i64> = starts[t0..=t0 + FRAMES_PER_WIN].iter().map(|value| value - starts[t0]).collect();
    let count = *bounds.last().unwrap_or(&0) as usize;
    if count > LATENT_WINDOW_MAX {
        bail!("a window covers {count} latents, more than the {LATENT_WINDOW_MAX} the encoder takes");
    }

    let mut latents = vec![0f32; LATENT_WINDOW_MAX * LATENT_CHANNELS];
    let mut file = std::fs::File::open(latents_path).with_context(|| format!("open {}", latents_path.display()))?;
    file.seek(SeekFrom::Start(starts[t0] as u64 * (LATENT_CHANNELS * 4) as u64))
        .context("seek to the window's latents")?;
    let mut bytes = vec![0u8; count * LATENT_CHANNELS * 4];
    file.read_exact(&mut bytes).context("read the window's latents")?;
    for (index, chunk) in bytes.chunks_exact(4).enumerate() {
        latents[index] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }

    let mut pool = vec![0f32; FRAMES_PER_WIN * LATENT_WINDOW_MAX];
    for frame in 0..FRAMES_PER_WIN {
        let (from, to) = (bounds[frame] as usize, bounds[frame + 1] as usize);
        if to <= from {
            continue;
        }
        let weight = 1.0 / (to - from) as f32;
        for latent in from..to {
            pool[frame * LATENT_WINDOW_MAX + latent] = weight;
        }
    }

    let latent_input = ort::value::Value::from_array(([1usize, LATENT_WINDOW_MAX, LATENT_CHANNELS], latents))
        .context("build the latent input")?;
    let pool_input =
        ort::value::Value::from_array(([1usize, FRAMES_PER_WIN, LATENT_WINDOW_MAX], pool)).context("build the pooling input")?;
    let outputs = session
        .run(ort::inputs!["latents" => latent_input, "pool" => pool_input])
        .context("run the audio encoder")?;
    let (shape, data) = outputs[0].try_extract_tensor::<i64>().context("read the codes")?;
    let per_frame = shape.last().copied().unwrap_or(0) as usize;
    if per_frame != CODES_PER_FRAME {
        bail!("the encoder returned {per_frame} codes per frame, not {CODES_PER_FRAME}");
    }
    Ok(data.to_vec())
}

/// The codes as the engine wants them: comma-separated, eight per frame.
pub fn codes_to_request(codes: &[i64]) -> String {
    codes.iter().map(|code| code.to_string()).collect::<Vec<_>>().join(",")
}

/// How many frames a code string carries.
pub fn frames_in(codes: &str) -> usize {
    codes.split(',').filter(|value| !value.trim().is_empty()).count() / CODES_PER_FRAME
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stitched timeline, against numbers produced by the reference itself
    /// rather than by arithmetic written here a second time. Run in the engine's
    /// repository:
    ///
    /// ```text
    /// python -c "from training.dataset import frame_latent_starts as f; print(list(f(300))[:6], list(f(300))[98:103], f(300)[300])"
    /// ```
    #[test]
    fn frame_boundaries_match_the_reference() {
        for frames in [300usize, 512, 1000] {
            let starts = frame_latent_starts(frames);
            assert_eq!(starts.len(), frames + 1);
            assert_eq!(starts[..6], [0, 4, 7, 11, 14, 18], "the first frames of a {frames} frame track");
            assert_eq!(starts[98..103], [338, 342, 345, 348, 352], "across the first stitch seam");
            assert!(starts.windows(2).all(|pair| pair[1] >= pair[0]), "boundaries never go backwards");
        }
        assert_eq!(frame_latent_starts(300)[300], 1034);
        assert_eq!(frame_latent_starts(512)[512], 1765);
        assert_eq!(frame_latent_starts(1000)[1000], 3449);
    }

    /// A window never asks for more latents than the encoder's input holds -
    /// the padded width is what the model was exported with.
    #[test]
    fn no_window_overflows_the_encoders_input() {
        for frames in [128usize, 200, 250, 512, 1000, 4500] {
            let starts = frame_latent_starts(frames);
            for t0 in (0..=frames - FRAMES_PER_WIN).step_by(17) {
                let span = starts[t0 + FRAMES_PER_WIN] - starts[t0];
                assert!(span <= LATENT_WINDOW_MAX as i64, "{frames} frames, window at {t0} covers {span} latents");
            }
        }
    }

    /// The frame count comes from the latent file and never claims frames whose
    /// latents are not in it.
    #[test]
    fn the_track_never_claims_latents_it_does_not_have() {
        for latents in [441usize, 1000, 5000, 20_000] {
            let (frames, starts) = track_frames(latents);
            assert!(starts[frames] <= latents as i64, "{frames} frames want {} of {latents} latents", starts[frames]);
        }
    }

    #[test]
    fn a_code_string_reads_back_as_frames() {
        let codes: Vec<i64> = (0..24).collect();
        let text = codes_to_request(&codes);
        assert_eq!(frames_in(&text), 3);
        assert!(text.starts_with("0,1,2,3,4,5,6,7,8"));
    }
}
