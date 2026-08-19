mod auto_title;
mod tagging;
mod cover_prompt;
mod providers;
mod assistant;
mod assistant_runtime;
mod audio_pcm;
mod downloads;
mod engine_runtime;
mod lyrics_sync;
mod credentials;
mod model_manager;
mod presets;
mod remote_zip;
mod resources;
mod separation;
mod skill;
mod library;
mod mm_result;
mod openrouter_stream;

use std::{collections::HashMap, env, fs, net::SocketAddr, path::PathBuf, sync::Arc};
use anyhow::Context;
use futures_util::StreamExt;

use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    body::Body,
    http::{header, HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use music_core::{Capability, EngineDescriptor, ExecutionMode, StudioConfiguration};
use model_manager::{InstallRequest, ModelManager};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

const PRIMARY_MUSIC_ENGINE_ID: &str = "minimaxmusic-cpp";

#[derive(Clone)]
struct AppState {
    configuration: Arc<RwLock<StudioConfiguration>>,
    jobs: Arc<RwLock<HashMap<String, MusicJob>>>,
    music_server: MmServerClient,
    model_manager: ModelManager,
    selected_profile_id: Arc<RwLock<Option<String>>>,
    selected_component_ids: Arc<RwLock<Option<Vec<String>>>>,
    settings_path: PathBuf,
    openrouter_catalog: Arc<RwLock<OpenRouterCatalogState>>,
    library: library::Library,
    /// Owned local engine process, when this service started one.
    engine: Arc<tokio::sync::Mutex<Option<music_engine::mm_server::MmServerSupervisor>>>,
    engine_options: Arc<RwLock<EngineOptions>>,
    /// The CUDA libraries the engine binary imports. They are downloaded, not
    /// installed, so the engine cannot start until they are on disk.
    engine_runtime: Arc<engine_runtime::EngineRuntime>,
    assistant: Arc<RwLock<AssistantConfig>>,
    assistant_runtime: Arc<assistant_runtime::AssistantRuntime>,
    lyrics_sync: Arc<lyrics_sync::LyricsSync>,
    lyrics_sync_config: Arc<RwLock<lyrics_sync::LyricsSyncConfig>>,
    /// Saved cover looks, filled in from whichever track a cover is for.
    cover_templates: Arc<RwLock<Vec<cover_prompt::CoverTemplate>>>,
    /// The look a new cover starts from, chosen in Settings.
    cover_template_default: Arc<RwLock<Option<String>>>,
    separator: Arc<separation::Separator>,
    separation_config: Arc<RwLock<separation::SeparationConfig>>,
    /// Draw a cover as soon as a track is finished.
    cover_auto: Arc<RwLock<bool>>,
    /// What is being done to finished tracks right now - covers, karaoke - so
    /// the interface can say it instead of leaving the user guessing.
    activity: Arc<RwLock<Vec<Activity>>>,
    /// The separation run in progress, if any. One at a time: the model wants
    /// the whole machine for a minute, and two runs would only make both slow.
    separation_run: Arc<RwLock<Option<SeparationRun>>>,
}

#[derive(Clone)]
struct MmServerClient {
    base_url: String,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateMusicJobRequest {
    caption: String,
    lyrics: String,
    duration_seconds: f64,
    steps: Option<u32>,
    seed: Option<i64>,
    lm_seed: Option<i64>,
    lm_cfg: Option<f64>,
    lm_top_k: Option<u32>,
    lm_batch_size: Option<u32>,
    synth_batch_size: Option<u32>,
    dit_cfg: Option<f64>,
    peak_clip: Option<i32>,
    output_format: Option<String>,
    mp3_bitrate: Option<u32>,
    models: Option<Mm3ModelSelection>,
    /// Library title only. It is never sent to mm-server, which has no title
    /// field, so it must not become part of the replayable request.
    title: Option<String>,
    /// What the cover should show, when the assistant already described it.
    /// Also library-only, for the same reason.
    cover_prompt: Option<String>,
}

/// The name this request goes into the library under: the user's, or one taken
/// from the song when they left the field empty.
fn titled(request: &CreateMusicJobRequest) -> String {
    request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| auto_title::auto_title(&request.caption, &request.lyrics, request.lyrics.trim().is_empty()))
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct Mm3ModelSelection {
    lm_model: Option<String>,
    depth_model: Option<String>,
    cond_model: Option<String>,
    dit_model: Option<String>,
    vae_model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum MusicJobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum MusicJobDispatch {
    NotConfigured,
    Local,
    OpenRouter,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum MusicJobPhase {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
struct MusicJob {
    id: String,
    engine_id: String,
    /// What the assistant said this track's cover should show, if anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    cover_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    status: MusicJobStatus,
    dispatch: MusicJobDispatch,
    phase: MusicJobPhase,
    caption: String,
    lyrics: String,
    duration_seconds: f64,
    generation_settings: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    song: Option<CompletedSong>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    songs: Vec<CompletedSong>,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
struct CompletedSong {
    id: String,
    song: library::Song,
    audio_url: String,
}

#[derive(Debug, Serialize)]
struct LocalMusicModelCatalog {
    engine_id: String,
    catalog: Value,
}

#[derive(Debug, Serialize)]
struct CapabilitiesResponse {
    engines: Vec<EngineDescriptor>,
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

#[derive(Debug, Deserialize)]
struct MmServerSubmitResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct MmServerJobResponse {
    status: String,
}

struct MmServerResultResponse {
    content_type: String,
    body: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct SetupSelectRequest {
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    component_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct SetupDownloadRequest {
    // The panel calls this field `component_ids`. Reading only `ids` meant a
    // download request arrived empty, and an empty request quietly fell back to
    // the default set - which is how pressing "download" on the 11.9 GB set
    // started fetching the 26.6 GB one.
    #[serde(default, alias = "component_ids")]
    ids: Vec<String>,
    profile_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApplyPresetRequest {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ReplayMusicJobRequest {
    song_id: Option<String>,
    replay_request: Option<Value>,
    steps: Option<u32>,
    seed: Option<i64>,
    dit_cfg: Option<f64>,
    output_format: Option<String>,
    models: Option<Mm3ModelSelection>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedStudioSettings {
    #[serde(default)]
    engine_options: EngineOptions,
    #[serde(default)]
    assistant: AssistantConfig,
    lyrics_sync: lyrics_sync::LyricsSyncConfig,
    configuration: StudioConfiguration,
    selected_profile_id: Option<String>,
    #[serde(default)]
    selected_component_ids: Option<Vec<String>>,
    #[serde(default)]
    cover_templates: Option<Vec<cover_prompt::CoverTemplate>>,
    #[serde(default)]
    cover_template_default: Option<String>,
    #[serde(default)]
    separation: Option<separation::SeparationConfig>,
    /// Whether a finished track gets its cover drawn without being asked.
    #[serde(default)]
    cover_auto: Option<bool>,
}

#[derive(Default)]
struct OpenRouterCatalogState {
    catalog: Option<providers::openrouter::CapabilityCatalog>,
    refreshed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterTranscriptionRequest {
    model_id: String,
    audio_base64: String,
    audio_format: String,
    language: Option<String>,
}

/// Launch flags for the local engine process. They are a property of the
/// running engine, so changing one restarts it; upstream has no way to apply
/// them to a live server.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct EngineOptions {
    keep_loaded: bool,
    max_batch: Option<u32>,
    max_seq: Option<u32>,
    disable_flash_attention: bool,
    split_cfg_forwards: bool,
    clamp_fp16: bool,
}

impl EngineOptions {
    /// `--max-batch` defaults to 1 upstream, and `lm_batch_size` may not exceed
    /// it, so this is also the number of songs one request can render.
    fn effective_max_batch(&self) -> u32 {
        self.max_batch.unwrap_or(1).max(1)
    }

    fn to_engine(self) -> music_engine::mm_server::MmServerOptions {
        music_engine::mm_server::MmServerOptions {
            keep_loaded: self.keep_loaded,
            max_batch: self.max_batch,
            max_seq: self.max_seq,
            disable_flash_attention: self.disable_flash_attention,
            split_cfg_forwards: self.split_cfg_forwards,
            clamp_fp16: self.clamp_fp16,
        }
    }
}

/// Where the optional writing assistant runs.
///
/// `None` is the default and a first-class state: the manual form is the
/// primary way to use this model, and on a modest card nobody wants a language
/// model competing for VRAM. Nothing is downloaded or started unless the user
/// picks a provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct AssistantConfig {
    provider: AssistantProvider,
    /// Base URL of an OpenAI-compatible server (llama.cpp, LM Studio, Ollama).
    local_base_url: Option<String>,
    local_model: Option<String>,
    openrouter_model: Option<String>,
    /// Id of a model downloaded through the assistant runtime, run as a
    /// sidecar by Studio itself.
    managed_model: Option<String>,
    /// A GGUF already on this machine, run by the same sidecar. Machines that
    /// already keep a Gemma around for another tool do not need a second copy.
    managed_path: Option<String>,
    /// How hard a reasoning model should think, in OpenRouter's own terms:
    /// minimal, low, medium, high, xhigh, max - or none.
    reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AssistantProvider {
    #[default]
    None,
    Local,
    OpenRouter,
    /// A model Studio downloaded and runs itself with llama.cpp.
    Managed,
}

impl AssistantConfig {
    fn available(&self) -> bool {
        match self.provider {
            AssistantProvider::None => false,
            AssistantProvider::Local => {
                self.local_base_url.as_deref().is_some_and(|url| !url.trim().is_empty())
                    && self.local_model.as_deref().is_some_and(|model| !model.trim().is_empty())
            }
            AssistantProvider::OpenRouter => {
                self.openrouter_model.as_deref().is_some_and(|model| !model.trim().is_empty())
                    && credentials::openrouter_source().is_some()
            }
            // Availability is confirmed against the disk in `assistant_status`;
            // a model id alone only says one was chosen.
            AssistantProvider::Managed => {
                self.managed_model.as_deref().is_some_and(|model| !model.trim().is_empty())
                    || self.managed_path.as_deref().is_some_and(|path| !path.trim().is_empty())
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ProxyImageRequest {
    url: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterSettingsRequest {
    /// `None` or an empty string clears the locally stored credential.
    api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterCompletionRequest {
    model_id: String,
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterCoverRequest {
    model_id: String,
    prompt: String,
}

#[derive(Debug, Serialize)]
struct OpenRouterResponse {
    body: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_id: Option<String>,
}

/// Runs the studio service until the process is asked to stop.
///
/// This is a library entry point on purpose: the desktop application hosts it
/// in-process, so a release is a single executable rather than a launcher that
/// has to start a second binary and keep it alive.
pub async fn serve() -> anyhow::Result<()> {
    let settings_path = studio_settings_path();
    let persisted = load_studio_settings(&settings_path);
    let model_manager = ModelManager::from_environment()?;
    let selected_component_ids = persisted
        .as_ref()
        .and_then(|settings| settings.selected_component_ids.clone())
        .filter(|ids| model_manager.installed_component_files(ids).is_ok());
    let selected_profile_id = if selected_component_ids.is_some() {
        None
    } else {
        persisted
            .as_ref()
            .and_then(|settings| settings.selected_profile_id.clone())
            .or_else(|| Some(presets::recommended_local_profile().into()))
    };
    let state = AppState {
        configuration: Arc::new(RwLock::new(sanitize_persisted_configuration(
            persisted.as_ref().map(|settings| settings.configuration.clone()).unwrap_or_else(initial_configuration),
        ))),
        jobs: Arc::new(RwLock::new(HashMap::new())),
        music_server: MmServerClient::from_environment(),
        model_manager,
        cover_templates: Arc::new(RwLock::new(
            persisted
                .as_ref()
                .and_then(|settings| settings.cover_templates.clone())
                .filter(|templates| !templates.is_empty())
                .unwrap_or_else(cover_prompt::default_templates),
        )),
        cover_template_default: Arc::new(RwLock::new(
            persisted.as_ref().and_then(|settings| settings.cover_template_default.clone()),
        )),
        activity: Arc::new(RwLock::new(Vec::new())),
        cover_auto: Arc::new(RwLock::new(
            persisted.as_ref().and_then(|settings| settings.cover_auto).unwrap_or(false),
        )),
        separation_config: Arc::new(RwLock::new(
            persisted.as_ref().and_then(|settings| settings.separation.clone()).unwrap_or_default(),
        )),
        separator: Arc::new(separation::Separator::new(
            &studio_data_root().unwrap_or_else(|| std::path::PathBuf::from(".")),
        )),
        separation_run: Arc::new(RwLock::new(None)),
        selected_profile_id: Arc::new(RwLock::new(selected_profile_id)),
        selected_component_ids: Arc::new(RwLock::new(selected_component_ids)),
        settings_path,
        openrouter_catalog: Arc::new(RwLock::new(OpenRouterCatalogState::default())),
        library: library::Library::open_default()?,
        engine: Arc::new(tokio::sync::Mutex::new(None)),
        engine_options: Arc::new(RwLock::new(persisted.as_ref().map(|settings| settings.engine_options).unwrap_or_default())),
        engine_runtime: Arc::new(engine_runtime::EngineRuntime::new(&engine_bundle_root())),
        assistant: Arc::new(RwLock::new(persisted.as_ref().map(|settings| settings.assistant.clone()).unwrap_or_default())),
        assistant_runtime: Arc::new(assistant_runtime::AssistantRuntime::new(
            &studio_data_root().unwrap_or_else(|| std::path::PathBuf::from(".")),
        )),
        lyrics_sync: Arc::new(lyrics_sync::LyricsSync::new(
            &studio_data_root().unwrap_or_else(|| std::path::PathBuf::from(".")),
        )),
        lyrics_sync_config: Arc::new(RwLock::new(
            persisted.as_ref().map(|settings| settings.lyrics_sync.clone()).unwrap_or_default(),
        )),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/configuration", get(configuration).put(update_configuration))
        .route("/engine/presets", get(engine_presets))
        .route("/engine/preset", post(apply_engine_preset))
        .route("/engine/start", post(start_local_engine))
        .route("/engine/options", get(engine_options).put(update_engine_options))
        .route("/engine/restart", post(restart_local_engine))
        .route("/v1/engine/logs", get(engine_logs))
        .route("/v1/system/resources", get(system_resources))
        .route("/v1/proxy/image", get(proxy_image))
        .route("/v1/openrouter/settings", get(openrouter_settings).put(update_openrouter_settings))
        .route("/v1/assistant/status", get(assistant_status).put(update_assistant_settings))
        .route("/v1/assistant/write", post(assistant_write))
        .route("/v1/assistant/write/stream", post(assistant_write_stream))
        .route("/v1/assistant/runtime", get(assistant_runtime_status))
        .route("/v1/assistant/runtime/install", post(assistant_runtime_install))
        .route("/v1/assistant/runtime/start", post(assistant_runtime_start))
        .route("/v1/assistant/runtime/stop", post(assistant_runtime_stop))
        .route("/v1/karaoke/status", get(karaoke_status).put(update_karaoke_settings))
        .route("/v1/karaoke/install", post(karaoke_install))
        .route("/v1/karaoke/remove", post(karaoke_remove))
        .route("/v1/library/songs/{id}/karaoke", post(create_song_karaoke).delete(delete_song_karaoke))
        .route("/v1/openrouter/catalog", get(openrouter_catalog))
        .route("/v1/openrouter/catalog/refresh", post(refresh_openrouter_catalog))
        .route("/v1/openrouter/transcriptions", post(create_openrouter_transcription))
        .route("/v1/openrouter/covers", post(create_openrouter_cover))
        .route("/editor", get(|| async { axum::response::Redirect::permanent("/editor/index.html") }))
        .route("/editor/{*path}", get(editor_asset))
        .route("/v1/separation/runtime", get(separation_assets))
        .route("/v1/separation/runtime/install", post(install_separation_asset))
        .route("/v1/separation/status", get(separation_status))
        .route("/v1/separation/settings", get(read_separation_settings).put(write_separation_settings))
        .route("/v1/separation/install", post(install_separation_model))
        .route("/v1/separation/remove", post(remove_separation_model))
        .route("/v1/library/songs/{id}/stems", get(read_stems).post(start_separation))
        .route("/v1/library/songs/{id}/stems/{stem}", get(read_stem_audio))
        .route("/v1/library/songs/{id}/cover/auto", post(draw_cover_now))
        .route("/v1/activity", get(read_activity))
        .route("/v1/cover-templates", get(read_cover_templates).put(write_cover_templates))
        .route("/v1/cover-templates/render", post(render_cover_template))
        .route("/v1/openrouter/completions", post(create_openrouter_completion))
        .route("/v1/library/songs", get(library_songs).post(create_library_song))
        .route("/v1/library/import", post(import_library_audio))
        .route("/v1/library/songs/{id}", get(library_song).put(update_library_song).delete(delete_library_song))
        .route("/v1/library/media/{song_id}", get(library_media))
        .route("/v1/library/songs/{id}/cover", get(library_cover).put(store_library_cover))
        .route("/v1/library/playlists", get(library_playlists).post(create_library_playlist))
        .route("/v1/library/playlists/{id}", get(library_playlist).put(update_library_playlist).delete(delete_library_playlist))
        .route("/setup/status", get(setup_status))
        .route("/setup/catalog", get(setup_catalog))
        .route("/setup/download", post(setup_download))
        .route("/setup/remove", post(setup_remove))
        .route("/v1/open-data-directory", post(open_data_directory))
        .route("/setup/select", post(setup_select))
        .route("/setup/cancel", post(setup_cancel))
        .route("/v1/local-models/music", get(local_music_model_catalog))
        .route("/v1/music/jobs", post(create_music_job))
        .route("/v1/music/replay", post(replay_music_job))
        .route(
            "/v1/music/jobs/{job_id}",
            get(music_job_status).post(cancel_music_job),
        )
        .with_state(state.clone())
        // Covers and imported audio are megabytes, not kilobytes. The default
        // two-megabyte cap rejected a generated cover by dropping the
        // connection, which reaches the interface as "Failed to fetch".
        .layer(DefaultBodyLimit::max(256 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    // The provider catalog is public and small; reading it once at startup
    // means the settings panel is right the first time it is opened, instead
    // of after the user presses a refresh button.
    // Start the engine as soon as a complete set is installed. It takes about
    // three seconds; making the user press a button for it - or worse, wait
    // without knowing what for - is the studio being lazy on their time.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let ready = state.model_manager.status(effective_install_target(&state).await).await.ready;
            if !ready || state.music_server.health().await {
                return;
            }
            if let Err(error) = restart_engine(&state).await {
                eprintln!("the local engine did not start on its own: {error}");
            }
        });
    }

    let address = SocketAddr::from(([127, 0, 0, 1], listen_port()));
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("music-server listening on http://{address}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn library_songs(State(state): State<AppState>) -> Result<Json<Vec<library::Song>>, (StatusCode, Json<ApiError>)> { state.library.list_songs().map(Json).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string())) }
async fn library_song(State(state): State<AppState>,Path(id):Path<String>)->Result<Json<library::Song>,(StatusCode,Json<ApiError>)>{state.library.get_song(&id).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?.map(Json).ok_or_else(||api_error(StatusCode::NOT_FOUND,"Song not found".into()))}
async fn library_media(State(state): State<AppState>, Path(song_id): Path<String>, headers: HeaderMap) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let song = state.library.get_song(&song_id).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?.ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
    let path = state.library.media_path_for_song(&song).ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song audio is not available in the studio media library".into()))?;
    // Tracks made before the studio tagged anything have no ID3 at all, and a
    // download of one lands in a player as an untitled file. Tag it on the way
    // out, once: the check is three bytes.
    if path.extension().and_then(|value| value.to_str()).map(str::to_ascii_lowercase).as_deref() == Some("mp3")
        && tokio::fs::read(&path).await.map(|bytes| bytes.get(..3) != Some(b"ID3")).unwrap_or(false)
    {
        tag_stored_song(&state, &song_id).await;
    }
    let bytes = tokio::fs::read(&path).await.map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("read song audio: {error}")))?;
    let content_type = match path.extension().and_then(|extension| extension.to_str()).map(|extension| extension.to_ascii_lowercase()).as_deref() {
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        _ => return Err(api_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Stored song has an unsupported audio extension".into())),
    };
    let total = bytes.len();
    let range = headers.get(header::RANGE).and_then(|value| value.to_str().ok()).and_then(|value| parse_single_byte_range(value, total));
    let response = if let Some((start, end)) = range {
        axum::response::Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
            .header(header::CONTENT_LENGTH, end - start + 1)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(bytes[start..=end].to_vec()))
    } else if headers.contains_key(header::RANGE) {
        axum::response::Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{total}"))
            .body(Body::empty())
    } else {
        axum::response::Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, total)
            .header(header::ACCEPT_RANGES, "bytes")
            .body(Body::from(bytes))
    };
    Ok(response.expect("valid audio response"))
}

/// Parses the one byte-range form used by HTMLAudioElement. Multiple ranges are
/// intentionally declined; a single 206 keeps native seeking interoperable.
fn parse_single_byte_range(value: &str, total: usize) -> Option<(usize, usize)> {
    let value = value.strip_prefix("bytes=")?;
    if value.contains(',') || total == 0 { return None; }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<usize>().ok()?;
        if suffix == 0 { return None; }
        return Some((total.saturating_sub(suffix), total - 1));
    }
    let start = start.parse::<usize>().ok()?;
    if start >= total { return None; }
    let end = if end.is_empty() { total - 1 } else { end.parse::<usize>().ok()?.min(total - 1) };
    (end >= start).then_some((start, end))
}
#[derive(Debug, Deserialize)]
struct StoreCoverRequest {
    /// Raw base64 image bytes, without a data-URL prefix.
    image_base64: String,
    media_type: String,
}

/// Cover art is Studio-side metadata: a track keeps working without one, so a
/// missing cover is a 404 the UI answers with its generated placeholder art
/// rather than an error state.
#[derive(Debug, Deserialize)]
struct CoverTemplatesRequest {
    templates: Vec<cover_prompt::CoverTemplate>,
    /// Draw a cover as soon as a track finishes.
    #[serde(default)]
    auto: Option<bool>,
    /// Which of them a new cover starts from. `None` leaves it as it was.
    #[serde(default)]
    default_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RenderCoverPromptRequest {
    template: String,
    /// The track the prompt is for. Without it the placeholders have nothing
    /// to stand in for, which is only useful for previewing the wording.
    song_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    lyrics: Option<String>,
}


/// The waveform editor, carried inside the binary.
///
/// It is a static web application; embedding it keeps the promise that the
/// studio is one executable, and serving it over the studio's own port means
/// the browser can open it with the track already loaded.
static EDITOR: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../app/public/editor");

async fn editor_asset(Path(path): Path<String>) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let file = EDITOR
        .get_file(path.trim_start_matches('/'))
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, format!("the editor has no file {path}")))?;
    let media_type = match std::path::Path::new(&path).extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("mp3") => "audio/mpeg",
        Some("mp4") => "video/mp4",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    };
    Ok(axum::response::Response::builder()
        .header(header::CONTENT_TYPE, media_type)
        .header(header::CONTENT_LENGTH, file.contents().len())
        .body(Body::from(file.contents().to_vec()))
        .expect("valid editor response"))
}

/// One separation in progress, as the interface sees it.
#[derive(Debug, Clone, Serialize)]
struct SeparationRun {
    song_id: String,
    /// Between 0 and 1.
    progress: f64,
    done: bool,
    error: Option<String>,
    stems: Vec<String>,
    /// Whether the graphics card did the work, once the run is over.
    used_gpu: Option<bool>,
}

/// Where a song's stems live: beside the track, named after it.
fn stem_path(state: &AppState, song_id: &str, stem: &str) -> PathBuf {
    state.library.media_dir().join(format!("{song_id}-{stem}.wav"))
}

fn stems_on_disk(state: &AppState, song_id: &str) -> Vec<String> {
    separation::STEMS
        .iter()
        .filter(|stem| stem_path(state, song_id, stem).is_file())
        .map(|stem| (*stem).to_string())
        .collect()
}

/// The separator as an optional module: its files, and whichever download is
/// running. The same envelope the assistant and karaoke use, because the models
/// page lists all three the same way.
async fn separation_assets(State(state): State<AppState>) -> Json<Value> {
    let runtime_installed = state.lyrics_sync.onnxruntime_library().is_some();
    let assets = serde_json::json!([
        {
            "id": separation::MODEL.id,
            "label": separation::MODEL.label,
            "bytes": separation::MODEL.bytes,
            "note": separation::MODEL.note,
            "installed": state.separator.is_installed(),
        },
        {
            "id": "onnxruntime-cuda",
            "label": "ONNX Runtime 1.24.2 · CUDA",
            "bytes": 280_855_316u64,
            "note": "The CUDA build of the runtime.",
            "installed": state.lyrics_sync.has_cuda_runtime(),
        },
        {
            "id": "cuda-cublas",
            "label": "NVIDIA cuBLAS 12.9",
            "bytes": 549_731_131u64,
            "note": "The linear algebra the CUDA provider is built on.",
            "installed": state.lyrics_sync.downloader().runtime_dir("onnx-cuda").join("cublasLt64_12.dll").is_file(),
        },
        {
            "id": "cuda-cudart",
            "label": "NVIDIA CUDA runtime 12.9",
            "bytes": 3_521_238u64,
            "note": "The CUDA runtime itself.",
            "installed": state.lyrics_sync.downloader().runtime_dir("onnx-cuda").join("cudart64_12.dll").is_file(),
        },
        {
            "id": "cuda-cudnn",
            "label": "NVIDIA cuDNN 9.25",
            "bytes": 1_904_452_100u64,
            "note": "The convolution kernels the separator spends its time in.",
            "installed": state.lyrics_sync.downloader().runtime_dir("onnx-cuda").join("cudnn64_9.dll").is_file(),
        },
        {
            "id": "onnxruntime",
            "label": "ONNX Runtime 1.24.2",
            "bytes": 74_075_355,
            "note": "Runs the separator and the karaoke recogniser; shared between them.",
            "installed": runtime_installed,
        }
    ]);
    Json(serde_json::json!({
        "assets": assets,
        "active_download": state.separator.downloader().active().await.or(state.lyrics_sync.downloader().active().await),
    }))
}

#[derive(Debug, Deserialize)]
struct InstallSeparationAssetRequest {
    asset_id: String,
}

/// Everything the card path needs, in the order it is used.
const CARD_ASSETS: [&str; 5] = ["onnxruntime-cuda", "cuda-cudart", "cuda-cublas", "cuda-cufft", "cuda-cudnn"];

async fn install_separation_asset(
    State(state): State<AppState>,
    Json(request): Json<InstallSeparationAssetRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    // "card" means whatever is still missing for the graphics card, one after
    // another: asking someone to press four buttons in the right order is not a
    // setup, it is a quiz.
    if request.asset_id == "card" {
        let sync = state.lyrics_sync.clone();
        tokio::spawn(async move {
            for id in CARD_ASSETS {
                let Some(asset) = lyrics_sync::asset(id) else { continue };
                if sync.downloader().is_installed(asset) {
                    continue;
                }
                if let Err(error) = sync.downloader().install(asset).await {
                    eprintln!("could not install {id}: {error}");
                    return;
                }
                // The downloader takes one job at a time on purpose; wait for
                // this one before starting the next.
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    match sync.downloader().active().await {
                        Some(progress) if !progress.done => continue,
                        _ => break,
                    }
                }
            }
        });
        return Ok(Json(serde_json::json!({ "started": true })));
    }
    // Anything in the catalogue may be installed by name; listing the ids here
    // by hand is how cuFFT ended up silently rejected.
    // `install` starts a background task and returns immediately; its Result
    // says whether the download was accepted at all. Discarding it inside a
    // spawn - which is what this did - meant a refusal ("another download is
    // already running") was thrown away while the endpoint answered
    // "started: true". The button then did nothing, twice, silently, with a
    // successful reply, and no amount of error handling in the interface could
    // have shown it.
    if request.asset_id == separation::MODEL.id {
        state
            .separator
            .downloader()
            .install(&separation::MODEL)
            .await
            .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    } else if let Some(asset) = lyrics_sync::asset(&request.asset_id) {
        state
            .lyrics_sync
            .downloader()
            .install(asset)
            .await
            .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    } else {
        return Err(api_error(StatusCode::BAD_REQUEST, format!("unknown asset {}", request.asset_id)));
    }
    Ok(Json(serde_json::json!({ "started": true })))
}

async fn read_separation_settings(State(state): State<AppState>) -> Json<separation::SeparationConfig> {
    Json(state.separation_config.read().await.clone())
}

async fn write_separation_settings(
    State(state): State<AppState>,
    Json(config): Json<separation::SeparationConfig>,
) -> Result<Json<separation::SeparationConfig>, (StatusCode, Json<ApiError>)> {
    // A run that writes nothing is a run nobody wanted; an empty choice means
    // everything, which is also what the studio starts with.
    let stems: Vec<String> = if config.stems.is_empty() {
        separation::STEMS.iter().map(|stem| (*stem).to_string()).collect()
    } else {
        config.stems.iter().filter(|stem| separation::STEMS.contains(&stem.as_str())).cloned().collect()
    };
    let stored = separation::SeparationConfig { runtime: config.runtime, stems, overlap: config.sane_overlap() };
    *state.separation_config.write().await = stored.clone();
    persist_studio_settings(&state)
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(stored))
}

async fn separation_status(State(state): State<AppState>) -> Json<Value> {
    let runtime = state.lyrics_sync.onnxruntime_library();
    Json(serde_json::json!({
        "model": {
            "id": separation::MODEL.id,
            "label": separation::MODEL.label,
            "bytes": separation::MODEL.bytes,
            "note": separation::MODEL.note,
            "installed": state.separator.is_installed(),
        },
        "runtime_installed": runtime.is_some(),
        "cuda_runtime_installed": state.lyrics_sync.has_cuda_libraries(),
        "card_missing_bytes": CARD_ASSETS
            .iter()
            .filter_map(|id| lyrics_sync::asset(id))
            .filter(|asset| !state.lyrics_sync.downloader().is_installed(asset))
            .map(|asset| asset.bytes)
            .sum::<u64>(),
        "ready": state.separator.ready(runtime.as_deref()),
        "stems": separation::STEMS,
        // Either downloader may be the busy one: the model has its own, the
        // card libraries come through karaoke's. Reporting only the first is
        // what made a running download look like a dead button.
        "download": match state.separator.downloader().active().await {
            Some(active) if !active.done => Some(active),
            other => match state.lyrics_sync.downloader().active().await {
                Some(active) if !active.done => Some(active),
                fallback => fallback.or(other),
            },
        },
        "settings": state.separation_config.read().await.clone(),
        "run": state.separation_run.read().await.clone(),
    }))
}

/// Fetches the separation model. Nothing here downloads on its own; this is the
/// button, and it also brings the runtime if karaoke has not already.
async fn install_separation_model(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let separator = state.separator.clone();
    let sync = state.lyrics_sync.clone();
    tokio::spawn(async move {
        if sync.onnxruntime_library().is_none() {
            if let Some(runtime) = lyrics_sync::asset("onnxruntime") {
                let _ = sync.downloader().install(runtime).await;
            }
        }
        let _ = separator.downloader().install(&separation::MODEL).await;
    });
    Ok(Json(serde_json::json!({ "started": true })))
}

async fn read_stems(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    Json(serde_json::json!({
        "song_id": id,
        "stems": stems_on_disk(&state, &id),
        "run": state.separation_run.read().await.clone(),
    }))
}

async fn read_stem_audio(
    State(state): State<AppState>,
    Path((id, stem)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    if !separation::STEMS.contains(&stem.as_str()) {
        return Err(api_error(StatusCode::BAD_REQUEST, format!("unknown stem {stem}")));
    }
    let path = stem_path(&state, &id, &stem);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "this track has no such stem yet".into()))?;
    // Without range support a player cannot seek: it can only start at zero and
    // wait. The library's own audio has answered ranges from the beginning;
    // stems were served whole, which is why dragging their position did
    // nothing.
    let total = bytes.len();
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_single_byte_range(value, total));
    let response = if let Some((start, end)) = range {
        axum::response::Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, "audio/wav")
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
            .header(header::CONTENT_LENGTH, end - start + 1)
            .body(Body::from(bytes[start..=end].to_vec()))
    } else {
        axum::response::Response::builder()
            .header(header::CONTENT_TYPE, "audio/wav")
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, total)
            .body(Body::from(bytes))
    };
    Ok(response.expect("valid stem response"))
}

/// Separates one track into stems, in the background, reporting progress.
async fn start_separation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    if state.separation_run.read().await.as_ref().is_some_and(|run| !run.done) {
        return Err(api_error(StatusCode::CONFLICT, "a track is already being separated".into()));
    }
    let wanted_runtime = state.separation_config.read().await.runtime;
    // Which library is loaded is decided once per process - `ort` binds it on
    // first use - so always take the CUDA build when it is complete: it carries
    // the processor provider too, and the setting below decides which of them
    // actually runs. Choosing by the setting meant a studio that had run once
    // on the processor could never reach the card without a restart.
    let runtime = state
        .lyrics_sync
        .onnxruntime_library_of(if state.lyrics_sync.has_cuda_libraries() {
            lyrics_sync::OnnxFlavour::Cuda
        } else {
            lyrics_sync::OnnxFlavour::Cpu
        })
        .or_else(|| state.lyrics_sync.onnxruntime_library())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "the ONNX Runtime is not installed yet".into()))?;
    // The card is only really available when every CUDA library the provider
    // links against is beside it.
    let on_gpu = !matches!(wanted_runtime, lyrics_sync::OnnxFlavour::Cpu)
        && state.lyrics_sync.has_cuda_libraries();
    if !state.separator.is_installed() {
        return Err(api_error(StatusCode::BAD_REQUEST, "the separation model is not installed yet".into()));
    }
    let song = state
        .library
        .get_song(&id)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
    let audio_path = state
        .library
        .media_path_for_song(&song)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "this track has no stored audio".into()))?;

    *state.separation_run.write().await =
        Some(SeparationRun { song_id: id.clone(), progress: 0.0, done: false, error: None, stems: vec![], used_gpu: None });

    let model = state.separator.model_path();
    let config = state.separation_config.read().await.clone();
    let overlap = config.sane_overlap();
    let wanted = config.stems.clone();
    let background = state.clone();
    let song_id = id.clone();
    tokio::task::spawn_blocking(move || {
        // Must be set before anything touches `ort`, or it binds to whatever
        // onnxruntime.dll the system happens to have.
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            std::env::set_var("ORT_DYLIB_PATH", &runtime);
            // Windows resolves a DLL's own dependencies through the process
            // search path, not through the folder the DLL came from. The CUDA
            // provider sits next to cuBLAS and cuDNN and still reported them
            // "missing" until this directory was on PATH.
            if let Some(directory) = runtime.parent() {
                let existing = std::env::var("PATH").unwrap_or_default();
                std::env::set_var("PATH", format!("{};{existing}", directory.display()));
            }
        });

        let outcome = (|| -> anyhow::Result<(Vec<String>, bool)> {
            let audio = audio_pcm::decode_stereo_44k(&audio_path)?;
            let handle = tokio::runtime::Handle::current();
            let separated = separation::separate(&model, &audio, separation::STEMS.len(), overlap, on_gpu, |fraction| {
                let state = background.clone();
                handle.spawn(async move {
                    if let Some(run) = state.separation_run.write().await.as_mut() {
                        run.progress = fraction;
                    }
                });
            })?;
            let mut written = Vec::new();
            let ran_on_gpu = separated.used_gpu;
            for stem in separated.stems {
                if !wanted.iter().any(|name| name == stem.name) {
                    continue;
                }
                let path = stem_path(&background, &song_id, stem.name);
                separation::write_wav_stereo(&path, &stem.samples)?;
                written.push(stem.name.to_string());
            }
            Ok((written, ran_on_gpu))
        })();

        let handle = tokio::runtime::Handle::current();
        handle.spawn(async move {
            if let Some(run) = background.separation_run.write().await.as_mut() {
                run.done = true;
                match outcome {
                    Ok((stems, ran_on_gpu)) => {
                        run.progress = 1.0;
                        run.stems = stems;
                        run.used_gpu = Some(ran_on_gpu);
                    }
                    Err(error) => run.error = Some(error.to_string()),
                }
            }
        });
    });

    Ok(Json(serde_json::json!({ "started": true, "song_id": id })))
}


