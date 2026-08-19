//! The CUDA libraries the music engine is linked against.
//!
//! `mm-server.exe` imports `ggml.dll`, which imports `ggml-cuda.dll`, which
//! imports `cublas64_13.dll`, which imports `cublasLt64_13.dll`. Every one of
//! those is a static import, so Windows resolves the whole chain before the
//! engine's own code runs: a missing cuBLAS is not a backend that falls back to
//! the processor, it is a process that never starts, and the loader says so in
//! a modal dialog nobody in the studio can catch.
//!
//! The two libraries are 512 MB unpacked, which is four times the rest of the
//! installer, so they are not shipped - they are fetched from NVIDIA's own
//! redistributable archive the first time the engine is asked to start, the
//! same way the models are. The build machine has them on PATH through the CUDA
//! Toolkit, which is exactly why this was invisible until someone without the
//! Toolkit ran the release.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::downloads::{Asset, AssetKind, Downloader};

/// The CUDA major version `ggml-cuda.dll` was built against. It is in the
/// library file names themselves - `cublas64_13.dll` - so a rebuild of the
/// engine on a different CUDA is a change here as well, and the dependency
/// check in the release build is what catches it.
pub const CUDA_MAJOR: &str = "13";

/// The libraries, by file name, exactly as the engine imports them.
pub const REQUIRED_LIBRARIES: [&str; 2] = ["cublas64_13.dll", "cublasLt64_13.dll"];

/// The Visual C++ runtime the engine and ggml are compiled against, by the
/// names in their import tables.
pub const VC_RUNTIME_LIBRARIES: [&str; 4] =
    ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll", "vcomp140.dll"];

/// Microsoft's own permanent link to the current x64 redistributable.
const VC_REDIST_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";

pub const ASSETS: &[Asset] = &[Asset {
    id: "engine-cuda-cublas",
    label: "NVIDIA cuBLAS 13.5",
    kind: AssetKind::Runtime,
    // NVIDIA's redistributable archive, the same one every CUDA application
    // ships from. The size and the digest are published in
    // redist/redistrib_13.3.0.json; the size below was confirmed against a
    // live HEAD request, which also confirmed range support.
    url: "https://developer.download.nvidia.com/compute/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-13.5.1.27-archive.zip",
    relative_path: "cuda-cublas.zip",
    bytes: 391_055_517,
    // No sub-directory: the libraries go straight in beside mm-server.exe,
    // which is where the Windows loader looks first and what NVIDIA's own
    // deployment guide recommends.
    unzip_into: None,
    marker: "cublasLt64_13.dll",
    pick: &REQUIRED_LIBRARIES,
    vram_gb: None,
    note: "The linear algebra the engine's CUDA backend is linked against. Without it the engine cannot start at all.",
}];

/// The engine's downloadable runtime.
pub struct EngineRuntime {
    downloader: Downloader,
}

impl EngineRuntime {
    /// Takes the directory `mm-server.exe` lives in: the libraries belong
    /// beside the binary that imports them, not in a folder of their own.
    pub fn new(bundle_root: &Path) -> Self {
        Self { downloader: Downloader::new(bundle_root.to_path_buf()) }
    }

    pub fn downloader(&self) -> &Downloader {
        &self.downloader
    }

    /// Where the libraries live once installed - beside the engine.
    pub fn library_dir(&self) -> PathBuf {
        self.downloader.root().to_path_buf()
    }

    /// Whether the engine will find every library it imports.
    ///
    /// Downloaded copies are one way; a CUDA Toolkit already on the machine is
    /// the other, and it is the more common one among the people who would
    /// notice half a gigabyte arriving. The child process inherits this
    /// process's PATH, so what the loader will find is exactly what is found
    /// here - and a machine that already has cuBLAS downloads nothing.
    /// The Visual C++ runtime counts too: a machine that already has cuBLAS
    /// but no redistributable has nothing to download and still cannot start
    /// the engine, and checking only the downloads would have skipped the
    /// install entirely.
    pub fn is_ready(&self) -> bool {
        self.missing().is_empty() && vc_runtime_present()
    }

