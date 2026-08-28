//! Narrow OmniBridge client for durable MiniMax Music jobs.
//!
//! The client owns the one non-idempotent operation in this integration: the
//! initial `POST /v1/jobs`. It deliberately performs that POST once and treats
//! transport ambiguity as `SubmissionUnknown`; recovery after acceptance is
//! GET-only. Gateway credentials and task tokens are kept in opaque types that
//! do not implement `Serialize` and redact their `Debug` output.

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STORE_SCHEMA: &str = "music-maker.omnibridge-jobs.v1";
const ARTIFACT_SCHEMA: &str = "omnibridge.artifact-ref.v1";
const CONTRACT_SCHEMA: &str = "omnibridge.contract-catalog.v1";
const CONTRACT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_JSON_BYTES: usize = 1024 * 1024;
const OPERATION: &str = "audio.music.generate";
const KIND: &str = "audio.music_generation";
const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
const ALLOWED_AUDIO_TYPES: &[&str] = &[
    "audio/mpeg",
    "audio/wav",
    "audio/x-wav",
    "audio/mp4",
    "audio/aac",
    "audio/flac",
    "audio/ogg",
];

#[derive(Clone)]
struct SecretString(String);

impl SecretString {
    fn new(value: String, name: &'static str) -> Result<Self, OmniBridgeError> {
        let value = value.trim().to_owned();
        if value.is_empty() {
            return Err(OmniBridgeError::NotReady(name));
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Clone)]
pub struct OmniBridgeConfig {
    pub base_url: String,
    gateway_key: SecretString,
    pub platform_id: String,
    pub music_route: String,
    expected_schema_digest: String,
}

impl OmniBridgeConfig {
    pub const fn operation() -> &'static str {
        OPERATION
    }

    pub const fn kind() -> &'static str {
        KIND
    }

    pub fn from_env() -> Result<Self, OmniBridgeError> {
        let route = required_env("MUSIC_MAKER_OMNIBRIDGE_MUSIC_ROUTE")?;
        Self::from_env_with_route(route)
    }

    /// Loads the managed Gateway identity while taking the project-owned
    /// business route from Project Profile v2.
    pub fn from_env_with_route(music_route: impl Into<String>) -> Result<Self, OmniBridgeError> {
        Self::new(
            required_env("MUSIC_MAKER_OMNIBRIDGE_BASE_URL")?,
            required_env("MUSIC_MAKER_OMNIBRIDGE_GATEWAY_KEY")?,
            required_env("MUSIC_MAKER_OMNIBRIDGE_PLATFORM_ID")?,
            music_route,
            required_env("MUSIC_MAKER_OMNIBRIDGE_SCHEMA_DIGEST")?,
        )
    }

    pub fn new(
        base_url: impl Into<String>,
        gateway_key: impl Into<String>,
        platform_id: impl Into<String>,
        music_route: impl Into<String>,
        expected_schema_digest: impl Into<String>,
    ) -> Result<Self, OmniBridgeError> {
        let base_url = base_url.into().trim().trim_end_matches('/').to_owned();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(OmniBridgeError::InvalidInput(
                "OmniBridge base URL must be HTTP or HTTPS".to_owned(),
            ));
        }
        let platform_id = platform_id.into().trim().to_owned();
        if platform_id.is_empty()
            || platform_id.len() > 64
            || !platform_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
        {
            return Err(OmniBridgeError::InvalidInput(
                "OmniBridge platform ID is invalid".to_owned(),
            ));
        }
        let music_route = music_route.into().trim().to_owned();
        if music_route.is_empty() {
            return Err(OmniBridgeError::NotReady(
                "MUSIC_MAKER_OMNIBRIDGE_MUSIC_ROUTE",
            ));
        }
        if !music_route.starts_with("route:music:") || music_route.len() > 200 {
            return Err(OmniBridgeError::InvalidInput(
                "OmniBridge music route must use route:music:*".to_owned(),
            ));
        }
        let expected_schema_digest = expected_schema_digest.into().trim().to_owned();
        if !valid_schema_digest(&expected_schema_digest) {
            return Err(OmniBridgeError::InvalidInput(
                "OmniBridge schema digest must use sha256:<64 lowercase hex>".to_owned(),
            ));
        }
        Ok(Self {
            base_url,
            gateway_key: SecretString::new(
                gateway_key.into(),
                "MUSIC_MAKER_OMNIBRIDGE_GATEWAY_KEY",
            )?,
            platform_id,
            music_route,
            expected_schema_digest,
        })
    }
}

impl fmt::Debug for OmniBridgeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OmniBridgeConfig")
            .field("base_url", &self.base_url)
            .field("gateway_key", &self.gateway_key)
            .field("platform_id", &self.platform_id)
            .field("music_route", &self.music_route)
            .field("expected_schema_digest", &self.expected_schema_digest)
            .finish()
    }
}

const REQUEST_RECEIPT_SCHEMA: &str = "omnibridge.request-receipt.v1";
const TEXT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// The two business-level text policies published by OmniBridge. The Studio
/// never sees provider credentials or candidate order; it only selects a
/// generic route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRoute {
    Fast,
    Quality,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RequestReceipt {
    pub schema: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_deployment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_adapter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_catalog_revision: Option<String>,
}

/// Managed text configuration is deliberately independent from the durable
/// music route. A missing music route must not disable the writing assistant,
/// and a missing text route must not disable music generation.
#[derive(Clone)]
pub struct OmniBridgeTextConfig {
    pub base_url: String,
    gateway_key: SecretString,
    pub client_id: String,
    pub platform_id: String,
    pub project_id: String,
    pub fast_route: String,
    pub quality_route: String,
}

impl OmniBridgeTextConfig {
    pub fn from_env() -> Result<Self, OmniBridgeError> {
        let fast = required_env("MUSIC_MAKER_OMNIBRIDGE_TEXT_FAST_ROUTE")?;
        let quality = required_env("MUSIC_MAKER_OMNIBRIDGE_TEXT_QUALITY_ROUTE")?;
        Self::from_env_with_routes(fast, quality)
    }

    pub fn from_env_with_route(route: impl Into<String>) -> Result<Self, OmniBridgeError> {
        let route = route.into();
        Self::from_env_with_routes(route.clone(), route)
    }

    fn from_env_with_routes(
        fast_route: impl Into<String>,
        quality_route: impl Into<String>,
    ) -> Result<Self, OmniBridgeError> {
        let platform_id = required_env("MUSIC_MAKER_OMNIBRIDGE_PLATFORM_ID")?;
        let client_id = optional_env("MUSIC_MAKER_OMNIBRIDGE_CLIENT_ID")
            .unwrap_or_else(|| platform_id.clone());
        let project_id = optional_env("MUSIC_MAKER_OMNIBRIDGE_PROJECT_ID")
            .unwrap_or_else(|| platform_id.clone());
        Self::new(
            required_env("MUSIC_MAKER_OMNIBRIDGE_BASE_URL")?,
            required_env("MUSIC_MAKER_OMNIBRIDGE_GATEWAY_KEY")?,
            client_id,
            platform_id,
            project_id,
            fast_route,
            quality_route,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        base_url: impl Into<String>,
        gateway_key: impl Into<String>,
        client_id: impl Into<String>,
        platform_id: impl Into<String>,
        project_id: impl Into<String>,
        fast_route: impl Into<String>,
        quality_route: impl Into<String>,
    ) -> Result<Self, OmniBridgeError> {
        let base_url = base_url.into().trim().trim_end_matches('/').to_owned();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(OmniBridgeError::InvalidInput(
                "OmniBridge base URL must be HTTP or HTTPS".to_owned(),
            ));
        }
        let client_id = validate_caller_id(client_id.into(), "OmniBridge client ID")?;
        let platform_id = validate_caller_id(platform_id.into(), "OmniBridge platform ID")?;
        let project_id = validate_caller_id(project_id.into(), "OmniBridge project ID")?;
        let fast_route = validate_text_route(
            fast_route.into(),
            "MUSIC_MAKER_OMNIBRIDGE_TEXT_FAST_ROUTE",
        )?;
        let quality_route = validate_text_route(
            quality_route.into(),
            "MUSIC_MAKER_OMNIBRIDGE_TEXT_QUALITY_ROUTE",
        )?;
        Ok(Self {
            base_url,
            gateway_key: SecretString::new(
                gateway_key.into(),
                "MUSIC_MAKER_OMNIBRIDGE_GATEWAY_KEY",
            )?,
            client_id,
            platform_id,
            project_id,
            fast_route,
            quality_route,
        })
    }

