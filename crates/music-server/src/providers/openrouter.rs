//! OpenRouter discovery and request construction.
//!
//! Model identifiers intentionally never live in this module.  The settings UI
//! must refresh `GET /api/v1/models`, let the user select a discovered model,
//! then persist that selected id in the provider profile.

use std::{collections::BTreeSet, env};

use anyhow::{bail, Context, Result};
use music_core::Capability;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const API_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const MODELS_PATH: &str = "/models";
pub const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
pub const TRANSCRIPTIONS_PATH: &str = "/audio/transcriptions";
pub const IMAGES_PATH: &str = "/images";

/// A serializable request for the HTTP client owned by the server shell.
/// Keeping transport outside the adapter prevents secrets from being stored in
/// a model registry or accidentally emitted in logs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenRouterRequest {
    pub method: HttpMethod,
    pub path: &'static str,
    pub body: Value,
}

/// The credential is intentionally non-serializable and is only obtained by a
/// server handler immediately before it calls OpenRouter. It must never cross
/// the Tauri boundary or become part of a persisted provider selection.
pub struct AuthenticatedOpenRouterRequest {
    pub request: OpenRouterRequest,
    pub api_key: String,
}

pub type OpenRouterMusicStreamRequest = AuthenticatedOpenRouterRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base64AudioInput<'a> {
    /// Raw base64 bytes, without a data-URL prefix.
    pub data: &'a str,
    pub format: &'a str,
    pub language: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelsResponse {
    #[serde(default)]
    pub data: Vec<RemoteModel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteModel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub supported_parameters: Vec<String>,
    #[serde(default)]
    pub pricing: Option<ModelPricing>,
    #[serde(default)]
    pub architecture: ModelArchitecture,
}

/// Prices are decimal strings in the upstream API. Preserve that representation
/// so the UI never introduces floating-point rounding into cost estimates.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ModelPricing {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub completion: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub request: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelArchitecture {
    #[serde(default)]
    pub modality: Option<String>,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub output_modalities: Vec<String>,
}

/// User-facing model metadata derived exclusively from the current OpenRouter
/// catalog. `capabilities` means *eligible by declared I/O modalities*, not a
/// guarantee that the model provides a particular product feature.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub context_length: Option<u64>,
    pub modality: Option<String>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub supported_parameters: Vec<String>,
    pub pricing: Option<ModelPricing>,
    pub capabilities: Vec<Capability>,
}

impl CatalogModel {
    fn from_remote(model: RemoteModel) -> Self {
        let capabilities = infer_capabilities(&model);
        Self {
            id: model.id,
            name: model.name,
            description: model.description,
            context_length: model.context_length,
            modality: model.architecture.modality,
            input_modalities: normalized(model.architecture.input_modalities),
            output_modalities: normalized(model.architecture.output_modalities),
            supported_parameters: normalized(model.supported_parameters),
            pricing: model.pricing,
            capabilities,
        }
    }

    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityCatalog {
    pub models: Vec<CatalogModel>,
}

impl CapabilityCatalog {
    pub fn from_models_response(response: ModelsResponse) -> Self {
        let mut models: Vec<_> = response
            .data
            .into_iter()
            .map(CatalogModel::from_remote)
            .filter(|model| !model.capabilities.is_empty())
            .collect();
        models.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        Self { models }
    }

    pub fn parse(body: &str) -> Result<Self> {
        let response: ModelsResponse = serde_json::from_str(body).context("invalid OpenRouter models response")?;
        Ok(Self::from_models_response(response))
    }

    pub fn models_for(&self, capability: Capability) -> impl Iterator<Item = &CatalogModel> {
        self.models.iter().filter(move |model| model.supports(capability))
    }

