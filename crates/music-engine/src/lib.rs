use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use thiserror::Error;

pub mod mm_server;
pub mod process_group;

/// The lifecycle exposed by audio.cpp. The upstream CLI has no verified numeric
/// progress protocol, so `Running` intentionally carries no percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationPhase {
    Queued,
    Preparing,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnginePaths {
    pub engine_root: PathBuf,
    pub model_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenerationRequest {
    pub prompt: String,
    pub lyrics: String,
    pub output_path: PathBuf,
    pub profile_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedGeneration {
    pub phase: GenerationPhase,
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub temporary_output_path: PathBuf,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationResult {
    pub phase: GenerationPhase,
    pub output_path: Option<PathBuf>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("profile `{0}` is not available for real inference")]
    UnsupportedProfile(String),
    #[error("profile `{profile}` is incomplete: missing {path}")]
    MissingComponent { profile: String, path: PathBuf },
    #[error("the output path already exists: {0}")]
    ExistingOutput(PathBuf),
}

#[derive(Debug, Deserialize)]
struct EngineManifest {
    engine: ManifestEngine,
    model_package: ModelPackage,
    component_compatibility: ComponentCompatibility,
    profiles: Vec<ModelProfile>,
}

#[derive(Debug, Deserialize)]
struct ManifestEngine {
    executable: String,
    windows_executable: String,
    family: String,
    task: String,
}

#[derive(Debug, Deserialize)]
struct ModelPackage {
    required_shared_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ComponentCompatibility {
    required_components: RequiredComponents,
}

#[derive(Debug, Deserialize)]
struct RequiredComponents {
    language_model: SelectableComponent,
    rvq_depth_decoder: SelectableComponent,
    flow_transformer: SelectableComponent,
}

#[derive(Debug, Deserialize)]
struct SelectableComponent {
    allowed_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ModelProfile {
    id: String,
    availability: String,
    component_overrides: std::collections::BTreeMap<String, String>,
}

pub struct AudioCppAdapter {
    manifest: EngineManifest,
    paths: EnginePaths,
}

impl AudioCppAdapter {
    pub fn load(manifest_path: impl AsRef<Path>, paths: EnginePaths) -> Result<Self> {
        let manifest_path = manifest_path.as_ref();
        let contents = fs::read_to_string(manifest_path)
            .with_context(|| format!("read engine manifest {}", manifest_path.display()))?;
        let manifest = serde_json::from_str(&contents)
            .with_context(|| format!("parse engine manifest {}", manifest_path.display()))?;
        Ok(Self { manifest, paths })
    }

    /// Validates the chosen full profile and creates the exact confirmed
    /// audio.cpp invocation. The manifest has not verified CLI flags for the
    /// optional generation/session fields, therefore this method deliberately
    /// does not emit any of them.
    pub fn prepare(&self, request: &GenerationRequest) -> Result<PreparedGeneration> {
        if request.prompt.trim().is_empty() {
            bail!("prompt is required");
        }
        if request.lyrics.trim().is_empty() {
            bail!("lyrics are required by the MiniMax Music3 audio.cpp workflow");
        }
        if request.output_path.as_os_str().is_empty() {
            bail!("output path is required");
        }
        if request.output_path.exists() {
            return Err(EngineError::ExistingOutput(request.output_path.clone()).into());
        }

        let profile = self.profile(&request.profile_id)?;
        self.validate_model_root(profile)?;

        let executable_name = if cfg!(windows) {
            &self.manifest.engine.windows_executable
        } else {
            &self.manifest.engine.executable
        };
        let executable = self.paths.engine_root.join(executable_name);
        if !executable.is_file() {
            bail!("audio.cpp executable is missing: {}", executable.display());
        }

        let temporary_output_path = temporary_output_path(&request.output_path)?;
        let text = compose_text(&request.prompt, &request.lyrics);
        let arguments = vec![
            OsString::from("--task"),
            OsString::from(&self.manifest.engine.task),
            OsString::from("--family"),
            OsString::from(&self.manifest.engine.family),
            OsString::from("--model"),
            self.paths.model_root.clone().into_os_string(),
            OsString::from("--backend"),
            OsString::from("cuda"),
            OsString::from("--text"),
            OsString::from(text),
            OsString::from("--out"),
            temporary_output_path.clone().into_os_string(),
        ];

        Ok(PreparedGeneration {
            phase: GenerationPhase::Preparing,
            executable,
            arguments,
            temporary_output_path,
            output_path: request.output_path.clone(),
        })
    }

    pub fn spawn(&self, prepared: PreparedGeneration) -> Result<RunningGeneration> {
        let child = Command::new(&prepared.executable)
            .args(&prepared.arguments)
            .spawn()
            .with_context(|| format!("start audio.cpp: {}", prepared.executable.display()))?;
        Ok(RunningGeneration { child, prepared })
    }

    fn profile(&self, profile_id: &str) -> Result<&ModelProfile> {
        let profile = self
            .manifest
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .with_context(|| format!("unknown MiniMax Music3 profile `{profile_id}`"))?;
        if profile.availability != "supported" {
            return Err(EngineError::UnsupportedProfile(profile.id.clone()).into());
        }
        Ok(profile)
    }

    fn validate_model_root(&self, profile: &ModelProfile) -> Result<()> {
        for shared_file in &self.manifest.model_package.required_shared_files {
            self.require_file(profile, Path::new(shared_file))?;
        }

        self.require_selected_component(
            profile,
            "minimax_music3.language_model_gguf",
            &self.manifest.component_compatibility.required_components.language_model,
        )?;
        self.require_selected_component(
            profile,
            "minimax_music3.rvq_depth_decoder_gguf",
            &self.manifest.component_compatibility.required_components.rvq_depth_decoder,
        )?;
        self.require_selected_component(
            profile,
            "minimax_music3.flow_transformer_gguf",
            &self.manifest.component_compatibility.required_components.flow_transformer,
        )?;
        Ok(())
    }

    fn require_selected_component(
        &self,
        profile: &ModelProfile,
        option_name: &str,
        component: &SelectableComponent,
    ) -> Result<()> {
        let filename = profile
            .component_overrides
            .get(option_name)
            .with_context(|| format!("profile `{}` has no `{option_name}` component", profile.id))?;
        if !component.allowed_files.iter().any(|allowed| allowed == filename) {
            bail!("profile `{}` selects an incompatible component `{filename}`", profile.id);
        }
        self.require_file(profile, Path::new(filename))
    }

    fn require_file(&self, profile: &ModelProfile, relative_path: &Path) -> Result<()> {
        let path = self.paths.model_root.join(relative_path);
        if !path.is_file() {
            return Err(EngineError::MissingComponent {
                profile: profile.id.clone(),
                path,
            }
            .into());
        }
        Ok(())
    }
}

pub struct RunningGeneration {
    child: Child,
    prepared: PreparedGeneration,
}

impl RunningGeneration {
    pub fn phase(&self) -> GenerationPhase {
        GenerationPhase::Running
    }

    pub fn wait(mut self) -> Result<GenerationResult> {
        let status = self.child.wait().context("wait for audio.cpp")?;
        self.finish(status)
    }

    /// audio.cpp has no verified cooperative cancel flag. We terminate only the
    /// child this adapter started and remove its unpublished temporary output.
    pub fn cancel(mut self) -> Result<GenerationResult> {
        self.child.kill().context("terminate audio.cpp child process")?;
        let status = self.child.wait().context("wait for terminated audio.cpp")?;
        remove_temporary_output(&self.prepared.temporary_output_path)?;
        Ok(GenerationResult {
            phase: GenerationPhase::Cancelled,
            output_path: None,
            exit_code: status.code(),
        })
    }

    fn finish(&mut self, status: ExitStatus) -> Result<GenerationResult> {
        if !status.success() {
            remove_temporary_output(&self.prepared.temporary_output_path)?;
            return Ok(GenerationResult {
                phase: GenerationPhase::Failed,
                output_path: None,
                exit_code: status.code(),
            });
        }
        if !self.prepared.temporary_output_path.is_file() {
            bail!(
                "audio.cpp exited successfully but did not create {}",
                self.prepared.temporary_output_path.display()
            );
        }
        fs::rename(
            &self.prepared.temporary_output_path,
            &self.prepared.output_path,
        )
        .with_context(|| {
            format!(
                "publish generated audio from {} to {}",
                self.prepared.temporary_output_path.display(),
                self.prepared.output_path.display()
            )
        })?;
        Ok(GenerationResult {
            phase: GenerationPhase::Completed,
            output_path: Some(self.prepared.output_path.clone()),
            exit_code: status.code(),
        })
    }
}

fn compose_text(prompt: &str, lyrics: &str) -> String {
    format!("{prompt}\n\nLyrics:\n{lyrics}")
}

fn temporary_output_path(output_path: &Path) -> Result<PathBuf> {
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    let file_stem = output_path
        .file_stem()
        .context("output path must have a filename")?;
    let extension = output_path.extension().and_then(|value| value.to_str());
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("read system time")?
        .as_nanos();
    let filename = match extension {
        Some(extension) => format!("{}.generating-{}.{}", file_stem.to_string_lossy(), nonce, extension),
        None => format!("{}.generating-{}", file_stem.to_string_lossy(), nonce),
    };
    Ok(parent.join(filename))
}

fn remove_temporary_output(path: &Path) -> Result<()> {
    if path.is_file() {
        fs::remove_file(path).with_context(|| format!("remove temporary output {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_prompt_and_lyrics_in_the_confirmed_text_argument() {
        assert_eq!(
            compose_text("fast synth pop", "hello world"),
            "fast synth pop\n\nLyrics:\nhello world"
        );
    }

    #[test]
    fn temporary_file_keeps_audio_extension() {
        let path = temporary_output_path(Path::new("C:/output/track.wav")).unwrap();
        assert_eq!(path.parent(), Some(Path::new("C:/output")));
        assert_eq!(path.extension().and_then(|value| value.to_str()), Some("wav"));
        assert!(path.file_name().unwrap().to_string_lossy().starts_with("track.generating-"));
    }

    #[test]
    fn manifest_rejects_benchmark_only_profiles() {
        let manifest: EngineManifest = serde_json::from_str(
            r#"{
              "engine":{"executable":"engine","windows_executable":"engine.exe","family":"minimax_music3","task":"gen"},
              "model_package":{"required_shared_files":[]},
              "component_compatibility":{"required_components":{
                "language_model":{"allowed_files":["lm.gguf"]},
                "rvq_depth_decoder":{"allowed_files":["rvq.gguf"]},
                "flow_transformer":{"allowed_files":["dit.gguf"]}
              }},
              "profiles":[{"id":"benchmark","availability":"benchmark_required","component_overrides":{}}]
            }"#,
        )
        .unwrap();
        let adapter = AudioCppAdapter {
            manifest,
            paths: EnginePaths { engine_root: PathBuf::new(), model_root: PathBuf::new() },
        };
        assert!(matches!(
            adapter.profile("benchmark").unwrap_err().downcast_ref::<EngineError>(),
            Some(EngineError::UnsupportedProfile(_))
        ));
    }
}
