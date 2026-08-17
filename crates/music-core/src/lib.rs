use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Local,
    OpenRouter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    MusicGeneration,
    SpeechToText,
    PromptEnhancement,
    CoverArt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSelection {
    pub capability: Capability,
    pub mode: ExecutionMode,
    pub local_engine: Option<String>,
    pub cloud_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudioConfiguration {
    pub selections: Vec<ProviderSelection>,
}

impl Default for StudioConfiguration {
    fn default() -> Self {
        Self {
            selections: vec![
                ProviderSelection {
                    capability: Capability::MusicGeneration,
                    mode: ExecutionMode::Local,
                    local_engine: Some("minimaxmusic-cpp".into()),
                    cloud_model: None,
                },
                ProviderSelection {
                    capability: Capability::SpeechToText,
                    mode: ExecutionMode::Local,
                    local_engine: Some("parakeet-tdt".into()),
                    cloud_model: None,
                },
                ProviderSelection {
                    capability: Capability::PromptEnhancement,
                    mode: ExecutionMode::OpenRouter,
                    local_engine: Some("local-llm".into()),
                    cloud_model: None,
                },
                ProviderSelection {
                    capability: Capability::CoverArt,
                    mode: ExecutionMode::OpenRouter,
                    local_engine: None,
                    cloud_model: None,
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicGenerationRequest {
    pub id: Uuid,
    pub caption: String,
    pub lyrics: String,
    pub duration_seconds: u16,
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineDescriptor {
    pub id: String,
    pub display_name: String,
    pub capabilities: Vec<Capability>,
    pub execution_mode: ExecutionMode,
    pub installed: bool,
}
