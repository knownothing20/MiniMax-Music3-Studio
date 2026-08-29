use std::{fs, io::{Cursor, Write}, path::Path};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use crate::{assistant, library::{Library, Song}, security};

const MAX_MEDIA_BYTES: u64 = 256 * 1024 * 1024;

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["authorization", "cookie", "password", "secret", "token", "api_key", "apikey", "credential", "private_key"]
        .iter()
        .any(|needle| key.contains(needle))
}

fn redacted(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(map.iter().map(|(key, value)| {
            let value = if sensitive_key(key) { Value::String("[REDACTED]".into()) } else { redacted(value) };
            (key.clone(), value)
        }).collect()),
        Value::Array(values) => Value::Array(values.iter().map(redacted).collect()),
        Value::String(value) => Value::String(security::redact_secrets(value)),
        value => value.clone(),
    }
}

fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(value)?)
}

fn add_if_readable(entries: &mut Vec<(String, Vec<u8>)>, name: String, path: &Path) -> Result<()> {
    let metadata = fs::metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if metadata.len() > MAX_MEDIA_BYTES {
        anyhow::bail!("{} exceeds the 256 MiB diagnostic bundle limit", path.display());
    }
    entries.push((name, fs::read(path).with_context(|| format!("read {}", path.display()))?));
    Ok(())
}

fn extension(path: &Path, fallback: &str) -> String {
    path.extension().and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_ascii_lowercase()
}

