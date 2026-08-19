//! Handing a finished track back to the model.
//!
//! Music3 writes music but cannot read it: MiniMax published the model without
//! the encoder that turns audio into the codes it generates. That single gap is
//! what makes the studio a slot machine - a chorus you dislike means generating
//! the whole song again and losing everything you liked.
//!
//! The encoder was reconstructed by the community and exported to ONNX, so the
//! whole path runs on parts the studio already has:
//!
//! ```text
//! track.mp3
//!   -> neural-codec   audio -> VAE latents at 86.13 Hz   (ships with the engine)
//!   -> this module    latents -> 8 codes per frame       (ONNX Runtime)
//!   -> mm-server      codes -> audio                     (the audio_codes field)
//! ```
//!
//! No second implementation of the network in C++, no fork of the engine: the
//! runtime for it is already here for stem separation and for Parakeet.
//!
//! What the engine does with those codes, read from its own replay path rather
//! than assumed: `audio_codes` replaces the autoregressive stage entirely. The
//! language model is fed the codes teacher-forced and the track is rendered
//! from them - so this is a re-render of the whole piece, deterministic and
//! complete. Continuing past the end and repainting a section are not in the
//! engine: they need the codes to be a prefix or a masked span, and the replay
//! path takes neither. ACE-Step exposes both because its own pipeline supports
//! them; here they would have to be built in the engine first.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::downloads::{Asset, AssetKind, Downloader};

/// The frame rate the model itself works at.
const MODEL_FRAMES_PER_SECOND: f64 = 25.0;
/// The frame rate the VAE latents come out at.
const LATENT_FRAMES_PER_SECOND: f64 = 86.1328125;
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
    note: "Lets the studio read a finished track: continue it, replace a section, or write an intro in front of it.",
};

/// Reads audio into the codes the engine renders from.
pub struct AudioInput {
    downloader: Downloader,
    /// `neural-codec.exe`, built and shipped alongside the engine.
    codec: PathBuf,
}

impl AudioInput {
    pub fn new(data_root: &Path, engine_bundle: &Path) -> Self {
        let name = if cfg!(windows) { "neural-codec.exe" } else { "neural-codec" };
        Self { downloader: Downloader::new(data_root.join("audio-input")), codec: engine_bundle.join(name) }
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

    /// Turns a finished track into VAE latents.
    ///
    /// The codec is the engine's own tool, so the latents are exactly what the
    /// model's decoder was trained against - no resampling of our own, no
    /// second opinion about what the audio means.
    pub fn latents_of(&self, audio: &Path, destination: &Path) -> Result<Latents> {
        if !self.codec.is_file() {
            bail!("neural-codec is missing from the engine bundle: {}", self.codec.display());
        }
        let output = std::process::Command::new(&self.codec)
            .arg("encode")
            .arg(audio)
            .arg(destination)
            .output()
            .with_context(|| format!("run {}", self.codec.display()))?;
        if !output.status.success() {
            bail!("neural-codec failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Latents::read(destination)
    }
}

/// A `.vae` file: flat f32 frames of 128 channels, no header.
pub struct Latents {
    pub channels: usize,
    pub frames: usize,
    pub values: Vec<f32>,
}

impl Latents {
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let channels = 128;
        if bytes.len() % (channels * 4) != 0 {
            bail!("{} is not a whole number of {channels}-channel frames", path.display());
        }
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        Ok(Self { channels, frames: values.len() / channels, values })
    }

    /// How many model frames this many latent frames become.
    pub fn model_frames(&self) -> usize {
        ((self.frames as f64) * MODEL_FRAMES_PER_SECOND / LATENT_FRAMES_PER_SECOND).floor() as usize
    }

    /// The pooling matrix the encoder expects: each model frame averages the
    /// latent frames that fall inside it. Written out rather than inferred,
    /// because the two rates do not divide evenly - 86.1328125 into 25 - and a
    /// rounded ratio drifts audibly over a three-minute track.
    pub fn pooling_matrix(&self) -> Vec<f32> {
        let frames = self.model_frames();
        let mut pool = vec![0.0f32; frames * self.frames];
        for frame in 0..frames {
            let start = ((frame as f64) * LATENT_FRAMES_PER_SECOND / MODEL_FRAMES_PER_SECOND).floor() as usize;
            let end = (((frame + 1) as f64) * LATENT_FRAMES_PER_SECOND / MODEL_FRAMES_PER_SECOND).ceil() as usize;
            let end = end.min(self.frames).max(start + 1);
            let weight = 1.0 / (end - start) as f32;
            for latent in start..end {
                pool[frame * self.frames + latent] = weight;
            }
        }
        pool
    }
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

    /// The two rates do not divide evenly, so every model frame has to claim at
    /// least one latent frame and none may be claimed by nobody - otherwise the
    /// track drifts against its own codes.
    #[test]
    fn pooling_covers_every_frame_without_gaps() {
        // Ten seconds is 861.33 latent frames, not 861: the rates do not divide
        // evenly, and rounding that ratio is exactly what makes a long track
        // drift against its own codes.
        let latents = Latents { channels: 128, frames: 862, values: vec![0.0; 862 * 128] };
        let frames = latents.model_frames();
        assert_eq!(frames, 250, "862 latent frames is just over ten seconds, so 250 model frames");

        let pool = latents.pooling_matrix();
        for frame in 0..frames {
            let row = &pool[frame * latents.frames..(frame + 1) * latents.frames];
            let sum: f32 = row.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "frame {frame} averages to {sum}, not 1");
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
