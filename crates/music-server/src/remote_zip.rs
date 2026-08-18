//! Taking a few files out of a large remote archive.
//!
//! NVIDIA ships its libraries as archives that carry far more than a program
//! needs: cuDNN is 1.8 GB, of which the separator uses about half, and the rest
//! is static libraries, headers and kernels for work this studio never does.
//! Downloading all of it to throw most of it away is not a reasonable thing to
//! ask of someone's connection.
//!
//! A zip's index sits at the end of the file, and every entry records where its
//! own bytes start. With range requests that is enough to read the index, then
//! fetch only the entries wanted - so a 1.8 GB archive costs the 500 MB that is
//! actually used.

use std::io::{Read, Seek, SeekFrom};

use anyhow::{bail, Context, Result};

/// A remote file read through range requests, seekable enough for a zip reader.
pub struct RangeReader {
    url: String,
    client: reqwest::blocking::Client,
    position: u64,
    length: u64,
    /// The last span fetched, kept so sequential reading costs one request per
    /// chunk instead of one per buffer: the difference is a download that runs
    /// at the line's speed rather than at the round-trip time.
    chunk: Vec<u8>,
    chunk_start: u64,
}

/// How much is fetched at once while reading forward.
const CHUNK: u64 = 16 * 1024 * 1024;

impl RangeReader {
    pub fn open(url: &str) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent("MiniMax-Music3-Studio")
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .context("prepare an HTTP client")?;
        // The length comes from a one-byte range rather than from HEAD: NVIDIA's
        // downloads answer HEAD without a length, and a zero-length archive
        // reads as an empty one - no index, no error, nothing.
        let probe = client
            .get(url)
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()
            .with_context(|| format!("ask about {url}"))?;
        if !probe.status().is_success() {
            bail!("{url} answered {}", probe.status());
        }
        let length = probe
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit('/').next().and_then(|total| total.trim().parse::<u64>().ok()))
            .or_else(|| probe.content_length().filter(|value| *value > 1))
            .context("the server did not say how large the archive is")?;
        if length == 0 {
            bail!("{url} reports no length, so nothing can be read out of it");
        }
        Ok(Self { url: url.to_string(), client, position: 0, length, chunk: Vec::new(), chunk_start: 0 })
    }

    pub fn length(&self) -> u64 {
        self.length
    }
}

impl RangeReader {
    /// Makes sure the current position is inside the buffered span.
    fn fill(&mut self) -> std::io::Result<()> {
        let inside = self.position >= self.chunk_start
            && self.position < self.chunk_start + self.chunk.len() as u64;
        if inside {
            return Ok(());
        }
        let start = self.position;
        let end = (start + CHUNK - 1).min(self.length - 1);
        let response = self
            .client
            .get(&self.url)
            .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
            .send()
            .map_err(std::io::Error::other)?;
        if !response.status().is_success() {
            return Err(std::io::Error::other(format!("{} answered {}", self.url, response.status())));
        }
        self.chunk = response.bytes().map_err(std::io::Error::other)?.to_vec();
        self.chunk_start = start;
        Ok(())
    }
}

impl Read for RangeReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.length || buffer.is_empty() {
            return Ok(0);
        }
        self.fill()?;
        let offset = (self.position - self.chunk_start) as usize;
        if offset >= self.chunk.len() {
            return Ok(0);
        }
        let taken = (self.chunk.len() - offset).min(buffer.len());
        buffer[..taken].copy_from_slice(&self.chunk[offset..offset + taken]);
        self.position += taken as u64;
        Ok(taken)
    }
}

impl Seek for RangeReader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.position = match to {
            SeekFrom::Start(offset) => offset,
            SeekFrom::End(offset) => (self.length as i64 + offset).max(0) as u64,
            SeekFrom::Current(offset) => (self.position as i64 + offset).max(0) as u64,
        };
        Ok(self.position)
    }
}


/// How many range requests are in flight at once. Enough to fill a fast line,
/// few enough that a slow one is not drowned in connections.
const PARALLEL: usize = 8;