/// Draws a cover for a finished track, if the studio was told to.
///
/// The same pieces the cover window uses: the default template, filled in from
/// this track, and the image model chosen on the provider page. Nothing happens
/// without a key, without a model, or when the user turned this off - and a
/// failure is written to the log rather than shown as a broken track.
/// Draws the cover for one track now, and says what went wrong if it did not.
async fn draw_cover_now(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    match draw_cover(&state, &id).await {
        Ok(()) => Ok(Json(serde_json::json!({ "drawn": true }))),
        Err(error) => Err(api_error(StatusCode::BAD_GATEWAY, error.to_string())),
    }
}


/// Times the lyrics of a finished track, if karaoke is switched on.
///
/// The switch said "on" and nothing happened: the timings were only ever made
/// by the button in the track menu. A track arrives with its words already
/// known, so this is the moment to time them.

/// One background piece of work on a finished track.
#[derive(Debug, Clone, Serialize)]
struct Activity {
    song_id: String,
    title: String,
    /// "cover" or "karaoke".
    kind: &'static str,
    /// "running", "done" or "failed".
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// Notes what is happening, keeping only the recent past.
async fn note_activity(state: &AppState, song_id: &str, title: &str, kind: &'static str, phase: &'static str, detail: Option<String>) {
    let mut activity = state.activity.write().await;
    if let Some(existing) = activity.iter_mut().find(|entry| entry.song_id == song_id && entry.kind == kind) {
        existing.state = phase;
        existing.detail = detail;
        existing.title = title.to_string();
    } else {
        activity.push(Activity {
            song_id: song_id.to_string(),
            title: title.to_string(),
            kind,
            state: phase,
            detail,
        });
    }
    let overflow = activity.len().saturating_sub(20);
    if overflow > 0 {
        activity.drain(..overflow);
    }
}

async fn read_activity(State(state): State<AppState>) -> Json<Value> {
    Json(serde_json::json!({ "activity": state.activity.read().await.clone() }))
}

/// Downloads whatever the chosen local recogniser is missing, then waits for it.
///
/// Choosing Parakeet or Whisper in the settings is the instruction to use it;
/// making the user then find a download button for it is a second instruction
/// nobody asked for. The first track that needs timings fetches the model and
/// carries on.
async fn ensure_local_recogniser(state: &AppState, config: &lyrics_sync::LyricsSyncConfig, song_id: &str) -> bool {
    let ready = |state: &AppState| match config.provider {
        lyrics_sync::AsrProvider::Parakeet => state.lyrics_sync.parakeet_ready(),
        lyrics_sync::AsrProvider::Whisper => {
            state.lyrics_sync.whisper_binary().is_some() && state.lyrics_sync.whisper_model_ready(config)
        }
        _ => true,
    };
    if ready(state) {
        return true;
    }

    let missing: Vec<&'static lyrics_sync::Asset> = match config.provider {
        lyrics_sync::AsrProvider::Parakeet => lyrics_sync::PARAKEET_ASSET_IDS
            .iter()
            .filter_map(|id| lyrics_sync::asset(id))
            .filter(|asset| !state.lyrics_sync.downloader().is_installed(asset))
            .collect(),
        lyrics_sync::AsrProvider::Whisper => ["whisper-cuda", "whisper-cpu"]
            .iter()
            .filter_map(|id| lyrics_sync::asset(id))
            .take(1)
            .chain(config.whisper_model.as_deref().and_then(lyrics_sync::asset))
            .filter(|asset| !state.lyrics_sync.downloader().is_installed(asset))
            .collect(),
        _ => Vec::new(),
    };
    if missing.is_empty() {
        return ready(state);
    }

