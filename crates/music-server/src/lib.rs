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
mod model_port;
mod presets;
mod request_log;
mod resources;
mod chunked;
mod separation;
mod sizes;
mod skill;
mod library;
mod mm_result;
mod openrouter_stream;
mod omnibridge;
mod security;

use std::{collections::HashMap, env, fs, net::SocketAddr, path::PathBuf, sync::Arc};
use anyhow::Context;
use futures_util::StreamExt;

use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware,
    routing::{get, post},
    Json, Router,
};
use music_core::{Capability, EngineDescriptor, ExecutionMode, StudioConfiguration};
use model_manager::{InstallRequest, ModelManager};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use tower_http::cors::{AllowOrigin, CorsLayer};

const PRIMARY_MUSIC_ENGINE_ID: &str = "minimaxmusic-cpp";
const JSON_BODY_LIMIT: usize = 2 * 1024 * 1024;
const COVER_BODY_LIMIT: usize = 32 * 1024 * 1024;
const AUDIO_BODY_LIMIT: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MusicExecutionTarget {
    Configuration,
    OmniBridge,
}

fn music_execution_target() -> Result<MusicExecutionTarget, String> {
    match env::var("MUSIC_MAKER_MUSIC_EXECUTION_TARGET")
        .unwrap_or_else(|_| "configuration".to_owned())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "configuration" | "legacy" => Ok(MusicExecutionTarget::Configuration),
        "omnibridge" => Ok(MusicExecutionTarget::OmniBridge),
        _ => Err("MUSIC_MAKER_MUSIC_EXECUTION_TARGET must be configuration or omnibridge".to_owned()),
    }
}

fn parse_requested_music_execution_target(value: Option<&str>) -> Result<Option<MusicExecutionTarget>, String> {
    match value.map(str::trim).filter(|value| !value.is_empty()).map(str::to_ascii_lowercase).as_deref() {
        None | Some("auto") => Ok(None),
        Some("cloud") | Some("omnibridge") => Ok(Some(MusicExecutionTarget::OmniBridge)),
        Some("local") | Some("configuration") => Ok(Some(MusicExecutionTarget::Configuration)),
        Some(_) => Err("execution_target must be auto, cloud, omnibridge, local, or configuration".to_owned()),
    }
}

fn requested_music_execution_target(request: &CreateMusicJobRequest) -> Result<MusicExecutionTarget, String> {
    parse_requested_music_execution_target(request.execution_target.as_deref())?
        .map(Ok)
        .unwrap_or_else(music_execution_target)
}

/// Returned only through the in-process Tauri command. The HTTP API never
/// exposes this value and the type's Debug representation is redacted.
pub fn desktop_session_token() -> String {
    security::SessionToken::global().expose_for_desktop_bridge()
}

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
    omnibridge_store: Arc<tokio::sync::Mutex<omnibridge::OmniBridgeMusicStore>>,
    model_port: model_port::ModelPort,
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
    #[serde(default)]
    client_request_id: Option<String>,
    #[serde(default)]
    execution_target: Option<String>,
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
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum MusicJobDispatch {
    NotConfigured,
    Local,
    OpenRouter,
    OmniBridge,
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
    SubmissionUnknown,
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
    /// How many songs one request may render, and the `--max-batch` the engine
    /// is started with - `lm_batch_size` may not exceed it.
    ///
    /// One, like the engine's own default. The flag reserves KV cache for the
    /// full batch when the weights load, whether or not anyone asks for it, so
    /// a studio that quietly asked for four made every single-song generation
    /// pay for three it would never render. Whoever wants more sets it, and the
    /// engine restarts with the memory that choice costs.
    fn effective_max_batch(&self) -> u32 {
        self.max_batch.unwrap_or(1).max(1)
    }

    fn to_engine(self) -> music_engine::mm_server::MmServerOptions {
        music_engine::mm_server::MmServerOptions {
            keep_loaded: self.keep_loaded,
            // The ceiling the studio offers, given to the engine that has to
            // honour it. `--max-batch` sizes the language model's KV sets when
            // the weights are loaded, and the engine refuses any request above
            // it: leaving the flag off meant it loaded with the upstream
            // default of one while the panel offered four, so asking for two
            // songs failed before it started.
            max_batch: Some(self.effective_max_batch()),
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
    None,
    Local,
    OpenRouter,
    /// A model Studio downloaded and runs itself with llama.cpp.
    Managed,
    /// The centrally managed OmniBridge text routes.
    #[default]
    #[serde(rename = "omnibridge", alias = "managed_cloud", alias = "cloud")]
    OmniBridge,
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
            AssistantProvider::OmniBridge => omnibridge::OmniBridgeTextConfig::from_env_with_route("route:text:configured").is_ok(),
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
    let address = SocketAddr::from(([127, 0, 0, 1], listen_port()));
    let listener = tokio::net::TcpListener::bind(address).await?;
    serve_with_listener(listener).await
}

/// Runs the service on a listener whose ownership has already been established.
/// The desktop shell binds synchronously before exposing its session credential,
/// so an unrelated process cannot win the loopback-port race and impersonate it.
pub async fn serve_with_listener(listener: tokio::net::TcpListener) -> anyhow::Result<()> {
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
    let data_root = studio_data_root().unwrap_or_else(|| PathBuf::from("."));
    let omnibridge_store = omnibridge::OmniBridgeMusicStore::new(data_root.join("omnibridge").join("music-jobs.json"));
    let model_port = model_port::ModelPort::new(&data_root)
        .map_err(anyhow::Error::msg)?;
    let restored_jobs = restore_omnibridge_jobs(&omnibridge_store)?;
    let state = AppState {
        configuration: Arc::new(RwLock::new(sanitize_persisted_configuration(
            persisted.as_ref().map(|settings| settings.configuration.clone()).unwrap_or_else(initial_configuration),
        ))),
        jobs: Arc::new(RwLock::new(restored_jobs)),
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
        omnibridge_store: Arc::new(tokio::sync::Mutex::new(omnibridge_store)),
        model_port,
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

    let session_token = security::SessionToken::global();
    let mut allowed_origins = vec![
        HeaderValue::from_static("http://tauri.localhost"),
        HeaderValue::from_static("https://tauri.localhost"),
        HeaderValue::from_static("tauri://localhost"),
        HeaderValue::from_static("http://127.0.0.1:8765"),
    ];
    let allow_dev_origins = cfg!(debug_assertions)
        || env::var("MINIMAX_STUDIO_ALLOW_DEV_ORIGINS").as_deref() == Ok("1");
    if allow_dev_origins {
        allowed_origins.extend([
            HeaderValue::from_static("http://127.0.0.1:3000"),
            HeaderValue::from_static("http://localhost:3000"),
        ]);
    }
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS])
        .allow_headers([
            header::ACCEPT,
            header::CONTENT_TYPE,
            header::HeaderName::from_static("x-studio-session"),
        ])
        .expose_headers([header::CONTENT_LENGTH, header::CONTENT_TYPE]);

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/integrations/omnibridge", get(omnibridge_integration_status))
        .route("/v1/model-bindings", get(model_bindings).put(update_model_bindings))
        .route("/v1/model-bindings/preview", post(preview_model_bindings))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/configuration", get(configuration).put(update_configuration))
        .route("/engine/presets", get(engine_presets))
        .route("/engine/preset", post(apply_engine_preset))
        .route("/engine/options", get(engine_options).put(update_engine_options))
        .route("/engine/restart", post(restart_local_engine))
        .route("/v1/engine/logs", get(engine_logs))
        .route("/v1/system/resources", get(system_resources))
        .route("/v1/proxy/image", get(proxy_image))
        .route("/v1/openrouter/settings", get(openrouter_settings).put(update_openrouter_settings))
        .route("/v1/openrouter/logs", get(openrouter_logs))
        .route("/v1/assistant/status", get(assistant_status).put(update_assistant_settings))
        .route("/v1/assistant/local-models", get(assistant_local_models))
        .route("/v1/assistant/write", post(assistant_write))
        .route("/v1/assistant/write/stream", post(assistant_write_stream))
        .route("/v1/assistant/runtime", get(assistant_runtime_status))
        .route("/v1/assistant/runtime/install", post(assistant_runtime_install))
        .route("/v1/assistant/runtime/cancel", post(cancel_assistant_download))
        .route("/v1/assistant/runtime/remove", post(assistant_runtime_remove))
        .route("/v1/assistant/runtime/start", post(assistant_runtime_start))
        .route("/v1/assistant/runtime/stop", post(assistant_runtime_stop))
        .route("/v1/karaoke/status", get(karaoke_status).put(update_karaoke_settings))
        .route("/v1/karaoke/install", post(karaoke_install))
        .route("/v1/karaoke/cancel", post(cancel_karaoke_download))
        .route("/v1/karaoke/remove", post(karaoke_remove))
        .route("/v1/library/songs/{id}/karaoke", post(create_song_karaoke).delete(delete_song_karaoke))
        .route("/v1/openrouter/catalog", get(openrouter_catalog))
        .route("/v1/openrouter/catalog/refresh", post(refresh_openrouter_catalog))
        .route(
            "/v1/openrouter/transcriptions",
            post(create_openrouter_transcription).layer(DefaultBodyLimit::max(AUDIO_BODY_LIMIT)),
        )
        .route("/v1/openrouter/covers", post(create_openrouter_cover))
        .route("/editor", get(|| async { axum::response::Redirect::permanent("/editor/index.html") }))
        .route("/editor/{*path}", get(editor_asset))
        .route("/v1/separation/runtime", get(separation_assets))
        .route("/v1/separation/runtime/install", post(install_separation_asset))
        .route("/v1/separation/runtime/cancel", post(cancel_separation_download))
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
        .route(
            "/v1/library/import",
            post(import_library_audio).layer(DefaultBodyLimit::max(AUDIO_BODY_LIMIT)),
        )
        .route("/v1/library/songs/{id}", get(library_song).put(update_library_song).delete(delete_library_song))
        .route("/v1/library/media/{song_id}", get(library_media))
        .route(
            "/v1/library/songs/{id}/cover",
            get(library_cover).put(store_library_cover).layer(DefaultBodyLimit::max(COVER_BODY_LIMIT)),
        )
        .route("/v1/library/playlists", get(library_playlists).post(create_library_playlist))
        .route("/v1/library/playlists/{id}", get(library_playlist).put(update_library_playlist).delete(delete_library_playlist))
        .route("/setup/status", get(setup_status))
        .route("/setup/catalog", get(setup_catalog))
        .route("/setup/download", post(setup_download))
        .route("/setup/remove", post(setup_remove))
        .route("/setup/adopt", post(setup_adopt))
        .route("/v1/open-data-directory", post(open_data_directory))
        .route("/setup/select", post(setup_select))
        .route("/setup/cancel", post(setup_cancel))
        .route("/v1/local-models/music", get(local_music_model_catalog))
        .route("/v1/music/jobs", get(list_music_jobs).post(create_music_job))
        .route("/v1/music/replay", post(replay_music_job))
        .route(
            "/v1/music/jobs/{job_id}",
            get(music_job_status).post(cancel_music_job),
        )
        .with_state(state.clone())
        .layer(DefaultBodyLimit::max(JSON_BODY_LIMIT))
        .layer(middleware::from_fn_with_state(
            session_token,
            security::require_session,
        ))
        .layer(cors);

    // The provider catalog is public and small; reading it once at startup
    // means the settings panel is right the first time it is opened, instead
    // of after the user presses a refresh button.
    // Start the engine as soon as a complete set is installed. It takes about
    // three seconds; making the user press a button for it - or worse, wait
    // without knowing what for - is the studio being lazy on their time.
    // The engine is supervised, not started once and forgotten. It used to be
    // launched a single time at startup, and only if a complete set of weights
    // was already on disk - so a first installation downloaded its models,
    // nothing started them, and the window waited on "loading the models into
    // memory" until the studio was restarted by hand. The same gap swallowed a
    // crashed engine. This watches instead: whenever a complete set is on disk
    // and nothing is answering on the engine port, it brings the engine up.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut complained = false;
            // Whether the engine was up on the last look. Only a fall from up to
            // down is a crash worth a line; an engine that has never started is
            // a startup that is failing, and repeating "stopped answering" every
            // two seconds is what filled a whole log with one sentence.
            let mut was_running = false;
            loop {
                let ready = state.model_manager.status(effective_install_target(&state).await).await.ready;
                let running = state.music_server.health().await;
                if ready && !running {
                    if was_running {
                        // It was answering and now it is not: the one line that
                        // explains a log which suddenly starts again from
                        // "Listening on". Written once, not once a cycle.
                        music_engine::mm_server::note_in_log("the engine stopped answering; restarting it");
                    }
                    match restart_engine(&state).await {
                        Ok(()) => complained = false,
                        Err(error) => {
                            // Say it once per failure, not once every few
                            // seconds: a card with too little memory would
                            // otherwise fill the log with the same line.
                            if !complained {
                                eprintln!("the local engine did not start: {error}");
                                complained = true;
                            }
                        }
                    }
                }
                was_running = running;
                tokio::time::sleep(std::time::Duration::from_secs(if running { 5 } else { 2 })).await;
            }
        });
    }

    let address = listener.local_addr()?;
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
    let config = state.separation_config.read().await.clone();
    let mut set: Vec<&'static lyrics_sync::Asset> = Vec::new();
    if let Some(asset) = lyrics_sync::asset("onnxruntime") { set.push(asset); }
    if !matches!(config.runtime, lyrics_sync::OnnxFlavour::Cpu) {
        set.extend(CARD_ASSETS.iter().filter_map(|id| lyrics_sync::asset(id)));
    }
    let runtime_progress = set_progress(state.lyrics_sync.downloader(), &set);
    let model_installed = state.separator.is_installed();
    let bytes = runtime_progress["bytes"].as_u64().unwrap_or(0) + separation::MODEL.bytes;
    let installed_bytes = runtime_progress["installed_bytes"].as_u64().unwrap_or(0)
        + if model_installed { separation::MODEL.bytes } else { 0 };
    Json(serde_json::json!({
        "assets": assets,
        "settings": { "runtime": config.runtime },
        "set": {
            "bytes": bytes,
            "installed_bytes": installed_bytes,
            "ready": installed_bytes == bytes,
            "files": set.len() + 1,
        },
        // Only this panel's own download. The recogniser shares this
        // downloader, and its gigabytes are not the separator's business.
        "active_download": state.separator.downloader().active_for("separation").await.or(state.lyrics_sync.downloader().active_for("separation").await),
        // Not an error and not a stall: the file server is asking us to wait.
        "waiting_for_server": crate::chunked::waiting_for_server(),
    }))
}