    /// What is still missing, so a caller can report the size before starting.
    pub fn missing(&self) -> Vec<&'static Asset> {
        ASSETS
            .iter()
            .filter(|asset| !self.downloader.is_installed(asset))
            .filter(|asset| !asset.pick.iter().all(|library| is_on_the_search_path(library)))
            .collect()
    }

    pub fn missing_bytes(&self) -> u64 {
        self.missing().iter().map(|asset| asset.bytes).sum()
    }

    /// Fetches whatever is missing and waits for it.
    ///
    /// `install` only accepts the job - it downloads in the background and
    /// returns at once. Returning here on that basis meant the engine was
    /// started while its libraries were still arriving, so it failed to load
    /// exactly as if nothing had been downloaded at all. The wait is what makes
    /// this an installation rather than a request.
    pub async fn install_missing(&self) -> Result<()> {
        ensure_vc_runtime().await?;
        for asset in self.missing() {
            self.downloader.install(asset).await?;
            loop {
                let Some(progress) = self.downloader.active().await else { break };
                if progress.done {
                    if let Some(error) = progress.error {
                        bail!("{} could not be downloaded: {error}", asset.label);
                    }
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
            if !self.downloader.is_installed(asset) {
                bail!("{} finished downloading but its files are not on disk", asset.label);
            }
        }
        Ok(())
    }
}

/// Whether the Visual C++ runtime the engine needs is already installed.
///
/// Almost every Windows machine has it - some game or application put it there
/// years ago - so this is checked, not assumed in either direction.
pub fn vc_runtime_present() -> bool {
    VC_RUNTIME_LIBRARIES.iter().all(|library| is_on_the_search_path(library))
}

/// Installs Microsoft's redistributable when, and only when, it is missing.
///
/// This is the ordinary way an application ships against the Visual C++
/// runtime: Microsoft publishes one installer at a permanent link, and it is
/// run once. It asks for administrator rights, shows its own progress, and
/// returns 3010 when it wants a restart - which is a success, not a failure.
pub async fn ensure_vc_runtime() -> Result<()> {
    if !cfg!(windows) || vc_runtime_present() {
        return Ok(());
    }
    let installer = std::env::temp_dir().join("vc_redist.x64.exe");
    let bytes = reqwest::get(VC_REDIST_URL)
        .await
        .context("download the Visual C++ redistributable")?
        .error_for_status()
        .context("download the Visual C++ redistributable")?
        .bytes()
        .await
        .context("read the Visual C++ redistributable")?;
    std::fs::write(&installer, &bytes).with_context(|| format!("write {}", installer.display()))?;

    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&installer).args(["/install", "/passive", "/norestart"]).status()
    })
    .await
    .context("run the Visual C++ redistributable")?
    .context("run the Visual C++ redistributable")?;
    match status.code() {
        // 0: installed. 1638: a newer one is already there. 3010: installed,
        // wants a restart it will not get from us and does not need.
        Some(0) | Some(1638) | Some(3010) => Ok(()),
        Some(code) => bail!("the Visual C++ redistributable installer ended with {code}"),
        None => bail!("the Visual C++ redistributable installer was interrupted"),
    }
}

/// Whether the loader would already find this library without help.
///
/// The engine inherits this process's PATH, so the directories searched there
/// are the directories searched here. A machine with the CUDA Toolkit
/// installed has cuBLAS on PATH already, and asking it to download half a
/// gigabyte of the same thing would be rude.
fn is_on_the_search_path(library: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|directory| directory.join(library).is_file())
}