    let title = state.library.get_song(song_id).ok().flatten().map(|song| song.title).unwrap_or_default();
    note_activity(state, song_id, &title, "karaoke", "running", Some("karaoke.downloading".into())).await;
    for asset in missing {
        if let Err(error) = state.lyrics_sync.downloader().install(asset).await {
            eprintln!("could not fetch the karaoke model {}: {error}", asset.id);
            note_activity(state, song_id, &title, "karaoke", "failed", Some(error.to_string())).await;
            return false;
        }
        // The downloader runs one file at a time in the background; the timings
        // wait for it rather than starting against half a model. The wait is
        // reported with real numbers: a spinner that says "downloading" for ten
        // minutes without moving is indistinguishable from one that is stuck.
        loop {
            let Some(progress) = state.lyrics_sync.downloader().active().await else { break };
            if progress.done {
                break;
            }
            let percent = if progress.total_bytes > 0 {
                (progress.downloaded_bytes * 100 / progress.total_bytes).min(100)
            } else {
                0
            };
            note_activity(state, song_id, &title, "karaoke", "running", Some(format!("karaoke.downloading {percent}%"))).await;
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        }
    }
    ready(state)
}

async fn time_lyrics_for(state: AppState, song_id: String) {
    let config = state.lyrics_sync_config.read().await.clone();
    if !config.enabled || config.provider == lyrics_sync::AsrProvider::None {
        return;
    }
    // The cloud recogniser is the one case that cannot be fixed from here: a key
    // is the user's to add, and announcing a failure they cannot act on is
    // noise. A local recogniser is different - if its model is not on disk yet,
    // choosing it is the instruction to fetch it, so the first use downloads it
    // and then does the work.
    if config.provider == lyrics_sync::AsrProvider::OpenRouter && credentials::openrouter_api_key().is_none() {
        return;
    }
    if !ensure_local_recogniser(&state, &config, &song_id).await {
        return;
    }
    let Ok(Some(song)) = state.library.get_song(&song_id) else { return };
    // An instrumental has section markers and no words. Timing it means asking
    // the recogniser to find lyrics that were never sung.
    if !auto_title::has_sung_lines(&song.lyrics) {
        return;
    }
    let Some(audio) = state.library.media_path_for_song(&song) else { return };
    let audio = audio.display().to_string();

    note_activity(&state, &song_id, &song.title, "karaoke", "running", None).await;
    let words = match config.provider {
        lyrics_sync::AsrProvider::None => return,
        lyrics_sync::AsrProvider::Parakeet => {
            let sync = state.lyrics_sync.clone();
            let path = std::path::PathBuf::from(&audio);
            match tokio::task::spawn_blocking(move || sync.parakeet_words(&path)).await {
                Ok(result) => result,
                Err(error) => {
                    eprintln!("no karaoke for {song_id}: {error}");
                    return;
                }
            }
        }
        lyrics_sync::AsrProvider::Whisper => {
            let sync = state.lyrics_sync.clone();
            let config = config.clone();
            let path = std::path::PathBuf::from(&audio);
            let lyrics = song.lyrics.clone();
            match tokio::task::spawn_blocking(move || sync.whisper_words(&config, &path, None, &lyrics)).await {
                Ok(result) => result,
                Err(error) => {
                    eprintln!("no karaoke for {song_id}: {error}");
                    return;
                }
            }
        }
        lyrics_sync::AsrProvider::OpenRouter => {
            karaoke_words_from_openrouter(&state, &config, &audio, None).await
        }
    };
    let words = match words {
        Ok(words) => words,
        Err(error) => {
            eprintln!("no karaoke for {song_id}: {error}");
            note_activity(&state, &song_id, &song.title, "karaoke", "failed", Some(error.to_string())).await;
            return;
        }
    };
    let lines = lyrics_sync::align_lyrics_words(&words, &song.lyrics);
    if lines.is_empty() {
        let reason = "karaoke.no-match";
        eprintln!("no karaoke for {song_id}: {reason}");
        note_activity(&state, &song_id, &song.title, "karaoke", "failed", Some(reason.to_string())).await;
        return;
    }
    match state.library.set_song_lrc(&song_id, &lyrics_sync::enhanced_lrc(&lines)) {
        Ok(_) => note_activity(&state, &song_id, &song.title, "karaoke", "done", None).await,
        Err(error) => {
            eprintln!("could not store karaoke for {song_id}: {error}");
            note_activity(&state, &song_id, &song.title, "karaoke", "failed", Some(error.to_string())).await;
        }
    }
}

async fn draw_cover_for(state: AppState, song_id: String) {
    if !*state.cover_auto.read().await {
        return;
    }
    // Drawing a cover needs a cloud key. Without one there is nothing to try,
    // and announcing a failure the user cannot act on is noise: the track keeps
    // the placeholder artwork the library already shows.
    if credentials::openrouter_api_key().is_none() {
        return;
    }
    let title = state.library.get_song(&song_id).ok().flatten().map(|song| song.title).unwrap_or_default();
    note_activity(&state, &song_id, &title, "cover", "running", None).await;
    match draw_cover(&state, &song_id).await {
        Ok(()) => note_activity(&state, &song_id, &title, "cover", "done", None).await,
        Err(error) => {
            eprintln!("no cover for {song_id}: {error}");
            note_activity(&state, &song_id, &title, "cover", "failed", Some(error.to_string())).await;
        }
    }
}

/// The work itself, with its reasons kept rather than printed.
async fn draw_cover(state: &AppState, song_id: &str) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let song = state
        .library
        .get_song(song_id)?
        .context("the track is not in the library")?;
    if song.metadata.get("cover_filename").is_some() {
        return Ok(());
    }
    let model = {
        let configuration = state.configuration.read().await;
        configuration
            .selections
            .iter()
            .find(|selection| selection.capability == Capability::CoverArt)
            .filter(|selection| selection.mode == ExecutionMode::OpenRouter)
            .and_then(|selection| selection.cloud_model.clone())
            .filter(|model| !model.trim().is_empty())
    };
    let catalog = catalog_for(state).await.map_err(|error| anyhow::anyhow!(error))?;
    let model = match model {
        Some(model) => model,
        None => providers::openrouter::suggested_model(&catalog, Capability::CoverArt)
            .context("no image model is chosen for covers")?,
    };
    let templates = state.cover_templates.read().await.clone();
    let default_id = state.cover_template_default.read().await.clone();
    let template = templates
        .iter()
        .find(|entry| Some(&entry.id) == default_id.as_ref())
        .or_else(|| templates.first())
        .map(|entry| entry.template.clone())
        .context("there are no cover templates")?;
    let facts = cover_prompt::TrackFacts {
        title: song.title.clone(),
        style: song.caption.clone(),
        lyrics: song.lyrics.clone(),
        duration_seconds: song.metadata.get("duration_seconds").and_then(Value::as_f64).unwrap_or(0.0),
    };
    let prompt = match song.metadata.get("cover_prompt").and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()) {
        Some(written) => written.to_string(),
        None => cover_prompt::render(&template, &facts),
    };
    let request = providers::openrouter::request_for(&catalog, Capability::CoverArt, &model, &prompt)?;
    let answered = execute_openrouter_json(request).await.map_err(|error| anyhow::anyhow!(error))?;
    let first = answered
        .body
        .get("data")
        .and_then(|data| data.get(0))
        .context("the model returned no image")?;
    let image = first
        .get("b64_json")
        .and_then(Value::as_str)
        .context("the model returned no image")?;
    // The answer states its own format, and it is not always PNG.
    let media_type = first.get("media_type").and_then(Value::as_str).unwrap_or("image/png").to_string();
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes = STANDARD.decode(image.trim()).context("the image was not valid base64")?;
    state.library.store_song_cover(song_id, &bytes, &media_type)?;
    tag_stored_song(state, song_id).await;
    Ok(())
}

#[allow(dead_code)]
async fn draw_cover_unused(state: AppState, song_id: String) {
    let Ok(Some(song)) = state.library.get_song(&song_id) else { return };
    if song.metadata.get("cover_filename").is_some() {
        return;
    }

    let model = {
        let configuration = state.configuration.read().await;
        configuration
            .selections
            .iter()
            .find(|selection| selection.capability == Capability::CoverArt)
            .filter(|selection| selection.mode == ExecutionMode::OpenRouter)
            .and_then(|selection| selection.cloud_model.clone())
    };
    let model = match model {
        Some(model) if !model.trim().is_empty() => model,
        _ => match catalog_for(&state).await.ok().and_then(|catalog| {
            providers::openrouter::suggested_model(&catalog, Capability::CoverArt)
        }) {
            Some(model) => model,
            None => return,
        },
    };

    let template = {
        let templates = state.cover_templates.read().await;
        let default_id = state.cover_template_default.read().await.clone();
        templates
            .iter()
            .find(|entry| Some(&entry.id) == default_id.as_ref())
            .or_else(|| templates.first())
            .map(|entry| entry.template.clone())
    };
    let Some(template) = template else { return };

    let facts = cover_prompt::TrackFacts {
        title: song.title.clone(),
        style: song.caption.clone(),
        lyrics: song.lyrics.clone(),
        duration_seconds: song.metadata.get("duration_seconds").and_then(Value::as_f64).unwrap_or(0.0),
    };
    // What the assistant wrote for this track beats the generic template: it
    // was written with the lyrics in front of it.
    let prompt = match song.metadata.get("cover_prompt").and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()) {
        Some(written) => written.to_string(),
        None => cover_prompt::render(&template, &facts),
    };

    let catalog = match catalog_for(&state).await {
        Ok(catalog) => catalog,
        Err(_) => return,
    };
    let request = match providers::openrouter::request_for(&catalog, Capability::CoverArt, &model, &prompt) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("no cover for {song_id}: {error}");
            return;
        }
    };
    let answered = match execute_openrouter_json(request).await {
        Ok(answered) => answered,
        Err(error) => {
            eprintln!("no cover for {song_id}: {error}");
            return;
        }
    };
    let image = answered
        .body
        .get("data")
        .and_then(|data| data.get(0))
        .and_then(|first| first.get("b64_json"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Some(image) = image else {
        eprintln!("no cover for {song_id}: the model returned no image");
        return;
    };
    use base64::{engine::general_purpose::STANDARD, Engine};
    let Ok(bytes) = STANDARD.decode(image.trim()) else { return };
    if let Err(error) = state.library.store_song_cover(&song_id, &bytes, "image/png") {
        eprintln!("could not store the cover for {song_id}: {error}");
        return;
    }
    tag_stored_song(&state, &song_id).await;
}

async fn read_cover_templates(State(state): State<AppState>) -> Json<Value> {
    Json(serde_json::json!({
        "auto": *state.cover_auto.read().await,
        "templates": state.cover_templates.read().await.clone(),
        "default_id": state.cover_template_default.read().await.clone(),
        "placeholders": ["title", "style", "lyrics", "excerpt", "duration"],
    }))
}

async fn write_cover_templates(
    State(state): State<AppState>,
    Json(request): Json<CoverTemplatesRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let templates = if request.templates.is_empty() { cover_prompt::default_templates() } else { request.templates };
    *state.cover_templates.write().await = templates.clone();
    // A default that names a template nobody kept is worse than none.
    let default_id = request
        .default_id
        .filter(|id| !id.trim().is_empty() && templates.iter().any(|entry| entry.id == *id));
    *state.cover_template_default.write().await = default_id.clone();
    if let Some(auto) = request.auto {
        *state.cover_auto.write().await = auto;
    }
    persist_studio_settings(&state)
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(serde_json::json!({
        "templates": templates,
        "default_id": default_id,
        "auto": *state.cover_auto.read().await,
    })))
}

/// The prompt a template turns into for one track, exactly as it would be sent.
async fn render_cover_template(
    State(state): State<AppState>,
    Json(request): Json<RenderCoverPromptRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let mut facts = cover_prompt::TrackFacts {
        title: request.title.unwrap_or_default(),
        style: request.style.unwrap_or_default(),
        lyrics: request.lyrics.unwrap_or_default(),
        duration_seconds: 0.0,
    };
    if let Some(song_id) = request.song_id.as_deref().filter(|value| !value.trim().is_empty()) {
        let song = state
            .library
            .get_song(song_id)
            .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
        facts.title = song.title.clone();
        facts.style = song.caption.clone();
        facts.lyrics = song.lyrics.clone();
        facts.duration_seconds = song
            .metadata
            .get("duration_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
    }
    Ok(Json(serde_json::json!({ "prompt": cover_prompt::render(&request.template, &facts) })))
}

async fn library_cover(State(state): State<AppState>, Path(id): Path<String>) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let song = state.library.get_song(&id).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found".into()))?;
    let (path, media_type) = state.library.cover_path_for_song(&song).ok_or_else(|| api_error(StatusCode::NOT_FOUND, "This song has no stored cover image".into()))?;
    let bytes = tokio::fs::read(&path).await.map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("read cover: {error}")))?;
    Ok(axum::response::Response::builder()
        .header(header::CONTENT_TYPE, media_type)
        .header(header::CONTENT_LENGTH, bytes.len())
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(bytes))
        .expect("valid cover response"))
}


/// Writes ID3 tags onto a stored MP3 from what the library knows about it.
///
/// Called after a track is stored, after its cover changes and after it is
/// renamed. Failure is logged and never fails the request: an untagged track
/// still plays, a lost one does not.
async fn tag_stored_song(state: &AppState, song_id: &str) {
    let Ok(Some(song)) = state.library.get_song(song_id) else { return };
    // `audio_path` is a full path, not a filename: resolve it the way playback
    // does, or tagging silently skips every track.
    let Some(audio_path) = state.library.media_path_for_song(&song) else { return };
    if audio_path.extension().and_then(|value| value.to_str()).map(str::to_lowercase).as_deref() != Some("mp3") {
        return;
    }
    let cover = state
        .library
        .cover_path_for_song(&song)
        .and_then(|(path, media_type)| std::fs::read(path).ok().map(|bytes| (media_type.to_string(), bytes)));
    let tags = tagging::TrackTags {
        title: song.title.clone(),
        album: "MiniMax Music3 Studio".to_string(),
        // The engine is the performer here; the studio is the label.
        artist: "MiniMax Music 3".to_string(),
        genre: tagging::genre_from_caption(&song.caption),
        lyrics: Some(song.lyrics.clone()).filter(|value| !value.trim().is_empty()),
        bpm: tagging::bpm_from_caption(&song.caption),
        cover,
    };
    if let Err(error) = tagging::write_mp3_tags(&audio_path, &tags) {
        eprintln!("could not tag {}: {error}", audio_path.display());
    }
}

async fn store_library_cover(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<StoreCoverRequest>,
) -> Result<Json<library::Song>, (StatusCode, Json<ApiError>)> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let image = STANDARD
        .decode(request.image_base64.trim())
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, format!("cover image is not valid base64: {error}")))?;
    let song = state
        .library
        .store_song_cover(&id, &image, &request.media_type)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    // The cover belongs in the file too, not only beside it.
    tag_stored_song(&state, &id).await;
    Ok(Json(song))
}

async fn create_library_song(State(state):State<AppState>,Json(input):Json<library::SongInput>)->Result<(StatusCode,Json<library::Song>),(StatusCode,Json<ApiError>)>{state.library.create_song(input).map(|s|(StatusCode::CREATED,Json(s))).map_err(|e|api_error(StatusCode::BAD_REQUEST,e.to_string()))}
async fn import_library_audio(State(state): State<AppState>, mut multipart: Multipart) -> Result<(StatusCode, Json<library::Song>), (StatusCode, Json<ApiError>)> {
    let mut title = None; let mut caption = String::new(); let mut lyrics = String::new(); let mut audio = None; let mut filename = None;
    while let Some(field) = multipart.next_field().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read import form: {e}")))? {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "audio" { filename = field.file_name().map(str::to_owned); audio = Some(field.bytes().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read audio upload: {e}")))?.to_vec()); }
        else { let value = field.text().await.map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("read import field: {e}")))?; match name.as_str() { "title" => title = Some(value), "caption" => caption = value, "lyrics" => lyrics = value, _ => {} } }
    }
    let filename = filename.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "audio file is required".into()))?;
    let extension = std::path::Path::new(&filename).extension().and_then(|value| value.to_str()).unwrap_or_default().to_owned();
    let title = title.filter(|value| !value.trim().is_empty()).unwrap_or_else(|| std::path::Path::new(&filename).file_stem().and_then(|value| value.to_str()).unwrap_or("Imported audio").to_owned());
    let audio = audio.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "audio file is required".into()))?;
    let duration = library::audio_duration_seconds(&audio, &extension.to_ascii_lowercase(), None);
    let song = state.library.import_audio_song(library::AudioImportInput { title, caption, lyrics, metadata: serde_json::json!({"imported_filename": filename, "duration_seconds": duration}), generation_settings: Value::Null, engine_id: "imported-audio".into(), profile_id: None, source: "audio_import".into(), audio_extension: extension, audio }).map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?.song;
    Ok((StatusCode::CREATED, Json(song)))
}
async fn update_library_song(State(state):State<AppState>,Path(id):Path<String>,Json(input):Json<library::SongInput>)->Result<Json<library::Song>,(StatusCode,Json<ApiError>)>{
    let song = state.library.update_song(&id,input).map_err(|e|api_error(StatusCode::BAD_REQUEST,e.to_string()))?.ok_or_else(||api_error(StatusCode::NOT_FOUND,"Song not found".into()))?;
    // A rename is a title change, and the file carries the title.
    tag_stored_song(&state, &id).await;
    Ok(Json(song))
}
async fn delete_library_song(State(state):State<AppState>,Path(id):Path<String>)->Result<StatusCode,(StatusCode,Json<ApiError>)>{if state.library.delete_song(&id).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?{Ok(StatusCode::NO_CONTENT)}else{Err(api_error(StatusCode::NOT_FOUND,"Song not found".into()))}}
async fn library_playlists(State(state):State<AppState>)->Result<Json<Vec<library::Playlist>>,(StatusCode,Json<ApiError>)>{state.library.list_playlists().map(Json).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))}
async fn create_library_playlist(State(state):State<AppState>,Json(input):Json<library::PlaylistInput>)->Result<(StatusCode,Json<library::Playlist>),(StatusCode,Json<ApiError>)>{state.library.create_playlist(input).map(|p|(StatusCode::CREATED,Json(p))).map_err(|e|api_error(StatusCode::BAD_REQUEST,e.to_string()))}
async fn library_playlist(State(state):State<AppState>,Path(id):Path<String>)->Result<Json<library::Playlist>,(StatusCode,Json<ApiError>)>{state.library.get_playlist(&id).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?.map(Json).ok_or_else(||api_error(StatusCode::NOT_FOUND,"Playlist not found".into()))}
async fn update_library_playlist(State(state):State<AppState>,Path(id):Path<String>,Json(input):Json<library::PlaylistInput>)->Result<Json<library::Playlist>,(StatusCode,Json<ApiError>)>{state.library.update_playlist(&id,input).map_err(|e|api_error(StatusCode::BAD_REQUEST,e.to_string()))?.map(Json).ok_or_else(||api_error(StatusCode::NOT_FOUND,"Playlist not found".into()))}
async fn delete_library_playlist(State(state):State<AppState>,Path(id):Path<String>)->Result<StatusCode,(StatusCode,Json<ApiError>)>{if state.library.delete_playlist(&id).map_err(|e|api_error(StatusCode::INTERNAL_SERVER_ERROR,e.to_string()))?{Ok(StatusCode::NO_CONTENT)}else{Err(api_error(StatusCode::NOT_FOUND,"Playlist not found".into()))}}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let engine_ready = state.music_server.health().await;
    // Which program is actually answering here, and whether it can start an
    // engine at all. A second copy of the studio - a development build, say -
    // takes this port and the window then waits forever on an engine that
    // copy has no bundle for. Saying whose service this is turns a hang into
    // a sentence.
    let executable = std::env::current_exe().ok().map(|path| path.display().to_string());
    let engine_available = engine_location(*state.engine_options.read().await, Vec::new()).bundle_root.is_dir();
    Json(serde_json::json!({
        "status": "ok",
        "runtime": "native",
        "service_executable": executable,
        "engine_bundle_present": engine_available,
        "music_engine": {
            "id": PRIMARY_MUSIC_ENGINE_ID,
            "base_url": state.music_server.base_url,
            "reachable": engine_ready,
        }
    }))
}

async fn configuration(State(state): State<AppState>) -> Json<StudioConfiguration> {
    Json(state.configuration.read().await.clone())
}