#[derive(Debug, Deserialize)]
struct InstallSeparationAssetRequest {
    asset_id: String,
}

/// Everything the card path needs, in the order it is used. `karaoke_set`
/// builds a recogniser out of these plus its own model files, so this is the
/// one place the CUDA provider's parts are named.
const CARD_ASSETS: [&str; 5] = ["onnxruntime-cuda", "cuda-cudart", "cuda-cublas", "cuda-cufft", "cuda-cudnn"];

/// Stops a download. What arrived stays on disk: pressing this again later
/// carries on from the last finished piece rather than starting the file over.
///
/// Two panels can be downloading at once, and until this existed the only way
/// to stop one of them was to close the studio.
/// Points `ort` at the ONNX Runtime the studio installed, once per process.
///
/// It has to happen before anything touches `ort`, or the library binds to
/// whatever `onnxruntime.dll` the system happens to have - and on Windows a
/// DLL's own dependencies are resolved through the process search path, not
/// through the folder it came from, which is why the directory joins PATH too.
/// Every caller needs this, not only the separator: reading a track through the
/// same runtime without it left the request waiting on a library that was never
/// located.
fn point_ort_at(runtime: &std::path::Path) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let runtime = runtime.to_path_buf();
    ONCE.call_once(|| unsafe {
        std::env::set_var("ORT_DYLIB_PATH", &runtime);
        if let Some(directory) = runtime.parent() {
            let existing = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{};{existing}", directory.display()));
        }
    });
}

async fn cancel_separation_download(State(state): State<AppState>) -> Json<Value> {
    state.separator.downloader().cancel();
    state.lyrics_sync.downloader().cancel();
    Json(serde_json::json!({ "cancelled": true }))
}

async fn cancel_karaoke_download(State(state): State<AppState>) -> Json<Value> {
    state.lyrics_sync.downloader().cancel();
    Json(serde_json::json!({ "cancelled": true }))
}

/// Frees the disk an assistant takes: the model, the runtime and any half of
/// either. Every other capability could be removed from its panel; this one
/// could only be added.
async fn assistant_runtime_remove(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<assistant_runtime::RuntimeStatus>, (StatusCode, Json<ApiError>)> {
    // "managed" from the panel means the whole thing: whichever llama.cpp build
    // is on disk, and the model that was chosen with it.
    let ids: Vec<String> = if request.asset_id == "managed" || request.asset_id == "cuda" || request.asset_id == "cpu" {
        let chosen = state.assistant.read().await.managed_model.clone();
        ["llama-cuda", "llama-cuda-runtime", "llama-cpu"].iter().map(|id| id.to_string()).chain(chosen).collect()
    } else {
        vec![request.asset_id.clone()]
    };
    for id in &ids {
        state
            .assistant_runtime
            .remove(id)
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    }
    {
        let mut assistant = state.assistant.write().await;
        assistant.managed_model = None;
    }
    let _ = persist_studio_settings(&state).await;
    Ok(Json(state.assistant_runtime.status().await))
}

async fn cancel_assistant_download(State(state): State<AppState>) -> Json<Value> {
    state.assistant_runtime.cancel();
    Json(serde_json::json!({ "cancelled": true }))
}

async fn install_separation_asset(
    State(state): State<AppState>,
    Json(request): Json<InstallSeparationAssetRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    // "card" means whatever is still missing for the graphics card, one after
    // another: asking someone to press four buttons in the right order is not a
    // setup, it is a quiz.
    // The separator as one thing: its model, the runtime that loads it, and -
    // for the card - the CUDA provider. Six rows of file names asked the user
    // to work out which of them belong together.
    if matches!(request.asset_id.as_str(), "auto" | "cuda" | "cpu") {
        let card = !matches!(request.asset_id.as_str(), "cpu");
        let separator = state.separator.clone();
        let sync = state.lyrics_sync.clone();
        let mut runtime: Vec<&'static lyrics_sync::Asset> = Vec::new();
        if let Some(asset) = lyrics_sync::asset("onnxruntime") { runtime.push(asset); }
        if card {
            runtime.extend(CARD_ASSETS.iter().filter_map(|id| lyrics_sync::asset(id)));
        }
        tokio::spawn(async move {
            if let Err(error) = separator.downloader().install_all("separation", &[&separation::MODEL]).await {
                eprintln!("the separator model could not be installed: {error}");
                return;
            }
            if let Err(error) = sync.downloader().install_all("separation", &runtime).await {
                eprintln!("the separator runtime could not be installed: {error}");
            }
        });
        return Ok(Json(serde_json::json!({ "started": true })));
    }
    if request.asset_id == "card" {
        let sync = state.lyrics_sync.clone();
        let card: Vec<&'static lyrics_sync::Asset> = CARD_ASSETS.iter().filter_map(|id| lyrics_sync::asset(id)).collect();
        tokio::spawn(async move {
            if let Err(error) = sync.downloader().install_all("separation", &card).await {
                eprintln!("the card path could not be installed: {error}");
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
        point_ort_at(&runtime);

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
        // Whatever the chosen recogniser is made of, by the same reckoning the
        // panel uses. Naming the files here by hand is how this went on asking
        // for `whisper-cuda` after that asset had ceased to exist, and then
        // concluded that nothing was missing and nothing was ready.
        lyrics_sync::AsrProvider::Whisper => karaoke_set("whisper", config.runtime, config.whisper_model.as_deref())
            .into_iter()
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
            karaoke_words_from_openrouter(&state, &config, std::path::Path::new(&audio), None).await
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

async fn create_library_song(State(state):State<AppState>,Json(input):Json<library::SongInput>)->Result<(StatusCode,Json<library::Song>),(StatusCode,Json<ApiError>)>{
    if input.audio_path.is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "audio_path cannot be supplied through the JSON API; upload audio instead".into(),
        ));
    }
    state.library.create_song(input).map(|s|(StatusCode::CREATED,Json(s))).map_err(|e|api_error(StatusCode::BAD_REQUEST,e.to_string()))
}
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
    if input.audio_path.is_some() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "audio_path cannot be supplied through the JSON API; upload audio instead".into(),
        ));
    }
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
    let engine_available = engine_location(*state.engine_options.read().await).bundle_root.is_dir();
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

