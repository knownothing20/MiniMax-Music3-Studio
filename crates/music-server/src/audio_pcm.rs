//! Decoding a finished track into what a recogniser can read.
//!
//! Library audio is MP3 or 24-bit WAV at 44.1 kHz; every speech recogniser here
//! wants 16 kHz mono 16-bit PCM. Doing this in-process keeps the promise that
//! the local runtime needs no Python and no ffmpeg on the user's machine.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const TARGET_RATE: u32 = 16_000;

/// Decodes `input` to 16 kHz mono samples, which is what every recogniser here
/// expects; Parakeet reads them straight out of memory.
pub fn decode_mono_16k(input: &Path) -> Result<Vec<f32>> {
    let (samples, rate) = decode_mono(input)?;
    if samples.is_empty() {
        bail!("{} decoded to no audio", input.display());
    }
    Ok(resample(&samples, rate, TARGET_RATE))
}

/// Decodes `input`, mixes it to mono, resamples to 16 kHz and writes a WAV.
pub fn write_wav16k_mono(input: &Path, output: &Path) -> Result<()> {
    write_wav(output, &decode_mono_16k(input)?)
}

/// Every channel averaged into one, at the file's own sample rate.
fn decode_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .with_context(|| format!("recognise the format of {}", path.display()))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .context("the file carries no decodable audio track")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("no decoder for this audio")?;

    let mut mono = Vec::new();
    let mut rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let mut buffer: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // The end of the stream arrives as an error from this API.
            Err(symphonia::core::errors::Error::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(error) => return Err(error).context("read audio packet"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(error) => return Err(error).context("decode audio packet"),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let channels = spec.channels.count().max(1);
        let target = buffer.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        target.copy_interleaved_ref(decoded);
        for frame in target.samples().chunks(channels) {
            mono.push(frame.iter().sum::<f32>() / channels as f32);
        }
    }

    Ok((mono, rate))
}

/// Linear resampling. The recogniser mel-filters everything down to 80 bands
/// anyway, so a sharper filter would buy nothing here.
fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.len() < 2 {
        return samples.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let length = ((samples.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(length);
    for index in 0..length {
        let position = index as f64 * ratio;
        let left = position.floor() as usize;
        let right = (left + 1).min(samples.len() - 1);
        let weight = (position - left as f64) as f32;
        out.push(samples[left] * (1.0 - weight) + samples[right] * weight);
    }
    out
}

fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut out = BufWriter::new(file);
    let data_bytes = (samples.len() * 2) as u32;

    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_bytes).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&1u16.to_le_bytes())?; // mono
    out.write_all(&TARGET_RATE.to_le_bytes())?;
    out.write_all(&(TARGET_RATE * 2).to_le_bytes())?; // byte rate
    out.write_all(&2u16.to_le_bytes())?; // block align
    out.write_all(&16u16.to_le_bytes())?; // bits per sample
    out.write_all(b"data")?;
    out.write_all(&data_bytes.to_le_bytes())?;
    for sample in samples {
        let clamped = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.write_all(&clamped.to_le_bytes())?;
    }
    out.flush().context("finish writing the decoded WAV")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_halves_the_length_when_the_rate_halves() {
        let input: Vec<f32> = (0..1000).map(|index| (index as f32 / 100.0).sin()).collect();
        let out = resample(&input, 32_000, 16_000);
        assert_eq!(out.len(), 500);
        assert!((out[0] - input[0]).abs() < 1e-6);
    }

    #[test]
    fn a_written_wav_carries_a_readable_header() {
        let path = std::env::temp_dir().join(format!("pcm-{}.wav", uuid::Uuid::now_v7()));
        write_wav(&path, &[0.0, 0.5, -0.5]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 16_000);
        assert_eq!(bytes.len(), 44 + 6);
        std::fs::remove_file(&path).ok();
    }
}