async fn update_configuration(
    State(state): State<AppState>,
    Json(update): Json<StudioConfiguration>,
) -> Json<StudioConfiguration> {
    // A page that changes one capability sends one selection. Storing the
    // request verbatim then erased every other choice - which is how a studio
    // with a downloaded engine started answering "the local music engine is not
    // configured" after the assistant was pointed at a local model.
    let configuration = {
        let mut stored = state.configuration.write().await;
        for selection in update.selections {
            match stored.selections.iter_mut().find(|existing| existing.capability == selection.capability) {
                Some(existing) => *existing = selection,
                None => stored.selections.push(selection),
            }
        }
        stored.clone()
    };

    // The choice has to reach the code that does the work, or the button is
    // decoration. Speech-to-text is done by the karaoke stack, and the writing
    // assistant has its own provider; both follow this page now.
    for selection in &configuration.selections {
        match selection.capability {
            Capability::SpeechToText => {
                let mut sync = state.lyrics_sync_config.write().await;
                sync.provider = match selection.mode {
                    ExecutionMode::OpenRouter => lyrics_sync::AsrProvider::OpenRouter,
                    ExecutionMode::Local => match selection.local_engine.as_deref() {
                        Some("whisper") => lyrics_sync::AsrProvider::Whisper,
                        Some("parakeet") => lyrics_sync::AsrProvider::Parakeet,
                        _ if state.lyrics_sync.parakeet_ready() => lyrics_sync::AsrProvider::Parakeet,
                        _ if state.lyrics_sync.whisper_binary().is_some() => lyrics_sync::AsrProvider::Whisper,
                        _ => sync.provider,
                    },
                };
                if selection.mode == ExecutionMode::OpenRouter {
                    sync.openrouter_model = selection.cloud_model.clone();
                }
            }
            Capability::PromptEnhancement => {
                let mut assistant = state.assistant.write().await;
                match selection.mode {
                    ExecutionMode::OpenRouter => {
                        assistant.provider = AssistantProvider::OpenRouter;
                        if let Some(model) = selection.cloud_model.clone() {
                            assistant.openrouter_model = Some(model);
                        }
                    }
                    ExecutionMode::Local => {
                        // Whichever local shape is set up: a managed model the
                        // studio downloaded, or a server the user runs.
                        assistant.provider = if assistant.managed_model.is_some() || assistant.managed_path.is_some() {
                            AssistantProvider::Managed
                        } else {
                            AssistantProvider::Local
                        };
                    }
                }
            }
            _ => {}
        }
    }

    let _ = persist_studio_settings(&state).await;
    Json(configuration)
}

/// Starts the local `minimaxmusic.cpp` runtime.
///
/// The service owns this, not the desktop shell: the studio has to work when it
/// is opened in a browser during development, and a UI that could only start
/// the engine through a Tauri command failed with "cannot read properties of
/// undefined" outside the packaged app. If a healthy engine is already
/// listening the call is a no-op and never spawns a second process.
async fn start_local_engine(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    if state.music_server.health().await {
        return Ok(Json(serde_json::json!({ "engine_id": PRIMARY_MUSIC_ENGINE_ID, "started": false, "reachable": true })));
    }

    // The engine binary imports CUDA libraries that are downloaded rather than
    // shipped. Without them Windows refuses to start the process at all, so
    // this is not an optional extra to offer later: selecting the local engine
    // is the instruction to fetch them.
    if let Some(preparing) = prepare_engine_runtime(&state).await {
        return Ok(Json(preparing));
    }

    let mut supervisor = state.engine.lock().await;
    if supervisor.is_none() {
        let location = engine_location(*state.engine_options.read().await, Vec::new());
        let config = location
            .resolve()
            .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, format!("The local engine runtime was not found: {error}")))?;
        *supervisor = Some(
            music_engine::mm_server::MmServerSupervisor::new(config)
                .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?,
        );
    }
    let engine = supervisor.as_mut().expect("supervisor was created above");
    let started = tokio::task::block_in_place(|| engine.ensure_started(std::time::Duration::from_secs(60)))
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, format!("The local engine did not start: {error}")))?;
    Ok(Json(serde_json::json!({
        "engine_id": PRIMARY_MUSIC_ENGINE_ID,
        "started": matches!(started, music_engine::mm_server::StartOutcome::Started),
        "reachable": true,
    })))
}

async fn engine_options(State(state): State<AppState>) -> Json<Value> {
    let options = *state.engine_options.read().await;
    Json(serde_json::json!({
        "options": options,
        "effective_max_batch": options.effective_max_batch(),
        "restart_required_to_apply": true,
    }))
}

/// Stores the launch flags and restarts the engine if it is running, because
/// upstream reads them once at startup.
async fn update_engine_options(
    State(state): State<AppState>,
    Json(request): Json<EngineOptions>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    if request.max_batch.is_some_and(|value| value == 0 || value > 8) {
        return Err(api_error(StatusCode::BAD_REQUEST, "max_batch must be between 1 and 8".into()));
    }
    if request.max_seq.is_some_and(|value| value < 512) {
        return Err(api_error(StatusCode::BAD_REQUEST, "max_seq must be at least 512".into()));
    }
    let changed = {
        let mut options = state.engine_options.write().await;
        let changed = *options != request;
        *options = request;
        changed
    };
    let _ = persist_studio_settings(&state).await;

    let mut restarted = false;
    if changed && state.music_server.health().await {
        restart_engine(&state).await.map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error))?;
        restarted = true;
    }
    Ok(Json(serde_json::json!({
        "options": request,
        "effective_max_batch": request.effective_max_batch(),
        "engine_restarted": restarted,
    })))
}

async fn restart_local_engine(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    restart_engine(&state).await.map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error))?;
    Ok(Json(serde_json::json!({ "engine_id": PRIMARY_MUSIC_ENGINE_ID, "restarted": true })))
}

async fn restart_engine(state: &AppState) -> Result<(), String> {
    let mut supervisor = state.engine.lock().await;
    let owned = supervisor.is_some();
    if let Some(engine) = supervisor.as_mut() {
        tokio::task::block_in_place(|| engine.stop(std::time::Duration::from_secs(10)))
            .map_err(|error| format!("stopping the local engine failed: {error}"))?;
    }
    // Launch flags are read once at engine startup. If something this service
    // does not own is still listening, starting again would silently reuse it
    // and the new flags would never take effect — report that instead of
    // claiming a restart that did not happen.
    if !owned && state.music_server.health().await {
        return Err(
            "An engine that this application did not start is already running on the engine port.              Close it and try again, otherwise the new options cannot be applied."
                .into(),
        );
    }
    // Nothing can start until the libraries the engine binary imports are on
    // disk: Windows resolves them before the process runs, so a missing cuBLAS
    // is not a slow start, it is no start at all. This is the path the studio
    // actually takes on launch, so the fetch belongs here rather than only in
    // the endpoint nothing calls.
    if !state.engine_runtime.is_ready() {
        state
            .engine_runtime
            .install_missing()
            .await
            .map_err(|error| format!("the engine's CUDA libraries could not be downloaded: {error}"))?;
    }
    // The engine loads eleven gigabytes of weights the moment it starts. If
    // the writing assistant is still holding the card, it does not finish.
    free_the_card_for_the_engine(state).await;
    let options = *state.engine_options.read().await;
    let config = engine_location(options, Vec::new())
        .resolve()
        .map_err(|error| format!("the local engine runtime was not found: {error}"))?;
    let mut engine = music_engine::mm_server::MmServerSupervisor::new(config).map_err(|error| error.to_string())?;
    tokio::task::block_in_place(|| engine.ensure_started(std::time::Duration::from_secs(60)))
        .map_err(|error| format!("the local engine did not start: {error}"))?;
    *supervisor = Some(engine);
    Ok(())
}

/// Where the packaged or developer-built `mm-server` lives. Every value is an
/// explicit override or a documented default; nothing is downloaded here.
/// Makes sure the libraries the engine is linked against are on disk.
///
/// Returns `Some(status)` while they are still being fetched, so the caller
/// answers with a real percentage instead of starting a process Windows will
/// refuse to load. The download runs in the background and survives the
/// request: the window asks again, and each answer carries the progress.
async fn prepare_engine_runtime(state: &AppState) -> Option<Value> {
    if state.engine_runtime.is_ready() {
        return None;
    }
    let active = state.engine_runtime.downloader().active().await;
    if active.is_none() {
        let runtime = state.engine_runtime.clone();
        tokio::spawn(async move {
            if let Err(error) = runtime.install_missing().await {
                eprintln!("could not fetch the engine's CUDA libraries: {error}");
            }
        });
    }
    let total = state.engine_runtime.missing_bytes().max(1);
    let downloaded = active.as_ref().map(|progress| progress.downloaded_bytes).unwrap_or(0);
    Some(serde_json::json!({
        "engine_id": PRIMARY_MUSIC_ENGINE_ID,
        "started": false,
        "reachable": false,
        "preparing": "engine.runtime",
        "downloaded_bytes": downloaded,
        "total_bytes": total,
        "percent": (downloaded.min(total) * 100 / total) as u32,
        "error": active.and_then(|progress| progress.error),
    }))
}

/// The directory holding `mm-server.exe` and everything it loads.
fn engine_bundle_root() -> PathBuf {
    env::var_os("MINIMAX_MM_SERVER_ROOT")
        .map(PathBuf::from)
        .or_else(|| env::var_os("MINIMAX_MM_SERVER_BIN").map(PathBuf::from).and_then(|path| path.parent().map(std::path::Path::to_path_buf)))
        .or_else(|| std::env::current_exe().ok().and_then(|path| path.parent().map(|parent| parent.join("resources").join("minimaxmusic-cpp"))))
        .unwrap_or_else(|| PathBuf::from("resources/minimaxmusic-cpp"))
}

fn engine_location(options: EngineOptions, library_dirs: Vec<PathBuf>) -> music_engine::mm_server::MmServerLocation {
    let configured_executable = env::var_os("MINIMAX_MM_SERVER_BIN").map(PathBuf::from);
    let bundle_root = engine_bundle_root();
    music_engine::mm_server::MmServerLocation {
        bundle_root,
        configured_executable,
        // The GGUFs live where the model manager put them, which is not
        // inside the engine bundle: pointing the service at a developer build
        // of mm-server used to leave it looking for models next to the binary
        // and failing with "models root is not a directory".
        configured_models_root: env::var_os("MINIMAX_MUSIC_MODELS_ROOT")
            .map(PathBuf::from)
            .or_else(|| {
                let managed = studio_data_root()?.join("models").join(model_manager::ENGINE_ID);
                managed.is_dir().then_some(managed)
            }),
        host: env::var("MINIMAX_MM_SERVER_HOST").ok(),
        port: env::var("MINIMAX_MM_SERVER_PORT").ok().and_then(|value| value.parse().ok()),
        options: options.to_engine(),
        library_dirs,
    }
}

async fn engine_presets(State(state): State<AppState>) -> Json<Value> {
    let catalog = state.openrouter_catalog.read().await;
    let music_available = catalog.catalog.as_ref().is_some_and(|catalog| catalog.models_for(Capability::MusicGeneration).next().is_some());
    Json(serde_json::json!({
        "presets": presets::list(),
        "hardware": presets::hardware(),
        "selected_profile_id": state.selected_profile_id.read().await.clone(),
        "openrouter_music": { "available": music_available, "refreshed_at": catalog.refreshed_at, "requires_live_catalog_resolution": !music_available }
    }))
}

async fn apply_engine_preset(
    State(state): State<AppState>,
    Json(request): Json<ApplyPresetRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let mut configuration = state.configuration.write().await;
    let music_available = state.openrouter_catalog.read().await.catalog.as_ref().is_some_and(|catalog| catalog.models_for(Capability::MusicGeneration).next().is_some());
    let application = presets::apply(&request.id, &mut configuration, music_available)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    if let Some(profile_id) = &application.profile_id {
        if !state.model_manager.catalog().profiles.iter().any(|profile| profile.id == profile_id && profile.installable) {
            return Err(api_error(StatusCode::BAD_REQUEST, format!("preset profile '{profile_id}' is not installable")));
        }
    }
    let applied_configuration = configuration.clone();
    drop(configuration);
    if application.selected_profile_changed {
        *state.selected_profile_id.write().await = application.profile_id;
        *state.selected_component_ids.write().await = None;
    }
    let selected_profile_id = state.selected_profile_id.read().await.clone();
    let selected_component_ids = state.selected_component_ids.read().await.clone();
    persist_studio_settings(&state)
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "id": request.id,
        "profile_id": selected_profile_id,
        "component_ids": selected_component_ids,
        "configuration": applied_configuration,
        "openrouter_music_requires_live_catalog_resolution": request.id == "full-openrouter" && !music_available,
    })))
}

async fn openrouter_catalog(State(state): State<AppState>) -> Json<Value> {
    // Fetch it if this process has not yet: an empty answer here made every
    // capability read "no model in the refreshed catalog", which is a lie -
    // the catalog had simply never been read.
    let catalog = catalog_for(&state).await.ok();
    let refreshed_at = state.openrouter_catalog.read().await.refreshed_at.clone();
    // What the studio would pick for each capability if the user picks
    // nothing. The panel shows these as the selection, so adding a key is
    // enough to start rather than the beginning of a shopping trip.
    let suggested = catalog.as_ref().map(|catalog| {
        serde_json::json!({
            "speech_to_text": providers::openrouter::suggested_model(catalog, Capability::SpeechToText),
            "prompt_enhancement": providers::openrouter::suggested_model(catalog, Capability::PromptEnhancement),
            "cover_art": providers::openrouter::suggested_model(catalog, Capability::CoverArt),
            "music_generation": providers::openrouter::suggested_model(catalog, Capability::MusicGeneration),
        })
    });
    Json(serde_json::json!({
        "models": catalog.map(|catalog| catalog.models),
        "refreshed_at": refreshed_at,
        "suggested": suggested,
    }))
}



/// Where the provider catalog is kept between runs.
fn openrouter_catalog_path() -> Option<PathBuf> {
    studio_data_root().map(|root| root.join("openrouter-catalog.json"))
}

/// Reads the catalog saved by the last refresh. Nothing here touches the
/// network: the studio refreshes when a key is connected and when the user
/// asks, and lives off this file the rest of the time.
fn load_cached_catalog() -> Option<providers::openrouter::CapabilityCatalog> {
    let path = openrouter_catalog_path()?;
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

fn save_cached_catalog(catalog: &providers::openrouter::CapabilityCatalog) {
    let Some(path) = openrouter_catalog_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string(catalog) {
        let _ = std::fs::write(path, body);
    }
}

/// The capability catalog, fetched if this process has not got it yet.
///
/// The catalog lives in memory, so it is empty after every restart. Telling
/// the user to "refresh the catalog" at the moment they press a button is
/// asking them to do the program's job - and the refresh button lives on
/// another screen entirely.
async fn catalog_for(state: &AppState) -> Result<providers::openrouter::CapabilityCatalog, String> {
    if let Some(catalog) = state.openrouter_catalog.read().await.catalog.clone() {
        return Ok(catalog);
    }
    // The last refresh, read from disk. A restart should not cost a request.
    if let Some(catalog) = load_cached_catalog() {
        let mut cached = state.openrouter_catalog.write().await;
        cached.catalog = Some(catalog.clone());
        return Ok(catalog);
    }
    let client = reqwest::Client::new();
    let fetch = |path: &'static str| {
        let client = client.clone();
        async move {
            client
                .get(format!("{}{}", providers::openrouter::API_BASE_URL, path))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await
        }
    };
    let general = fetch(providers::openrouter::MODELS_PATH)
        .await
        .map_err(|error| format!("OpenRouter catalog request failed: {error}"))?;
    let transcription = fetch(providers::openrouter::TRANSCRIPTION_MODELS_PATH).await.unwrap_or_default();
    let images = fetch(providers::openrouter::IMAGE_MODELS_PATH).await.unwrap_or_default();
    let parsed = providers::openrouter::CapabilityCatalog::parse_merged(&general, &[&transcription, &images])
    .map_err(|error| format!("OpenRouter catalog parse failed: {error}"))?;
    save_cached_catalog(&parsed);
    let mut cached = state.openrouter_catalog.write().await;
    cached.catalog = Some(parsed.clone());
    cached.refreshed_at = Some(chrono_like_timestamp());
    Ok(parsed)
}

async fn refresh_openrouter_catalog(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let client = reqwest::Client::new();
    let fetch = |path: &'static str| {
        let client = client.clone();
        async move {
            client
                .get(format!("{}{}", providers::openrouter::API_BASE_URL, path))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await
        }
    };
    let general = fetch(providers::openrouter::MODELS_PATH)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter catalog request failed: {error}")))?;
    // The recognisers live behind their own filter; without this second call
    // the catalog contains no model that can return timings.
    let transcription = fetch(providers::openrouter::TRANSCRIPTION_MODELS_PATH).await.unwrap_or_default();
    let images = fetch(providers::openrouter::IMAGE_MODELS_PATH).await.unwrap_or_default();
    let parsed = providers::openrouter::CapabilityCatalog::parse_merged(&general, &[&transcription, &images])
    .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter catalog parse failed: {error}")))?;
    let refreshed_at = chrono_like_timestamp();
    let models = parsed.models.clone();
    save_cached_catalog(&parsed);
    let mut cached = state.openrouter_catalog.write().await;
    cached.catalog = Some(parsed);
    cached.refreshed_at = Some(refreshed_at.clone());
    Ok(Json(serde_json::json!({ "models": models, "refreshed_at": refreshed_at })))
}

/// Sends an already catalog-validated OpenRouter JSON request. The frontend
/// supplies a model selection and input data, never an API key or endpoint.
async fn execute_openrouter_json(
    request: providers::openrouter::OpenRouterRequest,
) -> anyhow::Result<OpenRouterResponse> {
    let authenticated = providers::openrouter::authenticated_request_for(request)?;
    let response = reqwest::Client::new()
        .post(format!("{}{}", providers::openrouter::API_BASE_URL, authenticated.request.path))
        .bearer_auth(authenticated.api_key)
        .json(&authenticated.request.body)
        .send()
        .await?
        .error_for_status()?;
    let generation_id = response
        .headers()
        .get("X-Generation-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.json::<Value>().await?;
    Ok(OpenRouterResponse { body, generation_id })
}

async fn create_openrouter_transcription(
    State(state): State<AppState>,
    Json(input): Json<OpenRouterTranscriptionRequest>,
) -> Result<Json<OpenRouterResponse>, (StatusCode, Json<ApiError>)> {
    let catalog = catalog_for(&state)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
    let request = providers::openrouter::stt_request_for(
        &catalog,
        &input.model_id,
        providers::openrouter::Base64AudioInput {
            timestamps: false,
            data: &input.audio_base64,
            format: &input.audio_format,
            language: input.language.as_deref(),
        },
    )
    .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    execute_openrouter_json(request)
        .await
        .map(Json)
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter transcription failed: {error}")))
}

async fn create_openrouter_cover(
    State(state): State<AppState>,
    Json(input): Json<OpenRouterCoverRequest>,
) -> Result<Json<OpenRouterResponse>, (StatusCode, Json<ApiError>)> {
    let catalog = catalog_for(&state)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
    let request = providers::openrouter::request_for(
        &catalog,
        Capability::CoverArt,
        &input.model_id,
        &input.prompt,
    )
    .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    execute_openrouter_json(request)
        .await
        .map(Json)
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter cover generation failed: {error}")))
}

/// Text assistance (caption and lyric drafting). The model must declare the
/// prompt-enhancement capability in the refreshed catalog, so the studio can
/// never send this to an image or audio-only endpoint.
async fn create_openrouter_completion(
    State(state): State<AppState>,
    Json(input): Json<OpenRouterCompletionRequest>,
) -> Result<Json<OpenRouterResponse>, (StatusCode, Json<ApiError>)> {
    let catalog = catalog_for(&state)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
    let request = providers::openrouter::request_for(
        &catalog,
        Capability::PromptEnhancement,
        &input.model_id,
        &input.prompt,
    )
    .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    execute_openrouter_json(request)
        .await
        .map(Json)
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter completion failed: {error}")))
}

/// The loopback port the studio serves on. Overridable for development, so a
/// second instance can run beside a released one.
fn listen_port() -> u16 {
    env::var("MINIMAX_STUDIO_PORT").ok().and_then(|value| value.parse().ok()).unwrap_or(8765)
}

fn chrono_like_timestamp() -> String { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|value| value.as_secs().to_string()).unwrap_or_default() }

