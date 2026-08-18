use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, sync::RwLock};

pub const ENGINE_ID: &str = "minimaxmusic-cpp";
const REPOSITORY: &str = "Serveurperso/MiniMax-Music3-GGUF";
const REVISION: &str = "9cdffedb54de2509ae55a6831a677645fb353a7d";

/// The recommendation is a property of the machine, not of the catalog: a
/// 24 GB card must land on Full Native and a 12 GB card on Q8 Quality. The
/// Light set is only ever recommended in the low-VRAM tier.
fn recommended_profile() -> &'static str {
    crate::presets::recommended_local_profile()
}

#[derive(Clone)]
pub struct ModelManager {
    root: PathBuf,
    state_path: PathBuf,
    http: reqwest::Client,
    state: Arc<RwLock<PersistentState>>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub engine_id: &'static str,
    pub repository: &'static str,
    pub revision: &'static str,
    pub recommended_profile_id: &'static str,
    pub profiles: Vec<Profile>,
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub id: &'static str,
    pub label: &'static str,
    pub backend: &'static str,
    pub installable: bool,
    pub recommended: bool,
    pub components: Vec<&'static str>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Component {
    pub id: &'static str,
    pub kind: &'static str,
    pub filename: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstallRequest {
    pub profile_id: Option<String>,
    #[serde(default)]
    pub component_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagerStatus {
    pub engine_id: &'static str,
    pub model_root: String,
    pub first_run: bool,
    pub ready: bool,
    pub download_pending: u64,
    pub recommended_profile_id: String,
    pub active: Option<DownloadJob>,
    pub components: Vec<ComponentStatus>,
    pub installed_components: Vec<String>,
    /// The five files the selected set actually resolves to. The panel used to
    /// print "profile default" beside every role, which says nothing about
    /// what is loaded.
    pub profile_files: Option<ProfileModelFiles>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentStatus {
    pub id: &'static str,
    pub installed: bool,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadJob {
    pub id: String,
    pub profile_id: Option<String>,
    pub component_ids: Vec<String>,
    pub status: DownloadStatus,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub error: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ProfileModelFiles {
    pub lm_model: String,
    pub depth_model: String,
    pub cond_model: String,
    pub dit_model: String,
    pub vae_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Downloading,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistentState {
    active: Option<DownloadJob>,
}

impl ModelManager {
    pub fn from_environment() -> Result<Self> {
        let root = env::var_os("MINIMAX_MUSIC_MODELS_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(default_model_root);
        validate_model_root(&root)?;
        let state_path = root.join(".studio-download-state.json");
        let mut state = fs::read_to_string(&state_path)
            .ok()
            .and_then(|body| serde_json::from_str(&body).ok())
            .unwrap_or_default();
        if recover_interrupted_download(&mut state) {
            persist_state_file(&state_path, &state)?;
        }
        Ok(Self {
            root,
            state_path,
            http: reqwest::Client::new(),
            state: Arc::new(RwLock::new(state)),
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Where the weights live, for anything that needs to check a file exists.
    pub fn models_directory(&self) -> &Path {
        &self.root
    }

    pub fn catalog(&self) -> Catalog {
        Catalog {
            engine_id: ENGINE_ID,
            repository: REPOSITORY,
            revision: REVISION,
            recommended_profile_id: recommended_profile(),
            profiles: profiles(),
            components: components(),
        }
    }

    /// `target` is the set the user actually selected. Progress and readiness
    /// are reported against it, so a machine that deliberately runs the Light
    /// set is never told it is missing the hardware-recommended download.
    pub async fn status(&self, target: Option<InstallRequest>) -> ManagerStatus {
        let active = self.state.read().await.active.clone();
        let root = self.root.clone();
        // SHA-256 over a GGUF can take seconds. Keep it off the Tokio request
        // workers so setup polling and cancellation remain available.
        tokio::task::spawn_blocking(move || status_snapshot(root, active, target))
            .await
            .unwrap_or_else(|_| status_snapshot(PathBuf::from("."), None, None))
    }

    /// Deletes the files of the named components, freeing the disk they take.
    ///
    /// Downloading is undoable only if the user can also undo it. Ten gigabytes
    /// of weights with no way to remove them from inside the studio is how
    /// people end up hunting through their profile folder by hand.
    pub async fn remove(&self, component_ids: &[String]) -> Result<RemovalReport> {
        if self.state.read().await.active.as_ref().is_some_and(|job| matches!(job.status, DownloadStatus::Downloading)) {
            bail!("a model download is running; cancel it before removing files");
        }
        let catalog = components();
        let mut removed = Vec::new();
        let mut freed_bytes = 0u64;
        for id in component_ids {
            let component = catalog
                .iter()
                .find(|component| component.id == *id)
                .with_context(|| format!("unknown component '{id}'"))?;
            let path = self.root.join(&component.filename);
            let size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            match fs::remove_file(&path) {
                Ok(()) => {
                    freed_bytes += size;
                    removed.push(component.id.to_string());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
            }
            // A half-finished download of the same component is just as much
            // disk as the finished one.
            let _ = fs::remove_file(self.root.join(format!("{}.part", component.filename)));
        }
        // A remembered download resumes on the next start. Deleting the files
        // without forgetting the job is how twenty-six gigabytes came back by
        // themselves after being removed.
        {
            let mut state = self.state.write().await;
            let resumes_removed = state
                .active
                .as_ref()
                .is_some_and(|job| job.component_ids.iter().any(|id| component_ids.contains(id)));
            if resumes_removed {
                state.active = None;
                let _ = persist_state_file(&self.state_path, &state);
            }
        }
        Ok(RemovalReport { removed, freed_bytes })
    }

    pub async fn install(&self, request: InstallRequest) -> Result<DownloadJob> {
        let mut selection = resolve_install(request)?;
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create model root {}", self.root.display()))?;
        preflight_space(&self.root, &selection)?;
        // Progress starts from what is already published on disk. This uses the
        // cheap size check rather than a SHA-256 sweep: hashing a resumed 10 GB
        // set here would block this request — and every setup/status poll behind
        // it — for tens of seconds. Each component is still hash-verified in
        // `download_selection` before it is skipped or published.
        selection.already_present_bytes = selection
            .components
            .iter()
            .filter(|component| published_component(&self.root.join(component.filename), component))
            .map(|component| component.bytes)
            .sum();
        let mut state = self.state.write().await;
        if state.active.as_ref().is_some_and(|job| matches!(job.status, DownloadStatus::Downloading)) {
            bail!("a model download is already running");
        }
        self.cancelled.store(false, Ordering::SeqCst);
        let job = DownloadJob {
            id: uuid::Uuid::now_v7().to_string(),
            profile_id: selection.profile_id.clone(),
            component_ids: selection.components.iter().map(|component| component.id.into()).collect(),
            status: DownloadStatus::Downloading,
            downloaded_bytes: selection.already_present_bytes,
            total_bytes: selection.total_bytes,
            error: None,
        };
        state.active = Some(job.clone());
        self.persist_locked(&state)?;
        drop(state);
        let manager = self.clone();
        tokio::spawn(async move { manager.download_selection(selection).await });
        Ok(job)
    }

    pub async fn cancel(&self, target: Option<InstallRequest>) -> Result<ManagerStatus> {
        self.cancelled.store(true, Ordering::SeqCst);
        Ok(self.status(target).await)
    }

    pub async fn download_job(&self, id: &str) -> Option<DownloadJob> {
        self.state.read().await.active.as_ref().filter(|job| job.id == id).cloned()
    }

    pub fn installed_profile_files(&self, profile_id: &str) -> Result<ProfileModelFiles> {
        let selection = resolve_install(InstallRequest { profile_id: Some(profile_id.into()), component_ids: vec![] })?;
        self.installed_files_from_selection(selection, &format!("selected profile '{profile_id}'"))
    }

    /// Resolves an explicitly selected complete five-component set. This is
    /// deliberately component-id based rather than filename based: callers
    /// cannot persist or submit arbitrary paths to the native engine.
    pub fn installed_component_files(&self, component_ids: &[String]) -> Result<ProfileModelFiles> {
        let selection = resolve_install(InstallRequest { profile_id: None, component_ids: component_ids.to_vec() })?;
        self.installed_files_from_selection(selection, "selected custom component set")
    }

    fn installed_files_from_selection(&self, selection: ResolvedInstall, label: &str) -> Result<ProfileModelFiles> {
        for component in &selection.components {
            let path = self.root.join(component.filename);
            if !published_component(&path, component) {
                bail!("{label} is incomplete: missing or truncated {}", component.filename);
            }
        }
        Ok(profile_files_from_components(&selection.components))
    }

    async fn download_selection(&self, selection: ResolvedInstall) {
        let result = async {
            for component in &selection.components {
                if self.cancelled.load(Ordering::SeqCst) {
                    bail!("cancelled");
                }
                if verified_file_async(self.root.join(component.filename), component.clone()).await? {
                    self.set_published_progress(&selection).await?;
                    continue;
                }
                self.download_component(component).await?;
                // Streaming only counts the bytes that crossed the network. A
                // component resumed from a complete `.part`, or already present
                // from an earlier attempt, would otherwise leave the bar short
                // of 100% on a successful install.
                self.set_published_progress(&selection).await?;
            }
            Ok(())
        }
        .await;
        let mut state = self.state.write().await;
        if let Some(job) = &mut state.active {
            match result {
                Ok(()) => job.status = DownloadStatus::Completed,
                Err(error) if self.cancelled.load(Ordering::SeqCst) => {
                    job.status = DownloadStatus::Cancelled;
                    job.error = Some(error.to_string());
                }
                Err(error) => {
                    job.status = DownloadStatus::Failed;
                    job.error = Some(error.to_string());
                }
            }
            let _ = self.persist_locked(&state);
        }
    }

    async fn download_component(&self, component: &Component) -> Result<()> {
        let target = self.root.join(component.filename);
        let part = part_path(&target);
        let existing = fs::metadata(&part).map(|metadata| metadata.len()).unwrap_or(0);
        if existing > component.bytes {
            fs::remove_file(&part)?;
        }
        let offset = fs::metadata(&part).map(|metadata| metadata.len()).unwrap_or(0);
        // A resumed part can already hold the whole file — for example when the
        // process stopped between the last chunk and the rename. Asking for
        // `bytes=<size>-` is unsatisfiable and the server answers 416, so verify
        // and publish what is on disk instead of restarting the transfer.
        if offset == component.bytes {
            return self.publish_verified_part(component, &part, &target).await;
        }
        let url = format!(
            "https://huggingface.co/{REPOSITORY}/resolve/{REVISION}/{}?download=true",
            component.filename
        );
        let mut request = self.http.get(url);
        if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let response = request.send().await?.error_for_status()?;
        let append = offset > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let mut file = if append {
            tokio::fs::OpenOptions::new().append(true).open(&part).await?
        } else {
            tokio::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&part).await?
        };
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if self.cancelled.load(Ordering::SeqCst) {
                file.flush().await?;
                bail!("cancelled");
            }
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            self.add_progress(chunk.len() as u64).await?;
        }
        file.flush().await?;
        drop(file);
        self.publish_verified_part(component, &part, &target).await
    }

    /// Publishes a completed `.part` only after its SHA-256 matches the pinned
    /// Hugging Face LFS oid, so a truncated or corrupted transfer can never be
    /// presented to the engine as an installed component.
    async fn publish_verified_part(&self, component: &Component, part: &Path, target: &Path) -> Result<()> {
        let actual = fs::metadata(part)?.len();
        if actual != component.bytes {
            bail!("{} has {actual} bytes, expected {}", component.filename, component.bytes);
        }
        if !verified_file_async(part.to_path_buf(), component.clone()).await? {
            fs::remove_file(part).ok();
            bail!("{} SHA-256 does not match the pinned Hugging Face LFS oid; the partial file was discarded so the next attempt starts clean", component.filename);
        }
        fs::rename(part, target).with_context(|| format!("publish {}", target.display()))?;
        Ok(())
    }

    /// Re-bases progress on what is actually published on disk.
    async fn set_published_progress(&self, selection: &ResolvedInstall) -> Result<()> {
        let published: u64 = selection
            .components
            .iter()
            .filter(|component| published_component(&self.root.join(component.filename), component))
            .map(|component| component.bytes)
            .sum();
        let mut state = self.state.write().await;
        if let Some(job) = &mut state.active {
            job.downloaded_bytes = published.min(job.total_bytes);
            self.persist_locked(&state)?;
        }
        Ok(())
    }

    async fn add_progress(&self, bytes: u64) -> Result<()> {
        let mut state = self.state.write().await;
        if let Some(job) = &mut state.active {
            job.downloaded_bytes = (job.downloaded_bytes + bytes).min(job.total_bytes);
            self.persist_locked(&state)?;
        }
        Ok(())
    }

    fn persist_locked(&self, state: &PersistentState) -> Result<()> {
        persist_state_file(&self.state_path, state)
    }
}

fn recover_interrupted_download(state: &mut PersistentState) -> bool {
    let Some(job) = &mut state.active else { return false; };
    if !matches!(job.status, DownloadStatus::Downloading) { return false; }
    job.status = DownloadStatus::Cancelled;
    job.error = Some("Download was interrupted by an application restart. Partial .part files were preserved and the next download resumes them with HTTP Range.".into());
    true
}

fn persist_state_file(path: &Path, state: &PersistentState) -> Result<()> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    let temporary = part_path(path);
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

/// Direct `music-server` launches use the same OS application-data location
/// as the Tauri shell. The environment variable remains the explicit override
/// for development and a user-managed model library.
fn default_model_root() -> PathBuf {
    // A portable copy sets the studio's data root to its own folder, and the
    // models are the largest thing the studio owns: resolving them separately
    // would put ten gigabytes on the system drive while everything else stayed
    // beside the executable.
    if let Some(root) = crate::studio_data_root() {
        return root.join("models").join("minimaxmusic-cpp");
    }

    #[cfg(windows)]
    {
        if let Some(root) = env::var_os("LOCALAPPDATA").or_else(|| env::var_os("APPDATA")) {
            return PathBuf::from(root)
                .join("MiniMax Music3 Studio")
                .join("models")
                .join("minimaxmusic-cpp");
        }
    }

    #[cfg(not(windows))]
    {
        if let Some(root) = env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(root).join("minimax-music3-studio/models/minimaxmusic-cpp");
        }
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home).join(".local/share/minimax-music3-studio/models/minimaxmusic-cpp");
        }
    }

    env::temp_dir().join("minimax-music3-studio/models/minimaxmusic-cpp")
}

/// What a removal actually did.
#[derive(Debug, Clone, Serialize)]
pub struct RemovalReport {
    pub removed: Vec<String>,
    pub freed_bytes: u64,
}

#[derive(Debug, Clone)]
struct ResolvedInstall {
    profile_id: Option<String>,
    components: Vec<Component>,
    total_bytes: u64,
    already_present_bytes: u64,
}

fn resolve_install(request: InstallRequest) -> Result<ResolvedInstall> {
    if request.profile_id.is_some() && !request.component_ids.is_empty() {
        bail!("select either a complete profile or an advanced component set, not both");
    }
    // An empty request is a mistake, not an instruction to download the default
    // set. Silently substituting a profile turned a field-name mismatch into
    // twenty-six gigabytes nobody asked for.
    if request.profile_id.is_none() && request.component_ids.is_empty() {
        bail!("nothing was selected to download: name a profile or the components");
    }
    let profiles = profiles();
    let profile = request.profile_id.unwrap_or_else(|| recommended_profile().into());
    let (profile_id, ids) = if request.component_ids.is_empty() {
        let selected = profiles.iter().find(|candidate| candidate.id == profile)
            .with_context(|| format!("unknown profile '{profile}'"))?;
        if !selected.installable || selected.backend != ENGINE_ID {
            bail!("profile '{}' requires the '{}' backend and is not installable by the native GGUF manager", selected.id, selected.backend);
        }
        (Some(selected.id.into()), selected.components.clone())
    } else {
        (None, request.component_ids.iter().map(String::as_str).collect())
    };
    let catalog = components();
    let selected: Vec<Component> = ids
        .iter()
        .map(|id| catalog.iter().find(|candidate| candidate.id == *id).cloned().with_context(|| format!("unknown component '{id}'")))
        .collect::<Result<_>>()?;
    validate_complete_set(&selected)?;
    let total_bytes = selected.iter().map(|component| component.bytes).sum();
    Ok(ResolvedInstall { profile_id, components: selected, total_bytes, already_present_bytes: 0 })
}

fn validate_complete_set(selected: &[Component]) -> Result<()> {
    for kind in ["lm", "depth", "condition", "dit", "vocoder"] {
        if selected.iter().filter(|component| component.kind == kind).count() != 1 {
            bail!("a runnable MiniMax Music3 installation requires exactly one {kind} component");
        }
    }
    if selected.len() != 5 {
        bail!("advanced installation must contain exactly five compatible components");
    }
    Ok(())
}

fn profile_files_from_components(components: &[Component]) -> ProfileModelFiles {
    let filename = |kind| components.iter().find(|component| component.kind == kind).expect("complete profile").filename.to_owned();
    ProfileModelFiles { lm_model: filename("lm"), depth_model: filename("depth"), cond_model: filename("condition"), dit_model: filename("dit"), vae_model: filename("vocoder") }
}

fn preflight_space(root: &Path, selection: &ResolvedInstall) -> Result<()> {
    let available = fs2::available_space(root)?;
    // This preflight runs on the HTTP request path. A final GGUF is only
    // published after its SHA-256 has been checked and atomically renamed, so
    // checking its expected final size here is sufficient. Re-hashing a 6 GB
    // language model merely to calculate free space made the first-run UI look
    // frozen.
    let missing = selection.components.iter().filter(|component| !published_component(&root.join(component.filename), component)).map(|component| component.bytes).sum::<u64>();
    if available < missing {
        bail!("not enough disk space: need {missing} bytes, only {available} bytes available");
    }
    Ok(())
}

fn status_snapshot(root: PathBuf, active: Option<DownloadJob>, target: Option<InstallRequest>) -> ManagerStatus {
    let component_statuses: Vec<_> = components()
        .into_iter()
        .map(|component| ComponentStatus {
            id: component.id,
            // Final files are atomically published only after a full SHA-256
            // verification in `download_component`. Status is polled often,
            // therefore it must never hash multi-gigabyte weights again.
            installed: published_component(&root.join(component.filename), &component),
            bytes: component.bytes,
        })
        .collect();
    let target = target
        .and_then(|request| resolve_install(request).ok())
        .or_else(|| resolve_install(InstallRequest { profile_id: Some(recommended_profile().into()), component_ids: vec![] }).ok());
    let installed = |id: &str| component_statuses.iter().find(|component| component.id == id).is_some_and(|component| component.installed);
    let ready = target.as_ref().is_some_and(|selection| selection.components.iter().all(|component| installed(component.id)));
    let download_pending = target.as_ref().map(|selection| selection.components.iter()
        .filter(|component| !installed(component.id)).map(|component| component.bytes).sum()).unwrap_or_default();
    // What the five roles actually resolve to, so the panel can name the files
    // instead of saying "profile default".
    let profile_files = target.as_ref().and_then(|selection| {
        let file = |kind: &str| {
            selection
                .components
                .iter()
                .find(|component| component.kind == kind)
                .map(|component| component.filename.to_string())
        };
        Some(ProfileModelFiles {
            lm_model: file("lm")?,
            depth_model: file("depth")?,
            cond_model: file("condition")?,
            dit_model: file("dit")?,
            vae_model: file("vocoder")?,
        })
    });
    ManagerStatus {
        engine_id: ENGINE_ID, model_root: root.display().to_string(), first_run: !ready, ready, download_pending,
        recommended_profile_id: recommended_profile().into(), active, profile_files,
        installed_components: component_statuses.iter().filter(|component| component.installed).map(|component| component.id.into()).collect(),
        components: component_statuses,
    }
}

fn published_component(path: &Path, component: &Component) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.len() == component.bytes)
        .unwrap_or(false)
}

async fn verified_file_async(path: PathBuf, component: Component) -> Result<bool> {
    tokio::task::spawn_blocking(move || verified_file(&path, &component))
        .await
        .context("join GGUF SHA-256 verification task")?
}

fn profile_complete(profile_id: &str, root: &Path) -> bool {
    let Ok(selection) = resolve_install(InstallRequest { profile_id: Some(profile_id.into()), component_ids: vec![] }) else { return false; };
    selection.components.iter().all(|component| verified_file(&root.join(component.filename), component).unwrap_or(false))
}

fn verified_file(path: &Path, component: &Component) -> Result<bool> {
    if fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0) != component.bytes {
        return Ok(false);
    }
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()) == component.sha256)
}

fn validate_model_root(root: &Path) -> Result<()> {
    if root.as_os_str().is_empty() || root.parent().is_none() || root.file_name().is_none() {
        bail!("MINIMAX_MUSIC_MODELS_ROOT must be a specific non-root directory");
    }
    Ok(())
}

fn part_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".part");
    PathBuf::from(value)
}