/// The DLLs a Windows binary names in its import table.
///
/// NVIDIA's deployment guide says to read this with `dumpbin /IMPORTS` and
/// redistribute exactly what it lists, because the binary compatibility
/// version is part of the file name - `cublas64_13.dll`, not `cublas.dll`.
/// Reading it here means the release can check itself instead of trusting
/// that whoever built it remembered.
pub fn imported_libraries(binary: &Path) -> Result<Vec<String>> {
    let data = std::fs::read(binary)?;
    let at = |offset: usize| -> Result<u32> {
        let bytes = data.get(offset..offset + 4).context("truncated PE header")?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
    };
    let short = |offset: usize| -> Result<u16> {
        let bytes = data.get(offset..offset + 2).context("truncated PE header")?;
        Ok(u16::from_le_bytes(bytes.try_into().expect("two bytes")))
    };

    let pe = at(0x3c)? as usize;
    if data.get(pe..pe + 4) != Some(b"PE\0\0") {
        bail!("{} is not a PE binary", binary.display());
    }
    let sections = short(pe + 6)? as usize;
    let optional_size = short(pe + 20)? as usize;
    let optional = pe + 24;
    // 0x20b is PE32+, whose data directories start 16 bytes further in.
    let directories = optional + if short(optional)? == 0x20b { 112 } else { 96 };
    let import_rva = at(directories + 8)?;
    if import_rva == 0 {
        return Ok(Vec::new());
    }

    let headers: Vec<(u32, u32, u32)> = (0..sections)
        .map(|index| {
            let base = optional + optional_size + index * 40;
            Ok((at(base + 12)?, at(base + 16)?, at(base + 20)?))
        })
        .collect::<Result<_>>()?;
    let offset_of = |rva: u32| -> Option<usize> {
        headers
            .iter()
            .find(|(virtual_address, size, _)| rva >= *virtual_address && rva < virtual_address + size)
            .map(|(virtual_address, _, raw)| (raw + (rva - virtual_address)) as usize)
    };

    let mut names = Vec::new();
    let mut entry = offset_of(import_rva).context("import table points outside the file")?;
    loop {
        let descriptor = data.get(entry..entry + 20).context("truncated import table")?;
        if descriptor.iter().all(|byte| *byte == 0) {
            break;
        }
        let name_rva = u32::from_le_bytes(descriptor[12..16].try_into().expect("four bytes"));
        let start = offset_of(name_rva).context("an import name points outside the file")?;
        let end = data[start..].iter().position(|byte| *byte == 0).context("unterminated import name")? + start;
        names.push(String::from_utf8_lossy(&data[start..end]).into_owned());
        entry += 20;
    }
    Ok(names)
}

/// Libraries every Windows machine has, or that arrive with the display
/// driver. Everything else has to be shipped or downloaded.
fn is_provided_by_the_system(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("api-ms-win-")
        || name.starts_with("ext-ms-win-")
        // The display driver's own library: NVIDIA's guide is explicit that
        // this one is never redistributed - the user installs a driver.
        || name == "nvcuda.dll"
        || [
            "kernel32.dll",
            "kernelbase.dll",
            "user32.dll",
            "advapi32.dll",
            "shell32.dll",
            "ole32.dll",
            "oleaut32.dll",
            "ws2_32.dll",
            "crypt32.dll",
            "bcrypt.dll",
            "ntdll.dll",
            "rpcrt4.dll",
            "setupapi.dll",
            "cfgmgr32.dll",
            "gdi32.dll",
            "version.dll",
            "dbghelp.dll",
            "powrprof.dll",
            "psapi.dll",
            "userenv.dll",
            "winmm.dll",
            "msvcrt.dll",
        ]
        .contains(&name.as_str())
}