fn studio_settings_path() -> PathBuf {
    env::var_os("MINIMAX_STUDIO_SETTINGS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(default_studio_settings_path)
}

fn default_studio_settings_path() -> PathBuf {
    studio_data_root()
        .unwrap_or_else(|| env::temp_dir().join("minimax-music3-studio"))
        .join("studio-settings.json")
}

/// Single per-user directory for every piece of Studio runtime data: settings,
/// library, media and locally stored provider credentials.
pub fn studio_data_root() -> Option<PathBuf> {
    if let Some(root) = env::var_os("MINIMAX_STUDIO_DATA_ROOT") {
        return Some(PathBuf::from(root));
    }

    #[cfg(windows)]
    {
        if let Some(root) = env::var_os("LOCALAPPDATA").or_else(|| env::var_os("APPDATA")) {
            return Some(PathBuf::from(root).join("MiniMax Music3 Studio"));
        }
    }

    #[cfg(not(windows))]
    {
        if let Some(root) = env::var_os("XDG_DATA_HOME") {
            return Some(PathBuf::from(root).join("minimax-music3-studio"));
        }
        if let Some(home) = env::var_os("HOME") {
            return Some(PathBuf::from(home).join(".local/share/minimax-music3-studio"));
        }
    }

    None
}

fn load_studio_settings(path: &PathBuf) -> Option<PersistedStudioSettings> {
    fs::read_to_string(path).ok().and_then(|body| serde_json::from_str(&body).ok())
}

async fn persist_studio_settings(state: &AppState) -> anyhow::Result<()> {
    let settings = PersistedStudioSettings {
        engine_options: *state.engine_options.read().await,
        assistant: state.assistant.read().await.clone(),
        lyrics_sync: state.lyrics_sync_config.read().await.clone(),
        configuration: state.configuration.read().await.clone(),
        selected_profile_id: state.selected_profile_id.read().await.clone(),
        selected_component_ids: state.selected_component_ids.read().await.clone(),
        cover_templates: Some(state.cover_templates.read().await.clone()),
        cover_template_default: state.cover_template_default.read().await.clone(),
        separation: Some(state.separation_config.read().await.clone()),
        cover_auto: Some(*state.cover_auto.read().await),
    };
    if let Some(parent) = state.settings_path.parent() { fs::create_dir_all(parent)?; }
    let temporary = state.settings_path.with_extension("json.part");
    fs::write(&temporary, serde_json::to_vec_pretty(&settings)?)?;
    fs::rename(temporary, &state.settings_path)?;
    Ok(())
}

/// The set Studio will actually load. `None` means nothing has been selected
/// yet, so the manager falls back to the hardware recommendation for progress
/// reporting only — it still never downloads anything on its own.
async fn effective_install_target(state: &AppState) -> Option<InstallRequest> {
    if let Some(component_ids) = state.selected_component_ids.read().await.clone() {
        return Some(InstallRequest { profile_id: None, component_ids });
    }
    state
        .selected_profile_id
        .read()
        .await
        .clone()
        .map(|profile_id| InstallRequest { profile_id: Some(profile_id), component_ids: vec![] })
}

async fn compose_setup_status(state: &AppState, manager_status: model_manager::ManagerStatus) -> Value {
    let mut status = serde_json::to_value(manager_status).unwrap_or_else(|_| serde_json::json!({}));
    let selected_profile_id = state.selected_profile_id.read().await.clone();
    let selected_component_ids = state.selected_component_ids.read().await.clone();
    let selected_set_ready = match (&selected_profile_id, &selected_component_ids) {
        (_, Some(component_ids)) => state.model_manager.installed_component_files(component_ids).is_ok(),
        (Some(profile_id), None) => state.model_manager.installed_profile_files(profile_id).is_ok(),
        (None, None) => false,
    };
    if let Value::Object(ref mut fields) = status {
        fields.insert(
            "engine_ready".into(),
            Value::Bool(state.music_server.health().await),
        );
        fields.insert("engine_id".into(), Value::String(PRIMARY_MUSIC_ENGINE_ID.into()));
        fields.insert("selected_profile_id".into(), serde_json::to_value(selected_profile_id).unwrap_or(Value::Null));
        fields.insert("selected_component_ids".into(), serde_json::to_value(selected_component_ids).unwrap_or(Value::Null));
        fields.insert("hardware".into(), serde_json::to_value(presets::hardware()).unwrap_or(Value::Null));
        fields.insert("engine_options".into(), serde_json::to_value(*state.engine_options.read().await).unwrap_or(Value::Null));
        fields.insert("effective_max_batch".into(), Value::from(state.engine_options.read().await.effective_max_batch()));
        // Where everything the studio owns actually lives. People complained
        // they could not find the ten gigabytes afterwards, let alone delete
        // them; the model root is already reported, this is the folder that
        // holds it along with the library, the media and the logs.
        fields.insert(
            "data_directory".into(),
            studio_data_root().map(|root| Value::String(root.display().to_string())).unwrap_or(Value::Null),
        );
        fields.insert("portable".into(), Value::Bool(is_portable_installation()));
        // Half a gigabyte of CUDA libraries arriving is the difference between
        // an engine that starts in three seconds and one that starts in ten
        // minutes. A spinner that says nothing for ten minutes is the same
        // screen as a spinner that is stuck.
        let runtime_total = engine_runtime::ASSETS.iter().map(|asset| asset.bytes).sum::<u64>();
        let runtime_active = state.engine_runtime.downloader().active().await;
        fields.insert(
            "engine_runtime".into(),
            serde_json::json!({
                "ready": state.engine_runtime.is_ready(),
                "downloading": runtime_active.is_some(),
                "downloaded_bytes": runtime_active.as_ref().map(|progress| progress.downloaded_bytes).unwrap_or(0),
                "total_bytes": runtime_total,
                "error": runtime_active.and_then(|progress| progress.error),
            }),
        );
        fields.insert("ready".into(), Value::Bool(selected_set_ready));
        fields.insert("first_run".into(), Value::Bool(!selected_set_ready));
        if selected_set_ready { fields.insert("download_pending".into(), Value::from(0_u64)); }
    }
    status
}

/// Whether this copy keeps everything beside its own executable.
///
/// The desktop shell decides it by the marker file next to the binary and then
/// hands the service the data root; the service reports it so the interface can
/// say "this folder is the whole studio" rather than sending people hunting
/// through AppData.
fn is_portable_installation() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|directory| directory.join("portable.flag")))
        .is_some_and(|marker| marker.is_file())
}

/// Recent native engine output. This is the only progress detail upstream
/// exposes: `/job` reports a phase, and everything finer lives in the log ring.
async fn engine_logs(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    // While the engine is starting it has no HTTP log yet, so the file it
    // writes from its first line is the only thing that can be shown - and it
    // is exactly what the first-run screen needs.
    match state.music_server.logs_snapshot(std::time::Duration::from_millis(700)).await {
        Ok(lines) => Ok(Json(serde_json::json!({ "engine_id": PRIMARY_MUSIC_ENGINE_ID, "lines": lines }))),
        Err(error) => {
            let lines = music_engine::mm_server::startup_log_tail(60);
            if lines.is_empty() {
                return Err(api_error(StatusCode::SERVICE_UNAVAILABLE, format!("mm-server logs are unavailable: {error}")));
            }
            Ok(Json(serde_json::json!({ "engine_id": PRIMARY_MUSIC_ENGINE_ID, "lines": lines, "source": "startup" })))
        }
    }
}

/// Live machine resources. ACE Studio's resource readout is kept, but every
/// value now comes from a real measurement on this machine.
async fn system_resources() -> Json<Value> {
    let snapshot = tokio::task::spawn_blocking(resources::snapshot)
        .await
        .unwrap_or_else(|_| resources::snapshot());
    Json(serde_json::json!({
        "poll_interval_ms": resources::SUGGESTED_INTERVAL.as_millis() as u64,
        "resources": snapshot,
    }))
}

/// Fetches a remote image on behalf of the video composer.
///
/// The canvas has to stay untainted to read frames back, which a cross-origin
/// image without CORS headers prevents. Only http(s) is accepted and the
/// response must actually be an image, so this cannot be used to reach local
/// services or to pull arbitrary files.
async fn proxy_image(
    State(state): State<AppState>,
    axum::extract::Query(request): axum::extract::Query<ProxyImageRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let url = reqwest::Url::parse(&request.url)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, format!("invalid image url: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(api_error(StatusCode::BAD_REQUEST, "only http and https images can be proxied".into()));
    }
    if url.host_str().is_some_and(|host| host == "localhost" || host.starts_with("127.") || host == "0.0.0.0" || host == "[::1]") {
        return Err(api_error(StatusCode::BAD_REQUEST, "loopback addresses cannot be proxied".into()));
    }
    let response = state
        .music_server
        .http
        .get(url)
        .send()
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("image request failed: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(api_error(StatusCode::BAD_GATEWAY, format!("image request returned {status}")));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !content_type.starts_with("image/") {
        return Err(api_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "the proxied url is not an image".into()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("reading the image failed: {error}")))?;
    Ok(axum::response::Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(Body::from(bytes.to_vec()))
        .expect("valid image response"))
}

async fn assistant_status(State(state): State<AppState>) -> Json<Value> {
    let config = state.assistant.read().await.clone();
    let runtime = state.assistant_runtime.status().await;
    // A managed model is only usable once its file and the runtime are on disk.
    let available = match config.provider {
        AssistantProvider::Managed => {
            let has_runtime = runtime.server_path.is_some();
            let downloaded = config
                .managed_model
                .as_deref()
                .is_some_and(|model| runtime.installed_models.iter().any(|id| id == model));
            let own_file = config
                .managed_path
                .as_deref()
                .is_some_and(|path| !path.trim().is_empty() && std::path::Path::new(path.trim()).is_file());
            has_runtime && (downloaded || own_file)
        }
        _ => config.available(),
    };
    Json(serde_json::json!({
        "available": available,
        "managed_model": config.managed_model,
        "managed_path": config.managed_path,
        "reasoning_effort": config.reasoning_effort,
        "runtime_ready": runtime.ready,
        "provider": config.provider,
        "local_base_url": config.local_base_url,
        "local_model": config.local_model,
        "openrouter_model": config.openrouter_model,
    }))
}

async fn update_assistant_settings(
    State(state): State<AppState>,
    Json(request): Json<AssistantConfig>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    if request.provider == AssistantProvider::Local {
        let base = request.local_base_url.as_deref().unwrap_or_default();
        let url = reqwest::Url::parse(base)
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, format!("invalid assistant URL: {error}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(api_error(StatusCode::BAD_REQUEST, "the assistant URL must be http or https".into()));
        }
    }
    *state.assistant.write().await = request.clone();
    let _ = persist_studio_settings(&state).await;
    Ok(Json(serde_json::json!({ "available": request.available(), "provider": request.provider })))
}

#[derive(Debug, Deserialize)]
struct AssistantAssetRequest {
    asset_id: String,
}

#[derive(Debug, Deserialize)]
struct AssistantModelRequest {
    #[serde(default)]
    model_id: String,
    /// A GGUF already on this machine, used instead of a downloaded one.
    #[serde(default)]
    model_path: Option<String>,
}

async fn assistant_runtime_status(State(state): State<AppState>) -> Json<assistant_runtime::RuntimeStatus> {
    Json(state.assistant_runtime.status().await)
}

/// Starts one download. Nothing is fetched until this is called, and an
/// interrupted file resumes where it stopped.
async fn assistant_runtime_install(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<assistant_runtime::RuntimeStatus>, (StatusCode, Json<ApiError>)> {
    state
        .assistant_runtime
        .install(&request.asset_id)
        .await
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    Ok(Json(state.assistant_runtime.status().await))
}

/// How much room the local model gets.
///
/// The prompt alone is around 3200 tokens - the caption contract plus the three
/// reference captions the MiniMax skill selects - and a full answer is another
/// 700 to 1200. Eight thousand left almost no headroom for a long lyric, and a
/// model that runs out mid-JSON produces an answer nothing can parse.
const ASSISTANT_CONTEXT: u32 = 16384;

async fn assistant_runtime_start(
    State(state): State<AppState>,
    Json(request): Json<AssistantModelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let own_file = request.model_path.clone().unwrap_or_default();
    let reasoning = state.assistant.read().await.reasoning_effort.clone();
    let base_url = if own_file.trim().is_empty() {
        state.assistant_runtime.start(&request.model_id, ASSISTANT_CONTEXT, reasoning.as_deref()).await
    } else {
        state.assistant_runtime.start_path(std::path::Path::new(own_file.trim()), ASSISTANT_CONTEXT, reasoning.as_deref()).await
    }
    .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error.to_string()))?;
    Ok(Json(serde_json::json!({ "base_url": base_url, "model_id": request.model_id, "model_path": own_file })))
}

async fn assistant_runtime_stop(State(state): State<AppState>) -> Json<Value> {
    state.assistant_runtime.stop().await;
    Json(serde_json::json!({ "running": false }))
}

#[derive(Debug, Deserialize)]
struct KaraokeRequest {
    /// Overrides the language guess for this one track.
    #[serde(default)]
    language: Option<String>,
}

async fn karaoke_status(State(state): State<AppState>) -> Json<lyrics_sync::SyncStatus> {
    let config = state.lyrics_sync_config.read().await.clone();
    Json(state.lyrics_sync.status(&config).await)
}

async fn update_karaoke_settings(
    State(state): State<AppState>,
    Json(request): Json<lyrics_sync::LyricsSyncConfig>,
) -> Result<Json<lyrics_sync::SyncStatus>, (StatusCode, Json<ApiError>)> {
    *state.lyrics_sync_config.write().await = request.clone();
    let _ = persist_studio_settings(&state).await;
    Ok(Json(state.lyrics_sync.status(&request).await))
}

/// Frees the disk a karaoke recogniser takes.
async fn karaoke_remove(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<lyrics_sync::SyncStatus>, (StatusCode, Json<ApiError>)> {
    let asset = lyrics_sync::asset(&request.asset_id)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, format!("unknown karaoke asset: {}", request.asset_id)))?;
    state
        .lyrics_sync
        .downloader()
        .remove(asset)
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    let config = state.lyrics_sync_config.read().await.clone();
    Ok(Json(state.lyrics_sync.status(&config).await))
}

/// Frees the disk the stem separation model takes.
async fn remove_separation_model(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let freed = state
        .separator
        .downloader()
        .remove(&separation::MODEL)
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    Ok(Json(serde_json::json!({ "freed_bytes": freed })))
}

async fn karaoke_install(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<lyrics_sync::SyncStatus>, (StatusCode, Json<ApiError>)> {
    let asset = lyrics_sync::asset(&request.asset_id)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, format!("unknown karaoke asset: {}", request.asset_id)))?;
    state
        .lyrics_sync
        .downloader()
        .install(asset)
        .await
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    let config = state.lyrics_sync_config.read().await.clone();
    Ok(Json(state.lyrics_sync.status(&config).await))
}

/// Times one track's own lyrics and stores the result with it.
///
/// Recognition is CPU or GPU bound and takes tens of seconds, so it runs on a
/// blocking thread rather than holding an async worker hostage.
async fn create_song_karaoke(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<KaraokeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let config = state.lyrics_sync_config.read().await.clone();
    if !config.available() {
        return Err(api_error(StatusCode::CONFLICT, "karaoke.off".into()));
    }
    // Pressing the button on a track is the instruction to time it. If the
    // chosen local recogniser is not on disk yet, that is a download to start,
    // not a refusal to hand back.
    if !ensure_local_recogniser(&state, &config, &id).await {
        return Err(api_error(StatusCode::CONFLICT, "karaoke.model-missing".into()));
    }
    let song = state
        .library
        .get_song(&id)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "no such song".into()))?;
    let audio = song
        .audio_path
        .clone()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "this track has no audio to listen to".into()))?;
    if !auto_title::has_sung_lines(&song.lyrics) {
        return Err(api_error(StatusCode::BAD_REQUEST, "karaoke.instrumental".into()));
    }

    let words = match config.provider {
        lyrics_sync::AsrProvider::None => {
            return Err(api_error(StatusCode::CONFLICT, "karaoke.no-recogniser".into()))
        }
        lyrics_sync::AsrProvider::Parakeet => {
            let sync = state.lyrics_sync.clone();
            let path = std::path::PathBuf::from(&audio);
            tokio::task::spawn_blocking(move || sync.parakeet_words(&path))
                .await
                .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        }
        lyrics_sync::AsrProvider::Whisper => {
            let sync = state.lyrics_sync.clone();
            let config = config.clone();
            let path = std::path::PathBuf::from(&audio);
            let language = request.language.clone();
            let lyrics = song.lyrics.clone();
            tokio::task::spawn_blocking(move || sync.whisper_words(&config, &path, language.as_deref(), &lyrics))
                .await
                .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        }
        lyrics_sync::AsrProvider::OpenRouter => {
            karaoke_words_from_openrouter(&state, &config, &audio, request.language.as_deref()).await
        }
    }
    .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error.to_string()))?;

    // Word by word, because that is what karaoke means: a line time alone
    // leaves a player sweeping the highlight linearly through the line, which
    // drifts off the singing immediately.
    let lines = lyrics_sync::align_lyrics_words(&words, &song.lyrics);
    if lines.is_empty() {
        return Err(api_error(StatusCode::BAD_GATEWAY, "karaoke.no-match".into()));
    }
    let lrc = lyrics_sync::enhanced_lrc(&lines);
    state
        .library
        .set_song_lrc(&id, &lrc)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(serde_json::json!({ "lrc": lrc, "lines": lines.len(), "provider": config.provider })))
}

async fn delete_song_karaoke(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    state
        .library
        .set_song_lrc(&id, "")
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(serde_json::json!({ "lrc": Value::Null })))
}

/// The cloud path: base64 the audio, ask for verbose output, read the times.
async fn karaoke_words_from_openrouter(
    state: &AppState,
    config: &lyrics_sync::LyricsSyncConfig,
    audio: &str,
    language: Option<&str>,
) -> anyhow::Result<Vec<(f64, String)>> {
    use base64::Engine as _;
    let catalog = catalog_for(state).await.map_err(|error| anyhow::anyhow!(error))?;
    let model = config
        .openrouter_model
        .clone()
        .filter(|value| !value.trim().is_empty())
        // Only the Whisper family returns timings, and that is what karaoke is.
        .or_else(|| providers::openrouter::suggested_model(&catalog, Capability::SpeechToText))
        .unwrap_or_default();
    let bytes = std::fs::read(audio)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let format = std::path::Path::new(audio)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp3")
        .to_ascii_lowercase();
    let request = providers::openrouter::stt_request_for(
        &catalog,
        &model,
        providers::openrouter::Base64AudioInput { timestamps: true, data: &encoded, format: &format, language },
    )?;
    let response = execute_openrouter_json(request).await?;
    let segments = lyrics_sync::segments_from_verbose_json(&response.body);
    if segments.is_empty() {
        anyhow::bail!("{model} answered without timings; pick a model that returns them");
    }
    Ok(segments)
}

/// Writes lyrics and/or the structured caption. Optional by design: with no
/// provider configured this answers 409 and the manual form is unaffected.

/// The same request as `assistant_write`, reported while it happens.
///
/// A model can take a minute, and a button that only spins says nothing about
/// whether the request even left the machine. This sends the stages as they
/// occur - the request going out, the first token coming back - and then the
/// text itself, piece by piece, so the fields fill in front of the user.