/// Fetches one byte span with several parallel range requests.
fn fetch_span(
    client: &std::sync::Arc<reqwest::blocking::Client>,
    url: &str,
    start: u64,
    length: u64,
    progress: &mut dyn FnMut(u64),
) -> Result<Vec<u8>> {
    if length == 0 {
        return Ok(Vec::new());
    }
    let piece = (length / PARALLEL as u64).max(4 * 1024 * 1024);
    let mut ranges = Vec::new();
    let mut offset = 0u64;
    while offset < length {
        let take = piece.min(length - offset);
        ranges.push((offset, take));
        offset += take;
    }

    let done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let parts: Vec<Result<(usize, Vec<u8>)>> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (index, (offset, take)) in ranges.iter().copied().enumerate() {
            let client = client.clone();
            let done = done.clone();
            let url = url.to_string();
            handles.push(scope.spawn(move || -> Result<(usize, Vec<u8>)> {
                let first = start + offset;
                let last = first + take - 1;
                let response = client
                    .get(&url)
                    .header(reqwest::header::RANGE, format!("bytes={first}-{last}"))
                    .send()
                    .with_context(|| format!("fetch bytes {first}-{last}"))?;
                if !response.status().is_success() {
                    bail!("{url} answered {}", response.status());
                }
                let bytes = response.bytes().context("read a range")?;
                done.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
                Ok((index, bytes.to_vec()))
            }));
        }
        // Report while the threads work, so a big file does not look stuck.
        let total: u64 = length;
        while handles.iter().any(|handle| !handle.is_finished()) {
            std::thread::sleep(std::time::Duration::from_millis(200));
            progress(done.load(std::sync::atomic::Ordering::Relaxed).min(total));
        }
        handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| bail!("a range request panicked"))).collect()
    });

    let mut pieces: Vec<Option<Vec<u8>>> = (0..ranges.len()).map(|_| None).collect();
    for part in parts {
        let (index, bytes) = part?;
        pieces[index] = Some(bytes);
    }
    let mut packed = Vec::with_capacity(length as usize);
    for piece in pieces.into_iter() {
        packed.extend_from_slice(&piece.context("a range came back empty")?);
    }
    Ok(packed)
}

