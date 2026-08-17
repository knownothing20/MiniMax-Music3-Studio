//! Resumable downloads for the optional native extras.
//!
//! Every optional capability - the writing assistant, the karaoke aligner -
//! fetches the same way: a pinned URL, an exact size read from the live
//! endpoint, a `.part` file that survives an interrupted run, and a zip that
//! unpacks into its own directory so two runtimes never mix. Nothing here
//! starts on its own; a download happens only when the user asks for it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// What a downloadable asset is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// Model weights.
    Model,
    /// Executables and their libraries, delivered as a zip.
    Runtime,
}

#[derive(Debug, Clone, Copy)]
pub struct Asset {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: AssetKind,
    pub url: &'static str,
    /// Where the file lands, relative to the capability's root.
    pub relative_path: &'static str,
    /// Exact size in bytes, as reported by the origin.
    pub bytes: u64,
    /// Unpacked into this sub-directory of `runtime/` when set.
    pub unzip_into: Option<&'static str>,
    /// A file name prefix that proves this asset is unpacked. Without it a
    /// companion archive would look installed as soon as its neighbour was,
    /// because both land in the same directory.
    pub marker: &'static str,
    /// Roughly how much VRAM the asset wants; informational only.
    pub vram_gb: Option<u32>,
    pub note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssetStatus {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: AssetKind,
    pub bytes: u64,
    pub vram_gb: Option<u32>,
    pub note: &'static str,
    pub installed: bool,
    /// Bytes already on disk, so a resumed download reports honestly.
    pub downloaded_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub asset_id: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// Owns one capability's download directory.
pub struct Downloader {
    root: PathBuf,
    http: reqwest::Client,
    progress: Arc<Mutex<Option<DownloadProgress>>>,
}

impl Downloader {
    pub fn new(root: PathBuf) -> Self {
        Self { root, http: reqwest::Client::new(), progress: Arc::new(Mutex::new(None)) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_of(&self, asset: &Asset) -> PathBuf {
        self.root.join(asset.relative_path)
    }

    pub fn runtime_dir(&self, flavour: &str) -> PathBuf {
        self.root.join("runtime").join(flavour)
    }

    /// A model is installed when its size matches exactly; a runtime when its
    /// own marker file is present in the directory it unpacks into.
    pub fn is_installed(&self, asset: &Asset) -> bool {
        if let Some(flavour) = asset.unzip_into {
            let marker = asset.marker.to_ascii_lowercase();
            return fs::read_dir(self.runtime_dir(flavour))
                .map(|entries| {
                    entries
                        .flatten()
                        .any(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase().starts_with(&marker))
                })
                .unwrap_or(false);
        }
        fs::metadata(self.path_of(asset)).map(|meta| meta.len() == asset.bytes).unwrap_or(false)
    }

    pub fn partial_bytes(&self, asset: &Asset) -> u64 {
        fs::metadata(self.path_of(asset).with_extension("part")).map(|meta| meta.len()).unwrap_or(0)
    }

    pub fn status_of(&self, assets: &[Asset]) -> Vec<AssetStatus> {
        assets
            .iter()
            .map(|asset| AssetStatus {
                id: asset.id,
                label: asset.label,
                kind: asset.kind,
                bytes: asset.bytes,
                vram_gb: asset.vram_gb,
                note: asset.note,
                installed: self.is_installed(asset),
                downloaded_bytes: if self.is_installed(asset) { asset.bytes } else { self.partial_bytes(asset) },
            })
            .collect()
    }

    pub async fn active(&self) -> Option<DownloadProgress> {
        self.progress.lock().await.clone()
    }

    /// Starts one download in the background. Only one runs at a time, and an
    /// interrupted file resumes from what is already on disk.
    pub async fn install(&self, asset: &'static Asset) -> Result<()> {
        {
            let mut progress = self.progress.lock().await;
            if progress.as_ref().is_some_and(|active| !active.done && active.error.is_none()) {
                bail!("another download is already running");
            }
            *progress = Some(DownloadProgress {
                asset_id: asset.id.to_string(),
                downloaded_bytes: self.partial_bytes(asset),
                total_bytes: asset.bytes,
                done: false,
                error: None,
            });
        }

        let target = self.path_of(asset);
        let root = self.root.clone();
        let http = self.http.clone();
        let progress = self.progress.clone();
        tokio::spawn(async move {
            let outcome = fetch(&http, asset, &target, &progress).await;
            let mut guard = progress.lock().await;
            let error = match outcome {
                Ok(()) => match asset.unzip_into {
                    Some(flavour) => extract_zip(&target, &root.join("runtime").join(flavour)).err().map(|error| error.to_string()),
                    None => None,
                },
                Err(error) => Some(error.to_string()),
            };
            if let Some(active) = guard.as_mut() {
                active.done = true;
                active.error = error;
            }
        });
        Ok(())
    }
}

async fn fetch(
    http: &reqwest::Client,
    asset: &Asset,
    target: &Path,
    progress: &Arc<Mutex<Option<DownloadProgress>>>,
) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    if fs::metadata(target).map(|meta| meta.len() == asset.bytes).unwrap_or(false) {
        return Ok(());
    }

    let part = target.with_extension("part");
    let mut offset = fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
    if offset > asset.bytes {
        fs::remove_file(&part).ok();
        offset = 0;
    }

    let mut request = http.get(asset.url);
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let response = request.send().await.with_context(|| format!("request {}", asset.url))?;
    if !response.status().is_success() {
        bail!("{} answered {}", asset.url, response.status());
    }
    let append = offset > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if !append {
        offset = 0;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&part)
        .with_context(|| format!("open {}", part.display()))?;

    let mut stream = response.bytes_stream();
    let mut written = offset;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read download chunk")?;
        io::Write::write_all(&mut file, &chunk).context("write download chunk")?;
        written += chunk.len() as u64;
        if let Some(active) = progress.lock().await.as_mut() {
            active.downloaded_bytes = written;
        }
    }
    drop(file);

    let size = fs::metadata(&part)?.len();
    if size != asset.bytes {
        bail!("{} downloaded {size} bytes, expected {}", asset.label, asset.bytes);
    }
    fs::rename(&part, target).with_context(|| format!("publish {}", target.display()))?;
    Ok(())
}

/// Unpacks a release zip. Entries are flattened into `destination` because the
/// releases nest their binaries one or two directories deep.
pub fn extract_zip(archive: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination).with_context(|| format!("create {}", destination.display()))?;
    let file = fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file).with_context(|| format!("read {}", archive.display()))?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).context("read zip entry")?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry.enclosed_name().and_then(|path| path.file_name().map(|name| name.to_owned())) else {
            continue;
        };
        let target = destination.join(name);
        let mut out = fs::File::create(&target).with_context(|| format!("create {}", target.display()))?;
        io::copy(&mut entry, &mut out).with_context(|| format!("extract {}", target.display()))?;
    }
    fs::remove_file(archive).ok();
    Ok(())
}