/// The OpenRouter model the writing assistant should use.
///
/// Two screens name this: the provider page, where every capability picks its
/// model, and the assistant page, which has a field of its own. They disagreed,
/// and the request went to whichever the code happened to read - so the panel
/// showed one model while another answered. The provider selection wins,
/// because that page is where every other capability is chosen.
async fn assistant_openrouter_model(state: &AppState, config: &AssistantConfig) -> String {
    let selected = state
        .configuration
        .read()
        .await
        .selections
        .iter()
        .find(|selection| selection.capability == Capability::PromptEnhancement)
        .and_then(|selection| selection.cloud_model.clone())
        .filter(|model| !model.trim().is_empty());
    if let Some(model) = selected {
        return model;
    }
    if let Some(model) = config.openrouter_model.clone().filter(|model| !model.trim().is_empty()) {
        return model;
    }
    catalog_for(state)
        .await
        .ok()
        .and_then(|catalog| providers::openrouter::suggested_model(&catalog, Capability::PromptEnhancement))
        .unwrap_or_default()
}

async fn assistant_write_stream(
    State(state): State<AppState>,
    Json(request): Json<assistant::AssistRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let config = state.assistant.read().await.clone();
    if !config.available() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "No writing assistant is configured. The manual form does not need one.".into(),
        ));
    }
    let (system, required) = assistant::instructions(&request);
    let user = assistant::user_message(&request);

    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(64);
    let emit = |sender: tokio::sync::mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>, event: Value| async move {
        let line = format!("data: {event}\n\n");
        let _ = sender.send(Ok(axum::body::Bytes::from(line))).await;
    };

    tokio::spawn(async move {
        emit(sender.clone(), serde_json::json!({ "stage": "preparing" })).await;

        // Where the request goes, and with which model.
        let (base, model, key): (String, String, Option<String>) = match config.provider {
            AssistantProvider::OpenRouter => {
                let model = assistant_openrouter_model(&state, &config).await;
                let key = match credentials::openrouter_api_key().map(|(key, _)| key) {
                    Some(key) => key,
                    None => {
                        emit(sender.clone(), serde_json::json!({ "error": "no OpenRouter key is stored" })).await;
                        return;
                    }
                };
                ("https://openrouter.ai/api/v1".to_string(), model, Some(key))
            }
            AssistantProvider::Managed => {
                let own_file = config.managed_path.clone().unwrap_or_default();
                let id = config.managed_model.clone().unwrap_or_default();
                let reasoning = config.reasoning_effort.clone();
                let started = if own_file.trim().is_empty() {
                    state.assistant_runtime.start(&id, ASSISTANT_CONTEXT, reasoning.as_deref()).await
                } else {
                    state.assistant_runtime.start_path(std::path::Path::new(own_file.trim()), ASSISTANT_CONTEXT, reasoning.as_deref()).await
                };
                match started {
                    Ok(base) => (base, if own_file.trim().is_empty() { id } else { "local-model".to_string() }, None),
                    Err(error) => {
                        emit(sender.clone(), serde_json::json!({ "error": format!("the local assistant did not start: {error}") })).await;
                        return;
                    }
                }
            }
            _ => (
                config.local_base_url.clone().unwrap_or_default(),
                config.local_model.clone().unwrap_or_default(),
                None,
            ),
        };

        // A local llama-server enforces the shape while it samples, so the
        // answer cannot come back as prose or as a list where a string belongs.
        let schema = matches!(config.provider, AssistantProvider::Managed | AssistantProvider::Local)
            .then(|| assistant::draft_schema(&required));
        let mut body = assistant::chat_body_constrained(&model, &system, &user, config.reasoning_effort.as_deref(), None, schema);
        body["stream"] = Value::Bool(true);

        emit(sender.clone(), serde_json::json!({ "stage": "sent", "model": model })).await;

        let client = reqwest::Client::new();
        let mut outgoing = client
            .post(format!("{}/chat/completions", base.trim_end_matches('/')))
            .json(&body)
            .timeout(std::time::Duration::from_secs(600));
        if let Some(key) = key {
            outgoing = outgoing
                .header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"))
                .header("HTTP-Referer", "https://github.com/timoncool/MiniMax-Music3-Studio")
                .header("X-Title", "MiniMax Music3 Studio");
        }

        let response = match outgoing.send().await {
            Ok(response) => response,
            Err(error) => {
                emit(sender.clone(), serde_json::json!({ "error": format!("the assistant is unreachable: {error}") })).await;
                return;
            }
        };
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            emit(sender.clone(), serde_json::json!({ "error": format!("the assistant returned {status}: {text}") })).await;
            return;
        }

        // Server-sent events, one JSON object per `data:` line, with the text in
        // `choices[0].delta.content`.
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut first = true;
        let mut whole = String::new();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else { break };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer.drain(..line_end + 1);
                let Some(payload) = line.strip_prefix("data:") else { continue };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    continue;
                }
                let Ok(event): Result<Value, _> = serde_json::from_str(payload) else { continue };
                let delta = event
                    .get("choices")
                    .and_then(|choices| choices.get(0))
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|delta| delta.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if delta.is_empty() {
                    continue;
                }
                if first {
                    first = false;
                    emit(sender.clone(), serde_json::json!({ "stage": "writing" })).await;
                }
                whole.push_str(delta);
                emit(sender.clone(), serde_json::json!({ "delta": delta })).await;
            }
        }

        emit(sender.clone(), serde_json::json!({ "stage": "done", "text": whole })).await;
        // The card belongs to whatever runs next unless the user asked for
        // everything to stay resident.
        release_assistant_unless_kept(&state).await;
    });

    // A channel of chunks becomes the response body; the receiver is turned into
    // a stream by hand to avoid another dependency for four lines.
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    });
    let body = Body::from_stream(stream);
    Ok(axum::response::Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .expect("valid stream response"))
}

async fn assistant_write(
    State(state): State<AppState>,
    Json(request): Json<assistant::AssistRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let config = state.assistant.read().await.clone();
    if !config.available() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "No writing assistant is configured. The manual form does not need one.".into(),
        ));
    }
    let (system, required) = assistant::instructions(&request);
    let user = assistant::user_message(&request);

    let response: Value = match config.provider {
        AssistantProvider::None | AssistantProvider::Local | AssistantProvider::Managed => {
            // A managed model is started on first use and then stays loaded, so
            // the second request does not pay for the load again.
            let (base, model) = match config.provider {
                AssistantProvider::Managed => {
                    let own_file = config.managed_path.clone().unwrap_or_default();
                    let id = config.managed_model.clone().unwrap_or_default();
                    let reasoning = config.reasoning_effort.as_deref();
                    let base = if own_file.trim().is_empty() {
                        state.assistant_runtime.start(&id, 8192, reasoning).await
                    } else {
                        state.assistant_runtime.start_path(std::path::Path::new(own_file.trim()), 8192, reasoning).await
                    }
                    .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error.to_string()))?;
                    (base, if own_file.trim().is_empty() { id } else { own_file })
                }
                _ => (
                    config.local_base_url.clone().unwrap_or_default(),
                    config.local_model.clone().unwrap_or_default(),
                ),
            };
            let sent = reqwest::Client::new()
                .post(format!("{}/chat/completions", base.trim_end_matches('/')))
                .json(&assistant::chat_body_constrained(
                    &model,
                    &system,
                    &user,
                    None,
                    None,
                    matches!(config.provider, AssistantProvider::Managed | AssistantProvider::Local)
                        .then(|| assistant::draft_schema(&required)),
                ))
                .timeout(std::time::Duration::from_secs(180))
                .send()
                .await
                .map_err(|error| {
                    // A sidecar that died mid-request leaves nothing but a
                    // refused connection unless its own log is quoted back.
                    let tail = state.assistant_runtime.log_tail();
                    let detail = if tail.is_empty() { String::new() } else { format!("
{tail}") };
                    api_error(StatusCode::BAD_GATEWAY, format!("the local assistant is unreachable: {error}{detail}"))
                })?;
            let status = sent.status();
            let body = sent.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(api_error(StatusCode::BAD_GATEWAY, format!("the local assistant returned {status}: {body}")));
            }
            serde_json::from_str(&body)
                .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("invalid assistant response: {error}")))?
        }
        AssistantProvider::OpenRouter => {
            let catalog_now = catalog_for(&state).await.ok();
            let model = assistant_openrouter_model(&state, &config).await;
            let catalog = catalog_for(&state)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error))?;
            catalog
                .selected(Capability::PromptEnhancement, &model)
                .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
            let authenticated = providers::openrouter::authenticated_request_for(providers::openrouter::OpenRouterRequest {
                method: providers::openrouter::HttpMethod::Post,
                path: providers::openrouter::CHAT_COMPLETIONS_PATH,
                // Whatever this model publishes for itself; the studio's own
                // temperature is only for models that publish nothing.
                body: assistant::chat_body_full(
                    &model,
                    &system,
                    &user,
                    config.reasoning_effort.as_deref(),
                    catalog_now
                        .as_ref()
                        .and_then(|catalog| catalog.models.iter().find(|entry| entry.id == model))
                        .map(|entry| serde_json::to_value(&entry.defaults).unwrap_or(Value::Null))
                        .as_ref(),
                ),
            })
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
            execute_openrouter_json(authenticated.request)
                .await
                .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter assistant failed: {error}")))?
                .body
        }
        AssistantProvider::None => unreachable!("availability was checked above"),
    };

    release_assistant_unless_kept(&state).await;
    let content = assistant::content_of(&response).map_err(|error| api_error(StatusCode::BAD_GATEWAY, error.to_string()))?;
    let draft = assistant::parse_draft(&content, required).map_err(|error| api_error(StatusCode::BAD_GATEWAY, error.to_string()))?;
    Ok(Json(serde_json::to_value(draft).unwrap_or(Value::Null)))
}

/// Frees the assistant's five gigabytes as soon as it has answered.
///
/// "Keep models in VRAM between jobs" is off by default, and it means what it
/// says: nothing stays loaded. The assistant was the exception nobody chose -
/// it wrote a caption, kept the card, and Music3 then died trying to load its
/// own weights. With the setting on, it stays, because that is what the
/// setting is for. Either way the next request starts it again.
async fn release_assistant_unless_kept(state: &AppState) {
    if state.engine_options.read().await.keep_loaded {
        return;
    }
    if state.assistant_runtime.base_url().await.is_some() {
        state.assistant_runtime.stop().await;
    }
}

async fn openrouter_settings() -> Json<Value> {
    let source = credentials::openrouter_source();
    Json(serde_json::json!({
        "configured": source.is_some(),
        "source": source,
        "environment_variable": credentials::OPENROUTER_ENV_VAR,
    }))
}

async fn update_openrouter_settings(
    State(state): State<AppState>,
    Json(request): Json<OpenRouterSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let source = credentials::store_openrouter_api_key(request.api_key.as_deref())
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;

    // Connecting a key is the moment to learn what it can reach. After this
    // the catalog is served from the cache on disk until the user asks for a
    // refresh, so the studio never goes to the network on its own again.
    let mut refreshed = false;
    if source.is_some() {
        {
            let mut cached = state.openrouter_catalog.write().await;
            cached.catalog = None;
        }
        refreshed = catalog_for(&state).await.is_ok();
    }

    Ok(Json(serde_json::json!({
        "configured": source.is_some(),
        "source": source,
        "environment_variable": credentials::OPENROUTER_ENV_VAR,
        "catalog_refreshed": refreshed,
    })))
}

async fn setup_status(State(state): State<AppState>) -> Json<Value> {
    let target = effective_install_target(&state).await;
    let manager_status = state.model_manager.status(target).await;
    Json(compose_setup_status(&state, manager_status).await)
}

/// Frees the disk a set of components takes.
///
/// The studio downloads ten gigabytes on request; it must be able to give them
/// back on request too, without sending anyone to hunt through a profile folder.
async fn setup_remove(
    State(state): State<AppState>,
    Json(request): Json<SetupDownloadRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let report = state
        .model_manager
        .remove(&request.ids)
        .await
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
    Ok(Json(serde_json::json!({
        "removed": report.removed,
        "freed_bytes": report.freed_bytes,
    })))
}

/// Opens the studio's own folder in the system file manager.
///
/// Saying where the ten gigabytes are is half an answer; the other half is
/// getting there without retyping a path from a settings screen.
async fn open_data_directory() -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let Some(root) = studio_data_root() else {
        return Err(api_error(StatusCode::NOT_FOUND, "the studio has no data directory".into()));
    };
    let _ = std::fs::create_dir_all(&root);
    #[cfg(windows)]
    let opened = std::process::Command::new("explorer.exe").arg(&root).spawn();
    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open").arg(&root).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let opened = std::process::Command::new("xdg-open").arg(&root).spawn();
    opened.map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(serde_json::json!({ "opened": root.display().to_string() })))
}

async fn setup_catalog(State(state): State<AppState>) -> Json<model_manager::Catalog> {
    Json(state.model_manager.catalog())
}

async fn setup_download(
    State(state): State<AppState>,
    Json(request): Json<SetupDownloadRequest>,
) -> Result<(StatusCode, Json<model_manager::DownloadJob>), (StatusCode, Json<ApiError>)> {
    let job = state
        .model_manager
        .install(InstallRequest {
            profile_id: request.profile_id,
            component_ids: request.ids,
        })
        .await
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    let state_for_completion = state.clone();
    let job_id = job.id.clone();
    tokio::spawn(async move { persist_completed_download_profile(state_for_completion, job_id).await });
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn persist_completed_download_profile(state: AppState, job_id: String) {
    loop {
        let Some(job) = state.model_manager.download_job(&job_id).await else { return; };
        match job.status {
            model_manager::DownloadStatus::Completed => {
                if let Some(profile_id) = job.profile_id {
                    *state.selected_profile_id.write().await = Some(profile_id);
                    *state.selected_component_ids.write().await = None;
                } else if state.model_manager.installed_component_files(&job.component_ids).is_ok() {
                    *state.selected_profile_id.write().await = None;
                    *state.selected_component_ids.write().await = Some(job.component_ids);
                }
                let _ = persist_studio_settings(&state).await;
                return;
            }
            model_manager::DownloadStatus::Cancelled | model_manager::DownloadStatus::Failed => return,
            model_manager::DownloadStatus::Downloading => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
        }
    }
}

/// Uses a set that is already on disk.
///
/// Downloading was the only way to change which quantisation the studio runs,
/// so a machine with two sets installed was stuck on whichever arrived last.
/// This switches between what is already there, and refuses a set with a
/// missing file rather than failing at generation time.
async fn setup_select(
    State(state): State<AppState>,
    Json(request): Json<SetupSelectRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    if let Some(profile_id) = request.profile_id.clone().filter(|value| !value.trim().is_empty()) {
        let known = state.model_manager.catalog().profiles.iter().any(|profile| profile.id == profile_id);
        if !known {
            return Err(api_error(StatusCode::BAD_REQUEST, format!("unknown profile {profile_id}")));
        }
        *state.selected_profile_id.write().await = Some(profile_id);
        *state.selected_component_ids.write().await = None;
    } else {
        let ids = request.component_ids.unwrap_or_default();
        if ids.is_empty() {
            return Err(api_error(StatusCode::BAD_REQUEST, "nothing selected".to_string()));
        }
        state
            .model_manager
            .installed_component_files(&ids)
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
        *state.selected_profile_id.write().await = None;
        *state.selected_component_ids.write().await = Some(ids);
    }
    let _ = persist_studio_settings(&state).await;
    let target = effective_install_target(&state).await;
    let manager_status = state.model_manager.status(target).await;
    Ok(Json(serde_json::to_value(compose_setup_status(&state, manager_status).await).unwrap_or(Value::Null)))
}

async fn setup_cancel(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let target = effective_install_target(&state).await;
    let manager_status = state
        .model_manager
        .cancel(target)
        .await
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(compose_setup_status(&state, manager_status).await))
}

async fn capabilities(State(state): State<AppState>) -> Json<CapabilitiesResponse> {
    let primary_installed = state.music_server.health().await;
    let parakeet_installed = state.lyrics_sync.parakeet_ready();
    let whisper_installed = state.lyrics_sync.whisper_binary().is_some();
    let assistant = state.assistant.read().await.clone();
    let assistant_installed = assistant.available();
    let catalog = state.openrouter_catalog.read().await;
    Json(CapabilitiesResponse {
        engines: capability_engines_with(catalog.catalog.as_ref(), primary_installed, parakeet_installed, whisper_installed, assistant_installed),
    })
}

fn capability_engines(
    catalog: Option<&providers::openrouter::CapabilityCatalog>,
    primary_installed: bool,
) -> Vec<EngineDescriptor> {
    capability_engines_with(catalog, primary_installed, false, false, false)
}

/// The engines the studio can offer, including the local ones it only has when
/// their models are installed. Without these the "Local" button on the provider
/// page was disabled for ever: the studio recognises speech and writes captions
/// locally, but never said so here.
fn capability_engines_with(
    catalog: Option<&providers::openrouter::CapabilityCatalog>,
    primary_installed: bool,
    parakeet_installed: bool,
    whisper_installed: bool,
    assistant_installed: bool,
) -> Vec<EngineDescriptor> {
    let mut openrouter_capabilities = vec![
        Capability::SpeechToText,
        Capability::PromptEnhancement,
        Capability::CoverArt,
    ];
    if catalog.is_some_and(|catalog| catalog.models_for(Capability::MusicGeneration).next().is_some()) {
        openrouter_capabilities.push(Capability::MusicGeneration);
    }
    vec![
        EngineDescriptor {
            id: PRIMARY_MUSIC_ENGINE_ID.into(),
            display_name: "MiniMax Music3 C++ Server".into(),
            capabilities: vec![Capability::MusicGeneration],
            execution_mode: ExecutionMode::Local,
            installed: primary_installed,
        },
        // Two different recognisers, named. "Parakeet / Whisper" was not a
        // choice, it was a shrug.
        EngineDescriptor {
            id: "parakeet".into(),
            display_name: "Parakeet TDT 0.6B (local)".into(),
            capabilities: vec![Capability::SpeechToText],
            execution_mode: ExecutionMode::Local,
            installed: parakeet_installed,
        },
        EngineDescriptor {
            id: "whisper".into(),
            display_name: "Whisper.cpp (local)".into(),
            capabilities: vec![Capability::SpeechToText],
            execution_mode: ExecutionMode::Local,
            installed: whisper_installed,
        },
        EngineDescriptor {
            id: "local-assistant".into(),
            display_name: "Local GGUF model".into(),
            capabilities: vec![Capability::PromptEnhancement],
            execution_mode: ExecutionMode::Local,
            installed: assistant_installed,
        },
        EngineDescriptor {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            capabilities: openrouter_capabilities,
            execution_mode: ExecutionMode::OpenRouter,
            installed: false,
        },
    ]
}

/// Gets the writing assistant off the graphics card before the engine needs it.
///
/// There is one card, and both models want all of it: Gemma holds five
/// gigabytes from the moment it writes a caption, and Music3 then asks for
/// eleven more and dies. The assistant starts itself on the next request it
/// receives, so stopping it here costs a reload later and nothing else.
async fn free_the_card_for_the_engine(state: &AppState) {
    if state.assistant_runtime.base_url().await.is_some() {
        state.assistant_runtime.stop().await;
    }
}

/// What the engine's own log says about why it is not there any more.
///
/// A card that ran out of memory says so in the log and then the process is
/// gone; the studio saw only a refused connection, and told the user to
/// download models that were already on disk.
fn engine_failure_reason() -> Option<String> {
    let tail = music_engine::mm_server::startup_log_tail(80).join("\n").to_lowercase();
    describes_exhausted_memory(&tail)
        .then(|| "The graphics card ran out of memory while the engine was loading the models. Choose a smaller quantisation in the model manager, or close whatever else is using the card - the writing assistant holds several gigabytes of its own.".to_string())
}

/// Whether a lowercased log says the card ran out of room.
fn describes_exhausted_memory(log: &str) -> bool {
    [
        "out of memory",
        "cudamalloc",
        "failed to allocate",
        "insufficient memory",
        "cudaerrormemoryallocation",
        "bad_alloc",
    ]
    .iter()
    .any(|marker| log.contains(marker))
}

/// Sends a job to the engine, restarting it once if it is no longer there.
///
/// A crashed engine used to end the request: the studio reported "mm-server is
/// unavailable" and waited for a human to restart it. The supervisor already
/// knows how to bring it back, so it does, and the job goes through.
async fn submit_with_recovery(state: &AppState, request: Value) -> anyhow::Result<MmServerSubmitResponse> {
    match state.music_server.submit(request.clone()).await {
        Ok(response) => Ok(response),
        Err(first) => {
            if let Some(reason) = engine_failure_reason() {
                anyhow::bail!("{reason}");
            }
            if let Err(error) = restart_engine(state).await {
                anyhow::bail!("the engine had stopped and could not be restarted: {error} (first failure: {first})");
            }
            state
                .music_server
                .submit(request)
                .await
                .map_err(|second| anyhow::anyhow!("the engine was restarted and still refused the job: {second}"))
        }
    }
}