    pub fn route(&self, route: TextRoute) -> &str {
        match route {
            TextRoute::Fast => &self.fast_route,
            TextRoute::Quality => &self.quality_route,
        }
    }

    /// Safe status data for project UI. Provider URLs and credentials are
    /// deliberately absent.
    pub fn public_status(&self) -> Value {
        serde_json::json!({
            "provider": "omnibridge",
            "fast_route": self.fast_route,
            "quality_route": self.quality_route,
            "client_id": self.client_id,
            "platform_id": self.platform_id,
            "project_id": self.project_id,
        })
    }
}

impl fmt::Debug for OmniBridgeTextConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OmniBridgeTextConfig")
            .field("base_url", &self.base_url)
            .field("gateway_key", &self.gateway_key)
            .field("client_id", &self.client_id)
            .field("platform_id", &self.platform_id)
            .field("project_id", &self.project_id)
            .field("fast_route", &self.fast_route)
            .field("quality_route", &self.quality_route)
            .finish()
    }
}

pub struct OmniBridgeTextStream {
    pub receipt: Option<RequestReceipt>,
    response: reqwest::Response,
}

impl OmniBridgeTextStream {
    pub fn into_response(self) -> reqwest::Response {
        self.response
    }
}

#[derive(Clone)]
pub struct OmniBridgeTextClient {
    config: OmniBridgeTextConfig,
    client: reqwest::Client,
}

impl OmniBridgeTextClient {
    pub fn from_env() -> Result<Self, OmniBridgeError> {
        Self::new(OmniBridgeTextConfig::from_env()?)
    }

    pub fn from_env_with_route(route: impl Into<String>) -> Result<Self, OmniBridgeError> {
        Self::new(OmniBridgeTextConfig::from_env_with_route(route)?)
    }

    pub fn new(config: OmniBridgeTextConfig) -> Result<Self, OmniBridgeError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(TEXT_REQUEST_TIMEOUT)
            .timeout(TEXT_REQUEST_TIMEOUT)
            .build()
            .map_err(|error| OmniBridgeError::Transport(error.to_string()))?;
        Ok(Self { config, client })
    }

    pub fn route(&self, route: TextRoute) -> &str {
        self.config.route(route)
    }

    fn request(&self, route: TextRoute, accept: &'static str) -> reqwest::RequestBuilder {
        self.request_route(self.config.route(route), accept)
    }

    fn request_route(&self, route: &str, accept: &'static str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}/v1/chat/completions", self.config.base_url))
            .bearer_auth(self.config.gateway_key.expose())
            .header("x-omnibridge-client-id", &self.config.client_id)
            .header("X-Platform-Id", &self.config.platform_id)
            .header("X-Project-Id", &self.config.project_id)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, accept)
            .header("X-OmniBridge-Selector-Type", "route")
            .header("X-OmniBridge-Selector-Id", route)
    }

    /// Sends one synchronous POST. This client has no automatic retry or
    /// provider fallback; OmniBridge owns any provably safe route failover.
    pub async fn complete_once(
        &self,
        route: TextRoute,
        body: &Value,
    ) -> Result<(Value, Option<RequestReceipt>), OmniBridgeError> {
        self.complete_route_once(self.config.route(route), body).await
    }

    pub async fn complete_route_once(
        &self,
        route: &str,
        body: &Value,
    ) -> Result<(Value, Option<RequestReceipt>), OmniBridgeError> {
        let route = validate_text_route(route.to_owned(), "project profile text route")?;
        let body = text_request_body(body, &route, false)?;
        let response = self
            .request_route(&route, "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|error| OmniBridgeError::Transport(error.to_string()))?;
        let receipt = parse_request_receipt(response.headers());
        if !response.status().is_success() {
            return Err(OmniBridgeError::HttpStatus(response.status().as_u16()));
        }
        Ok((read_json_bounded(response).await?, receipt))
    }

    /// Sends one streaming POST and returns the original response body. Once a
    /// frame is observed the caller must never replay or switch candidates.
    pub async fn stream_once(
        &self,
        route: TextRoute,
        body: &Value,
    ) -> Result<OmniBridgeTextStream, OmniBridgeError> {
        self.stream_route_once(self.config.route(route), body).await
    }

    pub async fn stream_route_once(
        &self,
        route: &str,
        body: &Value,
    ) -> Result<OmniBridgeTextStream, OmniBridgeError> {
        let route = validate_text_route(route.to_owned(), "project profile text route")?;
        let body = text_request_body(body, &route, true)?;
        let response = self
            .request_route(&route, "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|error| OmniBridgeError::Transport(error.to_string()))?;
        let receipt = parse_request_receipt(response.headers());
        if !response.status().is_success() {
            return Err(OmniBridgeError::HttpStatus(response.status().as_u16()));
        }
        Ok(OmniBridgeTextStream { receipt, response })
    }
}

fn optional_env(name: &'static str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn validate_caller_id(value: String, name: &str) -> Result<String, OmniBridgeError> {
    let value = value.trim().to_owned();
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
    {
        return Err(OmniBridgeError::InvalidInput(format!("{name} is invalid")));
    }
    Ok(value)
}

fn validate_text_route(
    value: String,
    env_name: &'static str,
) -> Result<String, OmniBridgeError> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(OmniBridgeError::NotReady(env_name));
    }
    if !value.starts_with("route:text:") || value.len() > 200 {
        return Err(OmniBridgeError::InvalidInput(
            "OmniBridge text route must use route:text:*".to_owned(),
        ));
    }
    Ok(value)
}

fn text_request_body(body: &Value, route: &str, stream: bool) -> Result<Value, OmniBridgeError> {
    let mut object = body.as_object().cloned().ok_or_else(|| {
        OmniBridgeError::InvalidInput("chat completion body must be an object".to_owned())
    })?;
    object.insert("model".to_owned(), Value::String(route.to_owned()));
    object.insert("stream".to_owned(), Value::Bool(stream));
    Ok(Value::Object(object))
}

fn safe_receipt_header(
    headers: &reqwest::header::HeaderMap,
    name: &'static str,
) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?;
    let normalized: String = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let normalized: String = normalized.chars().take(256).collect();
    (!normalized.is_empty()).then_some(normalized)
}

