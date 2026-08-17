mod providers;
mod credentials;
mod model_manager;
mod presets;
mod library;
mod mm_result;
mod openrouter_stream;

use std::{collections::HashMap, env, fs, net::SocketAddr, path::PathBuf, sync::Arc};
use anyhow::Context;
use futures_util::StreamExt;

use axum::{
    extract::{Multipart, Path, State},
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
const AUDIOCPP_ENGINE_ID: &str = "audiocpp-minimax-music3";

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
struct SetupDownloadRequest {
    #[serde(default)]
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
    configuration: StudioConfiguration,
    selected_profile_id: Option<String>,
    #[serde(default)]
    selected_component_ids: Option<Vec<String>>,
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

#[derive(Debug, Deserialize)]
struct OpenRouterSettingsRequest {
    /// `None` or an empty string clears the locally stored credential.
    api_key: Option<String>,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
        selected_profile_id: Arc::new(RwLock::new(selected_profile_id)),
        selected_component_ids: Arc::new(RwLock::new(selected_component_ids)),
        settings_path,
        openrouter_catalog: Arc::new(RwLock::new(OpenRouterCatalogState::default())),
        library: library::Library::open_default()?,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/configuration", get(configuration).put(update_configuration))
        .route("/engine/presets", get(engine_presets))
        .route("/engine/preset", post(apply_engine_preset))
        .route("/v1/engine/logs", get(engine_logs))
        .route("/v1/openrouter/settings", get(openrouter_settings).put(update_openrouter_settings))
        .route("/v1/openrouter/catalog", get(openrouter_catalog))
        .route("/v1/openrouter/catalog/refresh", post(refresh_openrouter_catalog))
        .route("/v1/openrouter/transcriptions", post(create_openrouter_transcription))
        .route("/v1/openrouter/covers", post(create_openrouter_cover))
        .route("/v1/library/songs", get(library_songs).post(create_library_song))
        .route("/v1/library/import", post(import_library_audio))
        .route("/v1/library/songs/{id}", get(library_song).put(update_library_song).delete(delete_library_song))
        .route("/v1/library/media/{song_id}", get(library_media))
        .route("/v1/library/playlists", get(library_playlists).post(create_library_playlist))
        .route("/v1/library/playlists/{id}", get(library_playlist).put(update_library_playlist).delete(delete_library_playlist))
        .route("/setup/status", get(setup_status))
        .route("/setup/catalog", get(setup_catalog))
        .route("/setup/download", post(setup_download))
        .route("/setup/cancel", post(setup_cancel))
        .route("/v1/local-models/music", get(local_music_model_catalog))
        .route("/v1/music/jobs", post(create_music_job))
        .route("/v1/music/replay", post(replay_music_job))
        .route(
            "/v1/music/jobs/{job_id}",
            get(music_job_status).post(cancel_music_job),
        )
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let address = SocketAddr::from(([127, 0, 0, 1], 8765));
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
    let song = state.library.import_audio_song(library::AudioImportInput { title, caption, lyrics, metadata: serde_json::json!({"imported_filename": filename}), generation_settings: Value::Null, engine_id: "imported-audio".into(), profile_id: None, source: "audio_import".into(), audio_extension: extension, audio: audio.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "audio file is required".into()))? }).map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?.song;
    Ok((StatusCode::CREATED, Json(song)))
}
async fn update_library_song(State(state):State<AppState>,Path(id):Path<String>,Json(input):Json<library::SongInput>)->Result<Json<library::Song>,(StatusCode,Json<ApiError>)>{state.library.update_song(&id,input).map_err(|e|api_error(StatusCode::BAD_REQUEST,e.to_string()))?.map(Json).ok_or_else(||api_error(StatusCode::NOT_FOUND,"Song not found".into()))}
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
    Json(serde_json::json!({
        "status": "ok",
        "runtime": "native",
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
    Json(configuration): Json<StudioConfiguration>,
) -> Json<StudioConfiguration> {
    *state.configuration.write().await = configuration.clone();
    let _ = persist_studio_settings(&state).await;
    Json(configuration)
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
    let catalog = state.openrouter_catalog.read().await;
    Json(serde_json::json!({ "models": catalog.catalog.as_ref().map(|catalog| &catalog.models), "refreshed_at": catalog.refreshed_at }))
}

async fn refresh_openrouter_catalog(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let body = reqwest::Client::new()
        .get(format!("{}{}", providers::openrouter::API_BASE_URL, providers::openrouter::MODELS_PATH))
        .send().await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter catalog request failed: {error}")))?
        .error_for_status()
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter catalog request failed: {error}")))?
        .text().await
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter catalog response failed: {error}")))?;
    let parsed = providers::openrouter::CapabilityCatalog::parse(&body)
        .map_err(|error| api_error(StatusCode::BAD_GATEWAY, format!("OpenRouter catalog parse failed: {error}")))?;
    let refreshed_at = chrono_like_timestamp();
    let models = parsed.models.clone();
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
    let catalog = state
        .openrouter_catalog
        .read()
        .await
        .catalog
        .clone()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Refresh the OpenRouter catalog before transcribing audio.".into()))?;
    let request = providers::openrouter::stt_request_for(
        &catalog,
        &input.model_id,
        providers::openrouter::Base64AudioInput {
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
    let catalog = state
        .openrouter_catalog
        .read()
        .await
        .catalog
        .clone()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Refresh the OpenRouter catalog before generating a cover.".into()))?;
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
        configuration: state.configuration.read().await.clone(),
        selected_profile_id: state.selected_profile_id.read().await.clone(),
        selected_component_ids: state.selected_component_ids.read().await.clone(),
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
        fields.insert("ready".into(), Value::Bool(selected_set_ready));
        fields.insert("first_run".into(), Value::Bool(!selected_set_ready));
        if selected_set_ready { fields.insert("download_pending".into(), Value::from(0_u64)); }
    }
    status
}

/// Recent native engine output. This is the only progress detail upstream
/// exposes: `/job` reports a phase, and everything finer lives in the log ring.
async fn engine_logs(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let lines = state
        .music_server
        .logs_snapshot(std::time::Duration::from_millis(700))
        .await
        .map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, format!("mm-server logs are unavailable: {error}")))?;
    Ok(Json(serde_json::json!({ "engine_id": PRIMARY_MUSIC_ENGINE_ID, "lines": lines })))
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
    Json(request): Json<OpenRouterSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let source = credentials::store_openrouter_api_key(request.api_key.as_deref())
        .map_err(|error| api_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    Ok(Json(serde_json::json!({
        "configured": source.is_some(),
        "source": source,
        "environment_variable": credentials::OPENROUTER_ENV_VAR,
    })))
}

async fn setup_status(State(state): State<AppState>) -> Json<Value> {
    let target = effective_install_target(&state).await;
    let manager_status = state.model_manager.status(target).await;
    Json(compose_setup_status(&state, manager_status).await)
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
    let catalog = state.openrouter_catalog.read().await;
    let primary_installed = state.music_server.health().await;
    Json(CapabilitiesResponse { engines: capability_engines(catalog.catalog.as_ref(), primary_installed) })
}

fn capability_engines(
    catalog: Option<&providers::openrouter::CapabilityCatalog>,
    primary_installed: bool,
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
        EngineDescriptor {
            id: AUDIOCPP_ENGINE_ID.into(),
            display_name: "MiniMax Music3 (audio.cpp, optional)".into(),
            capabilities: vec![Capability::MusicGeneration],
            execution_mode: ExecutionMode::Local,
            installed: false,
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
    match state.music_server.submit(mm_request.clone()).await {
        Ok(remote) => {
            let job = MusicJob {
                id: remote.id,
                engine_id,
                title: request.title.clone(),
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
                message: format!("mm-server is unavailable; no inference started: {error}"),
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
    let replay = match (&request.song_id, &request.replay_request) {
        (Some(_), Some(_)) => return Err(api_error(StatusCode::BAD_REQUEST, "Provide either song_id or replay_request, not both.".into())),
        (Some(song_id), None) => state.library.get_song(song_id).map_err(|error| api_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Song not found.".into()))?
            .replay_request.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "This song has no MiniMax Music3 replay request.".into()))?,
        (None, Some(replay)) => replay.clone(),
        (None, None) => return Err(api_error(StatusCode::BAD_REQUEST, "Provide song_id or replay_request.".into())),
    };
    let synth_request = prepare_replay_synthesis(replay, &request).map_err(|error| api_error(StatusCode::BAD_REQUEST, error))?;
    let caption = synth_request.get("caption").and_then(Value::as_str).unwrap_or_default().to_owned();
    let lyrics = synth_request.get("lyrics").and_then(Value::as_str).unwrap_or_default().to_owned();
    let remote = state.music_server.submit(synth_request.clone()).await.map_err(|error| api_error(StatusCode::SERVICE_UNAVAILABLE, format!("mm-server replay synthesis is unavailable: {error}")))?;
    let job = MusicJob {
        id: remote.id, engine_id: PRIMARY_MUSIC_ENGINE_ID.into(), title: None, status: MusicJobStatus::Queued,
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
        id: format!("openrouter-{}", uuid_suffix()), engine_id: engine_id.clone(), title: request.title.clone(), status: MusicJobStatus::Running,
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
            title: job.title.clone(), caption: job.caption.clone(), lyrics: job.lyrics.clone(), generation_settings: job.generation_settings.clone(),
            replay_request: None, audio_codes: None, engine_id: "openrouter".into(), profile_id: None,
            source: "openrouter_generation".into(), audio_extension: "wav", audio,
        })?;
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
        let mut generation_settings = replay.clone();
        generation_settings.as_object_mut().context("replay request is not a JSON object")?.remove("audio_codes");
        let imported_song = state.library.import_generated_song(library::GeneratedSongInput {
            title: job.title.clone(), caption, lyrics, generation_settings, replay_request: Some(replay), audio_codes: Some(audio_codes),
            engine_id: job.engine_id.clone(), profile_id: profile_id.clone(),
            source: "local_generation".into(),
            audio_extension: mm_result::audio_extension(&track.audio_content_type)?, audio: track.audio,
        })?;
        let audio_url = format!("/v1/library/media/{}", imported_song.song.id);
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
            configuration: initial_configuration(),
            selected_profile_id: None,
            selected_component_ids: Some(vec!["lm-q8".into(), "depth-q8".into(), "condition-f32".into(), "dit-q6".into(), "vocoder-f32".into()]),
        };
        let restored: PersistedStudioSettings = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(restored.selected_profile_id.is_none());
        assert_eq!(restored.selected_component_ids.unwrap(), vec!["lm-q8", "depth-q8", "condition-f32", "dit-q6", "vocoder-f32"]);
    }

    #[test]
    fn remote_statuses_never_claim_success_for_an_unknown_value() {
        let mut job = queued_not_configured_job(
            CreateMusicJobRequest {
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
        assert_eq!(serde_json::to_value(after_refresh).unwrap()["engines"].as_array().unwrap().len(), 3);
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