async fn create_music_job(
    State(state): State<AppState>,
    Json(request): Json<CreateMusicJobRequest>,
) -> (StatusCode, Json<MusicJob>) {
    let music_selection = state.configuration.read().await.selections.iter().find(|selection| selection.capability == Capability::MusicGeneration).cloned();
    if music_selection.as_ref().is_some_and(|selection| selection.mode == ExecutionMode::OpenRouter) {
        return create_openrouter_music_job(state, request, music_selection.and_then(|selection| selection.cloud_model)).await;
    }
    let engine_id = selected_local_music_engine(&*state.configuration.read().await)
        .unwrap_or_else(|| "unconfigured".into());
    if engine_id != PRIMARY_MUSIC_ENGINE_ID {
        let job = queued_not_configured_job(request, engine_id);
        state.jobs.write().await.insert(job.id.clone(), job.clone());
        return (StatusCode::ACCEPTED, Json(job));
    }

    let selected_profile_id = state.selected_profile_id.read().await.clone();
    let selected_component_ids = state.selected_component_ids.read().await.clone();
    let mm_request = match mm_request_from(&request, selected_profile_id.as_deref(), selected_component_ids.as_deref(), Some(&state.model_manager)) {
        Ok(value) => value,
        Err(error) => return (StatusCode::BAD_REQUEST, Json(failed_request_job(request, engine_id, error))),
    };
    free_the_card_for_the_engine(&state).await;
    match submit_with_recovery(&state, mm_request.clone()).await {
        Ok(remote) => {
            let job = MusicJob {
                id: remote.id,
                engine_id,
                cover_prompt: request.cover_prompt.clone(),
                title: Some(titled(&request)),
                status: MusicJobStatus::Queued,
                dispatch: MusicJobDispatch::Local,
                phase: MusicJobPhase::Queued,
                caption: request.caption,
                lyrics: request.lyrics,
                duration_seconds: request.duration_seconds,
                generation_settings: mm_request.clone(),
                song: None,
                songs: vec![],
                message: "Submitted to mm-server. Progress is phase-only: queued, running, completed, failed, or cancelled.".into(),
            };
            state.jobs.write().await.insert(job.id.clone(), job.clone());
            (StatusCode::ACCEPTED, Json(job))
        }
        Err(error) => {
            let job = queued_not_configured_job(request, engine_id);
            let job = MusicJob {
                cover_prompt: None,
                message: error.to_string(),
                ..job
            };
            state.jobs.write().await.insert(job.id.clone(), job.clone());
            (StatusCode::ACCEPTED, Json(job))
        }
    }
}

async fn replay_music_job(
    State(state): State<AppState>,
    Json(request): Json<ReplayMusicJobRequest>,
) -> Result<(StatusCode, Json<MusicJob>), (StatusCode, Json<ApiError>)> {
    if selected_local_music_engine(&*state.configuration.read().await).as_deref() != Some(PRIMARY_MUSIC_ENGINE_ID) {
        return Err(api_error(StatusCode::CONFLICT, "Replay synthesis requires the local minimaxmusic-cpp engine.".into()));
    }
    let mut source_title = None;
    let replay = match (&request.song_id, &request.replay_request) {
        (Some(_), Some(_)) => return Err(api_error(StatusCode::BAD_REQUEST, "Provide either song_id or replay_request, not both.".into())),
        (Some(song_id), None) => {
            let song = state.library.get_song(song_id).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
                .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found.".into()))?;
            source_title = Some(song.title.clone());
            song.replay_request.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "This song has no MiniMax Music3 replay request.".into()))?
        }
        (None, Some(replay)) => replay.clone(),
        (None, None) => return Err(api_error(StatusCode::BAD_REQUEST, "Provide song_id or replay_request.".into())),
    };
    let synth_request = prepare_replay_synthesis(replay, &request).map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    let caption = synth_request.get("caption").and_then(Value::as_str).unwrap_or_default().to_owned();
    let lyrics = synth_request.get("lyrics").and_then(Value::as_str).unwrap_or_default().to_owned();
    free_the_card_for_the_engine(&state).await;
    let remote = submit_with_recovery(&state, synth_request.clone())
        .await
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    let job = MusicJob {
        cover_prompt: None,
        id: remote.id, engine_id: PRIMARY_MUSIC_ENGINE_ID.into(), title: source_title, status: MusicJobStatus::Queued,
        dispatch: MusicJobDispatch::Local, phase: MusicJobPhase::Queued, caption, lyrics,
        duration_seconds: synth_request.get("duration").and_then(Value::as_f64).unwrap_or_default(), generation_settings: synth_request,
        song: None, songs: vec![], message: "Submitted replay synthesis to mm-server. audio_codes are present, so the autoregressive LM stage is skipped.".into(),
    };
    state.jobs.write().await.insert(job.id.clone(), job.clone());
    Ok((StatusCode::ACCEPTED, Json(job)))
}

fn prepare_replay_synthesis(mut replay: Value, overrides: &ReplayMusicJobRequest) -> Result<Value, String> {
    let object = replay.as_object_mut().ok_or("replay_request must be a JSON object")?;
    for required in ["caption", "lyrics"] {
        if !object.get(required).and_then(Value::as_str).is_some_and(|value| !value.trim().is_empty()) { return Err(format!("replay_request has no {required}")); }
    }
    if !object.get("audio_codes").and_then(Value::as_str).is_some_and(|value| !value.trim().is_empty()) {
        return Err("replay_request has no audio_codes; it cannot skip the autoregressive LM stage".into());
    }
    if let Some(steps) = overrides.steps { if steps < 2 { return Err("steps must be at least 2".into()); } object.insert("steps".into(), Value::from(steps)); }
    if let Some(seed) = overrides.seed { object.insert("seed".into(), Value::from(seed)); }
    if let Some(dit_cfg) = overrides.dit_cfg { if !dit_cfg.is_finite() { return Err("dit_cfg must be finite".into()); } object.insert("dit_cfg".into(), Value::from(dit_cfg)); }
    if let Some(format) = &overrides.output_format {
        if !matches!(format.as_str(), "mp3" | "wav16" | "wav24" | "wav32") { return Err("output_format must be mp3, wav16, wav24, or wav32".into()); }
        object.insert("output_format".into(), Value::String(format.clone()));
    }
    if let Some(models) = &overrides.models {
        for (key, value) in [("lm_model", &models.lm_model), ("depth_model", &models.depth_model), ("cond_model", &models.cond_model), ("dit_model", &models.dit_model), ("vae_model", &models.vae_model)] {
            if let Some(value) = value { if value.trim().is_empty() { return Err(format!("{key} cannot be empty")); } object.insert(key.into(), Value::String(value.clone())); }
        }
    }
    Ok(replay)
}

async fn create_openrouter_music_job(state: AppState, request: CreateMusicJobRequest, model_id: Option<String>) -> (StatusCode, Json<MusicJob>) {
    let engine_id = "openrouter".to_owned();
    let model_id = match model_id.filter(|model| !model.trim().is_empty()) {
        Some(model) => model,
        None => return (StatusCode::BAD_REQUEST, Json(failed_request_job(request, engine_id, "OpenRouter music requires a selected model from the refreshed catalog".into()))),
    };
    let prompt = openrouter_music_prompt(&request);
    let catalog = match state.openrouter_catalog.read().await.catalog.clone() {
        Some(catalog) => catalog,
        None => return (StatusCode::BAD_REQUEST, Json(failed_request_job(request, engine_id, "Refresh the OpenRouter catalog before starting cloud music generation".into()))),
    };
    let stream_request = match providers::openrouter::music_stream_request_for(&catalog, &model_id, &prompt) {
        Ok(request) => request,
        Err(error) => return (StatusCode::BAD_REQUEST, Json(failed_request_job(request, engine_id, error.to_string()))),
    };
    let job = MusicJob {
        id: format!("openrouter-{}", uuid_suffix()), engine_id: engine_id.clone(), cover_prompt: request.cover_prompt.clone(), title: Some(titled(&request)), status: MusicJobStatus::Running,
        dispatch: MusicJobDispatch::OpenRouter, phase: MusicJobPhase::Running, caption: request.caption, lyrics: request.lyrics,
        duration_seconds: request.duration_seconds, generation_settings: stream_request.request.body.clone(), song: None, songs: vec![],
        message: "OpenRouter music stream started; the completed audio will be imported into the studio library.".into(),
    };
    state.jobs.write().await.insert(job.id.clone(), job.clone());
    let job_id = job.id.clone();
    tokio::spawn(async move { run_openrouter_music_generation(state, job_id, stream_request).await });
    (StatusCode::ACCEPTED, Json(job))
}

fn openrouter_music_prompt(request: &CreateMusicJobRequest) -> String {
    format!("Structured music caption:\n{}\n\nLyrics:\n{}", request.caption.trim(), request.lyrics.trim())
}

async fn run_openrouter_music_generation(state: AppState, job_id: String, stream_request: providers::openrouter::OpenRouterMusicStreamRequest) {
    let outcome = async {
        let response = reqwest::Client::new()
            .post(format!("{}{}", providers::openrouter::API_BASE_URL, stream_request.request.path))
            .bearer_auth(stream_request.api_key)
            .json(&stream_request.request.body)
            .send().await?
            .error_for_status()?;
        let mut stream = response.bytes_stream();
        let mut sse = Vec::new();
        while let Some(chunk) = stream.next().await { sse.extend_from_slice(&chunk?); }
        let audio = openrouter_stream::decode_audio_sse(&sse)?;
        let job = state.jobs.read().await.get(&job_id).cloned().context("cloud music job disappeared before import")?;
        let imported_song = state.library.import_generated_song(library::GeneratedSongInput {
            title: job.title.clone(),
            metadata: serde_json::json!({ "duration_seconds": job.duration_seconds, "cover_prompt": job.cover_prompt.clone() }),
            caption: job.caption.clone(), lyrics: job.lyrics.clone(), generation_settings: job.generation_settings.clone(),
            replay_request: None, audio_codes: None, engine_id: "openrouter".into(), profile_id: None,
            source: "openrouter_generation".into(), audio_extension: "wav", audio,
        })?;
        // A track goes into the library carrying its own name, style and words.
        tag_stored_song(&state, &imported_song.song.id).await;
        {
            let state = state.clone();
            let song_id = imported_song.song.id.clone();
            let timing_state = state.clone();
            let timing_song = song_id.clone();
            tokio::spawn(async move { draw_cover_for(state, song_id).await });
            tokio::spawn(async move { time_lyrics_for(timing_state, timing_song).await });
        }
        Ok::<CompletedSong, anyhow::Error>(CompletedSong { id: imported_song.song.id.clone(), audio_url: format!("/v1/library/media/{}", imported_song.song.id), song: imported_song.song })
    }.await;
    let mut jobs = state.jobs.write().await;
    let Some(job) = jobs.get_mut(&job_id) else { return; };
    match outcome {
        Ok(song) => { job.status = MusicJobStatus::Completed; job.phase = MusicJobPhase::Completed; job.song = Some(song.clone()); job.songs = vec![song]; job.message = "OpenRouter music stream completed and its audio was imported into the studio library.".into(); }
        Err(error) => { job.status = MusicJobStatus::Failed; job.phase = MusicJobPhase::Failed; job.message = format!("OpenRouter music generation failed: {error}"); }
    }
}

async fn music_job_status(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<MusicJob>, (StatusCode, Json<ApiError>)> {
    let existing = state
        .jobs
        .read()
        .await
        .get(&job_id)
        .cloned()
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Music job was not found.".into()))?;
    if existing.engine_id != PRIMARY_MUSIC_ENGINE_ID
        || matches!(existing.status, MusicJobStatus::Cancelled | MusicJobStatus::Failed)
    {
        return Ok(Json(existing));
    }
    let remote = state.music_server.job(&job_id).await.map_err(|error| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("mm-server status is unavailable: {error}"),
        )
    })?;
    let imported = if remote.status == "done" {
        Some(import_completed_mm_result(&state, &existing, &job_id).await)
    } else { None };
    let mut jobs = state.jobs.write().await;
    let job = jobs
        .get_mut(&job_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Music job was not found.".into()))?;
    if let Some(imported) = imported {
        match imported {
            Ok(songs) => {
                job.status = MusicJobStatus::Completed;
                job.phase = MusicJobPhase::Completed;
                job.song = songs.first().cloned();
                job.songs = songs;
                job.message = "mm-server completed this job and its result was imported into the studio library.".into();
            }
            Err(error) => {
                job.status = MusicJobStatus::Failed;
                job.phase = MusicJobPhase::Failed;
                job.message = format!("mm-server completed the job, but the studio could not safely import its result: {error}");
            }
        }
    } else {
        apply_remote_status(job, &remote.status);
    }
    Ok(Json(job.clone()))
}

async fn import_completed_mm_result(state: &AppState, job: &MusicJob, job_id: &str) -> anyhow::Result<Vec<CompletedSong>> {
    let result = state.music_server.result(job_id).await?;
    let tracks = mm_result::parse_multipart_result(&result.content_type, &result.body)?;
    let profile_id = state.selected_profile_id.read().await.clone();
    let mut imported = Vec::with_capacity(tracks.len());
    for track in tracks {
        let replay = track.replay_request;
        let caption = replay.get("caption").and_then(Value::as_str).filter(|value| !value.is_empty()).context("replay request has no caption")?.to_owned();
        let lyrics = replay.get("lyrics").and_then(Value::as_str).context("replay request has no lyrics")?.to_owned();
        let audio_codes = replay.get("audio_codes").filter(|value| value.as_str().is_some_and(|value| !value.is_empty())).context("replay request has no audio_codes")?.clone();
        // The replay request upstream returns is sparse: any field still at its
        // default is omitted, so a 60-second render carried no "duration" key
        // at all and the stored provenance looked half empty. Start from what
        // was actually submitted and let the per-song values - the seeds it
        // rolled, the model files it used - win over it.
        let mut generation_settings = job.generation_settings.clone();
        match (generation_settings.as_object_mut(), replay.as_object()) {
            (Some(target), Some(source)) => {
                for (key, value) in source {
                    target.insert(key.clone(), value.clone());
                }
            }
            _ => generation_settings = replay.clone(),
        }
        generation_settings.as_object_mut().context("generation settings are not a JSON object")?.remove("audio_codes");
        // The engine returns audio, not metadata. Duration and the identifying
        // seeds come from the replay request, so the library row can show a real
        // length instead of "unknown" and the track can be traced back.
        // The replay request is sparse: upstream omits any field that still
        // holds its default, so a 60-second track has no "duration" key at all.
        // Take the length from the job that was actually submitted.
        let extension = mm_result::audio_extension(&track.audio_content_type)?;
        let metadata = serde_json::json!({
            "duration_seconds": library::audio_duration_seconds(
                &track.audio,
                extension,
                replay.get("mp3_bitrate").and_then(Value::as_u64).map(|value| value as u32),
            ),
            "seed": replay.get("seed"),
            "lm_seed": replay.get("lm_seed"),
            "output_format": replay.get("output_format"),
            "cover_prompt": job.cover_prompt.clone(),
        });
        let imported_song = state.library.import_generated_song(library::GeneratedSongInput {
            title: job.title.clone(), metadata, caption, lyrics, generation_settings, replay_request: Some(replay), audio_codes: Some(audio_codes),
            engine_id: job.engine_id.clone(), profile_id: profile_id.clone(),
            source: "local_generation".into(),
            audio_extension: extension, audio: track.audio,
        })?;
        let audio_url = format!("/v1/library/media/{}", imported_song.song.id);
        tag_stored_song(&state, &imported_song.song.id).await;
        {
            // Locally generated tracks get a cover too: this call was lost in an
            // edit and only the cloud path kept it, which is why covers appeared
            // for one kind of track and not the other.
            let state = state.clone();
            let song_id = imported_song.song.id.clone();
            let timing_state = state.clone();
            let timing_song = song_id.clone();
            tokio::spawn(async move { draw_cover_for(state, song_id).await });
            tokio::spawn(async move { time_lyrics_for(timing_state, timing_song).await });
        }
        imported.push(CompletedSong { id: imported_song.song.id.clone(), song: imported_song.song, audio_url });
    }
    Ok(imported)
}

async fn cancel_music_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<MusicJob>, (StatusCode, Json<ApiError>)> {
    let existing = state
        .jobs
        .read()
        .await
        .get(&job_id)
        .cloned()
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Music job was not found.".into()))?;
    if existing.engine_id != PRIMARY_MUSIC_ENGINE_ID {
        return Err(api_error(
            StatusCode::NOT_IMPLEMENTED,
            format!("The selected engine '{}' has no installed cancel adapter.", existing.engine_id),
        ));
    }
    let remote = state.music_server.cancel(&job_id).await.map_err(|error| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("mm-server cancel is unavailable: {error}"),
        )
    })?;
    let mut jobs = state.jobs.write().await;
    let job = jobs
        .get_mut(&job_id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Music job was not found.".into()))?;
    apply_remote_status(job, &remote.status);
    Ok(Json(job.clone()))
}

async fn local_music_model_catalog(
    State(state): State<AppState>,
) -> Result<Json<LocalMusicModelCatalog>, (StatusCode, Json<ApiError>)> {
    let engine_id = selected_local_music_engine(&*state.configuration.read().await).ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "No local music engine is selected in the capability configuration.".into(),
        )
    })?;
    if engine_id != PRIMARY_MUSIC_ENGINE_ID {
        return Err(api_error(
            StatusCode::NOT_IMPLEMENTED,
            format!("The selected engine '{engine_id}' has no installed server catalog adapter."),
        ));
    }
    let catalog = state.music_server.props().await.map_err(|error| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("mm-server model catalog is unavailable: {error}"),
        )
    })?;
    Ok(Json(LocalMusicModelCatalog { engine_id, catalog }))
}

impl MmServerClient {
    fn from_environment() -> Self {
        let base_url = env::var("MINIMAX_MUSIC_CPP_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8086".into())
            .trim_end_matches('/')
            .to_owned();
        Self {
            base_url,
            http: reqwest::Client::new(),
        }
    }

    async fn health(&self) -> bool {
        self.http
            .get(self.url("/health"))
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
    }

    async fn props(&self) -> anyhow::Result<Value> {
        self.json_response(self.http.get(self.url("/props")).send().await?).await
    }

    /// Upstream `GET /logs` is an endless SSE stream: it replays the server's
    /// log ring immediately and then blocks waiting for new lines. Studio wants
    /// the ring, not a permanent connection, so the stream is consumed until it
    /// goes quiet and then dropped.
    async fn logs_snapshot(&self, quiet_period: std::time::Duration) -> anyhow::Result<Vec<String>> {
        let response = self.http.get(self.url("/logs")).send().await?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("mm-server returned {status} for /logs");
        }
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        // Hard ceiling so a chatty engine cannot hold the request open.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(quiet_period.min(remaining), stream.next()).await {
                Ok(Some(chunk)) => buffer.extend_from_slice(&chunk?),
                Ok(None) | Err(_) => break,
            }
        }
        Ok(String::from_utf8_lossy(&buffer)
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect())
    }

    async fn submit(&self, request: Value) -> anyhow::Result<MmServerSubmitResponse> {
        self.json_response(self.http.post(self.url("/synth")).json(&request).send().await?)
            .await
    }

    async fn job(&self, job_id: &str) -> anyhow::Result<MmServerJobResponse> {
        self.json_response(self.http.get(self.url("/job")).query(&[("id", job_id)]).send().await?)
            .await
    }

    async fn cancel(&self, job_id: &str) -> anyhow::Result<MmServerJobResponse> {
        self.json_response(self.http.post(self.url("/job")).query(&[("id", job_id), ("cancel", "1")]).send().await?)
            .await
    }

    async fn result(&self, job_id: &str) -> anyhow::Result<MmServerResultResponse> {
        let response = self.http.get(self.url("/job")).query(&[("id", job_id), ("result", "1")]).send().await?;
        let status = response.status();
        if !status.is_success() { anyhow::bail!("mm-server returned {status}: {}", response.text().await?); }
        let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or("").to_owned();
        Ok(MmServerResultResponse { content_type, body: response.bytes().await?.to_vec() })
    }

    async fn json_response<T: serde::de::DeserializeOwned>(&self, response: reqwest::Response) -> anyhow::Result<T> {
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("mm-server returned {status}: {body}");
        }
        Ok(serde_json::from_str(&body)?)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