/// The first executable named `name` in the given flavour directories, in
/// preference order - a GPU build before a CPU one.
pub fn locate_binary(root: &Path, flavours: &[&str], name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    for flavour in flavours {
        let candidate = root.join("runtime").join(flavour).join(&file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let direct = root.join("runtime").join(&file);
    direct.is_file().then_some(direct)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset() -> Asset {
        Asset {
            id: "test-model",
            label: "Test model",
            kind: AssetKind::Model,
            url: "https://example.invalid/model.bin",
            relative_path: "models/model.bin",
            bytes: 64,
            unzip_into: None,
            marker: "",
            vram_gb: None,
            note: "",
        }
    }

    #[test]
    fn a_partial_file_is_reported_but_never_counts_as_installed() {
        let root = std::env::temp_dir().join(format!("downloads-{}", uuid::Uuid::now_v7()));
        let downloader = Downloader::new(root.clone());
        let entry = asset();
        fs::create_dir_all(downloader.path_of(&entry).parent().unwrap()).unwrap();
        fs::write(downloader.path_of(&entry).with_extension("part"), vec![0u8; 20]).unwrap();

        assert!(!downloader.is_installed(&entry));
        assert_eq!(downloader.partial_bytes(&entry), 20);

        fs::write(downloader.path_of(&entry), vec![0u8; 64]).unwrap();
        assert!(downloader.is_installed(&entry));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_runtime_is_installed_only_when_its_own_marker_is_present() {
        let root = std::env::temp_dir().join(format!("downloads-{}", uuid::Uuid::now_v7()));
        let downloader = Downloader::new(root.clone());
        let runtime = Asset {
            id: "test-runtime",
            kind: AssetKind::Runtime,
            relative_path: "runtime/test.zip",
            unzip_into: Some("cuda"),
            marker: "whisper-cli",
            ..asset()
        };
        fs::create_dir_all(downloader.runtime_dir("cuda")).unwrap();
        fs::write(downloader.runtime_dir("cuda").join("ggml.dll"), b"x").unwrap();
        assert!(!downloader.is_installed(&runtime));

        fs::write(downloader.runtime_dir("cuda").join("whisper-cli.exe"), b"x").unwrap();
        assert!(downloader.is_installed(&runtime));
        fs::remove_dir_all(&root).ok();
    }
}