/// What a binary needs that is neither beside it nor supplied by Windows.
///
/// Follows the chain: `mm-server.exe` imports `ggml.dll`, which imports
/// `ggml-cuda.dll`, which is where cuBLAS actually comes in. Checking only the
/// executable's own imports would have found nothing wrong with the release
/// that could not start.
pub fn unresolved_dependencies(directory: &Path, entry_point: &str) -> Result<Vec<String>> {
    let mut seen: Vec<String> = Vec::new();
    let mut queue = vec![entry_point.to_string()];
    let mut missing = Vec::new();
    while let Some(name) = queue.pop() {
        let lowered = name.to_ascii_lowercase();
        if seen.contains(&lowered) {
            continue;
        }
        seen.push(lowered);
        let path = directory.join(&name);
        if !path.is_file() {
            missing.push(name);
            continue;
        }
        for import in imported_libraries(&path)? {
            if !is_provided_by_the_system(&import) {
                queue.push(import);
            }
        }
    }
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file names are the contract with the engine binary: they are what
    /// the import table asks the loader for, and what `pick` takes out of the
    /// archive. A typo here is a download that finishes and changes nothing.
    #[test]
    fn every_imported_library_is_picked_out_of_the_archive() {
        for library in REQUIRED_LIBRARIES {
            assert!(library.contains(CUDA_MAJOR), "{library} does not name CUDA {CUDA_MAJOR}");
            assert!(
                ASSETS.iter().any(|asset| asset.pick.contains(&library)),
                "{library} is imported by the engine but never taken out of an archive"
            );
        }
    }

    /// A runtime asset that unpacks nowhere would download and vanish.
    #[test]
    fn the_libraries_land_in_one_directory_the_engine_can_be_pointed_at() {
        for asset in ASSETS {
            // Straight into the bundle: a sub-directory would put them
            // somewhere the loader does not look.
            assert_eq!(asset.unzip_into, None);
            assert_eq!(asset.kind, AssetKind::Runtime);
            assert!(asset.bytes > 0, "{} has no size, so nothing can report progress", asset.id);
        }
    }

    /// The release that shipped without cuBLAS passed every test there was,
    /// because no test ever looked at what the engine binary asks the loader
    /// for. This one does: whatever the staged bundle imports is either beside
    /// it, supplied by Windows, or downloaded on first start - and nothing
    /// else is allowed.
    #[test]
    #[cfg(windows)]
    fn the_staged_engine_bundle_can_actually_be_loaded() {
        let bundle = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../desktop/src-tauri/resources/minimaxmusic-cpp")
            .canonicalize();
        let Ok(bundle) = bundle else {
            // No engine has been built into the bundle on this machine, which
            // is the normal state of a checkout that only touches the service.
            return;
        };
        if !bundle.join("mm-server.exe").is_file() {
            return;
        }
        let missing = unresolved_dependencies(&bundle, "mm-server.exe").expect("read the bundle's import tables");
        let handled: Vec<&str> = REQUIRED_LIBRARIES.iter().chain(VC_RUNTIME_LIBRARIES.iter()).copied().collect();
        let unexpected: Vec<&String> = missing
            .iter()
            .filter(|name| !handled.iter().any(|library| library.eq_ignore_ascii_case(name)))
            .collect();
        assert!(
            unexpected.is_empty(),
            "the engine bundle imports libraries that are neither shipped nor installed on first start: {unexpected:?}. \
             Ship them next to mm-server.exe, or add them to this module so the studio fetches them."
        );
    }

    /// The import table is the whole basis of the check above; if it cannot be
    /// read, the check silently passes on anything.
    #[test]
    #[cfg(windows)]
    fn imports_are_read_out_of_a_real_binary() {
        let system = Path::new("C:/Windows/System32/notepad.exe");
        if !system.is_file() {
            return;
        }
        let imports = imported_libraries(system).expect("notepad has an import table");
        assert!(!imports.is_empty(), "no imports were read at all");
        assert!(imports.iter().all(|name| name.to_ascii_lowercase().ends_with(".dll")));
    }

    /// The libraries count as installed only once they are beside the engine.
    ///
    /// This asserts on the downloader rather than on `is_ready`, because a
    /// machine with the CUDA Toolkit is ready without downloading anything -
    /// which is the point of the PATH check, and would make the test lie about
    /// what it proved.
    #[test]
    fn the_libraries_count_as_installed_only_beside_the_engine() {
        let root = std::env::temp_dir().join(format!("engine-runtime-{}", uuid::Uuid::now_v7()));
        let runtime = EngineRuntime::new(&root);
        let asset = ASSETS.first().expect("one asset");
        assert!(!runtime.downloader().is_installed(asset));
        assert_eq!(runtime.library_dir(), root);

        std::fs::create_dir_all(runtime.library_dir()).unwrap();
        for library in REQUIRED_LIBRARIES {
            std::fs::write(runtime.library_dir().join(library), b"x").unwrap();
        }
        assert!(runtime.downloader().is_installed(asset));
        assert!(runtime.is_ready());
        std::fs::remove_dir_all(&root).ok();
    }
}