fn initial_configuration() -> StudioConfiguration {
    let mut configuration = StudioConfiguration::default();
    if let Some(selection) = configuration
        .selections
        .iter_mut()
        .find(|selection| selection.capability == Capability::MusicGeneration)
    {
        selection.mode = ExecutionMode::Local;
        selection.local_engine = Some(PRIMARY_MUSIC_ENGINE_ID.into());
        selection.cloud_model = None;
    }
    configuration
}

/// Settings written by an earlier build may still name local engines this
/// build does not ship (an ASR or LLM engine, for example). Keeping them would
/// make the UI offer a provider the engine registry cannot serve, so any local
/// engine that is not declared by `capability_engines` is dropped and the
/// capability falls back to a mode that actually has an implementation.
fn sanitize_persisted_configuration(mut configuration: StudioConfiguration) -> StudioConfiguration {
    let declared = capability_engines(None, false);
    for selection in &mut configuration.selections {
        let engine_serves_capability = selection.local_engine.as_deref().is_some_and(|engine_id| {
            declared.iter().any(|engine| {
                engine.id == engine_id
                    && engine.execution_mode == ExecutionMode::Local
                    && engine.capabilities.contains(&selection.capability)
            })
        });
        if engine_serves_capability {
            continue;
        }
        selection.local_engine = None;
        if selection.mode == ExecutionMode::Local {
            selection.mode = ExecutionMode::OpenRouter;
        }
    }
    configuration
}

fn selected_local_music_engine(configuration: &StudioConfiguration) -> Option<String> {
    configuration
        .selections
        .iter()
        .find(|selection| {
            selection.capability == Capability::MusicGeneration
                && selection.mode == ExecutionMode::Local
        })
        .and_then(|selection| selection.local_engine.clone())
}

fn mm_request_from(request: &CreateMusicJobRequest, selected_profile_id: Option<&str>, selected_component_ids: Option<&[String]>, manager: Option<&ModelManager>) -> Result<Value, String> {
    if request.caption.trim().is_empty() || request.lyrics.trim().is_empty() {
        return Err("caption and lyrics are required by mm-server".into());
    }
    if !request.duration_seconds.is_finite() || request.duration_seconds <= 0.0 {
        return Err("duration_seconds must be greater than zero".into());
    }
    if request.steps.is_some_and(|steps| steps < 2) {
        return Err("steps must be at least 2".into());
    }
    if request.lm_batch_size.is_some_and(|size| size < 1) {
        return Err("lm_batch_size must be at least 1".into());
    }
    if request.synth_batch_size.is_some_and(|size| !(1..=9).contains(&size)) {
        return Err("synth_batch_size must be between 1 and 9".into());
    }
    for (field, value) in [("lm_cfg", request.lm_cfg), ("dit_cfg", request.dit_cfg)] {
        if value.is_some_and(|value| !value.is_finite()) {
            return Err(format!("{field} must be finite"));
        }
    }
    if let Some(output_format) = request.output_format.as_deref() {
        if !matches!(output_format, "mp3" | "wav16" | "wav24" | "wav32") {
            return Err("output_format must be one of: mp3, wav16, wav24, wav32".into());
        }
    }
    let models = match request.models.clone() {
        Some(models) => {
            let all_explicit = [models.lm_model.as_deref(), models.depth_model.as_deref(), models.cond_model.as_deref(), models.dit_model.as_deref(), models.vae_model.as_deref()]
                .iter().all(|value| value.is_some_and(|value| !value.trim().is_empty()));
            if !all_explicit { return Err("advanced model selection must explicitly provide all five MM3 component filenames".into()); }
            // A job carries the names of the weights it wants. A replayed or
            // queued one can name a set that has since been removed, and the
            // engine then spends a minute loading nothing before failing. The
            // files are checked here, while there is still someone to tell.
            if let Some(manager) = manager {
                let root = manager.models_directory();
                for name in [models.lm_model.as_deref(), models.depth_model.as_deref(), models.cond_model.as_deref(), models.dit_model.as_deref(), models.vae_model.as_deref()].into_iter().flatten() {
                    if !root.join(name).is_file() {
                        return Err(format!("this request names a model file that is no longer on disk: {name}. Choose a set in Settings - Models and try again."));
                    }
                }
            }
            models
        }
        None => {
            let manager = manager.ok_or("local model manager is unavailable")?;
            let files = if let Some(component_ids) = selected_component_ids {
                manager.installed_component_files(component_ids).map_err(|error| error.to_string())?
            } else {
                let profile_id = selected_profile_id.ok_or("no installed local model profile or complete custom component set is selected")?;
                manager.installed_profile_files(profile_id).map_err(|error| error.to_string())?
            };
            Mm3ModelSelection { lm_model: Some(files.lm_model), depth_model: Some(files.depth_model), cond_model: Some(files.cond_model), dit_model: Some(files.dit_model), vae_model: Some(files.vae_model) }
        }
    };
    let mut body = serde_json::json!({
        "caption": request.caption,
        "lyrics": request.lyrics,
        "duration": request.duration_seconds,
        "lm_model": models.lm_model.expect("complete selection"),
        "depth_model": models.depth_model.expect("complete selection"),
        "cond_model": models.cond_model.expect("complete selection"),
        "dit_model": models.dit_model.expect("complete selection"),
        "vae_model": models.vae_model.expect("complete selection"),
    });
    // Make the submitted and replayed request a complete provenance record,
    // rather than relying on hidden mm-server defaults.
    body["steps"] = Value::from(request.steps.unwrap_or(30));
    body["lm_batch_size"] = Value::from(request.lm_batch_size.unwrap_or(1));
    body["synth_batch_size"] = Value::from(request.synth_batch_size.unwrap_or(1));
    body["peak_clip"] = Value::from(request.peak_clip.unwrap_or(10));
    body["mp3_bitrate"] = Value::from(request.mp3_bitrate.unwrap_or(128));
    insert_optional(&mut body, "seed", request.seed);
    insert_optional(&mut body, "lm_seed", request.lm_seed);
    insert_optional(&mut body, "lm_cfg", request.lm_cfg);
    insert_optional(&mut body, "lm_top_k", request.lm_top_k);
    insert_optional(&mut body, "dit_cfg", request.dit_cfg);
    insert_optional(&mut body, "output_format", request.output_format.clone());
    Ok(body)
}

fn insert_optional<T: Serialize>(body: &mut Value, key: &str, value: Option<T>) {
    if let Some(value) = value {
        body[key] = serde_json::to_value(value).expect("serializable MM3 request value");
    }
}

fn queued_not_configured_job(request: CreateMusicJobRequest, engine_id: String) -> MusicJob {
    MusicJob {
        cover_prompt: None,
        id: format!("unconfigured-{}", uuid_suffix()),
        engine_id,
        title: request.title.clone(),
        status: MusicJobStatus::Queued,
        dispatch: MusicJobDispatch::NotConfigured,
        phase: MusicJobPhase::Queued,
        caption: request.caption,
        lyrics: request.lyrics,
        duration_seconds: request.duration_seconds,
        generation_settings: serde_json::Value::Null,
        song: None,
        songs: vec![],
        message: "The selected local music engine is not configured; this job remains queued and no inference has started.".into(),
    }
}

fn failed_request_job(request: CreateMusicJobRequest, engine_id: String, error: String) -> MusicJob {
    MusicJob {
        cover_prompt: None,
        title: request.title.clone(),
        id: format!("rejected-{}", uuid_suffix()),
        engine_id,
        status: MusicJobStatus::Failed,
        dispatch: MusicJobDispatch::NotConfigured,
        phase: MusicJobPhase::Failed,
        caption: request.caption,
        lyrics: request.lyrics,
        duration_seconds: request.duration_seconds,
        generation_settings: serde_json::Value::Null,
        song: None,
        songs: vec![],
        message: error,
    }
}

fn uuid_suffix() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn apply_remote_status(job: &mut MusicJob, remote_status: &str) {
    match remote_status {
        "queued" => {
            job.status = MusicJobStatus::Queued;
            job.phase = MusicJobPhase::Queued;
            job.message = "mm-server has queued this job.".into();
        }
        "running" => {
            job.status = MusicJobStatus::Running;
            job.phase = MusicJobPhase::Running;
            job.message = "mm-server is running this job. Numeric progress is not provided by the engine.".into();
        }
        "done" => {
            job.status = MusicJobStatus::Completed;
            job.phase = MusicJobPhase::Completed;
            job.message = "mm-server completed this job; fetch its result from mm-server using the job id.".into();
        }
        "failed" => {
            job.status = MusicJobStatus::Failed;
            job.phase = MusicJobPhase::Failed;
            job.message = "mm-server reported a failed job.".into();
        }
        "cancelled" => {
            job.status = MusicJobStatus::Cancelled;
            job.dispatch = MusicJobDispatch::Cancelled;
            job.phase = MusicJobPhase::Cancelled;
            job.message = "mm-server cancelled this job.".into();
        }
        other => {
            job.status = MusicJobStatus::Failed;
            job.phase = MusicJobPhase::Failed;
            job.message = format!("mm-server returned an unknown job status: {other}");
        }
    }
}

fn api_error(status: StatusCode, error: String) -> (StatusCode, Json<ApiError>) {
    (status, Json(ApiError { error }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing stays in VRAM unless the user asked for it. This is the setting
    /// the assistant's unload is tied to, and it is off to begin with.
    #[test]
    fn nothing_is_kept_in_memory_by_default() {
        assert!(!EngineOptions::default().keep_loaded);
        assert!(!EngineOptions::default().to_engine().keep_loaded);
    }

    /// A card that ran out of memory has to say so. The studio used to answer
    /// with "download the five components", pointing at models already on disk.
    #[test]
    fn an_out_of_memory_engine_is_named_as_one() {
        for line in [
            "ggml_backend_cuda_buffer_type_alloc_buffer: allocating 4096 MB on device 0: cudaMalloc failed: out of memory",
            "CUDA error: out of memory",
            "std::bad_alloc",
        ] {
            assert!(
                describes_exhausted_memory(&line.to_lowercase()),
                "this is what running out of memory looks like and it was not recognised: {line}"
            );
        }
        assert!(!describes_exhausted_memory("loading model from disk"));
    }

    #[test]
    fn primary_engine_is_selected_by_default() {
        assert_eq!(
            selected_local_music_engine(&initial_configuration()).as_deref(),
            Some(PRIMARY_MUSIC_ENGINE_ID)
        );
    }

    #[test]
    fn request_maps_only_confirmed_mm_server_fields() {
        let body = mm_request_from(&CreateMusicJobRequest {
            cover_prompt: None,
            title: None,
            caption: "night drive".into(),
            lyrics: "one line".into(),
            duration_seconds: 30.0,
            steps: Some(30),
            seed: Some(7),
            lm_seed: None,
            lm_cfg: Some(1.5),
            lm_top_k: Some(50),
            lm_batch_size: Some(2),
            synth_batch_size: Some(3),
            dit_cfg: Some(1.7),
            peak_clip: Some(10),
            output_format: Some("wav24".into()),
            mp3_bitrate: Some(320),
            models: Some(Mm3ModelSelection {
                lm_model: Some("lm.gguf".into()),
                depth_model: Some("depth.gguf".into()),
                cond_model: Some("condition.gguf".into()),
                dit_model: Some("dit.gguf".into()),
                vae_model: Some("vocoder.gguf".into()),
                ..Default::default()
            }),
        }, None, None, None)
        .unwrap();
        assert_eq!(body["duration"], 30.0);
        assert_eq!(body["lm_model"], "lm.gguf");
        assert_eq!(body["output_format"], "wav24");
        assert_eq!(body["lm_batch_size"], 2);
        assert_eq!(body["synth_batch_size"], 3);
        assert_eq!(body["peak_clip"], 10);
        assert_eq!(body["mp3_bitrate"], 320);
        assert!(body.get("reference_audio").is_none());
    }

    #[test]
    fn request_rejects_legacy_audio_formats_not_supported_by_mm_server() {
        let error = mm_request_from(&CreateMusicJobRequest {
            cover_prompt: None,
            title: None,
            caption: "night drive".into(),
            lyrics: "one line".into(),
            duration_seconds: 30.0,
            steps: Some(30),
            seed: None,
            lm_seed: None,
            lm_cfg: Some(1.5),
            lm_top_k: Some(50),
            lm_batch_size: None,
            synth_batch_size: None,
            dit_cfg: Some(1.7),
            peak_clip: None,
            output_format: Some("flac".into()),
            mp3_bitrate: None,
            models: Some(Mm3ModelSelection {
                lm_model: Some("lm.gguf".into()),
                depth_model: Some("depth.gguf".into()),
                cond_model: Some("condition.gguf".into()),
                dit_model: Some("dit.gguf".into()),
                vae_model: Some("vocoder.gguf".into()),
            }),
        }, None, None, None).unwrap_err();
        assert!(error.contains("output_format"));
    }

    #[test]
    fn request_uses_confirmed_mm3_defaults_and_rejects_invalid_synth_batch() {
        let request = CreateMusicJobRequest {
            cover_prompt: None,
            title: None,
            caption: "night drive".into(), lyrics: "[verse] one line".into(), duration_seconds: 60.0,
            steps: None, seed: None, lm_seed: None, lm_cfg: None, lm_top_k: None,
            lm_batch_size: None, synth_batch_size: None, dit_cfg: None, peak_clip: None,
            output_format: None, mp3_bitrate: None,
            models: Some(Mm3ModelSelection { lm_model: Some("lm.gguf".into()), depth_model: Some("depth.gguf".into()), cond_model: Some("condition.gguf".into()), dit_model: Some("dit.gguf".into()), vae_model: Some("vocoder.gguf".into()) }),
        };
        let body = mm_request_from(&request, None, None, None).unwrap();
        assert_eq!(body["steps"], 30);
        assert_eq!(body["lm_batch_size"], 1);
        assert_eq!(body["synth_batch_size"], 1);
        assert_eq!(body["peak_clip"], 10);
        assert_eq!(body["mp3_bitrate"], 128);

        let invalid = CreateMusicJobRequest { synth_batch_size: Some(10), ..request };
        assert!(mm_request_from(&invalid, None, None, None).unwrap_err().contains("synth_batch_size"));
    }

    #[test]
    fn persisted_settings_round_trip_a_complete_custom_component_selection() {
        let settings = PersistedStudioSettings {
            engine_options: EngineOptions { keep_loaded: true, max_batch: Some(2), ..EngineOptions::default() },
            assistant: AssistantConfig { provider: AssistantProvider::Local, local_base_url: Some("http://127.0.0.1:8080/v1".into()), local_model: Some("gemma".into()), openrouter_model: None, managed_model: None, managed_path: None, reasoning_effort: None },
            configuration: initial_configuration(),
            // Karaoke is off by default and has to survive a restart the same
            // way the assistant does.
            lyrics_sync: lyrics_sync::LyricsSyncConfig {
                enabled: true,
                provider: lyrics_sync::AsrProvider::Parakeet,
                whisper_model: None,
                openrouter_model: None,
            },
            selected_profile_id: None,
            selected_component_ids: Some(vec!["lm-q8".into(), "depth-q8".into(), "condition-f32".into(), "dit-q6".into(), "vocoder-f32".into()]),
            cover_templates: Some(cover_prompt::default_templates()),
            cover_auto: Some(true),
            separation: Some(separation::SeparationConfig::default()),
            cover_template_default: Some("photographic".into()),
        };
        let restored: PersistedStudioSettings = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(restored.lyrics_sync.available());
        assert_eq!(restored.lyrics_sync.provider, lyrics_sync::AsrProvider::Parakeet);
        assert!(restored.selected_profile_id.is_none());
        assert_eq!(restored.selected_component_ids.unwrap(), vec!["lm-q8", "depth-q8", "condition-f32", "dit-q6", "vocoder-f32"]);
        // Engine flags survive a restart, and the songs-per-request ceiling is
        // derived from them rather than assumed.
        assert!(restored.engine_options.keep_loaded);
        assert_eq!(restored.engine_options.effective_max_batch(), 2);
        assert_eq!(EngineOptions::default().effective_max_batch(), 1);
        // The assistant is optional: it must survive a restart when configured,
        // and stay unavailable when it is not.
        assert!(restored.assistant.available());
        assert!(!AssistantConfig::default().available());
    }

    #[test]
    fn remote_statuses_never_claim_success_for_an_unknown_value() {
        let mut job = queued_not_configured_job(
            CreateMusicJobRequest {
                cover_prompt: None,
                title: None,
            caption: "night drive".into(),
                lyrics: "one line".into(),
                duration_seconds: 30.0,
                steps: None,
                seed: None,
                lm_seed: None,
                lm_cfg: None,
                lm_top_k: None,
                lm_batch_size: None,
                synth_batch_size: None,
                dit_cfg: None,
                peak_clip: None,
                output_format: None,
                mp3_bitrate: None,
                models: None,
            },
            PRIMARY_MUSIC_ENGINE_ID.into(),
        );
        apply_remote_status(&mut job, "not-a-real-status");
        assert!(matches!(job.status, MusicJobStatus::Failed));
    }

    #[test]
    fn capabilities_use_the_engines_envelope_and_only_advertise_catalog_verified_cloud_music() {
        let before_refresh = capability_engines(None, false);
        assert!(!before_refresh.iter().find(|engine| engine.id == "openrouter").unwrap().capabilities.contains(&Capability::MusicGeneration));
        let catalog = providers::openrouter::CapabilityCatalog::parse(r#"{"data":[{"id":"catalog/music","name":"Song","description":"Music generation for full-length songs","architecture":{"input_modalities":["text"],"output_modalities":["audio"]}}]}"#).unwrap();
        let after_refresh = CapabilitiesResponse { engines: capability_engines(Some(&catalog), false) };
        assert!(after_refresh.engines.iter().find(|engine| engine.id == "openrouter").unwrap().capabilities.contains(&Capability::MusicGeneration));
        // The music engine, two recognisers, the local assistant and
        // OpenRouter: everything listed is something the studio can actually do.
        assert_eq!(serde_json::to_value(after_refresh).unwrap()["engines"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn replay_synthesis_keeps_audio_codes_and_only_applies_confirmed_synthesis_overrides() {
        let request = ReplayMusicJobRequest { song_id: None, replay_request: None, steps: Some(42), seed: Some(9), dit_cfg: Some(1.9), output_format: Some("wav24".into()), models: Some(Mm3ModelSelection { dit_model: Some("dit-q6.gguf".into()), ..Default::default() }) };
        let replay = serde_json::json!({"caption":"night drive","lyrics":"[verse] hi","audio_codes":"1,2,3,4,5,6,7,8","lm_seed":123,"lm_cfg":1.5,"dit_cfg":1.7,"steps":30,"seed":1,"dit_model":"dit-q4.gguf"});
        let prepared = prepare_replay_synthesis(replay, &request).unwrap();
        assert_eq!(prepared["audio_codes"], "1,2,3,4,5,6,7,8");
        assert_eq!(prepared["steps"], 42);
        assert_eq!(prepared["seed"], 9);
        assert_eq!(prepared["dit_cfg"], 1.9);
        assert_eq!(prepared["output_format"], "wav24");
        assert_eq!(prepared["dit_model"], "dit-q6.gguf");
        assert_eq!(prepared["lm_seed"], 123);
    }

    #[test]
    fn replay_without_audio_codes_is_rejected_before_mm_server_submission() {
        let request = ReplayMusicJobRequest { song_id: None, replay_request: None, steps: None, seed: None, dit_cfg: None, output_format: None, models: None };
        assert!(prepare_replay_synthesis(serde_json::json!({"caption":"c","lyrics":"l"}), &request).is_err());
    }
}