/// Reports local configuration plus a GET-only OmniBridge contract handshake.
/// It never calls a Provider and it never returns the Gateway credential.
/// Route publication and real capability
/// evidence must still be verified by OmniBridge before paid generation is enabled.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OmniBridgeContractDiagnostic {
    NotChecked,
    Verified,
    Failed(String),
}

fn music_execution_target_name() -> &'static str {
    match music_execution_target() {
        Ok(MusicExecutionTarget::Configuration) => "configuration",
        Ok(MusicExecutionTarget::OmniBridge) => "omnibridge",
        Err(_) => "invalid",
    }
}

fn safe_omnibridge_diagnostic_error(error: &omnibridge::OmniBridgeError) -> String {
    match error {
        omnibridge::OmniBridgeError::NotReady(name) => {
            format!("OmniBridge configuration is incomplete: {name} is missing.")
        }
        omnibridge::OmniBridgeError::InvalidInput(message)
        | omnibridge::OmniBridgeError::Protocol(message) => {
            security::redact_secrets(message)
        }
        omnibridge::OmniBridgeError::Transport(_) => {
            "OmniBridge contract endpoint is unreachable.".to_owned()
        }
        omnibridge::OmniBridgeError::RateLimited(delay_ms) => {
            format!("OmniBridge contract check was rate limited; retry after {delay_ms} ms.")
        }
        omnibridge::OmniBridgeError::HttpStatus(status) => {
            format!("OmniBridge contract endpoint returned HTTP {status}.")
        }
        omnibridge::OmniBridgeError::SubmissionUnknown(_) => {
            "OmniBridge contract check returned an unknown outcome.".to_owned()
        }
        omnibridge::OmniBridgeError::Integrity(_) => {
            "OmniBridge contract check returned invalid integrity metadata.".to_owned()
        }
        omnibridge::OmniBridgeError::NotCancellable => {
            "OmniBridge contract check is unavailable.".to_owned()
        }
        omnibridge::OmniBridgeError::Io(_) => {
            "OmniBridge local integration state is unavailable.".to_owned()
        }
    }
}

fn durable_record_matches_music_route(
    record: &omnibridge::DurableMusicRecord,
    music_route: &str,
) -> bool {
    let stored_route = record
        .context()
        .and_then(|context| context.generation_settings.get("model"))
        .and_then(Value::as_str);
    stored_route == Some(music_route)
        && matches!(
            record.submit_state(),
            omnibridge::DurableSubmitState::Accepted
        )
        && matches!(
            record.status(),
            Some("completed") | Some("succeeded") | Some("done")
        )
        && record.artifact().is_some()
        && record.imported_song_id().is_some()
}

fn omnibridge_route_generation_verified(
    records: &[omnibridge::DurableMusicRecord],
    music_route: &str,
) -> bool {
    records
        .iter()
        .any(|record| durable_record_matches_music_route(record, music_route))
}

fn omnibridge_integration_status_payload(
    config: Option<&omnibridge::OmniBridgeConfig>,
    configuration_error: Option<String>,
    contract: OmniBridgeContractDiagnostic,
    execution_target: &str,
    real_generation_verified: bool,
) -> Value {
    let configured = config.is_some();
    let (diagnostic_status, contract_status, contract_verified, contract_error) = match contract {
        OmniBridgeContractDiagnostic::NotChecked if configured => {
            ("configured", "not_checked", false, None)
        }
        OmniBridgeContractDiagnostic::NotChecked => {
            ("not_configured", "not_checked", false, None)
        }
        OmniBridgeContractDiagnostic::Verified => {
            ("contract_verified", "verified", true, None)
        }
        OmniBridgeContractDiagnostic::Failed(error) => {
            ("contract_failed", "failed", false, Some(error))
        }
    };
    let mut payload = serde_json::json!({
        "schema": "music-maker.omnibridge-integration-status.v2",
        "configured": configured,
        "diagnostic_status": diagnostic_status,
        "contract_status": contract_status,
        "contract_verified": contract_verified,
        "contract_client": "temporary-rust-adapter",
        "execution_target": execution_target,
        "operation": omnibridge::OmniBridgeConfig::operation(),
        "kind": omnibridge::OmniBridgeConfig::kind(),
        "route_readiness": if real_generation_verified {
            "ready"
        } else if configured {
            "unverified"
        } else {
            "not_ready"
        },
        "route_resolution_verified": real_generation_verified,
        "provider_resolution_verified": real_generation_verified,
        "real_generation_verified": real_generation_verified,
    });
    if let Some(config) = config {
        payload["music_route"] = Value::String(config.music_route.clone());
    }
    if let Some(error) = contract_error.or(configuration_error) {
        payload["error"] = Value::String(security::redact_secrets(error));
    }
    payload
}

async fn model_bindings(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    state.model_port.bindings_payload().await.map(Json).map_err(|error| {
        api_error(StatusCode::INTERNAL_SERVER_ERROR, security::redact_secrets(error))
    })
}

async fn preview_model_bindings(
    State(state): State<AppState>,
    Json(request): Json<model_port::PreviewProfileRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let profile = match request.profile {
        Some(profile) => profile,
        None => state.model_port.profile().map_err(|error| {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, security::redact_secrets(error))
        })?,
    };
    state.model_port.validate_with_hub(&profile).await.map(Json).map_err(|error| {
        api_error(StatusCode::BAD_GATEWAY, security::redact_secrets(error))
    })
}

async fn update_model_bindings(
    State(state): State<AppState>,
    Json(request): Json<model_port::SaveProfileRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let preview = state.model_port.validate_with_hub(&request.profile).await.map_err(|error| {
        api_error(StatusCode::UNPROCESSABLE_ENTITY, security::redact_secrets(error))
    })?;
    let profile = state.model_port.save(request).map_err(|error| {
        let status = if error.starts_with("profile revision conflict") {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        };
        api_error(status, security::redact_secrets(error))
    })?;
    Ok(Json(serde_json::json!({
        "schema": "music-maker.model-bindings-save.v1",
        "profile": profile,
        "preview": preview,
    })))
}

async fn omnibridge_integration_status(State(state): State<AppState>) -> Json<Value> {
    let execution_target = music_execution_target_name();
    let music_route = match state.model_port.music_route() {
        Ok(route) => route,
        Err(error) => {
            return Json(omnibridge_integration_status_payload(None, Some(error), OmniBridgeContractDiagnostic::NotChecked, execution_target, false));
        }
    };
    let config = match omnibridge::OmniBridgeConfig::from_env_with_route(music_route) {
        Ok(config) => config,
        Err(error) => {
            return Json(omnibridge_integration_status_payload(
                None,
                Some(safe_omnibridge_diagnostic_error(&error)),
                OmniBridgeContractDiagnostic::NotChecked,
                execution_target,
                false,
            ));
        }
    };
    let contract = match omnibridge::OmniBridgeMusicClient::new(config.clone()) {
        Ok(client) => match client.verify_contracts().await {
            Ok(()) => OmniBridgeContractDiagnostic::Verified,
            Err(error) => OmniBridgeContractDiagnostic::Failed(
                safe_omnibridge_diagnostic_error(&error),
            ),
        },
        Err(error) => OmniBridgeContractDiagnostic::Failed(
            safe_omnibridge_diagnostic_error(&error),
        ),
    };
    let (real_generation_verified, store_error) = {
        let store = state.omnibridge_store.lock().await;
        match store.list() {
            Ok(records) => (
                omnibridge_route_generation_verified(&records, &config.music_route),
                None,
            ),
            Err(error) => (
                false,
                Some(safe_omnibridge_diagnostic_error(&error)),
            ),
        }
    };
    Json(omnibridge_integration_status_payload(
        Some(&config),
        store_error,
        contract,
        execution_target,
        real_generation_verified,
    ))
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
    let config = engine_location(options)
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
fn engine_bundle_root() -> PathBuf {
    env::var_os("MINIMAX_MM_SERVER_ROOT")
        .map(PathBuf::from)
        .or_else(|| env::var_os("MINIMAX_MM_SERVER_BIN").map(PathBuf::from).and_then(|path| path.parent().map(std::path::Path::to_path_buf)))
        .or_else(|| std::env::current_exe().ok().and_then(|path| path.parent().map(|parent| parent.join("resources").join("minimaxmusic-cpp"))))
        .unwrap_or_else(|| PathBuf::from("resources/minimaxmusic-cpp"))
}

fn engine_location(options: EngineOptions) -> music_engine::mm_server::MmServerLocation {
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
/// The catalogue, and a fresh one when the model asked about is not in it.
///
/// The record is what the request is built from - the model's own parameters,
/// the efforts it accepts - so a catalogue that predates the model would have
/// the studio guessing about a model OpenRouter can describe exactly.
async fn catalog_describing(state: &AppState, model: &str) -> Result<providers::openrouter::CapabilityCatalog, String> {
    let catalog = catalog_for(state).await?;
    if model.is_empty() || catalog.models.iter().any(|entry| entry.id == model) {
        return Ok(catalog);
    }
    {
        let mut cached = state.openrouter_catalog.write().await;
        cached.catalog = None;
    }
    catalog_for(state).await
}

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
    // Every cloud request passes through here, so this is where they are all
    // written down: which model, how long, and what came back when it was not
    // a success.
    let what = authenticated.request.path.trim_matches('/');
    let model = authenticated
        .request
        .body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    request_log::asked(what, &model, authenticated.request.body.to_string().chars().count());
    let started = std::time::Instant::now();
    let response = reqwest::Client::new()
        .post(format!("{}{}", providers::openrouter::API_BASE_URL, authenticated.request.path))
        .bearer_auth(authenticated.api_key)
        .json(&authenticated.request.body)
        .send()
        .await
        .inspect_err(|error| request_log::failed(what, &model, &error.to_string()))?;
    let status = response.status();
    let response = match response.error_for_status_ref() {
        Ok(_) => response,
        Err(error) => {
            let text = response.text().await.unwrap_or_default();
            request_log::answered(what, &model, status.as_u16(), started.elapsed().as_secs_f64(), text.chars().count());
            request_log::unusable(what, &model, &error.to_string(), &text);
            return Err(error.into());
        }
    };
    let generation_id = response
        .headers()
        .get("X-Generation-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.json::<Value>().await?;
    request_log::answered(what, &model, status.as_u16(), started.elapsed().as_secs_f64(), body.to_string().chars().count());
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
    State(_state): State<AppState>,
    axum::extract::Query(request): axum::extract::Query<ProxyImageRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ApiError>)> {
    let (content_type, bytes) = security::fetch_public_image(&request.url)
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error.to_string()))?;
    Ok(axum::response::Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, bytes.len())
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "private, max-age=300")
        .body(Body::from(bytes.to_vec()))
        .expect("valid image response"))
}

