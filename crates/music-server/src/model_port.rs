//! Project-owned business roles layered over centrally managed OmniBridge routes.
//!
//! This module is the Studio's model port. It persists only capability defaults
//! and role selectors; provider credentials, deployments and candidate order
//! remain in OmniBridge. The browser talks to this module, never to the Gateway.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PROFILE_SCHEMA: &str = "omnibridge.project-profile.v2";
const BINDINGS_SCHEMA: &str = "music-maker.model-bindings.v1";
const PROJECT_ID: &str = "music-maker";
const DEFAULT_PROFILE: &str = include_str!("../../../.omnibridge/project-profile.json");

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityBinding {
    pub selector: Selector,
    pub operation: String,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_policy: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoleBinding {
    pub capability: String,
    pub selector: Selector,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectProfile {
    pub schema: String,
    pub project_id: String,
    pub profile_revision: u64,
    pub capability_defaults: BTreeMap<String, CapabilityBinding>,
    pub roles: BTreeMap<String, RoleBinding>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RoleDefinition {
    pub id: &'static str,
    pub label_zh: &'static str,
    pub description_zh: &'static str,
    pub capability: &'static str,
}

#[derive(Clone)]
pub struct ModelPort {
    store: ProjectProfileStore,
    write_lock: Arc<Mutex<()>>,
}

#[derive(Clone)]
struct ProjectProfileStore {
    path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct SaveProfileRequest {
    pub expected_profile_revision: u64,
    pub profile: ProjectProfile,
}

#[derive(Debug, Deserialize)]
pub struct PreviewProfileRequest {
    #[serde(default)]
    pub profile: Option<ProjectProfile>,
}

impl ModelPort {
    pub fn new(data_root: &Path) -> Result<Self, String> {
        let store = ProjectProfileStore {
            path: data_root.join("omnibridge").join("project-profile.json"),
        };
        let profile = store.load()?;
        if !store.path.exists() {
            store.write(&profile)?;
        }
        Ok(Self { store, write_lock: Arc::new(Mutex::new(())) })
    }

    pub fn profile(&self) -> Result<ProjectProfile, String> {
        self.store.load()
    }

    pub fn save(&self, request: SaveProfileRequest) -> Result<ProjectProfile, String> {
        let _guard = self.write_lock.lock().map_err(|_| "project profile write lock is poisoned".to_owned())?;
        let current = self.store.load()?;
        if current.profile_revision != request.expected_profile_revision {
            return Err(format!(
                "profile revision conflict: expected {}, current {}",
                request.expected_profile_revision, current.profile_revision
            ));
        }
        if request.profile.profile_revision != current.profile_revision + 1 {
            return Err("profile_revision must increase by exactly one".to_owned());
        }
        validate_profile(&request.profile)?;
        self.store.write(&request.profile)?;
        Ok(request.profile)
    }

    pub fn role_route(&self, role_id: &str) -> Result<String, String> {
        resolve_role_route(&self.store.load()?, role_id)
    }

    pub fn music_route(&self) -> Result<String, String> {
        self.role_route("music_generation_cloud")
    }

    pub fn assistant_role(request: &crate::assistant::AssistRequest) -> &'static str {
        use crate::assistant::AssistTarget;
        match request.target {
            AssistTarget::All => "song_package_draft",
            AssistTarget::Lyrics if request.lyrics.trim().is_empty() => "lyrics_draft",
            AssistTarget::Lyrics => "lyrics_refine",
            AssistTarget::Prompt => "music_prompt_structuring",
        }
    }

    pub async fn bindings_payload(&self) -> Result<Value, String> {
        let profile = self.profile()?;
        match GatewayClient::from_env() {
            Ok(client) => match client.provider_strategies().await {
                Ok(strategies) => Ok(serde_json::json!({
                    "schema": BINDINGS_SCHEMA,
                    "profile": profile,
                    "roles": role_definitions(),
                    "strategies": strategies.get("strategies").cloned().unwrap_or(Value::Array(vec![])),
                    "strategy_schema": strategies.get("schema").cloned().unwrap_or(Value::Null),
                    "hub": { "available": true, "centrally_managed": true }
                })),
                Err(error) => Ok(bindings_offline(profile, error)),
            },
            Err(error) => Ok(bindings_offline(profile, error)),
        }
    }

    pub async fn validate_with_hub(&self, profile: &ProjectProfile) -> Result<Value, String> {
        validate_profile(profile)?;
        GatewayClient::from_env()?.resolve(profile).await
    }

    pub fn cloud_configured(&self) -> bool {
        GatewayClient::from_env().is_ok() && self.profile().is_ok()
    }
}

fn bindings_offline(profile: ProjectProfile, error: String) -> Value {
    serde_json::json!({
        "schema": BINDINGS_SCHEMA,
        "profile": profile,
        "roles": role_definitions(),
        "strategies": [],
        "strategy_schema": Value::Null,
        "hub": { "available": false, "centrally_managed": true, "error": error }
    })
}

pub fn role_definitions() -> Vec<RoleDefinition> {
    vec![
        RoleDefinition { id: "song_package_draft", label_zh: "整首歌曲方案", description_zh: "一次生成歌词与结构化音乐描述", capability: "text" },
        RoleDefinition { id: "song_concept_draft", label_zh: "歌曲概念起草", description_zh: "从创意形成主题、方向与初稿", capability: "text" },
        RoleDefinition { id: "lyrics_draft", label_zh: "歌词起草", description_zh: "从描述生成完整歌词", capability: "text" },
        RoleDefinition { id: "lyrics_refine", label_zh: "歌词润色", description_zh: "在现有歌词基础上修改与增强", capability: "text" },
        RoleDefinition { id: "music_prompt_structuring", label_zh: "音乐描述结构化", description_zh: "整理全局元数据、人声细节与编曲", capability: "text" },
        RoleDefinition { id: "title_cover_brief", label_zh: "标题与封面简报", description_zh: "为歌曲生成标题及视觉方向", capability: "text" },
        RoleDefinition { id: "music_generation_cloud", label_zh: "云端音乐生成", description_zh: "提交 MiniMax Music 生成任务", capability: "music" },
    ]
}

fn default_profile() -> Result<ProjectProfile, String> {
    let mut profile: ProjectProfile = serde_json::from_str(DEFAULT_PROFILE)
        .map_err(|error| format!("default project profile is invalid: {error}"))?;
    merge_required_roles(&mut profile)?;
    validate_profile(&profile)?;
    Ok(profile)
}

fn merge_required_roles(profile: &mut ProjectProfile) -> Result<(), String> {
    let seed: ProjectProfile = serde_json::from_str(DEFAULT_PROFILE)
        .map_err(|error| format!("default project profile is invalid: {error}"))?;
    for (capability, binding) in seed.capability_defaults {
        profile.capability_defaults.entry(capability).or_insert(binding);
    }
    for (role, binding) in seed.roles {
        profile.roles.entry(role).or_insert(binding);
    }
    profile.roles.entry("song_package_draft".to_owned()).or_insert(RoleBinding {
        capability: "text".to_owned(),
        selector: Selector { kind: "route".to_owned(), id: Some("route:text:quality".to_owned()) },
    });
    Ok(())
}

impl ProjectProfileStore {
    fn load(&self) -> Result<ProjectProfile, String> {
        if !self.path.exists() {
            return default_profile();
        }
        let bytes = fs::read(&self.path).map_err(|error| format!("project profile is unavailable: {error}"))?;
        if bytes.len() > 256 * 1024 {
            return Err("project profile is too large".to_owned());
        }
        let mut profile: ProjectProfile = serde_json::from_slice(&bytes)
            .map_err(|error| format!("project profile is invalid: {error}"))?;
        merge_required_roles(&mut profile)?;
        validate_profile(&profile)?;
        Ok(profile)
    }

    fn write(&self, profile: &ProjectProfile) -> Result<(), String> {
        validate_profile(profile)?;
        let parent = self.path.parent().ok_or_else(|| "project profile path has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| format!("cannot create project profile directory: {error}"))?;
        #[cfg(unix)]
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot protect project profile directory: {error}"))?;
        let temporary = parent.join(format!(".project-profile-{}.tmp", uuid::Uuid::now_v7()));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).map_err(|error| format!("cannot stage project profile: {error}"))?;
        let bytes = serde_json::to_vec_pretty(profile).map_err(|error| format!("cannot encode project profile: {error}"))?;
        file.write_all(&bytes).and_then(|_| file.write_all(b"\n")).and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot persist project profile: {error}"))?;
        fs::rename(&temporary, &self.path).map_err(|error| format!("cannot publish project profile: {error}"))?;
        Ok(())
    }
}

fn validate_profile(profile: &ProjectProfile) -> Result<(), String> {
    if profile.schema != PROFILE_SCHEMA || profile.project_id != PROJECT_ID || profile.profile_revision == 0 {
        return Err("project profile identity or revision is invalid".to_owned());
    }
    for capability in ["text", "music"] {
        let binding = profile.capability_defaults.get(capability)
            .ok_or_else(|| format!("missing {capability} capability default"))?;
        validate_route_selector(&binding.selector, capability, false)?;
    }
    for definition in role_definitions() {
        let role = profile.roles.get(definition.id)
            .ok_or_else(|| format!("missing business role {}", definition.id))?;
        if role.capability != definition.capability {
            return Err(format!("role {} must use {} capability", definition.id, definition.capability));
        }
        validate_route_selector(&role.selector, definition.capability, true)?;
    }
    Ok(())
}

fn validate_route_selector(selector: &Selector, capability: &str, allow_inherit: bool) -> Result<(), String> {
    if selector.kind == "inherit" && allow_inherit && selector.id.is_none() {
        return Ok(());
    }
    if selector.kind != "route" {
        return Err("only route selectors and role inheritance are supported".to_owned());
    }
    let id = selector.id.as_deref().ok_or_else(|| "route selector requires an id".to_owned())?;
    let prefix = format!("route:{capability}:");
    if !id.starts_with(&prefix) || id.len() > 200 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte)) {
        return Err(format!("selector {id} is not a valid {capability} route"));
    }
    Ok(())
}

fn resolve_role_route(profile: &ProjectProfile, role_id: &str) -> Result<String, String> {
    let role = profile.roles.get(role_id).ok_or_else(|| format!("unknown business role {role_id}"))?;
    let selector = if role.selector.kind == "inherit" {
        &profile.capability_defaults.get(&role.capability)
            .ok_or_else(|| format!("missing {} capability default", role.capability))?.selector
    } else {
        &role.selector
    };
    validate_route_selector(selector, &role.capability, false)?;
    selector.id.clone().ok_or_else(|| "resolved route is missing".to_owned())
}

struct GatewayClient {
    base_url: String,
    gateway_key: String,
    client_id: String,
    platform_id: String,
    project_id: String,
    http: reqwest::Client,
}

impl GatewayClient {
    fn from_env() -> Result<Self, String> {
        let required = |name: &'static str| env::var(name).ok().map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()).ok_or_else(|| format!("{name} is not configured"));
        let base_url = required("MUSIC_MAKER_OMNIBRIDGE_BASE_URL")?.trim_end_matches('/').to_owned();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err("OmniBridge base URL is invalid".to_owned());
        }
        let platform_id = required("MUSIC_MAKER_OMNIBRIDGE_PLATFORM_ID")?;
        let client_id = env::var("MUSIC_MAKER_OMNIBRIDGE_CLIENT_ID").ok().filter(|value| !value.trim().is_empty()).unwrap_or_else(|| platform_id.clone());
        let project_id = env::var("MUSIC_MAKER_OMNIBRIDGE_PROJECT_ID").ok().filter(|value| !value.trim().is_empty()).unwrap_or_else(|| PROJECT_ID.to_owned());
        let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(30)).build()
            .map_err(|_| "cannot initialize OmniBridge client".to_owned())?;
        Ok(Self { base_url, gateway_key: required("MUSIC_MAKER_OMNIBRIDGE_GATEWAY_KEY")?, client_id, platform_id, project_id, http })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(method, format!("{}{}", self.base_url, path))
            .bearer_auth(&self.gateway_key)
            .header("x-omnibridge-client-id", &self.client_id)
            .header("X-Platform-Id", &self.platform_id)
            .header("X-Project-Id", &self.project_id)
    }

    async fn provider_strategies(&self) -> Result<Value, String> {
        let response = self.request(reqwest::Method::GET, "/v1/provider-strategies").send().await
            .map_err(|_| "OmniBridge strategy catalog is unreachable".to_owned())?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("OmniBridge strategy catalog returned HTTP {}", status.as_u16()));
        }
        response.json::<Value>().await.map_err(|_| "OmniBridge strategy catalog returned invalid JSON".to_owned())
    }

    async fn resolve(&self, profile: &ProjectProfile) -> Result<Value, String> {
        let response = self.request(reqwest::Method::POST, "/v1/project-profiles/resolve")
            .json(profile).send().await.map_err(|_| "OmniBridge profile resolver is unreachable".to_owned())?;
        let status = response.status();
        let body = response.json::<Value>().await.map_err(|_| "OmniBridge profile resolver returned invalid JSON".to_owned())?;
        if !status.is_success() {
            return Err(format!("OmniBridge rejected the project profile (HTTP {})", status.as_u16()));
        }
        if body.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err("OmniBridge rejected the project profile".to_owned());
        }
        Ok(body)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_resolves_business_roles() {
        let profile = default_profile().expect("default profile");
        assert_eq!(resolve_role_route(&profile, "song_concept_draft").unwrap(), "route:text:fast");
        assert_eq!(resolve_role_route(&profile, "song_package_draft").unwrap(), "route:text:quality");
        assert_eq!(resolve_role_route(&profile, "music_generation_cloud").unwrap(), "route:music:default");
    }

    #[test]
    fn wrong_capability_route_fails_closed() {
        let mut profile = default_profile().expect("default profile");
        profile.roles.get_mut("lyrics_draft").unwrap().selector = Selector {
            kind: "route".to_owned(),
            id: Some("route:music:default".to_owned()),
        };
        assert!(validate_profile(&profile).unwrap_err().contains("valid text route"));
    }

    #[test]
    fn optimistic_revision_rejects_stale_writer() {
        let root = std::env::temp_dir().join(format!("music-maker-model-port-{}", uuid::Uuid::now_v7()));
        let port = ModelPort::new(&root).expect("model port");
        let mut profile = port.profile().expect("profile");
        profile.profile_revision += 1;
        port.save(SaveProfileRequest { expected_profile_revision: 1, profile }).expect("first save");
        let stale = port.profile().expect("profile");
        let error = port.save(SaveProfileRequest { expected_profile_revision: 1, profile: stale }).unwrap_err();
        assert!(error.contains("revision conflict"));
        let _ = std::fs::remove_dir_all(root);
    }
}