fn profiles() -> Vec<Profile> {
    vec![
        profile("recommended-light", "Light - Q5_K_M / Q8_0 / Q4_K_M (speed / low VRAM)", false, &["lm-q5", "depth-q8", "condition-f32", "dit-q4", "vocoder-f32"]),
        profile("balanced", "Balanced - Q6_K / Q8_0 / Q5_K_M", false, &["lm-q6", "depth-q8", "condition-f32", "dit-q5", "vocoder-f32"]),
        profile("quality-q8", "Recommended - Quality Q8_0", true, &["lm-q8", "depth-q8", "condition-f32", "dit-q8", "vocoder-f32"]),
        profile("native", "Full native - BF16 / F32 original weights", false, &["lm-bf16", "depth-bf16", "condition-f32", "dit-f32", "vocoder-f32"]),
    ]
}

pub fn profile_exists(id: &str) -> bool {
    profiles().iter().any(|profile| profile.id == id && profile.installable && profile.backend == ENGINE_ID)
}

fn profile(id: &'static str, label: &'static str, recommended: bool, ids: &[&'static str]) -> Profile {
    let all = components();
    Profile { id, label, backend: ENGINE_ID, installable: true, recommended, components: ids.to_vec(), total_bytes: ids.iter().filter_map(|id| all.iter().find(|component| component.id == *id)).map(|component| component.bytes).sum() }
}

fn components() -> Vec<Component> {
    vec![
        c("condition-f32", "condition", "MiniMax-Music3-condition_encoder-F32.gguf", 100672192, "ebb69ec6e6d730b4dcc48ba1b51da4f201514cbaaa7cae0c8d6259d3b178efb0"),
        c("lm-bf16", "lm", "MiniMax-Music3-language_model-BF16.gguf", 17174297792, "6fd8a735ed9bba12c620f86504bb062a4ed5ba2237615f318dcfe14ee911c4ae"),
        c("lm-q5", "lm", "MiniMax-Music3-language_model-Q5_K_M.gguf", 6277696800, "6e34cc2be16c7f832198ca07837d026d8adc1f5b7914b76a95b9b4f7e5acd902"),
        c("lm-q6", "lm", "MiniMax-Music3-language_model-Q6_K.gguf", 7048279328, "ca605cbae894696f9c111bfef33023b29722e93fe2c287071250b0af1a0e5416"),
        c("lm-q8", "lm", "MiniMax-Music3-language_model-Q8_0.gguf", 9127257376, "9ffa190eb5892c5c0829014dc3070b9ce8d2464636bbaae884b08f1bf6de0ad3"),
        c("depth-bf16", "depth", "MiniMax-Music3-rvq_depth_decoder-BF16.gguf", 1292053888, "d566347637257b86e4ce598907caa257be9794f70fc38861da70f0d8bd2f0686"),
        c("depth-q8", "depth", "MiniMax-Music3-rvq_depth_decoder-Q8_0.gguf", 686513600, "7da73a953747f3f40857ba266a53eeb5487adbe473d4e7a53c99d5eaf9e5c3d5"),
        c("dit-f32", "dit", "MiniMax-Music3-transformer-F32.gguf", 9727655296, "ffcf7873d475448ba7b18b62b2495d7e1f2941f9fe5cba315557f6307367ae59"),
        c("dit-q4", "dit", "MiniMax-Music3-transformer-Q4_K_M.gguf", 1389592032, "4afc48e737c28788b4679f0303ed4d00f13c1f23cf9cc7ff408a2d8266ac6ba8"),
        c("dit-q5", "dit", "MiniMax-Music3-transformer-Q5_K_M.gguf", 1692794336, "35d1ed8902237215064a4bb5a9bf3e03507d65b89146bea9e24e915cb4c5c8ce"),
        c("dit-q6", "dit", "MiniMax-Music3-transformer-Q6_K.gguf", 2014946784, "9682694cd37d49361315204f69a25b054dbc817e4ed77487fc47d6f8ce7650ac"),
        c("dit-q8", "dit", "MiniMax-Music3-transformer-Q8_0.gguf", 2602401248, "cbadca0600f325ba9263ea4dcf0d71d0361abf0733303398b9995fbadac6b38e"),
        c("vocoder-f32", "vocoder", "MiniMax-Music3-vocoder-F32.gguf", 306102784, "4eaa451e54fa755cfe7b0fd15b0bfe64458db35b822e1d250488d0a2a363507d"),
    ]
}

fn c(id: &'static str, kind: &'static str, filename: &'static str, bytes: u64, sha256: &'static str) -> Component {
    Component { id, kind, filename, bytes, sha256 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommended_profile_is_a_complete_runnable_set() {
        let selected = resolve_install(InstallRequest {
            profile_id: Some(recommended_profile().into()),
            component_ids: vec![],
        })
        .unwrap();
        assert_eq!(selected.profile_id.as_deref(), Some(recommended_profile()));
        assert_eq!(selected.components.len(), 5);
        validate_complete_set(&selected.components).unwrap();
    }

    /// An empty request used to mean "download the default set", so a request
    /// that lost its field on the way turned into a 26 GB download of a set
    /// nobody had chosen.
    #[test]
    fn an_empty_request_downloads_nothing() {
        let error = resolve_install(InstallRequest { profile_id: None, component_ids: vec![] })
            .expect_err("an empty request is a mistake, not a default");
        assert!(error.to_string().contains("nothing was selected"));
    }

    #[test]
    fn advanced_selection_rejects_partial_sets() {
        let result = resolve_install(InstallRequest { profile_id: None, component_ids: vec!["lm-q5".into(), "dit-q4".into()] });
        assert!(result.is_err());
    }

    #[test]
    fn catalog_contains_every_pinned_component_variant() {
        assert_eq!(components().len(), 13);
        assert!(components().iter().all(|component| component.sha256.len() == 64));
    }

    #[test]
    fn light_profile_maps_to_the_exact_q5_q8_q4_component_files() {
        let selection = resolve_install(InstallRequest { profile_id: Some("recommended-light".into()), component_ids: vec![] }).unwrap();
        let file = |kind| selection.components.iter().find(|component| component.kind == kind).unwrap().filename;
        assert_eq!(file("lm"), "MiniMax-Music3-language_model-Q5_K_M.gguf");
        assert_eq!(file("depth"), "MiniMax-Music3-rvq_depth_decoder-Q8_0.gguf");
        assert_eq!(file("dit"), "MiniMax-Music3-transformer-Q4_K_M.gguf");
    }

    #[test]
    fn valid_custom_set_resolves_exact_component_filenames() {
        let selection = resolve_install(InstallRequest {
            profile_id: None,
            component_ids: vec!["lm-q8".into(), "depth-q8".into(), "condition-f32".into(), "dit-q6".into(), "vocoder-f32".into()],
        }).unwrap();
        let files = profile_files_from_components(&selection.components);
        assert_eq!(files.lm_model, "MiniMax-Music3-language_model-Q8_0.gguf");
        assert_eq!(files.dit_model, "MiniMax-Music3-transformer-Q6_K.gguf");
        assert_eq!(files.vae_model, "MiniMax-Music3-vocoder-F32.gguf");
    }

    #[tokio::test]
    async fn file_hashing_does_not_block_the_async_runtime() {
        let path = std::env::temp_dir().join(format!("mm3-hash-test-{}", uuid::Uuid::now_v7()));
        fs::File::create(&path).unwrap().set_len(8 * 1024 * 1024).unwrap();
        let component = Component { id: "test", kind: "test", filename: "test", bytes: 8 * 1024 * 1024, sha256: "not-a-real-digest" };
        let hash_task = tokio::spawn(verified_file_async(path.clone(), component));
        tokio::time::timeout(std::time::Duration::from_secs(1), tokio::time::sleep(std::time::Duration::from_millis(1))).await.unwrap();
        assert!(!hash_task.await.unwrap().unwrap());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn startup_marks_orphaned_download_as_cancelled_without_erasing_resume_state() {
        let mut state = PersistentState { active: Some(DownloadJob { id: "job".into(), profile_id: Some("recommended-light".into()), component_ids: vec!["lm-q5".into()], status: DownloadStatus::Downloading, downloaded_bytes: 123, total_bytes: 456, error: None }) };
        assert!(recover_interrupted_download(&mut state));
        let recovered = state.active.unwrap();
        assert!(matches!(recovered.status, DownloadStatus::Cancelled));
        assert_eq!(recovered.downloaded_bytes, 123);
        assert!(recovered.error.unwrap().contains("Partial .part files were preserved"));
    }
}