async fn assistant_status(State(state): State<AppState>) -> Json<Value> {
    let config = state.assistant.read().await.clone();
    let runtime = state.assistant_runtime.status().await;
    let text_config = state.model_port.role_route("song_package_draft").ok()
        .and_then(|route| omnibridge::OmniBridgeTextConfig::from_env_with_route(route).ok());
    let cloud_available = text_config.is_some();
    let cloud = text_config.as_ref().map(|managed| managed.public_status());
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
        AssistantProvider::OmniBridge => cloud_available,
        _ => config.available(),
    };
    Json(serde_json::json!({
        "available": available,
        "cloud_available": cloud_available,
        "cloud": cloud,
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

/// Every field optional, so the download page can set the provider without
/// blanking the model, the path and the reasoning effort it knows nothing of.
#[derive(Debug, Deserialize)]
struct AssistantSettingsRequest {
    provider: Option<AssistantProvider>,
    local_base_url: Option<Option<String>>,
    local_model: Option<Option<String>>,
    openrouter_model: Option<Option<String>>,
    managed_model: Option<Option<String>>,
    managed_path: Option<Option<String>>,
    reasoning_effort: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
struct LocalModelsQuery {
    base: String,
}

/// The models an OpenAI-compatible server the user runs offers, fetched through
/// the studio so the browser is never asked to reach another origin itself.
///
/// LM Studio, llama-server, Ollama's OpenAI shim - all answer `GET <base>/models`
/// with `{ "data": [ { "id": ... } ] }`. Typing the model name by hand, which
/// is what this replaces, meant a typo read as a server that answered nothing.
async fn assistant_local_models(
    axum::extract::Query(query): axum::extract::Query<LocalModelsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let base = query.base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "no server address".into()));
    }
    let url = format!("{base}/models");
    let response = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("the server at {base} did not answer: {error}")))?;
    if !response.status().is_success() {
        return Err(api_error(StatusCode::BAD_GATEWAY, format!("{url} answered {}", response.status())));
    }
    let body = response
        .json::<Value>()
        .await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("{url} did not return JSON: {error}")))?;
    let models: Vec<String> = body
        .get("data")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(Json(serde_json::json!({ "models": models })))
}

async fn update_assistant_settings(
    State(state): State<AppState>,
    Json(incoming): Json<AssistantSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let request = {
        let current = state.assistant.read().await.clone();
        AssistantConfig {
            provider: incoming.provider.unwrap_or(current.provider),
            local_base_url: incoming.local_base_url.unwrap_or(current.local_base_url),
            local_model: incoming.local_model.unwrap_or(current.local_model),
            openrouter_model: incoming.openrouter_model.unwrap_or(current.openrouter_model),
            managed_model: incoming.managed_model.unwrap_or(current.managed_model),
            managed_path: incoming.managed_path.unwrap_or(current.managed_path),
            reasoning_effort: incoming.reasoning_effort.unwrap_or(current.reasoning_effort),
        }
    };
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
    /// The model to install along with the runtime, when the capability has a
    /// choice of them. A runtime with no model does nothing.
    #[serde(default)]
    model_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AssistantModelRequest {
    #[serde(default)]
    model_id: String,
    /// A GGUF already on this machine, used instead of a downloaded one.
    #[serde(default)]
    model_path: Option<String>,
}

/// The runtime, and the choice it belongs to.
///
/// The panel reads this one address to draw itself, and the chosen engine is
/// kept with the assistant's settings rather than with its files - so without
/// it here the tabs came back to the first one on every open, whatever the user
/// had picked.
async fn assistant_runtime_status(State(state): State<AppState>) -> Json<Value> {
    let status = state.assistant_runtime.status().await;
    let provider = state.assistant.read().await.provider;
    let mut value = serde_json::to_value(&status).unwrap_or(Value::Null);
    let cloud_available = state.model_port.cloud_configured();
    let chosen = state.assistant.read().await.managed_model.clone();
    if let Value::Object(ref mut fields) = value {
        fields.insert("provider".into(), serde_json::to_value(provider).unwrap_or(Value::Null));
        // And which model, so the dropdown reopens on the one that was picked
        // rather than on whichever happens to be installed first.
        fields.insert("chosen_model".into(), serde_json::to_value(chosen).unwrap_or(Value::Null));
        fields.insert("cloud_available".into(), Value::Bool(cloud_available));
        fields.insert("available".into(), Value::Bool(cloud_available || status.assets.iter().any(|asset| asset.installed)));
    }
    Json(value)
}

/// Starts one download. Nothing is fetched until this is called, and an
/// interrupted file resumes where it stopped.
/// The llama.cpp build for a device, with the CUDA libraries it needs.
///
/// The card build is useless without its runtime companion - two downloads
/// that are one decision, the same way a recogniser is.
fn assistant_set(device: &str) -> Vec<&'static str> {
    match device {
        "cpu" => vec!["llama-cpu"],
        _ => vec!["llama-cuda", "llama-cuda-runtime"],
    }
}