/// Extracts the named files - matched on file name, not path - into `destination`.
///
/// `progress` is called with the bytes written so far, so a long extraction can
/// report itself the way a download does.
pub fn extract_named(
    url: &str,
    wanted: &[&str],
    destination: &std::path::Path,
    mut progress: impl FnMut(u64),
) -> Result<Vec<String>> {
    std::fs::create_dir_all(destination).with_context(|| format!("create {}", destination.display()))?;
    let reader = RangeReader::open(url)?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::with_capacity(1 << 20, reader))
        .map_err(|error| anyhow::anyhow!("read the index of {url}: {error}"))?;

    // Where each wanted file's bytes actually are, so they can be fetched
    // without walking the archive in order.
    struct Span {
        name: String,
        start: u64,
        compressed: u64,
        stored: bool,
    }
    let mut spans = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).context("read an archive entry")?;
        let Some(name) = entry.enclosed_name().and_then(|path| path.file_name().map(|name| name.to_string_lossy().to_string()))
        else {
            continue;
        };
        if !wanted.iter().any(|candidate| candidate.eq_ignore_ascii_case(&name)) {
            continue;
        }
        spans.push(Span {
            name,
            start: entry.data_start(),
            compressed: entry.compressed_size(),
            stored: entry.compression() == zip::CompressionMethod::Stored,
        });
    }

    let client = std::sync::Arc::new(
        reqwest::blocking::Client::builder()
            .user_agent("MiniMax-Music3-Studio")
            .timeout(std::time::Duration::from_secs(600))
            .pool_max_idle_per_host(PARALLEL)
            .build()
            .context("prepare an HTTP client")?,
    );

    let mut written = Vec::new();
    let mut bytes_written = 0u64;
    for span in spans {
        let packed = fetch_span(&client, url, span.start, span.compressed, &mut |done| {
            progress(bytes_written + done);
        })?;
        let target = destination.join(&span.name);
        let partial = target.with_extension("part");
        let mut file = std::io::BufWriter::new(
            std::fs::File::create(&partial).with_context(|| format!("create {}", partial.display()))?,
        );
        if span.stored {
            std::io::Write::write_all(&mut file, &packed).with_context(|| format!("write {}", span.name))?;
        } else {
            let mut inflater = flate2::read::DeflateDecoder::new(std::io::Cursor::new(&packed));
            std::io::copy(&mut inflater, &mut file).with_context(|| format!("unpack {}", span.name))?;
        }
        std::io::Write::flush(&mut file).ok();
        drop(file);
        std::fs::rename(&partial, &target).with_context(|| format!("publish {}", target.display()))?;
        bytes_written += span.compressed;
        progress(bytes_written);
        written.push(span.name);
    }

    if written.len() != wanted.len() {
        let missing: Vec<&str> = wanted
            .iter()
            .copied()
            .filter(|name| !written.iter().any(|found| found.eq_ignore_ascii_case(name)))
            .collect();
        bail!("{url} does not contain {}", missing.join(", "));
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network test, run on purpose: `cargo test -p music-server -- --ignored`.
    #[test]
    #[ignore]
    fn the_reader_returns_the_tail_a_zip_index_lives_in() {
        let url = "https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/windows-x86_64/cuda_cudart-windows-x86_64-12.9.79-archive.zip";
        let mut reader = RangeReader::open(url).expect("open");
        println!("length {}", reader.length());
        reader.seek(SeekFrom::End(-22)).expect("seek");
        let mut tail = [0u8; 22];
        let mut filled = 0;
        while filled < tail.len() {
            let read = reader.read(&mut tail[filled..]).expect("read");
            println!("read {read} bytes at {}", reader.position);
            if read == 0 { break }
            filled += read;
        }
        println!("tail {:x?}", &tail[..filled]);
        assert_eq!(&tail[..4], b"PK", "the end of central directory should be here");
    }

    /// The real thing, end to end, on the archive the engine depends on:
    /// `cargo test -p music-server -- --ignored the_engines_libraries`.
    /// Half a gigabyte over range requests is not something to guess about.
    #[test]
    #[ignore]
    fn the_engines_libraries_come_out_of_nvidias_archive() {
        let destination = std::env::temp_dir().join("mm3-cublas-extract-check");
        std::fs::remove_dir_all(&destination).ok();
        let asset = crate::engine_runtime::ASSETS.first().expect("one asset");
        let names = extract_named(asset.url, asset.pick, &destination, |written| {
            if written % (64 * 1024 * 1024) < 4 * 1024 * 1024 {
                println!("{} MB", written / (1024 * 1024));
            }
        })
        .expect("extract the engine's CUDA libraries");
        for name in asset.pick {
            let file = destination.join(name);
            let size = std::fs::metadata(&file).map(|meta| meta.len()).unwrap_or(0);
            println!("{name}: {size} bytes");
            assert!(size > 1_000_000, "{name} came out empty or truncated");
            // A DLL that unpacked wrong is still a file; the header is what the
            // loader actually reads.
            let head = std::fs::read(&file).expect("read the library");
            assert_eq!(&head[..2], b"MZ", "{name} is not a Windows binary");
        }
        assert_eq!(names.len(), asset.pick.len());
        std::fs::remove_dir_all(&destination).ok();
    }

    #[test]
    fn a_range_reader_seeks_the_way_a_zip_expects() {
        // No network here: the arithmetic is what breaks a zip reader, and it
        // is what this checks.
        let mut reader = RangeReader {
            url: String::new(),
            client: reqwest::blocking::Client::new(),
            position: 0,
            length: 1000,
            chunk: Vec::new(),
            chunk_start: 0,
        };
        assert_eq!(reader.seek(SeekFrom::Start(10)).unwrap(), 10);
        assert_eq!(reader.seek(SeekFrom::Current(5)).unwrap(), 15);
        assert_eq!(reader.seek(SeekFrom::End(-100)).unwrap(), 900);
        assert_eq!(reader.seek(SeekFrom::End(-2000)).unwrap(), 0);
    }
}