pub fn build_song_bundle(library: &Library, song: &Song) -> Result<Vec<u8>> {
    let diagnostics = song.generation_settings.get("studio_diagnostics").cloned().unwrap_or(Value::Null);
    let trace_available = diagnostics.get("assistant_trace").and_then(Value::as_array).is_some_and(|trace| !trace.is_empty());
    let safe_metadata = redacted(&song.metadata);
    let safe_settings = redacted(&song.generation_settings);
    let safe_replay = song.replay_request.as_ref().map(redacted);

    let mut entries = Vec::<(String, Vec<u8>)>::new();
    entries.push(("copy/structured-caption.md".into(), song.caption.as_bytes().to_vec()));
    entries.push(("copy/lyrics.md".into(), song.lyrics.as_bytes().to_vec()));
    entries.push(("logic/application-prompt-contracts.json".into(), json_bytes(&assistant::diagnostic_prompt_contracts())?));
    entries.push(("logic/generation-request.json".into(), json_bytes(&safe_settings)?));
    if !diagnostics.is_null() {
        entries.push(("process/studio-input-and-assistant-trace.json".into(), json_bytes(&redacted(&diagnostics))?));
    }
    if let Some(replay) = safe_replay {
        entries.push(("logic/replay-request.json".into(), json_bytes(&replay)?));
    }
    if let Some(lrc) = song.metadata.get("lrc").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) {
        entries.push(("output/karaoke.lrc".into(), lrc.as_bytes().to_vec()));
    }

    let safe_song = json!({
        "id": song.id,
        "title": song.title,
        "caption": song.caption,
        "lyrics": song.lyrics,
        "metadata": safe_metadata,
        "generation_settings": safe_settings,
        "engine_id": song.engine_id,
        "profile_id": song.profile_id,
        "source": song.source,
        "created_at": song.created_at,
        "updated_at": song.updated_at,
        "audio_codes_included": false,
        "audio_codes_note": "The rendered audio is included; raw audio token codes are intentionally omitted from the diagnostic bundle."
    });
    entries.push(("output/song.json".into(), json_bytes(&safe_song)?));

    if let Some(path) = library.media_path_for_song(song) {
        add_if_readable(&mut entries, format!("output/song-output.{}", extension(&path, "bin")), &path)?;
    }
    if let Some((path, _)) = library.cover_path_for_song(song) {
        add_if_readable(&mut entries, format!("output/cover.{}", extension(&path, "img")), &path)?;
    }
    for stem in crate::separation::STEMS {
        let filename = format!("{}-{stem}.wav", song.id);
        if let Some(path) = library.media_file(&filename) {
            add_if_readable(&mut entries, format!("output/stems/{stem}.wav"), &path)?;
        }
    }

    let files = entries.iter().map(|(name, bytes)| json!({
        "path": name,
        "bytes": bytes.len(),
        "sha256": format!("{:x}", Sha256::digest(bytes)),
    })).collect::<Vec<_>>();
    let manifest = json!({
        "schema_version": 1,
        "kind": "music-maker-song-diagnostic-bundle",
        "song_id": song.id,
        "title": song.title,
        "assistant_trace_available": trace_available,
        "assistant_trace_note": if trace_available { "Recorded assistant request, visible stream stages and final structured draft are included." } else { "This song predates assistant trace capture or was created manually; final structured copy and application prompt contracts are still included." },
        "security": "Credentials, tokens, cookies, hidden model reasoning and full server logs are not exported.",
        "files": files,
    });
    let readme = format!(
        "# Music Maker diagnostic bundle\n\nSong: {} (`{}`)\n\nThis archive contains the saved Studio inputs, structured caption, lyrics, application prompt contracts, redacted generation request, song metadata and available rendered media. It contains no provider credentials, session token, hidden model reasoning or full server logs.\n\nAssistant trace available: {}.\n",
        song.title, song.id, trace_available
    );
    entries.insert(0, ("README.md".into(), readme.into_bytes()));
    entries.insert(1, ("manifest.json".into(), json_bytes(&manifest)?));

    let cursor = Cursor::new(Vec::new());
    let mut archive = ZipWriter::new(cursor);
    for (name, bytes) in entries {
        let compression = if name.ends_with(".mp3") || name.ends_with(".png") || name.ends_with(".jpg") || name.ends_with(".webp") {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        archive.start_file(name, SimpleFileOptions::default().compression_method(compression))?;
        archive.write_all(&bytes)?;
    }
    Ok(archive.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::GeneratedSongInput;
    use std::io::Read;

    #[test]
    fn bundle_contains_copy_logic_media_and_redacts_credentials() {
        let root = std::env::temp_dir().join(format!("music-diagnostics-{}", uuid::Uuid::now_v7()));
        let library = Library::open_at(root.join("library.sqlite"), root.join("media")).unwrap();
        let imported = library.import_generated_song(GeneratedSongInput {
            title: Some("Trace Song".into()),
            metadata: json!({"duration_seconds": 1, "api_token": "do-not-export"}),
            caption: "Global Metadata: test".into(),
            lyrics: "[verse]\nhello".into(),
            generation_settings: json!({
                "api_key": "do-not-export",
                "studio_diagnostics": {"assistant_trace": [{"target": "all", "instruction": "make it warm", "final_draft": {"lyrics": "hello"}}]}
            }),
            replay_request: None,
            audio_codes: Some(json!("large-token-stream")),
            engine_id: "omnibridge".into(),
            profile_id: None,
            source: "test".into(),
            audio_extension: "mp3",
            audio: b"ID3-test-audio".to_vec(),
        }).unwrap();

        let bytes = build_song_bundle(&library, &imported.song).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        for expected in ["README.md", "manifest.json", "copy/structured-caption.md", "copy/lyrics.md", "logic/application-prompt-contracts.json", "process/studio-input-and-assistant-trace.json", "output/song.json", "output/song-output.mp3"] {
            assert!(zip.by_name(expected).is_ok(), "missing {expected}");
        }
        let mut song_json = String::new();
        zip.by_name("output/song.json").unwrap().read_to_string(&mut song_json).unwrap();
        assert!(!song_json.contains("do-not-export"));
        assert!(!song_json.contains("large-token-stream"));
        assert!(song_json.contains("[REDACTED]"));
        let _ = fs::remove_dir_all(root);
    }
}