async fn assistant_runtime_install(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<assistant_runtime::RuntimeStatus>, (StatusCode, Json<ApiError>)> {
    let set = match request.asset_id.as_str() {
        "auto" | "cuda" | "cpu" => assistant_set(&request.asset_id),
        _ => Vec::new(),
    };
    if !set.is_empty() {
        // Downloading a model is choosing it. Nothing else recorded which one,
        // so a freshly installed Gemma left the studio reporting that no
        // assistant was set up - with the model sitting on the disk.
        if let Some(model) = request.model_id.clone() {
            let mut assistant = state.assistant.write().await;
            assistant.managed_model = Some(model);
            if assistant.provider == AssistantProvider::None {
                assistant.provider = AssistantProvider::Managed;
            }
        }
        let _ = persist_studio_settings(&state).await;
        let runtime = state.assistant_runtime.clone();
        // The whole thing - runtime, CUDA libraries, model - as one download.
        // Starting them one after another only looked like a queue: each call
        // returned before its file had arrived, so the next one was refused and
        // the model, always last, was never fetched at all.
        let ids: Vec<String> = set
            .into_iter()
            .map(str::to_string)
            .chain(request.model_id.clone())
            .collect();
        tokio::spawn(async move {
            if let Err(error) = runtime.install_all(&ids).await {
                eprintln!("the assistant could not be installed: {error}");
            }
        });
        return Ok(Json(state.assistant_runtime.status().await));
    }
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

/// What the chosen engine would install: its files, their weight, and how much
/// of it is already here.
fn set_progress(downloader: &crate::downloads::Downloader, set: &[&'static lyrics_sync::Asset]) -> Value {
    let total: u64 = set.iter().map(|asset| asset.bytes).sum();
    let installed: u64 = set.iter().filter(|asset| downloader.is_installed(asset)).map(|asset| asset.bytes).sum();
    serde_json::json!({
        "bytes": total,
        "installed_bytes": installed,
        "ready": !set.is_empty() && installed == total,
        "files": set.len(),
    })
}

async fn karaoke_status(State(state): State<AppState>) -> Json<Value> {
    let config = state.lyrics_sync_config.read().await.clone();
    let status = state.lyrics_sync.status(&config).await;
    let name = match config.provider {
        lyrics_sync::AsrProvider::Whisper => "whisper",
        _ => "parakeet",
    };
    let set = karaoke_set(name, config.runtime, config.whisper_model.as_deref());
    let mut value = serde_json::to_value(&status).unwrap_or(Value::Null);
    if let Value::Object(ref mut fields) = value {
        fields.insert("set".into(), set_progress(state.lyrics_sync.downloader(), &set));
    }
    Json(value)
}

/// Every field optional, so a panel that changes one thing changes one thing.
///
/// This took the whole configuration before: the download page, which knows
/// only which recogniser and which device were picked, would have blanked the
/// switch and both model choices by sending them absent.
#[derive(Debug, Deserialize)]
struct KaraokeSettingsRequest {
    enabled: Option<bool>,
    provider: Option<lyrics_sync::AsrProvider>,
    whisper_model: Option<Option<String>>,
    openrouter_model: Option<Option<String>>,
    runtime: Option<lyrics_sync::OnnxFlavour>,
}

async fn update_karaoke_settings(
    State(state): State<AppState>,
    Json(request): Json<KaraokeSettingsRequest>,
) -> Result<Json<lyrics_sync::SyncStatus>, (StatusCode, Json<ApiError>)> {
    let merged = {
        let mut config = state.lyrics_sync_config.write().await;
        if let Some(enabled) = request.enabled { config.enabled = enabled; }
        if let Some(provider) = request.provider { config.provider = provider; }
        if let Some(model) = request.whisper_model { config.whisper_model = model; }
        if let Some(model) = request.openrouter_model { config.openrouter_model = model; }
        if let Some(runtime) = request.runtime { config.runtime = runtime; }
        config.clone()
    };
    let _ = persist_studio_settings(&state).await;
    Ok(Json(state.lyrics_sync.status(&merged).await))
}

/// Frees the disk a karaoke recogniser takes.
/// Removes a recogniser the same way it was installed: whole.
async fn karaoke_remove(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<lyrics_sync::SyncStatus>, (StatusCode, Json<ApiError>)> {
    let config = state.lyrics_sync_config.read().await.clone();
    let set = karaoke_set(&request.asset_id, config.runtime, config.whisper_model.as_deref());
    if !set.is_empty() {
        for asset in set {
            let _ = state.lyrics_sync.downloader().remove(asset);
        }
        return Ok(Json(state.lyrics_sync.status(&config).await));
    }
    let asset = lyrics_sync::asset(&request.asset_id)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, format!("unknown karaoke asset: {}", request.asset_id)))?;
    state
        .lyrics_sync
        .downloader()
        .remove(asset)
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
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

/// Installs a recogniser, not a file.
///
/// Parakeet is six downloads and Whisper is two, and which two depends on the
/// card. Asking a person to work that out from a list of file names - and to
/// notice that one of them is a runtime - is not a setup screen, it is a quiz.
/// The whole set is named here, in the order it is used.
fn karaoke_set(name: &str, device: lyrics_sync::OnnxFlavour, whisper_model: Option<&str>) -> Vec<&'static lyrics_sync::Asset> {
    let mut wanted: Vec<String> = Vec::new();
    match name {
        "parakeet" => {
            wanted.push("onnxruntime".into());
            if !matches!(device, lyrics_sync::OnnxFlavour::Cpu) {
                wanted.extend(CARD_ASSETS.map(String::from));
            }
            // The precision is chosen the same way a Whisper model is: through
            // the dropdown, which names one of the two encoders.
            if whisper_model.is_some_and(|id| id.contains("fp32")) {
                wanted.extend(lyrics_sync::PARAKEET_FP32_ASSET_IDS.map(String::from));
            } else {
                wanted.extend(lyrics_sync::PARAKEET_ASSET_IDS.map(String::from));
            }
        }
        "whisper" => {
            // One binary whichever device is chosen; the card needs CUDA 11's
            // libraries beside it, and without them CTranslate2 silently uses
            // the processor instead of saying so.
            wanted.push("whisper-engine".into());
            if !matches!(device, lyrics_sync::OnnxFlavour::Cpu) {
                wanted.push("whisper-cublas".into());
                wanted.push("whisper-cudnn".into());
            }
            // A model is a directory of files, and it is useless one file
            // short, so the whole set goes together.
            let chosen = whisper_model.unwrap_or("whisper-large-v3-turbo");
            if let Some(size) = chosen.strip_prefix("whisper-") {
                let prefix = format!("models/whisper/faster-whisper-{size}/");
                wanted.extend(
                    lyrics_sync::ASSETS
                        .iter()
                        .filter(|asset| asset.relative_path.starts_with(&prefix))
                        .map(|asset| asset.id.to_string()),
                );
            }
        }
        _ => {}
    }
    wanted.iter().filter_map(|id| lyrics_sync::asset(id)).collect()
}

async fn karaoke_install(
    State(state): State<AppState>,
    Json(request): Json<AssistantAssetRequest>,
) -> Result<Json<lyrics_sync::SyncStatus>, (StatusCode, Json<ApiError>)> {
    let config = state.lyrics_sync_config.read().await.clone();
    let set = karaoke_set(
        &request.asset_id,
        config.runtime,
        request.model_id.as_deref().or(config.whisper_model.as_deref()),
    );
    if !set.is_empty() {
        // In the background, so the panel keeps answering while half a
        // gigabyte arrives; the whole set is one button, not eight.
        let sync = state.lyrics_sync.clone();
        tokio::spawn(async move {
            if let Err(error) = sync.downloader().install_all("karaoke", &set).await {
                eprintln!("the karaoke recogniser could not be installed: {error}");
            }
        });
        return Ok(Json(state.lyrics_sync.status(&config).await));
    }

    let asset = lyrics_sync::asset(&request.asset_id)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, format!("unknown karaoke asset: {}", request.asset_id)))?;
    state
        .lyrics_sync
        .downloader()
        .install(asset)
        .await
        .map_err(|error| api_error(StatusCode::CONFLICT, error.to_string()))?;
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
    let audio = state
        .library
        .media_path_for_song(&song)
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
            tokio::task::spawn_blocking(move || sync.parakeet_words(&audio))
                .await
                .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        }
        lyrics_sync::AsrProvider::Whisper => {
            let sync = state.lyrics_sync.clone();
            let config = config.clone();
            let path = audio;
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
    audio: &std::path::Path,
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
    let format = audio
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

fn assistant_done_event(
    draft: assistant::AssistDraft,
    text: String,
    receipt: Option<omnibridge::RequestReceipt>,
) -> Value {
    serde_json::json!({
        "stage": "done",
        "draft": draft,
        "text": text,
        "receipt": receipt,
    })
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

        if config.provider == AssistantProvider::OmniBridge {
            let role_id = model_port::ModelPort::assistant_role(&request);
            let route_id = match state.model_port.role_route(role_id) {
                Ok(route) => route,
                Err(error) => {
                    emit(sender.clone(), serde_json::json!({ "error": error, "role": role_id })).await;
                    return;
                }
            };
            let client = match omnibridge::OmniBridgeTextClient::from_env_with_route(&route_id) {
                Ok(client) => client,
                Err(error) => {
                    emit(sender.clone(), serde_json::json!({ "error": error.to_string() })).await;
                    return;
                }
            };
            let body = assistant::chat_body_full(
                &route_id,
                &system,
                &user,
                config.reasoning_effort.as_deref(),
                None,
            );
            emit(
                sender.clone(),
                serde_json::json!({ "stage": "sent", "route": route_id }),
            )
            .await;
            request_log::asked(
                "assistant",
                &route_id,
                system.chars().count() + user.chars().count(),
            );
            let started = std::time::Instant::now();
            let streamed = match client.stream_route_once(&route_id, &body).await {
                Ok(streamed) => streamed,
                Err(error) => {
                    request_log::failed("assistant", &route_id, &error.to_string());
                    emit(sender.clone(), serde_json::json!({ "error": error.to_string() })).await;
                    return;
                }
            };
            let receipt = streamed.receipt.clone();
            let mut response = streamed.into_response().bytes_stream();
            let mut buffer = String::new();
            let mut whole = String::new();
            let mut first = true;
            while let Some(chunk) = response.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        request_log::failed("assistant", &route_id, &error.to_string());
                        emit(
                            sender.clone(),
                            serde_json::json!({
                                "error": format!("OmniBridge assistant stream failed: {error}"),
                                "receipt": receipt,
                            }),
                        )
                        .await;
                        return;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim().to_owned();
                    buffer.drain(..line_end + 1);
                    let Some(payload) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let payload = payload.trim();
                    if payload.is_empty() || payload == "[DONE]" {
                        continue;
                    }
                    let event: Value = match serde_json::from_str(payload) {
                        Ok(event) => event,
                        Err(_) => continue,
                    };
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
                        request_log::answered(
                            "assistant first token",
                            &route_id,
                            200,
                            started.elapsed().as_secs_f64(),
                            0,
                        );
                        emit(
                            sender.clone(),
                            serde_json::json!({ "stage": "writing", "receipt": receipt }),
                        )
                        .await;
                    }
                    whole.push_str(delta);
                    emit(sender.clone(), serde_json::json!({ "delta": delta })).await;
                }
            }
            request_log::answered(
                "assistant",
                &route_id,
                200,
                started.elapsed().as_secs_f64(),
                whole.chars().count(),
            );
            let draft = match assistant::parse_draft(&whole, &required) {
                Ok(draft) => draft,
                Err(error) => {
                    request_log::unusable("assistant", &route_id, &error.to_string(), &whole);
                    emit(
                        sender.clone(),
                        serde_json::json!({
                            "error": error.to_string(),
                            "text": whole,
                            "receipt": receipt,
                        }),
                    )
                    .await;
                    return;
                }
            };
            emit(sender.clone(), assistant_done_event(draft, whole, receipt)).await;
            return;
        }

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
        // What the model publishes for itself, exactly as the non-streaming
        // path uses it. Passing nothing here meant every streamed request went
        // out with the studio's own temperature on top of models that had
        // stated their own - a different request from the one the catalogue
        // describes, and the streamed path is the one the window uses.
        let entry = if matches!(config.provider, AssistantProvider::OpenRouter) {
            catalog_describing(&state, &model)
                .await
                .ok()
                .and_then(|catalog| catalog.models.iter().find(|item| item.id == model).cloned())
        } else {
            None
        };
        let published = entry.as_ref().map(|entry| serde_json::to_value(&entry.defaults).unwrap_or(Value::Null));
        // Thinking on the model's own terms: it publishes which efforts it
        // takes, which one it prefers, and whether it can be asked not to
        // think at all. A setting of ours that is not on its list becomes the
        // one it named, because naming an unknown effort is refused outright.
        let effort = match (&entry, config.provider) {
            (Some(entry), AssistantProvider::OpenRouter) => entry
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.effort_for(config.reasoning_effort.as_deref())),
            (_, AssistantProvider::OpenRouter) => None,
            _ => config.reasoning_effort.clone(),
        };
        let mut body = assistant::chat_body_constrained(&model, &system, &user, effort.as_deref(), published.as_ref(), schema);
        body["stream"] = Value::Bool(true);

        emit(sender.clone(), serde_json::json!({ "stage": "sent", "model": model })).await;
        request_log::asked("assistant", &model, system.chars().count() + user.chars().count());
        let started = std::time::Instant::now();

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
                request_log::failed("assistant", &model, &error.to_string());
                emit(sender.clone(), serde_json::json!({ "error": format!("the assistant is unreachable: {error}") })).await;
                return;
            }
        };
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            request_log::answered("assistant", &model, status.as_u16(), started.elapsed().as_secs_f64(), text.chars().count());
            request_log::unusable("assistant", &model, &format!("http {status}"), &text);
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
                    // The moment the model started answering. Without it a log
                    // of a run that never came back cannot say whether it was
                    // thinking or simply gone.
                    request_log::answered("assistant first token", &model, 200, started.elapsed().as_secs_f64(), 0);
                    emit(sender.clone(), serde_json::json!({ "stage": "writing" })).await;
                }
                whole.push_str(delta);
                emit(sender.clone(), serde_json::json!({ "delta": delta })).await;
            }
        }

        request_log::answered("assistant", &model, 200, started.elapsed().as_secs_f64(), whole.chars().count());
        // The answer is kept whenever it cannot be turned into a draft. That is
        // the case this log exists for: the window shows one red line, and
        // without this the text behind it is gone the moment it is closed.
        let draft = match assistant::parse_draft(&whole, &required) {
            Ok(draft) => draft,
            Err(error) => {
                request_log::unusable("assistant", &model, &error.to_string(), &whole);
                emit(
                    sender.clone(),
                    serde_json::json!({ "error": error.to_string(), "text": whole }),
                )
                .await;
                release_assistant_unless_kept(&state).await;
                return;
            }
        };
        emit(sender.clone(), assistant_done_event(draft, whole, None)).await;
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
        AssistantProvider::Local | AssistantProvider::Managed => {
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
        AssistantProvider::OmniBridge => {
            let role_id = model_port::ModelPort::assistant_role(&request);
            let route_id = state.model_port.role_route(role_id).map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
            let client = omnibridge::OmniBridgeTextClient::from_env_with_route(&route_id)
                .map_err(|error| api_error(StatusCode::BAD_GATEWAY, error.to_string()))?;
            let body = assistant::chat_body_full(
                &route_id,
                &system,
                &user,
                config.reasoning_effort.as_deref(),
                None,
            );
            let (response, _receipt) = client
                .complete_route_once(&route_id, &body)
                .await
                .map_err(|error| {
                    api_error(
                        StatusCode::BAD_GATEWAY,
                        format!("OmniBridge assistant failed: {error}"),
                    )
                })?;
            response
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
                body: {
                    let entry = catalog_now.as_ref().and_then(|catalog| catalog.models.iter().find(|entry| entry.id == model));
                    let effort = entry
                        .and_then(|entry| entry.reasoning.as_ref())
                        .and_then(|reasoning| reasoning.effort_for(config.reasoning_effort.as_deref()));
                    assistant::chat_body_full(
                        &model,
                        &system,
                        &user,
                        effort.as_deref(),
                        entry.map(|entry| serde_json::to_value(&entry.defaults).unwrap_or(Value::Null)).as_ref(),
                    )
                },
            })
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
            execute_openrouter_json(authenticated.request)
                .await
                .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter assistant failed: {error}")))?
                .body
        }
        AssistantProvider::None => {
            return Err(api_error(
                StatusCode::CONFLICT,
                "No writing assistant is configured. The manual form does not need one.".into(),
            ));
        }
    };

    release_assistant_unless_kept(&state).await;
    let content = assistant::content_of(&response).map_err(|error| {
        request_log::unusable("assistant", "", &error.to_string(), &response.to_string());
        api_error(StatusCode::BAD_GATEWAY, error.to_string())
    })?;
    let draft = assistant::parse_draft(&content, required).map_err(|error| {
        // The answer, kept: this is the difference between "invalid JSON" and
        // seeing that the model wrote an apology instead of a song.
        request_log::unusable("assistant", "", &error.to_string(), &content);
        api_error(StatusCode::BAD_GATEWAY, error.to_string())
    })?;
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