    pub fn selected(&self, capability: Capability, model_id: &str) -> Result<&CatalogModel> {
        let model = self
            .models
            .iter()
            .find(|model| model.id == model_id)
            .with_context(|| format!("OpenRouter model '{model_id}' is not present in the refreshed catalog"))?;
        if !model.supports(capability) {
            bail!("OpenRouter model '{model_id}' does not declare support for {capability:?}");
        }
        Ok(model)
    }
}

pub fn models_request() -> OpenRouterRequest {
    OpenRouterRequest {
        method: HttpMethod::Get,
        path: MODELS_PATH,
        body: Value::Null,
    }
}

/// Build the exact request shape for a catalog-backed selection. The HTTP
/// layer supplies the Authorization bearer credential from secure storage.
pub fn request_for(
    catalog: &CapabilityCatalog,
    capability: Capability,
    model_id: &str,
    prompt: &str,
) -> Result<OpenRouterRequest> {
    let model = catalog.selected(capability, model_id)?;
    let prompt = prompt.trim();
    if prompt.is_empty() {
        bail!("OpenRouter prompt must not be empty");
    }

    let (path, body) = match capability {
        Capability::PromptEnhancement => (
            CHAT_COMPLETIONS_PATH,
            json!({
                "model": model.id,
                "messages": [{ "role": "user", "content": prompt }],
                "stream": false,
            }),
        ),
        Capability::CoverArt => (
            IMAGES_PATH,
            json!({ "model": model.id, "prompt": prompt }),
        ),
        Capability::SpeechToText => bail!(
            "use stt_request_for with base64 audio; a text prompt is not a valid OpenRouter transcription input"
        ),
        Capability::MusicGeneration => (
            CHAT_COMPLETIONS_PATH,
            json!({
                "model": model.id,
                "messages": [{ "role": "user", "content": prompt }],
                "stream": true,
                "modalities": ["text", "audio"],
                "audio": { "format": "wav" },
            }),
        ),
    };

    Ok(OpenRouterRequest {
        method: HttpMethod::Post,
        path,
        body,
    })
}

/// Builds the documented OpenRouter streaming chat request for a catalog
/// verified music model. The caller must attach this key as a Bearer token and
/// consume SSE audio chunks; this adapter never starts a paid generation.
pub fn music_stream_request_for(
    catalog: &CapabilityCatalog,
    model_id: &str,
    prompt: &str,
) -> Result<OpenRouterMusicStreamRequest> {
    authenticated_request_for(
        request_for(catalog, Capability::MusicGeneration, model_id, prompt)?,
    )
}

/// Add the locally-held credential to an already catalog-validated request.
/// The API key source is deliberately centralized so every cloud capability
/// follows the same boundary and never accepts secrets from the frontend.
pub fn authenticated_request_for(request: OpenRouterRequest) -> Result<AuthenticatedOpenRouterRequest> {
    let api_key = env::var("OPENROUTER_API_KEY")
        .context("OPENROUTER_API_KEY is required before calling OpenRouter")?;
    if api_key.trim().is_empty() {
        bail!("OPENROUTER_API_KEY is empty; configure an API key before calling OpenRouter");
    }
    Ok(AuthenticatedOpenRouterRequest { request, api_key })
}

/// Construct the JSON request specified by OpenRouter's `/audio/transcriptions`
/// endpoint. Audio bytes are supplied by the media layer and must not be put
/// into the provider profile.
pub fn stt_request_for(
    catalog: &CapabilityCatalog,
    model_id: &str,
    audio: Base64AudioInput<'_>,
) -> Result<OpenRouterRequest> {
    catalog.selected(Capability::SpeechToText, model_id)?;
    if audio.data.trim().is_empty() || audio.format.trim().is_empty() {
        bail!("OpenRouter transcription requires base64 audio data and an audio format");
    }
    let mut body = json!({
        "model": model_id,
        "input_audio": { "data": audio.data, "format": audio.format },
    });
    if let Some(language) = audio.language.filter(|language| !language.trim().is_empty()) {
        body["language"] = Value::String(language.to_owned());
    }
    Ok(OpenRouterRequest {
        method: HttpMethod::Post,
        path: TRANSCRIPTIONS_PATH,
        body,
    })
}

fn infer_capabilities(model: &RemoteModel) -> Vec<Capability> {
    let input: BTreeSet<_> = normalized(model.architecture.input_modalities.clone()).into_iter().collect();
    let output: BTreeSet<_> = normalized(model.architecture.output_modalities.clone()).into_iter().collect();
    let mut capabilities = Vec::new();

    if input.contains("text") && output.contains("text") {
        capabilities.push(Capability::PromptEnhancement);
    }
    if input.contains("audio") && output.contains("text") {
        capabilities.push(Capability::SpeechToText);
    }
    if input.contains("text") && output.contains("image") {
        capabilities.push(Capability::CoverArt);
    }
    // Generic audio is insufficient: it may mean TTS. OpenRouter's models API
    // exposes the needed product evidence in title/description for models such
    // as music generators, while the streaming endpoint remains common.
    if input.contains("text") && output.contains("audio") && music_evidence(model) {
        capabilities.push(Capability::MusicGeneration);
    }
    capabilities
}

fn music_evidence(model: &RemoteModel) -> bool {
    let text = format!("{} {}", model.name, model.description.as_deref().unwrap_or_default()).to_ascii_lowercase();
    ["music generation", "music generator", "generate music", "full-length song", "full length song", "songs", "song generation"]
        .iter()
        .any(|needle| text.contains(needle))
}

fn normalized(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_derives_only_declared_capabilities() {
        let catalog = CapabilityCatalog::parse(
            r#"{"data":[
                {"id":"catalog/text","name":"Text","architecture":{"input_modalities":["text"],"output_modalities":["text"]}},
                {"id":"catalog/stt","name":"STT","architecture":{"input_modalities":["audio"],"output_modalities":["text"]}},
                {"id":"catalog/image","name":"Image","architecture":{"input_modalities":["text"],"output_modalities":["image"]}},
                {"id":"catalog/audio","name":"Audio TTS","description":"Speech synthesis","architecture":{"input_modalities":["text"],"output_modalities":["audio"]}},
                {"id":"catalog/music","name":"Lyria Preview","description":"Generate full-length songs from text prompts.","architecture":{"input_modalities":["text"],"output_modalities":["audio"]}}
            ]}"#,
        )
        .unwrap();