pub fn parse_request_receipt(
    headers: &reqwest::header::HeaderMap,
) -> Option<RequestReceipt> {
    let request_id = safe_receipt_header(headers, "x-omnibridge-request-id")?;
    let attempt = safe_receipt_header(headers, "x-omnibridge-attempt")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 1);
    Some(RequestReceipt {
        schema: REQUEST_RECEIPT_SCHEMA.to_owned(),
        request_id,
        selector_type: safe_receipt_header(headers, "x-omnibridge-selector-type"),
        resolved_provider: safe_receipt_header(headers, "x-omnibridge-resolved-provider"),
        resolved_deployment: safe_receipt_header(
            headers,
            "x-omnibridge-resolved-deployment",
        ),
        resolved_model: safe_receipt_header(headers, "x-omnibridge-resolved-model"),
        route_id: safe_receipt_header(headers, "x-omnibridge-route-id"),
        route_revision: safe_receipt_header(headers, "x-omnibridge-route-revision"),
        attempt,
        provider_adapter: safe_receipt_header(headers, "x-omnibridge-provider-adapter"),
        compute_catalog_revision: safe_receipt_header(
            headers,
            "x-omnibridge-compute-catalog-revision",
        ),
    })
}

fn required_env(name: &'static str) -> Result<String, OmniBridgeError> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(OmniBridgeError::NotReady(name))
}

#[derive(Debug)]
pub enum OmniBridgeError {
    NotReady(&'static str),
    InvalidInput(String),
    SubmissionUnknown(String),
    Transport(String),
    RateLimited(u64),
    HttpStatus(u16),
    Protocol(String),
    Integrity(String),
    NotCancellable,
    Io(String),
}

impl fmt::Display for OmniBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReady(name) => write!(formatter, "OmniBridge is not ready: {name} is missing"),
            Self::InvalidInput(message) => formatter.write_str(message),
            Self::SubmissionUnknown(message) => {
                write!(
                    formatter,
                    "OmniBridge submission outcome is unknown: {message}"
                )
            }
            Self::Transport(message) => write!(formatter, "OmniBridge transport failed: {message}"),
            Self::RateLimited(delay_ms) => {
                write!(
                    formatter,
                    "OmniBridge requested GET retry after {delay_ms} ms"
                )
            }
            Self::HttpStatus(status) => write!(formatter, "OmniBridge returned HTTP {status}"),
            Self::Protocol(message) => write!(formatter, "OmniBridge protocol error: {message}"),
            Self::Integrity(message) => {
                write!(formatter, "OmniBridge artifact integrity error: {message}")
            }
            Self::NotCancellable => formatter.write_str(
                "accepted GMI music jobs are not reliably cancellable; continue GET-only recovery",
            ),
            Self::Io(message) => write!(formatter, "OmniBridge sidecar store error: {message}"),
        }
    }
}

impl std::error::Error for OmniBridgeError {}