/// What was asked of the cloud and what came back, newest last.
///
/// A failed draft used to leave one red line and nothing behind it; this is
/// where the answer itself is kept, so a model that wrote almost the right
/// thing can be told from one that wrote nothing.
async fn openrouter_logs() -> Json<Value> {
    Json(serde_json::json!({ "path": request_log::path().display().to_string(), "lines": request_log::tail(400) }))
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

/// Takes models the user already has instead of downloading them again.
///
/// Anyone who has run Music3 through ComfyUI or another build already has these
/// weights on disk, and they are gigabytes each. This opens a folder picker,
/// looks for the files the catalogue names - by name, then by matching size -
/// and hard-links or copies them into the studio's own model directory.
#[cfg(any(target_os = "windows", target_os = "macos"))]
async fn setup_adopt(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let Some(models_root) = studio_data_root().map(|root| root.join("models").join(model_manager::ENGINE_ID)) else {
        return Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, "the studio has no data directory".into()));
    };
    let catalog = state.model_manager.catalog();
    let picked = tokio::task::spawn_blocking(move || {
        rfd::FileDialog::new().set_title("Folder with Music3 models").pick_folder()
    })
    .await
    .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let Some(folder) = picked else {
        return Ok(Json(serde_json::json!({ "picked": false, "adopted": [] })));
    };

    let _ = std::fs::create_dir_all(&models_root);
    let mut adopted: Vec<String> = Vec::new();
    let entries: Vec<std::path::PathBuf> = std::fs::read_dir(&folder)
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();

    for component in &catalog.components {
        let target = models_root.join(component.filename);
        if target.is_file() {
            continue;
        }
        // The name first, because that is unambiguous; then the exact size,
        // because other builds rename the same file.
        let source = entries
            .iter()
            .find(|path| path.file_name().is_some_and(|name| name == component.filename))
            .or_else(|| {
                entries.iter().find(|path| {
                    std::fs::metadata(path).map(|meta| meta.len() == component.bytes).unwrap_or(false)
                })
            });
        let Some(source) = source else { continue };
        // A hard link costs nothing and keeps one copy on disk; a folder on
        // another drive cannot have one, so that falls back to a copy.
        if std::fs::hard_link(source, &target).is_err() && std::fs::copy(source, &target).is_err() {
            continue;
        }
        adopted.push(component.id.to_string());
    }

    let target = effective_install_target(&state).await;
    let status = state.model_manager.status(target).await;
    Ok(Json(serde_json::json!({
        "picked": true,
        "folder": folder.display().to_string(),
        "adopted": adopted,
        "status": compose_setup_status(&state, status).await,
    })))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