        assert_eq!(catalog.models_for(Capability::PromptEnhancement).count(), 1);
        assert_eq!(catalog.models_for(Capability::SpeechToText).count(), 1);
        assert_eq!(catalog.models_for(Capability::CoverArt).count(), 1);
        assert_eq!(catalog.models_for(Capability::MusicGeneration).count(), 1);
    }

    #[test]
    fn music_request_uses_documented_streaming_chat_shape() {
        let catalog = CapabilityCatalog::parse(r#"{"data":[{"id":"catalog/music","name":"Song creator","description":"Music generation for full-length songs","architecture":{"input_modalities":["text"],"output_modalities":["audio"]}}]}"#).unwrap();
        let request = request_for(&catalog, Capability::MusicGeneration, "catalog/music", "dream pop").unwrap();
        assert_eq!(request.path, CHAT_COMPLETIONS_PATH);
        assert_eq!(request.body["stream"], true);
        assert_eq!(request.body["modalities"], json!(["text", "audio"]));
    }

    #[test]
    fn audio_only_tts_is_not_music() {
        let catalog = CapabilityCatalog::parse(r#"{"data":[{"id":"catalog/tts","name":"Voice TTS","description":"Natural speech synthesis","architecture":{"input_modalities":["text"],"output_modalities":["audio"]}}]}"#).unwrap();
        assert_eq!(catalog.models_for(Capability::MusicGeneration).count(), 0);
    }

    #[test]
    fn request_requires_a_refreshed_eligible_model() {
        let catalog = CapabilityCatalog::parse(
            r#"{"data":[{"id":"catalog/text","name":"Text","architecture":{"input_modalities":["text"],"output_modalities":["text"]}}]}"#,
        )
        .unwrap();

        let request = request_for(&catalog, Capability::PromptEnhancement, "catalog/text", "make a chorus").unwrap();
        assert_eq!(request.path, CHAT_COMPLETIONS_PATH);
        assert_eq!(request.body["model"], "catalog/text");
        assert!(request_for(&catalog, Capability::CoverArt, "catalog/text", "cover").is_err());
    }

    #[test]
    fn transcription_uses_openrouter_audio_input_shape() {
        let catalog = CapabilityCatalog::parse(
            r#"{"data":[{"id":"catalog/stt","name":"STT","architecture":{"input_modalities":["audio"],"output_modalities":["text"]}}]}"#,
        )
        .unwrap();
        let request = stt_request_for(
            &catalog,
            "catalog/stt",
            Base64AudioInput { data: "UklGRg==", format: "wav", language: Some("en") },
        )
        .unwrap();
        assert_eq!(request.path, TRANSCRIPTIONS_PATH);
        assert_eq!(request.body["input_audio"]["format"], "wav");
        assert_eq!(request.body["language"], "en");
    }

    #[test]
    fn cover_uses_the_dedicated_image_api_and_a_catalog_selected_model() {
        let catalog = CapabilityCatalog::parse(
            r#"{"data":[{"id":"catalog/image","name":"Image","architecture":{"input_modalities":["text"],"output_modalities":["image"]}}]}"#,
        )
        .unwrap();

        let request = request_for(&catalog, Capability::CoverArt, "catalog/image", "retro album cover").unwrap();
        assert_eq!(request.path, IMAGES_PATH);
        assert_eq!(request.body, json!({"model":"catalog/image","prompt":"retro album cover"}));
    }

    #[test]
    fn transcription_rejects_a_model_that_is_not_declared_for_audio_to_text() {
        let catalog = CapabilityCatalog::parse(
            r#"{"data":[{"id":"catalog/image","name":"Image","architecture":{"input_modalities":["text"],"output_modalities":["image"]}}]}"#,
        )
        .unwrap();

        assert!(stt_request_for(
            &catalog,
            "catalog/image",
            Base64AudioInput { data: "UklGRg==", format: "wav", language: None },
        )
        .is_err());
    }
}