impl From<std::io::Error> for OmniBridgeError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<serde_json::Error> for OmniBridgeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Protocol(error.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn new(value: impl Into<String>) -> Result<Self, OmniBridgeError> {
        let value = value.into().trim().to_owned();
        if value.is_empty() || value.len() > 128 {
            return Err(OmniBridgeError::InvalidInput(
                "Idempotency-Key must contain 1-128 characters".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MusicGenerationPayload {
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    pub lyrics: String,
    #[serde(rename = "format")]
    pub output_format: String,
    pub sample_rate: u32,
    pub bitrate: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MusicSubmitRequest {
    pub operation: String,
    pub kind: String,
    pub model: String,
    pub payload: MusicGenerationPayload,
}

impl MusicSubmitRequest {
    pub fn new(
        route: impl Into<String>,
        prompt: impl Into<String>,
        lyrics: impl Into<String>,
        output_format: impl Into<String>,
        sample_rate: u32,
        bitrate: u32,
    ) -> Result<Self, OmniBridgeError> {
        let route = route.into().trim().to_owned();
        if !route.starts_with("route:music:") || route.len() > 200 {
            return Err(OmniBridgeError::InvalidInput(
                "music model must use route:music:*".to_owned(),
            ));
        }
        let prompt = prompt.into().trim().to_owned();
        let lyrics = lyrics.into().trim().to_owned();
        if lyrics.is_empty() || lyrics.encode_utf16().count() > 3_500 {
            return Err(OmniBridgeError::InvalidInput(
                "lyrics must contain 1-3500 characters; instrumental mode is not verified"
                    .to_owned(),
            ));
        }
        if prompt.encode_utf16().count() > 2_000 {
            return Err(OmniBridgeError::InvalidInput(
                "prompt must not exceed 2000 characters".to_owned(),
            ));
        }
        let output_format = output_format.into().trim().to_ascii_lowercase();
        if output_format != "mp3" {
            return Err(OmniBridgeError::InvalidInput(
                "the current MiniMax Music route only verifies mp3 output".to_owned(),
            ));
        }
        if sample_rate != 44_100 || bitrate != 256_000 {
            return Err(OmniBridgeError::InvalidInput(
                "the current MiniMax Music route requires 44100 Hz and 256000 bps".to_owned(),
            ));
        }
        Ok(Self {
            operation: OPERATION.to_owned(),
            kind: KIND.to_owned(),
            model: route.clone(),
            payload: MusicGenerationPayload {
                model: route,
                prompt,
                lyrics,
                output_format,
                sample_rate,
                bitrate,
            },
        })
    }

    pub fn payload_digest(&self) -> Result<String, OmniBridgeError> {
        let bytes = serde_json::to_vec(self)?;
        Ok(hex_sha256(&bytes))
    }
}

#[derive(Clone)]
pub struct PrivateTaskHandle {
    task_id: String,
    task_token: SecretString,
}

impl PrivateTaskHandle {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
}

impl fmt::Debug for PrivateTaskHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivateTaskHandle")
            .field("task_id", &self.task_id)
            .field("task_token", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRef {
    pub schema: String,
    pub id: String,
    pub content_type: String,
    pub bytes: u64,
    pub sha256: String,
    pub availability: String,
}

impl ArtifactRef {
    pub fn validate(&self) -> Result<(), OmniBridgeError> {
        if self.schema != ARTIFACT_SCHEMA
            || self.availability != "metadata-only"
            || !valid_path_segment(&self.id)
            || self.bytes == 0
            || self.bytes > MAX_ARTIFACT_BYTES
            || !ALLOWED_AUDIO_TYPES.contains(&normalized_mime(&self.content_type).as_str())
            || !valid_sha256(&self.sha256)
        {
            return Err(OmniBridgeError::Protocol(
                "invalid or unsupported audio ArtifactRef".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OmniBridgeJobStatus {
    pub task_id: String,
    pub status: String,
    pub provider_status: Option<String>,
    pub poll_after_ms: Option<u64>,
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Deserialize)]
struct SubmitResponse {
    #[serde(alias = "task_id")]
    id: String,
    task_token: String,
}

#[derive(Clone)]
pub struct OmniBridgeMusicClient {
    config: OmniBridgeConfig,
    client: reqwest::Client,
}

impl OmniBridgeMusicClient {
    pub fn new(config: OmniBridgeConfig) -> Result<Self, OmniBridgeError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|error| OmniBridgeError::Transport(error.to_string()))?;
        Ok(Self { config, client })
    }

    pub fn music_request(
        &self,
        prompt: impl Into<String>,
        lyrics: impl Into<String>,
    ) -> Result<MusicSubmitRequest, OmniBridgeError> {
        MusicSubmitRequest::new(
            self.config.music_route.clone(),
            prompt,
            lyrics,
            "mp3",
            44_100,
            256_000,
        )
    }

    /// Verifies the durable no-replay contract without invoking a provider.
    pub async fn verify_contracts(&self) -> Result<(), OmniBridgeError> {
        let response = self
            .client
            .get(format!("{}/v1/contracts", self.config.base_url))
            .bearer_auth(self.config.gateway_key.expose())
            .header("X-Platform-Id", &self.config.platform_id)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(CONTRACT_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| OmniBridgeError::Transport(error.to_string()))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(OmniBridgeError::HttpStatus(response.status().as_u16()));
        }
        let value = read_json_bounded(response).await?;
        validate_contract_catalog(&value, &self.config.expected_schema_digest)
    }

    /// Performs exactly one POST. `SubmissionUnknown` must never be auto-replayed.
    pub async fn submit_once(
        &self,
        request: &MusicSubmitRequest,
        idempotency_key: &IdempotencyKey,
    ) -> Result<PrivateTaskHandle, OmniBridgeError> {
        validate_request_route(request, &self.config.music_route)?;
        let response = self
            .client
            .post(format!("{}/v1/jobs", self.config.base_url))
            .bearer_auth(self.config.gateway_key.expose())
            .header("X-Platform-Id", &self.config.platform_id)
            .header("Idempotency-Key", idempotency_key.as_str())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .json(request)
            .send()
            .await
            .map_err(|error| OmniBridgeError::SubmissionUnknown(error.to_string()))?;
        let status = response.status();
        if status.is_server_error() {
            return Err(OmniBridgeError::SubmissionUnknown(format!(
                "HTTP {} did not prove rejection",
                status.as_u16()
            )));
        }
        if status != reqwest::StatusCode::OK && status != reqwest::StatusCode::ACCEPTED {
            return Err(OmniBridgeError::HttpStatus(status.as_u16()));
        }
        let value = read_json_bounded(response).await.map_err(|_| {
            OmniBridgeError::SubmissionUnknown(
                "accepted response did not contain a recoverable handle".to_owned(),
            )
        })?;
        parse_submit_response(value).map_err(|_| {
            OmniBridgeError::SubmissionUnknown(
                "accepted response did not contain a recoverable handle".to_owned(),
            )
        })
    }

    /// Polls only the already accepted task; this method never submits.
    pub async fn get_status(
        &self,
        handle: &PrivateTaskHandle,
    ) -> Result<OmniBridgeJobStatus, OmniBridgeError> {
        ensure_path_segment(&handle.task_id, "task id")?;
        let response = self
            .client
            .get(format!(
                "{}/v1/jobs/{}",
                self.config.base_url, handle.task_id
            ))
            .bearer_auth(self.config.gateway_key.expose())
            .header("X-Task-Token", handle.task_token.expose())
            .header("X-Platform-Id", &self.config.platform_id)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| OmniBridgeError::Transport(error.to_string()))?;
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(OmniBridgeError::RateLimited(retry_after_ms(
                response.headers(),
            )));
        }
        if status != reqwest::StatusCode::OK && status != reqwest::StatusCode::ACCEPTED {
            return Err(OmniBridgeError::HttpStatus(status.as_u16()));
        }
        let value = read_json_bounded(response).await?;
        parse_status_response(value)
    }

    /// GMI RequestQueue has no accepted-task cancellation contract.
    pub fn cancel_after_accept(&self, _handle: &PrivateTaskHandle) -> Result<(), OmniBridgeError> {
        Err(OmniBridgeError::NotCancellable)
    }

    pub async fn download_artifact(
        &self,
        handle: &PrivateTaskHandle,
        artifact: &ArtifactRef,
    ) -> Result<Vec<u8>, OmniBridgeError> {
        artifact.validate()?;
        ensure_path_segment(&handle.task_id, "task id")?;
        let response = self
            .client
            .get(format!(
                "{}/v1/jobs/{}/artifacts/{}/content",
                self.config.base_url, handle.task_id, artifact.id
            ))
            .bearer_auth(self.config.gateway_key.expose())
            .header("X-Task-Token", handle.task_token.expose())
            .header("X-Platform-Id", &self.config.platform_id)
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|error| OmniBridgeError::Transport(error.to_string()))?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(OmniBridgeError::RateLimited(retry_after_ms(
                response.headers(),
            )));
        }
        if response.status() != reqwest::StatusCode::OK {
            return Err(OmniBridgeError::HttpStatus(response.status().as_u16()));
        }
        if response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| !value.trim().is_empty() && value.trim() != "identity")
        {
            return Err(OmniBridgeError::Integrity(
                "artifact content encoding must be identity".to_owned(),
            ));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(normalized_mime)
            .ok_or_else(|| OmniBridgeError::Integrity("artifact MIME is missing".to_owned()))?;
        if content_type != normalized_mime(&artifact.content_type) {
            return Err(OmniBridgeError::Integrity(
                "artifact MIME does not match ArtifactRef".to_owned(),
            ));
        }
        if let Some(length) = response.content_length() {
            if length != artifact.bytes {
                return Err(OmniBridgeError::Integrity(
                    "artifact Content-Length does not match ArtifactRef".to_owned(),
                ));
            }
        }
        let mut response = response;
        let mut bytes = Vec::with_capacity(usize::try_from(artifact.bytes).unwrap_or(0));
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| OmniBridgeError::Transport(error.to_string()))?
        {
            let next_len = bytes.len().saturating_add(chunk.len());
            if next_len as u64 > artifact.bytes || next_len as u64 > MAX_ARTIFACT_BYTES {
                return Err(OmniBridgeError::Integrity(
                    "artifact body exceeds declared size".to_owned(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        verify_artifact_bytes(artifact, &content_type, &bytes)?;
        Ok(bytes)
    }
}

async fn read_json_bounded(response: reqwest::Response) -> Result<Value, OmniBridgeError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_JSON_BYTES as u64)
    {
        return Err(OmniBridgeError::Protocol(
            "OmniBridge JSON response exceeds the size limit".to_owned(),
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| OmniBridgeError::Transport(error.to_string()))?;
        if body.len().saturating_add(chunk.len()) > MAX_JSON_BYTES {
            return Err(OmniBridgeError::Protocol(
                "OmniBridge JSON response exceeds the size limit".to_owned(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(Into::into)
}

fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> u64 {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(5)
        .clamp(1, 300)
        .saturating_mul(1000)
}

fn validate_contract_catalog(value: &Value, expected_digest: &str) -> Result<(), OmniBridgeError> {
    let object = value.as_object().ok_or_else(|| {
        OmniBridgeError::Protocol("contract catalog must be an object".to_owned())
    })?;
    if object.get("schema").and_then(Value::as_str) != Some(CONTRACT_SCHEMA) {
        return Err(OmniBridgeError::Protocol(
            "unexpected OmniBridge contract schema".to_owned(),
        ));
    }
    if object
        .get("openapi")
        .and_then(Value::as_object)
        .and_then(|openapi| openapi.get("schema_digest"))
        .and_then(Value::as_str)
        != Some(expected_digest)
    {
        return Err(OmniBridgeError::Protocol(
            "OmniBridge schema digest does not match the configured digest".to_owned(),
        ));
    }
    let operation_ok = object
        .get("operations")
        .and_then(Value::as_array)
        .is_some_and(|operations| {
            operations.iter().any(|operation| {
                operation.get("id").and_then(Value::as_str) == Some(OPERATION)
                    && operation.get("transport").and_then(Value::as_str) == Some("durable")
                    && operation.get("path").and_then(Value::as_str) == Some("/v1/jobs")
                    && operation.get("no_replay").and_then(Value::as_bool) == Some(true)
            })
        });
    let durable = object.get("durable_jobs").and_then(Value::as_object);
    let durable_ok = durable.is_some_and(|durable| {
        durable.get("submit").and_then(Value::as_str) == Some("POST /v1/jobs")
            && durable.get("poll").and_then(Value::as_str) == Some("GET /v1/jobs/:taskId")
            && durable.get("artifact_download").and_then(Value::as_str)
                == Some("GET /v1/jobs/:taskId/artifacts/:artifactId/content")
            && durable.get("idempotency_header").and_then(Value::as_str) == Some("Idempotency-Key")
            && durable.get("platform_header").and_then(Value::as_str) == Some("X-Platform-Id")
            && durable.get("task_token_header").and_then(Value::as_str) == Some("X-Task-Token")
            && durable.get("recovery").and_then(Value::as_str) == Some("GET-only")
    });
    if !operation_ok || !durable_ok {
        return Err(OmniBridgeError::Protocol(
            "OmniBridge durable music contract is incomplete".to_owned(),
        ));
    }
    Ok(())
}

fn validate_request_route(
    request: &MusicSubmitRequest,
    configured_route: &str,
) -> Result<(), OmniBridgeError> {
    if request.operation != OPERATION
        || request.kind != KIND
        || request.model != configured_route
        || request.payload.model != configured_route
    {
        return Err(OmniBridgeError::InvalidInput(
            "music request does not match the configured OmniBridge route".to_owned(),
        ));
    }
    Ok(())
}

fn parse_submit_response(value: Value) -> Result<PrivateTaskHandle, OmniBridgeError> {
    let response: SubmitResponse = serde_json::from_value(value)?;
    ensure_path_segment(&response.id, "task id")?;
    Ok(PrivateTaskHandle {
        task_id: response.id,
        task_token: SecretString::new(response.task_token, "task_token")?,
    })
}

fn parse_status_response(value: Value) -> Result<OmniBridgeJobStatus, OmniBridgeError> {
    let object = value
        .as_object()
        .ok_or_else(|| OmniBridgeError::Protocol("job response must be an object".to_owned()))?;
    let task_id = object
        .get("id")
        .or_else(|| object.get("task_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    ensure_path_segment(&task_id, "task id")?;
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if status.is_empty() {
        return Err(OmniBridgeError::Protocol(
            "job response is missing status".to_owned(),
        ));
    }
    let artifacts_value = object
        .get("result")
        .and_then(Value::as_object)
        .and_then(|result| result.get("artifacts"))
        .or_else(|| object.get("artifacts"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let artifacts: Vec<ArtifactRef> = serde_json::from_value(artifacts_value)?;
    for artifact in &artifacts {
        artifact.validate()?;
    }
    Ok(OmniBridgeJobStatus {
        task_id,
        status,
        provider_status: object
            .get("provider_status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        poll_after_ms: object.get("poll_after_ms").and_then(Value::as_u64),
        artifacts,
    })
}

fn normalized_mime(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_schema_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(valid_sha256)
}

fn valid_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 200
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || byte == b'-'
                || byte == b'_'
                || byte == b'.'
                || byte == b':'
        })
}

fn ensure_path_segment(value: &str, name: &str) -> Result<(), OmniBridgeError> {
    if valid_path_segment(value) {
        Ok(())
    } else {
        Err(OmniBridgeError::Protocol(format!("invalid {name}")))
    }
}

fn audio_magic_matches(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "audio/wav" | "audio/x-wav" => {
            bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
        }
        "audio/ogg" => bytes.starts_with(b"OggS"),
        "audio/flac" => bytes.starts_with(b"fLaC"),
        "audio/mp4" => bytes.len() >= 12 && &bytes[4..8] == b"ftyp",
        "audio/aac" => bytes.len() >= 2 && bytes[0] == 0xff && (bytes[1] & 0xf6) == 0xf0,
        "audio/mpeg" => {
            bytes.starts_with(b"ID3")
                || (bytes.len() >= 2 && bytes[0] == 0xff && (bytes[1] & 0xe0) == 0xe0)
        }
        _ => false,
    }
}

fn verify_artifact_bytes(
    artifact: &ArtifactRef,
    mime: &str,
    bytes: &[u8],
) -> Result<(), OmniBridgeError> {
    if bytes.len() as u64 != artifact.bytes {
        return Err(OmniBridgeError::Integrity(
            "artifact byte count does not match ArtifactRef".to_owned(),
        ));
    }
    if hex_sha256(bytes) != artifact.sha256 {
        return Err(OmniBridgeError::Integrity(
            "artifact SHA-256 does not match ArtifactRef".to_owned(),
        ));
    }
    if !audio_magic_matches(mime, bytes) {
        return Err(OmniBridgeError::Integrity(
            "artifact bytes do not match the declared audio MIME".to_owned(),
        ));
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DurableMusicContext {
    pub caption: String,
    pub lyrics: String,
    pub duration_seconds: f64,
    pub title: Option<String>,
    pub cover_prompt: Option<String>,
    pub generation_settings: Value,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DurableSubmitState {
    IntentPersisted,
    Accepted,
    SubmissionUnknown,
    Rejected,
}

#[derive(Clone)]
pub struct DurableMusicRecord {
    local_job_id: String,
    payload_digest: String,
    idempotency_key: IdempotencyKey,
    submit_state: DurableSubmitState,
    handle: Option<PrivateTaskHandle>,
    status: Option<String>,
    poll_not_before_ms: Option<u64>,
    artifact: Option<ArtifactRef>,
    context: Option<DurableMusicContext>,
    imported_song_id: Option<String>,
}

impl DurableMusicRecord {
    pub fn local_job_id(&self) -> &str {
        &self.local_job_id
    }
    pub fn payload_digest(&self) -> &str {
        &self.payload_digest
    }
    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
    pub fn submit_state(&self) -> &DurableSubmitState {
        &self.submit_state
    }
    pub fn task_handle(&self) -> Option<&PrivateTaskHandle> {
        self.handle.as_ref()
    }
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }
    pub fn artifact(&self) -> Option<&ArtifactRef> {
        self.artifact.as_ref()
    }
    pub fn context(&self) -> Option<&DurableMusicContext> {
        self.context.as_ref()
    }
    pub fn imported_song_id(&self) -> Option<&str> {
        self.imported_song_id.as_deref()
    }
    pub fn poll_is_due(&self) -> bool {
        self.poll_not_before_ms
            .is_none_or(|deadline| now_ms() >= deadline)
    }
}

impl fmt::Debug for DurableMusicRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableMusicRecord")
            .field("local_job_id", &self.local_job_id)
            .field("payload_digest", &self.payload_digest)
            .field("idempotency_key", &self.idempotency_key)
            .field("submit_state", &self.submit_state)
            .field("handle", &self.handle)
            .field("status", &self.status)
            .field("poll_not_before_ms", &self.poll_not_before_ms)
            .field("artifact", &self.artifact)
            .field("context", &self.context)
            .field("imported_song_id", &self.imported_song_id)
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
struct StoredState {
    schema: String,
    records: Vec<StoredRecord>,
}

#[derive(Serialize, Deserialize)]
struct StoredRecord {
    local_job_id: String,
    payload_digest: String,
    idempotency_key: String,
    submit_state: DurableSubmitState,
    task_id: Option<String>,
    task_token: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    poll_not_before_ms: Option<u64>,
    #[serde(default)]
    artifact: Option<ArtifactRef>,
    #[serde(default)]
    context: Option<DurableMusicContext>,
    #[serde(default)]
    imported_song_id: Option<String>,
}

impl TryFrom<StoredRecord> for DurableMusicRecord {
    type Error = OmniBridgeError;

    fn try_from(record: StoredRecord) -> Result<Self, Self::Error> {
        let handle = match (record.task_id, record.task_token) {
            (Some(task_id), Some(task_token)) => {
                ensure_path_segment(&task_id, "task id")?;
                Some(PrivateTaskHandle {
                    task_id,
                    task_token: SecretString::new(task_token, "task_token")?,
                })
            }
            (None, None) => None,
            _ => {
                return Err(OmniBridgeError::Protocol(
                    "sidecar task handle is incomplete".to_owned(),
                ));
            }
        };
        if !valid_sha256(&record.payload_digest) {
            return Err(OmniBridgeError::Protocol(
                "sidecar payload digest is invalid".to_owned(),
            ));
        }
        if let Some(artifact) = &record.artifact {
            artifact.validate()?;
        }
        Ok(Self {
            local_job_id: record.local_job_id,
            payload_digest: record.payload_digest,
            idempotency_key: IdempotencyKey::new(record.idempotency_key)?,
            submit_state: record.submit_state,
            handle,
            status: record.status,
            poll_not_before_ms: record.poll_not_before_ms,
            artifact: record.artifact,
            context: record.context,
            imported_song_id: record.imported_song_id,
        })
    }
}

impl From<&DurableMusicRecord> for StoredRecord {
    fn from(record: &DurableMusicRecord) -> Self {
        Self {
            local_job_id: record.local_job_id.clone(),
            payload_digest: record.payload_digest.clone(),
            idempotency_key: record.idempotency_key.as_str().to_owned(),
            submit_state: record.submit_state.clone(),
            task_id: record.handle.as_ref().map(|handle| handle.task_id.clone()),
            task_token: record
                .handle
                .as_ref()
                .map(|handle| handle.task_token.expose().to_owned()),
            status: record.status.clone(),
            poll_not_before_ms: record.poll_not_before_ms,
            artifact: record.artifact.clone(),
            context: record.context.clone(),
            imported_song_id: record.imported_song_id.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OmniBridgeMusicStore {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub enum PrepareIntentOutcome {
    Created(DurableMusicRecord),
    Existing(DurableMusicRecord),
}

impl PrepareIntentOutcome {
    pub fn into_record(self) -> DurableMusicRecord {
        match self {
            Self::Created(record) | Self::Existing(record) => record,
        }
    }
}

impl OmniBridgeMusicStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn get(&self, local_job_id: &str) -> Result<Option<DurableMusicRecord>, OmniBridgeError> {
        Ok(self
            .load_all()?
            .into_iter()
            .find(|record| record.local_job_id == local_job_id))
    }

    pub fn list(&self) -> Result<Vec<DurableMusicRecord>, OmniBridgeError> {
        self.load_all()
    }

    /// Persists intent before the caller is allowed to invoke `submit_once`.
    pub fn prepare_intent(
        &self,
        local_job_id: impl Into<String>,
        request: &MusicSubmitRequest,
        idempotency_key: IdempotencyKey,
        context: DurableMusicContext,
    ) -> Result<DurableMusicRecord, OmniBridgeError> {
        self.prepare_intent_once(local_job_id, request, idempotency_key, context)
            .map(PrepareIntentOutcome::into_record)
    }

    /// Atomically claims the one allowed submit for a stable local job ID.
    /// An existing identical intent never grants a second submit permission.
    pub fn prepare_intent_once(
        &self,
        local_job_id: impl Into<String>,
        request: &MusicSubmitRequest,
        idempotency_key: IdempotencyKey,
        context: DurableMusicContext,
    ) -> Result<PrepareIntentOutcome, OmniBridgeError> {
        let local_job_id = local_job_id.into().trim().to_owned();
        if local_job_id.is_empty() {
            return Err(OmniBridgeError::InvalidInput(
                "local job id is required".to_owned(),
            ));
        }
        let digest = request.payload_digest()?;
        let mut records = self.load_all()?;
        if let Some(existing) = records
            .iter()
            .find(|record| record.local_job_id == local_job_id)
        {
            if existing.payload_digest == digest && existing.idempotency_key == idempotency_key {
                return Ok(PrepareIntentOutcome::Existing(existing.clone()));
            }
            return Err(OmniBridgeError::InvalidInput(
                "local job id already owns a different OmniBridge intent".to_owned(),
            ));
        }
        if records
            .iter()
            .any(|record| record.idempotency_key == idempotency_key)
        {
            return Err(OmniBridgeError::InvalidInput(
                "Idempotency-Key already belongs to another local job".to_owned(),
            ));
        }
        let record = DurableMusicRecord {
            local_job_id,
            payload_digest: digest,
            idempotency_key,
            submit_state: DurableSubmitState::IntentPersisted,
            handle: None,
            status: None,
            poll_not_before_ms: None,
            artifact: None,
            context: Some(context),
            imported_song_id: None,
        };
        records.push(record.clone());
        self.save_all(&records)?;
        Ok(PrepareIntentOutcome::Created(record))
    }

    pub fn mark_accepted(
        &self,
        local_job_id: &str,
        handle: PrivateTaskHandle,
    ) -> Result<DurableMusicRecord, OmniBridgeError> {
        self.update(local_job_id, move |record| {
            record.submit_state = DurableSubmitState::Accepted;
            record.handle = Some(handle);
            Ok(())
        })
    }

    pub fn mark_submission_unknown(
        &self,
        local_job_id: &str,
    ) -> Result<DurableMusicRecord, OmniBridgeError> {
        self.update(local_job_id, |record| {
            if record.handle.is_some() {
                return Err(OmniBridgeError::InvalidInput(
                    "an accepted job cannot become submission_unknown".to_owned(),
                ));
            }
            record.submit_state = DurableSubmitState::SubmissionUnknown;
            Ok(())
        })
    }

    pub fn mark_rejected(
        &self,
        local_job_id: &str,
        status: impl Into<String>,
    ) -> Result<DurableMusicRecord, OmniBridgeError> {
        let status = status.into();
        self.update(local_job_id, move |record| {
            record.submit_state = DurableSubmitState::Rejected;
            record.status = Some(status);
            Ok(())
        })
    }

    pub fn update_status(
        &self,
        local_job_id: &str,
        status: &OmniBridgeJobStatus,
    ) -> Result<DurableMusicRecord, OmniBridgeError> {
        self.update(local_job_id, |record| {
            let Some(handle) = &record.handle else {
                return Err(OmniBridgeError::InvalidInput(
                    "cannot store status before an accepted task handle".to_owned(),
                ));
            };
            if handle.task_id != status.task_id {
                return Err(OmniBridgeError::InvalidInput(
                    "status belongs to another OmniBridge task".to_owned(),
                ));
            }
            record.status = Some(status.status.clone());
            record.poll_not_before_ms = status
                .poll_after_ms
                .map(|delay| now_ms().saturating_add(delay));
            if let Some(artifact) = status.artifacts.first() {
                record.artifact = Some(artifact.clone());
            }
            Ok(())
        })
    }

    pub fn defer_poll(
        &self,
        local_job_id: &str,
        delay_ms: u64,
    ) -> Result<DurableMusicRecord, OmniBridgeError> {
        self.update(local_job_id, |record| {
            if record.handle.is_none() {
                return Err(OmniBridgeError::InvalidInput(
                    "cannot defer polling before an accepted task handle".to_owned(),
                ));
            }
            record.poll_not_before_ms =
                Some(now_ms().saturating_add(delay_ms.clamp(1000, 300_000)));
            Ok(())
        })
    }

    pub fn mark_imported(
        &self,
        local_job_id: &str,
        song_id: impl Into<String>,
    ) -> Result<DurableMusicRecord, OmniBridgeError> {
        let song_id = song_id.into();
        self.update(local_job_id, move |record| {
            record.imported_song_id = Some(song_id);
            Ok(())
        })
    }

    fn update(
        &self,
        local_job_id: &str,
        mutate: impl FnOnce(&mut DurableMusicRecord) -> Result<(), OmniBridgeError>,
    ) -> Result<DurableMusicRecord, OmniBridgeError> {
        let mut records = self.load_all()?;
        let record = records
            .iter_mut()
            .find(|record| record.local_job_id == local_job_id)
            .ok_or_else(|| {
                OmniBridgeError::InvalidInput("OmniBridge intent not found".to_owned())
            })?;
        mutate(record)?;
        let updated = record.clone();
        self.save_all(&records)?;
        Ok(updated)
    }

    fn load_all(&self) -> Result<Vec<DurableMusicRecord>, OmniBridgeError> {
        let backup = self.path.with_extension("backup");
        let source = if self.path.exists() {
            &self.path
        } else if backup.exists() {
            &backup
        } else {
            return Ok(Vec::new());
        };
        let mut bytes = Vec::new();
        File::open(source)?.read_to_end(&mut bytes)?;
        let state: StoredState = serde_json::from_slice(&bytes)?;
        if state.schema != STORE_SCHEMA {
            return Err(OmniBridgeError::Protocol(
                "unsupported OmniBridge sidecar schema".to_owned(),
            ));
        }
        state.records.into_iter().map(TryInto::try_into).collect()
    }

    fn save_all(&self, records: &[DurableMusicRecord]) -> Result<(), OmniBridgeError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let state = StoredState {
            schema: STORE_SCHEMA.to_owned(),
            records: records.iter().map(StoredRecord::from).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&state)?;
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = self
            .path
            .with_extension(format!("tmp-{}-{suffix}", std::process::id()));
        let result = (|| -> Result<(), OmniBridgeError> {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            let backup = self.path.with_extension("backup");
            let moved_primary = if self.path.exists() {
                if backup.exists() {
                    fs::remove_file(&backup)?;
                }
                fs::rename(&self.path, &backup)?;
                true
            } else {
                false
            };
            if let Err(error) = fs::rename(&temporary, &self.path) {
                if moved_primary {
                    let _ = fs::rename(&backup, &self.path);
                }
                return Err(error.into());
            }
            if backup.exists() {
                fs::remove_file(backup)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEST_SCHEMA_DIGEST: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::AsyncReadExt;

    fn config() -> OmniBridgeConfig {
        OmniBridgeConfig::new(
            "http://127.0.0.1:8787",
            "gateway-secret-value",
            "music-maker-test",
            "route:music:minimax-3",
            TEST_SCHEMA_DIGEST,
        )
        .unwrap()
    }

    fn request() -> MusicSubmitRequest {
        MusicSubmitRequest::new(
            "route:music:minimax-3",
            "soft electronic pop",
            "[Verse]\nSafe test lyric",
            "mp3",
            44_100,
            256_000,
        )
        .unwrap()
    }

    fn context() -> DurableMusicContext {
        DurableMusicContext {
            caption: "soft electronic pop".into(),
            lyrics: "[Verse]\nSafe test lyric".into(),
            duration_seconds: 30.0,
            title: Some("Test".into()),
            cover_prompt: None,
            generation_settings: serde_json::json!({"format":"mp3"}),
        }
    }

    fn handle() -> PrivateTaskHandle {
        PrivateTaskHandle {
            task_id: "job_123".to_owned(),
            task_token: SecretString::new("private-task-token".to_owned(), "task_token").unwrap(),
        }
    }

    #[test]
    fn contract_catalog_is_fail_closed_and_paths_reject_dot_segments() {
        let mut catalog = serde_json::json!({
            "schema": CONTRACT_SCHEMA,
            "openapi": {"schema_digest": TEST_SCHEMA_DIGEST},
            "operations": [{
                "id": OPERATION,
                "transport": "durable",
                "path": "/v1/jobs",
                "no_replay": true
            }],
            "durable_jobs": {
                "submit": "POST /v1/jobs",
                "poll": "GET /v1/jobs/:taskId",
                "artifact_download": "GET /v1/jobs/:taskId/artifacts/:artifactId/content",
                "idempotency_header": "Idempotency-Key",
                "platform_header": "X-Platform-Id",
                "task_token_header": "X-Task-Token",
                "recovery": "GET-only"
            }
        });
        validate_contract_catalog(&catalog, TEST_SCHEMA_DIGEST).unwrap();
        catalog["operations"][0]["no_replay"] = Value::Bool(false);
        assert!(validate_contract_catalog(&catalog, TEST_SCHEMA_DIGEST).is_err());
        assert!(validate_contract_catalog(
            &serde_json::json!({
                "schema": CONTRACT_SCHEMA,
                "openapi": {"schema_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
                "operations": [],
                "durable_jobs": {}
            }),
            TEST_SCHEMA_DIGEST,
        ).is_err());
        assert!(!valid_path_segment("."));
        assert!(!valid_path_segment(".."));
        assert!(valid_path_segment("job_123"));
    }

    #[test]
    fn configuration_debug_redacts_gateway_key_and_route_has_no_default() {
        let debug = format!("{:?}", config());
        assert!(!debug.contains("gateway-secret-value"));
        assert!(debug.contains("<redacted>"));
        let missing = OmniBridgeConfig::new(
            "http://127.0.0.1:8787",
            "key",
            "music-maker",
            "",
            TEST_SCHEMA_DIGEST,
        )
        .unwrap_err();
        assert!(matches!(missing, OmniBridgeError::NotReady(_)));
    }

    #[test]
    fn music_wire_contract_is_route_scoped_and_fails_closed_on_empty_lyrics() {
        let value = serde_json::to_value(request()).unwrap();
        assert_eq!(value["operation"], OPERATION);
        assert_eq!(value["kind"], KIND);
        assert_eq!(value["model"], "route:music:minimax-3");
        assert_eq!(value["payload"]["model"], "route:music:minimax-3");
        assert_eq!(value["payload"]["format"], "mp3");
        assert_eq!(value["payload"]["sample_rate"], 44_100);
        assert!(
            MusicSubmitRequest::new(
                "route:music:minimax-3",
                "instrumental",
                "",
                "mp3",
                44_100,
                256_000,
            )
            .is_err()
        );
    }

    #[test]
    fn submit_response_accepts_id_and_task_id_without_exposing_token_in_debug() {
        for value in [
            serde_json::json!({"id":"job_123","task_token":"private-task-token"}),
            serde_json::json!({"task_id":"job_123","task_token":"private-task-token"}),
        ] {
            let parsed = parse_submit_response(value).unwrap();
            assert_eq!(parsed.task_id(), "job_123");
            let debug = format!("{parsed:?}");
            assert!(!debug.contains("private-task-token"));
            assert!(debug.contains("<redacted>"));
        }
    }

    #[test]
    fn artifact_validation_checks_bytes_mime_magic_and_sha256() {
        let bytes = b"ID3unit-test-audio";
        let artifact = ArtifactRef {
            schema: ARTIFACT_SCHEMA.to_owned(),
            id: "audio-output".to_owned(),
            content_type: "audio/mpeg".to_owned(),
            bytes: bytes.len() as u64,
            sha256: hex_sha256(bytes),
            availability: "metadata-only".to_owned(),
        };
        artifact.validate().unwrap();
        verify_artifact_bytes(&artifact, "audio/mpeg", bytes).unwrap();
        assert!(verify_artifact_bytes(&artifact, "audio/mpeg", b"ID3tampered").is_err());
        assert!(verify_artifact_bytes(&artifact, "audio/wav", bytes).is_err());
    }

    #[test]
    fn durable_store_round_trips_private_handle_and_rejects_conflicting_intent() {
        let directory = env::temp_dir().join(format!(
            "music-maker-omnibridge-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = directory.join("jobs.json");
        let store = OmniBridgeMusicStore::new(&path);
        let key = IdempotencyKey::new("music-maker:local-1:v1").unwrap();
        let first = store
            .prepare_intent("local-1", &request(), key.clone(), context())
            .unwrap();
        assert_eq!(first.submit_state(), &DurableSubmitState::IntentPersisted);
        let accepted = store.mark_accepted("local-1", handle()).unwrap();
        assert_eq!(accepted.submit_state(), &DurableSubmitState::Accepted);
        let loaded = store.get("local-1").unwrap().unwrap();
        assert_eq!(loaded.task_handle().unwrap().task_id(), "job_123");
        assert!(!format!("{loaded:?}").contains("private-task-token"));
        assert!(
            store
                .prepare_intent(
                    "local-1",
                    &MusicSubmitRequest::new(
                        "route:music:minimax-3",
                        "different prompt",
                        "different lyric",
                        "mp3",
                        44_100,
                        256_000,
                    )
                    .unwrap(),
                    key,
                    context(),
                )
                .is_err()
        );
        assert!(
            store
                .prepare_intent(
                    "local-2",
                    &MusicSubmitRequest::new(
                        "route:music:minimax-3",
                        "another prompt",
                        "another lyric",
                        "mp3",
                        44_100,
                        256_000,
                    )
                    .unwrap(),
                    IdempotencyKey::new("music-maker:local-1:v1").unwrap(),
                    context(),
                )
                .is_err()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn identical_intent_grants_submit_permission_only_once() {
        let directory = env::temp_dir().join(format!(
            "music-maker-omnibridge-claim-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = OmniBridgeMusicStore::new(directory.join("jobs.json"));
        let key = IdempotencyKey::new("music-maker:claim-once:v1").unwrap();
        let first = store
            .prepare_intent_once("claim-once", &request(), key.clone(), context())
            .unwrap();
        let second = store
            .prepare_intent_once("claim-once", &request(), key, context())
            .unwrap();
        assert!(matches!(first, PrepareIntentOutcome::Created(_)));
        assert!(matches!(second, PrepareIntentOutcome::Existing(_)));
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn ambiguous_transport_posts_once_and_restart_recovery_never_replays() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let post_count = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&post_count);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            observed.fetch_add(1, Ordering::SeqCst);
            let mut bytes = vec![0_u8; 8 * 1024];
            let read = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..read]);
            assert!(request.starts_with("POST /v1/jobs HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("idempotency-key: music-maker:fake-http:v1")
            );
            // Drop the socket without an HTTP response: the POST may have reached
            // the gateway, so the only safe client result is SubmissionUnknown.
        });
        let config = OmniBridgeConfig::new(
            format!("http://{address}"),
            "gateway-secret-value",
            "music-maker-test",
            "route:music:minimax-3",
            TEST_SCHEMA_DIGEST,
        )
        .unwrap();
        let client = OmniBridgeMusicClient::new(config).unwrap();
        let key = IdempotencyKey::new("music-maker:fake-http:v1").unwrap();
        let error = client.submit_once(&request(), &key).await.unwrap_err();
        assert!(matches!(error, OmniBridgeError::SubmissionUnknown(_)));
        server.await.unwrap();
        assert_eq!(post_count.load(Ordering::SeqCst), 1);

        let directory = env::temp_dir().join(format!(
            "music-maker-omnibridge-unknown-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = OmniBridgeMusicStore::new(directory.join("jobs.json"));
        store
            .prepare_intent("local-unknown", &request(), key, context())
            .unwrap();
        store.mark_submission_unknown("local-unknown").unwrap();
        let restarted = OmniBridgeMusicStore::new(directory.join("jobs.json"));
        let record = restarted.get("local-unknown").unwrap().unwrap();
        assert_eq!(
            record.submit_state(),
            &DurableSubmitState::SubmissionUnknown
        );
        assert!(record.task_handle().is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    fn text_config() -> OmniBridgeTextConfig {
        OmniBridgeTextConfig::new(
            "http://127.0.0.1:8787",
            "text-gateway-secret",
            "music-maker-client",
            "music-maker-platform",
            "music-maker",
            "route:text:fast",
            "route:text:quality",
        )
        .unwrap()
    }

    #[test]
    fn text_routes_are_isolated_and_missing_required_routes_fail_closed() {
        let config = text_config();
        assert_eq!(config.route(TextRoute::Fast), "route:text:fast");
        assert_eq!(config.route(TextRoute::Quality), "route:text:quality");
        assert!(!config.route(TextRoute::Fast).starts_with("route:music:"));

        let missing = OmniBridgeTextConfig::new(
            "http://127.0.0.1:8787",
            "key",
            "music-maker-client",
            "music-maker-platform",
            "music-maker",
            "",
            "route:text:quality",
        )
        .unwrap_err();
        assert!(matches!(missing, OmniBridgeError::NotReady(_)));

        let wrong_family = OmniBridgeTextConfig::new(
            "http://127.0.0.1:8787",
            "key",
            "music-maker-client",
            "music-maker-platform",
            "music-maker",
            "route:music:default",
            "route:text:quality",
        )
        .unwrap_err();
        assert!(matches!(wrong_family, OmniBridgeError::InvalidInput(_)));
    }

    #[test]
    fn text_configuration_debug_redacts_gateway_key() {
        let config = text_config();
        let debug = format!("{config:?}");
        assert!(!debug.contains("text-gateway-secret"));
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("route:text:fast"));
        assert!(debug.contains("route:text:quality"));
        let status = config.public_status().to_string();
        assert!(!status.contains("text-gateway-secret"));
        assert!(!status.contains("base_url"));
    }

    #[test]
    fn receipt_parser_keeps_only_the_sdk_allowlist() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-omnibridge-request-id", "req-1".parse().unwrap());
        headers.insert("x-omnibridge-selector-type", "route".parse().unwrap());
        headers.insert(
            "x-omnibridge-resolved-provider",
            "provider-safe".parse().unwrap(),
        );
        headers.insert(
            "x-omnibridge-resolved-deployment",
            "deployment:provider-safe:text".parse().unwrap(),
        );
        headers.insert(
            "x-omnibridge-resolved-model",
            "upstream-model".parse().unwrap(),
        );
        headers.insert("x-omnibridge-route-id", "route:text:quality".parse().unwrap());
        headers.insert("x-omnibridge-route-revision", "revision-1".parse().unwrap());
        headers.insert("x-omnibridge-attempt", "2".parse().unwrap());
        headers.insert(reqwest::header::AUTHORIZATION, "Bearer never-copy".parse().unwrap());
        headers.insert("x-untrusted-provider-secret", "never-copy".parse().unwrap());

        let receipt = parse_request_receipt(&headers).unwrap();
        assert_eq!(receipt.schema, REQUEST_RECEIPT_SCHEMA);
        assert_eq!(receipt.request_id, "req-1");
        assert_eq!(receipt.route_id.as_deref(), Some("route:text:quality"));
        assert_eq!(receipt.attempt, Some(2));
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(!json.contains("never-copy"));
        assert!(!json.contains("authorization"));
    }

    #[test]
    fn text_request_body_overrides_selector_and_stream_mode() {
        let input = serde_json::json!({
            "model": "deployment:must-not-escape",
            "stream": false,
            "messages": [{"role": "user", "content": "hello"}]
        });
        let body = text_request_body(&input, "route:text:quality", true).unwrap();
        assert_eq!(body["model"], "route:text:quality");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["content"], "hello");
    }
}