async fn setup_adopt(
    State(_state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    Err(api_error(
        StatusCode::NOT_IMPLEMENTED,
        "setup/adopt folder picking is only supported on Windows and macOS desktop builds; Linux/headless servers can use /setup/download or place models in the configured data directory.".into(),
    ))
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

async fn create_music_job(
    State(state): State<AppState>,
    Json(request): Json<CreateMusicJobRequest>,
) -> (StatusCode, Json<MusicJob>) {
    match requested_music_execution_target(&request) {
        Ok(MusicExecutionTarget::OmniBridge) => {
            return create_omnibridge_music_job(state, request).await;
        }
        Ok(MusicExecutionTarget::Configuration) => {}
        Err(error) => {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(failed_request_job(request, "configuration".into(), error)));
        }
    }
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
    match state.music_server.submit(mm_request.clone()).await {
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

async fn list_music_jobs(State(state): State<AppState>) -> Json<Vec<MusicJob>> {
    let mut jobs = state.jobs.read().await.values().cloned().collect::<Vec<_>>();
    jobs.sort_by(|left, right| left.id.cmp(&right.id));
    Json(jobs)
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
    let remote = state
        .music_server
        .submit(synth_request.clone())
        .await
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, format!("the engine refused the job: {error}")))?;
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

fn restore_omnibridge_jobs(
    store: &omnibridge::OmniBridgeMusicStore,
) -> Result<HashMap<String, MusicJob>, omnibridge::OmniBridgeError> {
    let mut jobs = HashMap::new();
    for mut record in store.list()? {
        if matches!(record.submit_state(), omnibridge::DurableSubmitState::IntentPersisted)
            && record.task_handle().is_none()
        {
            record = store.mark_submission_unknown(record.local_job_id())?;
        }
        if let Some(job) = music_job_from_durable_record(&record) {
            jobs.insert(job.id.clone(), job);
        }
    }
    Ok(jobs)
}

fn music_job_from_durable_record(record: &omnibridge::DurableMusicRecord) -> Option<MusicJob> {
    let context = record.context()?.clone();
    let (status, phase, message) = match record.submit_state() {
        omnibridge::DurableSubmitState::IntentPersisted
        | omnibridge::DurableSubmitState::SubmissionUnknown => (
            MusicJobStatus::Unknown,
            MusicJobPhase::SubmissionUnknown,
            "OmniBridge submission outcome is unknown. This job will not be submitted again automatically.".to_owned(),
        ),
        omnibridge::DurableSubmitState::Rejected => (
            MusicJobStatus::Failed,
            MusicJobPhase::Failed,
            "OmniBridge rejected this job before returning a task handle.".to_owned(),
        ),
        omnibridge::DurableSubmitState::Accepted => match record.status().unwrap_or("queued") {
            "completed" | "succeeded" | "done" if record.imported_song_id().is_some() => (
                MusicJobStatus::Completed, MusicJobPhase::Completed, "OmniBridge artifact was imported into the library.".to_owned(),
            ),
            "failed" | "dead_letter" => (MusicJobStatus::Failed, MusicJobPhase::Failed, "OmniBridge reported a failed job.".to_owned()),
            "cancelled" => (MusicJobStatus::Cancelled, MusicJobPhase::Cancelled, "OmniBridge reported a cancelled job.".to_owned()),
            "running" | "processing" => (MusicJobStatus::Running, MusicJobPhase::Running, "OmniBridge is processing this job.".to_owned()),
            _ => (MusicJobStatus::Queued, MusicJobPhase::Queued, "OmniBridge accepted this job; recovery is GET-only.".to_owned()),
        },
    };
    Some(MusicJob {
        id: record.local_job_id().to_owned(), engine_id: "omnibridge".into(), cover_prompt: context.cover_prompt,
        title: context.title, status, dispatch: MusicJobDispatch::OmniBridge, phase,
        caption: context.caption, lyrics: context.lyrics, duration_seconds: context.duration_seconds,
        generation_settings: context.generation_settings, song: None, songs: vec![], message,
    })
}

async fn create_omnibridge_music_job(
    state: AppState,
    request: CreateMusicJobRequest,
) -> (StatusCode, Json<MusicJob>) {
    let engine_id = "omnibridge".to_owned();
    let music_route = match state.model_port.music_route() {
        Ok(route) => route,
        Err(error) => return (StatusCode::SERVICE_UNAVAILABLE, Json(failed_request_job(request, engine_id, error))),
    };
    let config = match omnibridge::OmniBridgeConfig::from_env_with_route(music_route) {
        Ok(config) => config,
        Err(error) => return (StatusCode::SERVICE_UNAVAILABLE, Json(failed_request_job(request, engine_id, error.to_string()))),
    };
    if request.output_format.as_deref().is_some_and(|value| value != "mp3") {
        return (StatusCode::BAD_REQUEST, Json(failed_request_job(request, engine_id, "OmniBridge Music currently verifies mp3 output only.".into())));
    }
    let client = match omnibridge::OmniBridgeMusicClient::new(config) {
        Ok(client) => client,
        Err(error) => return (StatusCode::SERVICE_UNAVAILABLE, Json(failed_request_job(request, engine_id, error.to_string()))),
    };
    let submit = match client.music_request(request.caption.clone(), request.lyrics.clone()) {
        Ok(submit) => submit,
        Err(error) => return (StatusCode::BAD_REQUEST, Json(failed_request_job(request, engine_id, error.to_string()))),
    };
    let client_request_id = request.client_request_id.as_deref().map(str::trim).unwrap_or_default();
    if !valid_music_client_request_id(client_request_id) {
        return (StatusCode::BAD_REQUEST, Json(failed_request_job(request, engine_id, "client_request_id is required for OmniBridge and must be a safe stable identifier.".into())));
    }
    let job_id = format!("omnibridge-{client_request_id}");
    let idempotency_key = match omnibridge::IdempotencyKey::new(format!("music-maker:{client_request_id}:v1")) {
        Ok(key) => key,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(failed_request_job(request, engine_id, error.to_string()))),
    };
    let generation_settings = serde_json::to_value(&submit).unwrap_or(Value::Null);
    let title = titled(&request);
    let context = omnibridge::DurableMusicContext {
        caption: request.caption.clone(), lyrics: request.lyrics.clone(), duration_seconds: request.duration_seconds,
        title: Some(title.clone()), cover_prompt: request.cover_prompt.clone(), generation_settings: generation_settings.clone(),
    };
    let existing_record = {
        let store = state.omnibridge_store.lock().await;
        store.get(&job_id)
    };
    match existing_record {
        Ok(Some(record)) => {
            let digest = match submit.payload_digest() {
                Ok(digest) => digest,
                Err(error) => return (StatusCode::BAD_REQUEST, Json(failed_request_job(request, engine_id, error.to_string()))),
            };
            if record.payload_digest() != digest || record.idempotency_key() != &idempotency_key {
                return (StatusCode::CONFLICT, Json(failed_request_job(request, engine_id, "client_request_id already owns a different OmniBridge intent.".into())));
            }
            let job = music_job_from_durable_record(&record).unwrap_or_else(|| failed_request_job(request, engine_id, "Stored OmniBridge intent has no recoverable context.".into()));
            state.jobs.write().await.insert(job.id.clone(), job.clone());
            return (StatusCode::ACCEPTED, Json(job));
        }
        Ok(None) => {}
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(failed_request_job(request, engine_id, error.to_string()))),
    }
    if let Err(error) = client.verify_contracts().await {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(failed_request_job(request, engine_id, format!("OmniBridge contract handshake failed before submit: {}", safe_omnibridge_diagnostic_error(&error)))));
    }
    let prepared = {
        let store = state.omnibridge_store.lock().await;
        store.prepare_intent_once(&job_id, &submit, idempotency_key.clone(), context)
    };
    match prepared {
        Ok(omnibridge::PrepareIntentOutcome::Created(_)) => {}
        Ok(omnibridge::PrepareIntentOutcome::Existing(record)) => {
            let job = music_job_from_durable_record(&record).unwrap_or_else(|| failed_request_job(request, engine_id, "Stored OmniBridge intent has no recoverable context.".into()));
            state.jobs.write().await.insert(job.id.clone(), job.clone());
            return (StatusCode::ACCEPTED, Json(job));
        }
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(failed_request_job(request, engine_id, error.to_string())));
        }
    }
    let mut job = MusicJob {
        id: job_id.clone(), engine_id, cover_prompt: request.cover_prompt, title: Some(title),
        status: MusicJobStatus::Queued, dispatch: MusicJobDispatch::OmniBridge, phase: MusicJobPhase::Queued,
        caption: request.caption, lyrics: request.lyrics, duration_seconds: request.duration_seconds,
        generation_settings, song: None, songs: vec![],
        message: "OmniBridge intent persisted. The submit operation will run once.".into(),
    };
    state.jobs.write().await.insert(job_id.clone(), job.clone());
    match client.submit_once(&submit, &idempotency_key).await {
        Ok(handle) => {
            let result = {
                let store = state.omnibridge_store.lock().await;
                store.mark_accepted(&job_id, handle)
            };
            if let Err(error) = result {
                job.status = MusicJobStatus::Unknown;
                job.phase = MusicJobPhase::SubmissionUnknown;
                job.message = format!("OmniBridge accepted the request but its private handle could not be durably stored: {}", security::redact_secrets(error.to_string()));
            } else {
                job.message = "OmniBridge accepted this job; all recovery is GET-only.".into();
            }
        }
        Err(omnibridge::OmniBridgeError::SubmissionUnknown(_)) => {
            let _ = {
                let store = state.omnibridge_store.lock().await;
                store.mark_submission_unknown(&job_id)
            };
            job.status = MusicJobStatus::Unknown;
            job.phase = MusicJobPhase::SubmissionUnknown;
            job.message = "OmniBridge submission outcome is unknown. Automatic replay is blocked.".into();
        }
        Err(error) => {
            let _ = {
                let store = state.omnibridge_store.lock().await;
                store.mark_rejected(&job_id, "failed")
            };
            job.status = MusicJobStatus::Failed;
            job.phase = MusicJobPhase::Failed;
            job.message = security::redact_secrets(error.to_string());
        }
    }
    state.jobs.write().await.insert(job_id, job.clone());
    (StatusCode::ACCEPTED, Json(job))
}

fn valid_music_client_request_id(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 96
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || byte == b'-'
                || byte == b'_'
                || byte == b'.'
                || byte == b':'
        })
}

async fn poll_omnibridge_music_job(
    state: &AppState,
    existing: MusicJob,
) -> Result<Json<MusicJob>, (StatusCode, Json<ApiError>)> {
    let record = state.omnibridge_store.lock().await.get(&existing.id)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "OmniBridge job intent was not found.".into()))?;
    if !matches!(record.submit_state(), omnibridge::DurableSubmitState::Accepted) {
        return Ok(Json(music_job_from_durable_record(&record).unwrap_or(existing)));
    }
    if let Some(song_id) = record.imported_song_id() {
        if let Some(song) = state.library.get_song(song_id).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))? {
            let completed = CompletedSong { id: song.id.clone(), audio_url: format!("/v1/library/media/{}", song.id), song };
            let mut job = existing; job.status = MusicJobStatus::Completed; job.phase = MusicJobPhase::Completed;
            job.song = Some(completed.clone()); job.songs = vec![completed]; job.message = "OmniBridge artifact was imported into the library.".into();
            state.jobs.write().await.insert(job.id.clone(), job.clone()); return Ok(Json(job));
        }
    }
    if !record.poll_is_due() { return Ok(Json(existing)); }
    let handle = record.task_handle().cloned().ok_or_else(|| api_error(StatusCode::CONFLICT, "Accepted OmniBridge job has no recoverable handle.".into()))?;
    let client = omnibridge::OmniBridgeMusicClient::new(omnibridge::OmniBridgeConfig::from_env_with_route("route:music:recovery")
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?)
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    let remote = match client.get_status(&handle).await {
        Ok(remote) => remote,
        Err(omnibridge::OmniBridgeError::RateLimited(delay_ms)) => {
            state.omnibridge_store.lock().await.defer_poll(&existing.id, delay_ms)
                .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
            let mut job = existing;
            job.message = format!("OmniBridge asked this GET-only recovery poll to wait {} seconds.", delay_ms / 1000);
            state.jobs.write().await.insert(job.id.clone(), job.clone());
            return Ok(Json(job));
        }
        Err(error) => return Err(api_error(StatusCode::BAD_GATEWAY, error.to_string())),
    };
    let record = state.omnibridge_store.lock().await.update_status(&existing.id, &remote)
        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let mut job = existing;
    match remote.status.as_str() {
        "completed" | "succeeded" | "done" => {
            let artifact = record.artifact().cloned().ok_or_else(|| api_error(StatusCode::BAD_GATEWAY, "Completed OmniBridge job has no verified audio artifact.".into()))?;
            let bytes = match client.download_artifact(&handle, &artifact).await {
                Ok(bytes) => bytes,
                Err(omnibridge::OmniBridgeError::RateLimited(delay_ms)) => {
                    state.omnibridge_store.lock().await.defer_poll(&job.id, delay_ms)
                        .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
                    job.message = format!("OmniBridge asked artifact recovery to wait {} seconds; recovery remains GET-only.", delay_ms / 1000);
                    state.jobs.write().await.insert(job.id.clone(), job.clone());
                    return Ok(Json(job));
                }
                Err(error) => return Err(api_error(StatusCode::BAD_GATEWAY, error.to_string())),
            };
            let extension = match artifact.content_type.split(';').next().unwrap_or_default() {
                "audio/mpeg" => "mp3", "audio/wav" | "audio/x-wav" => "wav", "audio/mp4" => "m4a",
                "audio/aac" => "aac", "audio/flac" => "flac", "audio/ogg" => "ogg",
                _ => return Err(api_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Unsupported OmniBridge artifact MIME.".into())),
            };
            let metadata = serde_json::json!({"duration_seconds":job.duration_seconds,"omnibridge_job_id":job.id,"artifact_sha256":artifact.sha256});
            let imported = state.library.import_generated_song_idempotent(&job.id, library::GeneratedSongInput {
                title: job.title.clone(), metadata, caption: job.caption.clone(), lyrics: job.lyrics.clone(),
                generation_settings: job.generation_settings.clone(), replay_request: None, audio_codes: None,
                engine_id: "omnibridge".into(), profile_id: None, source: "omnibridge_generation".into(), audio_extension: extension, audio: bytes,
            }).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
            state.omnibridge_store.lock().await.mark_imported(&job.id, imported.song.id.clone())
                .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
            let completed = CompletedSong { id: imported.song.id.clone(), audio_url: format!("/v1/library/media/{}", imported.song.id), song: imported.song };
            job.status = MusicJobStatus::Completed; job.phase = MusicJobPhase::Completed; job.song = Some(completed.clone()); job.songs = vec![completed];
            job.message = "OmniBridge completed this job; its verified artifact was imported exactly once.".into();
        }
        "running" | "processing" => { job.status = MusicJobStatus::Running; job.phase = MusicJobPhase::Running; job.message = "OmniBridge is processing this job.".into(); }
        "failed" | "dead_letter" => { job.status = MusicJobStatus::Failed; job.phase = MusicJobPhase::Failed; job.message = "OmniBridge reported a failed job.".into(); }
        "cancelled" => { job.status = MusicJobStatus::Cancelled; job.phase = MusicJobPhase::Cancelled; job.message = "OmniBridge reported a cancelled job.".into(); }
        _ => { job.status = MusicJobStatus::Queued; job.phase = MusicJobPhase::Queued; job.message = "OmniBridge accepted this job; recovery remains GET-only.".into(); }
    }
    state.jobs.write().await.insert(job.id.clone(), job.clone());
    Ok(Json(job))
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
    if existing.engine_id == "omnibridge" {
        return poll_omnibridge_music_job(&state, existing).await;
    }
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
    if existing.engine_id == "omnibridge" {
        let record = state.omnibridge_store.lock().await.get(&job_id)
            .map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "OmniBridge job intent was not found.".into()))?;
        let handle = record.task_handle().ok_or_else(|| api_error(StatusCode::CONFLICT, "Submission outcome is unknown; cancellation cannot be proven safe.".into()))?;
        let client = omnibridge::OmniBridgeMusicClient::new(omnibridge::OmniBridgeConfig::from_env_with_route("route:music:recovery")
            .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?)
            .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
        return match client.cancel_after_accept(handle) {
            Err(error) => Err(api_error(StatusCode::CONFLICT, error.to_string())),
            Ok(()) => Err(api_error(StatusCode::NOT_IMPLEMENTED, "OmniBridge cancel unexpectedly returned without a state.".into())),
        };
    }
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
    (status, Json(ApiError { error: security::redact_secrets(error) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_execution_target_is_explicit_and_backward_compatible() {
        assert_eq!(parse_requested_music_execution_target(None).unwrap(), None);
        assert_eq!(parse_requested_music_execution_target(Some("auto")).unwrap(), None);
        assert_eq!(parse_requested_music_execution_target(Some("cloud")).unwrap(), Some(MusicExecutionTarget::OmniBridge));
        assert_eq!(parse_requested_music_execution_target(Some("omnibridge")).unwrap(), Some(MusicExecutionTarget::OmniBridge));
        assert_eq!(parse_requested_music_execution_target(Some("local")).unwrap(), Some(MusicExecutionTarget::Configuration));
        assert_eq!(parse_requested_music_execution_target(Some("configuration")).unwrap(), Some(MusicExecutionTarget::Configuration));
        assert!(parse_requested_music_execution_target(Some("other")).is_err());
    }

    #[test]
    fn omnibridge_diagnostic_is_secret_free_and_distinguishes_contract_states() {
        let config = omnibridge::OmniBridgeConfig::new(
            "https://private-gateway.example/internal",
            "gateway-secret-value",
            "private-platform-id",
            "route:music:cloud",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();

        let configured = omnibridge_integration_status_payload(
            Some(&config),
            None,
            OmniBridgeContractDiagnostic::NotChecked,
            "omnibridge",
            false,
        );
        assert_eq!(configured["diagnostic_status"], "configured");
        assert_eq!(configured["contract_status"], "not_checked");

        let verified = omnibridge_integration_status_payload(
            Some(&config),
            None,
            OmniBridgeContractDiagnostic::Verified,
            "omnibridge",
            false,
        );
        assert_eq!(verified["diagnostic_status"], "contract_verified");
        assert_eq!(verified["contract_status"], "verified");
        assert_eq!(verified["route_readiness"], "unverified");
        assert_eq!(verified["route_resolution_verified"], false);
        assert_eq!(verified["real_generation_verified"], false);

        let failed = omnibridge_integration_status_payload(
            Some(&config),
            None,
            OmniBridgeContractDiagnostic::Failed(
                "Authorization: Bearer diagnostic-secret".to_owned(),
            ),
            "omnibridge",
            false,
        );
        let encoded = serde_json::to_string(&failed).unwrap();
        assert_eq!(failed["diagnostic_status"], "contract_failed");
        assert_eq!(failed["contract_status"], "failed");
        assert_eq!(failed["music_route"], "route:music:cloud");
        for private in [
            "private-gateway.example",
            "private-platform-id",
            "gateway-secret-value",
            "diagnostic-secret",
        ] {
            assert!(!encoded.contains(private));
        }
    }

    #[test]
    fn omnibridge_transport_diagnostic_never_returns_the_remote_url() {
        let error = omnibridge::OmniBridgeError::Transport(
            "request to https://private-gateway.example/v1/contracts failed".to_owned(),
        );
        let safe = safe_omnibridge_diagnostic_error(&error);
        assert_eq!(safe, "OmniBridge contract endpoint is unreachable.");
        assert!(!safe.contains("private-gateway.example"));
    }

    fn durable_receipt_records(route: &str, status: &str) -> Vec<omnibridge::DurableMusicRecord> {
        let path = env::temp_dir().join(format!(
            "music-maker-integration-evidence-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sidecar = serde_json::json!({
            "schema": "music-maker.omnibridge-jobs.v1",
            "records": [{
                "local_job_id": "omnibridge-evidence",
                "payload_digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "idempotency_key": "music-maker:evidence:v1",
                "submit_state": "accepted",
                "task_id": "task-evidence",
                "task_token": "test-only-task-token",
                "status": status,
                "artifact": {
                    "schema": "omnibridge.artifact-ref.v1",
                    "id": "artifact-evidence",
                    "content_type": "audio/mpeg",
                    "bytes": 3,
                    "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "availability": "metadata-only"
                },
                "context": {
                    "caption": "receipt",
                    "lyrics": "receipt",
                    "duration_seconds": 1.0,
                    "title": "Receipt",
                    "cover_prompt": null,
                    "generation_settings": {
                        "operation": "audio.music.generate",
                        "kind": "audio.music_generation",
                        "model": route,
                        "payload": { "model": route }
                    }
                },
                "imported_song_id": "song-evidence"
            }]
        });
        fs::write(&path, serde_json::to_vec(&sidecar).unwrap()).unwrap();
        let records = omnibridge::OmniBridgeMusicStore::new(&path)
            .list()
            .unwrap();
        fs::remove_file(path).unwrap();
        records
    }

    #[test]
    fn omnibridge_generation_evidence_requires_complete_receipt_for_current_route() {
        let records = durable_receipt_records("route:music:minimax-3", "completed");
        assert!(omnibridge_route_generation_verified(
            &records,
            "route:music:minimax-3"
        ));
    }

    #[test]
    fn omnibridge_generation_evidence_rejects_another_route() {
        let records = durable_receipt_records("route:music:other", "completed");
        assert!(!omnibridge_route_generation_verified(
            &records,
            "route:music:minimax-3"
        ));
    }

    #[test]
    fn omnibridge_generation_evidence_rejects_failed_status() {
        let records = durable_receipt_records("route:music:minimax-3", "failed");
        assert!(!omnibridge_route_generation_verified(
            &records,
            "route:music:minimax-3"
        ));
    }

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
            client_request_id: None,
            execution_target: None,
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
            client_request_id: None,
            execution_target: None,
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
            client_request_id: None,
            execution_target: None,
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
                runtime: lyrics_sync::OnnxFlavour::default(),
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
        // Four by default: songs decoded together are nearly free after the
        // first, and the upstream default of 1 left the slider disabled.
        // Nothing is reserved that nobody asked for.
        assert_eq!(EngineOptions::default().effective_max_batch(), 1);
        // And whatever it is, the engine is started with it: the request
        // carries `lm_batch_size`, and the engine refuses anything above the
        // ceiling it was loaded with. Offering more in the panel than the
        // engine was given is what made a request for two songs fail at once.
        assert_eq!(EngineOptions::default().to_engine().max_batch, Some(1));
        assert_eq!(EngineOptions { max_batch: Some(3), ..EngineOptions::default() }.to_engine().max_batch, Some(3));
        // The assistant is optional: it must survive a restart when configured,
        // and stay unavailable when it is not.
        assert!(restored.assistant.available());
        assert!(!AssistantConfig::default().available());
    }

    #[test]
    fn remote_statuses_never_claim_success_for_an_unknown_value() {
        let mut job = queued_not_configured_job(
            CreateMusicJobRequest {
                client_request_id: None,
            execution_target: None,
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
