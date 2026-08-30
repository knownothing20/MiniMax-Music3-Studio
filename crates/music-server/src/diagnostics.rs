use std::{
    fs,
    io::{Cursor, Write},
    path::Path,
};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{
    library::{self, Library, Song},
    security,
};

const MAX_MEDIA_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BUNDLE_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SongBundleInput {
    pub requested_id: String,
    pub song: Option<Song>,
    pub job: Option<Value>,
    pub durable: Option<Value>,
}

#[derive(Debug)]
struct BundleEntry {
    path: String,
    source: String,
    bytes: Vec<u8>,
    metadata: Option<Value>,
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "password",
        "secret",
        "token",
        "api_key",
        "apikey",
        "credential",
        "private_key",
        "signed_url",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn looks_like_internal_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with("/home/")
        || value.starts_with("/root/")
        || value.starts_with("/Users/")
        || (bytes.len() > 2
            && bytes[1] == b':'
            && bytes[0].is_ascii_alphabetic()
            && matches!(bytes[2], b'\\' | b'/'))
}

fn safe_string(value: &str) -> String {
    if looks_like_internal_path(value) {
        return "[REDACTED_INTERNAL_PATH]".into();
    }
    if let Ok(mut url) = reqwest::Url::parse(value) {
        if matches!(url.scheme(), "http" | "https") {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            return url.to_string();
        }
    }
    security::redact_secrets(value)
}

fn redacted(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let value = if sensitive_key(key) {
                        Value::String("[REDACTED]".into())
                    } else {
                        redacted(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redacted).collect()),
        Value::String(value) => Value::String(safe_string(value)),
        value => value.clone(),
    }
}

fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn safe_extension(path: &Path, fallback: &str, allowed: &[&str]) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|value| allowed.contains(&value.as_str()))
        .unwrap_or_else(|| fallback.to_owned())
}

fn push_entry(
    entries: &mut Vec<BundleEntry>,
    path: impl Into<String>,
    source: impl Into<String>,
    bytes: Vec<u8>,
    metadata: Option<Value>,
) -> Result<()> {
    let next_size = entries
        .iter()
        .try_fold(bytes.len(), |size, entry| {
            size.checked_add(entry.bytes.len())
        })
        .context("diagnostic bundle size overflow")?;
    if next_size > MAX_BUNDLE_BYTES {
        anyhow::bail!("diagnostic bundle exceeds the 512 MiB limit");
    }
    entries.push(BundleEntry {
        path: path.into(),
        source: source.into(),
        bytes,
        metadata,
    });
    Ok(())
}

fn read_media(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if metadata.len() > MAX_MEDIA_BYTES {
        anyhow::bail!("diagnostic media exceeds the 256 MiB per-file limit");
    }
    fs::read(path).with_context(|| "read diagnostic media")
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn job_string<'a>(job: Option<&'a Value>, key: &str) -> Option<&'a str> {
    job.and_then(|value| value.get(key)).and_then(Value::as_str)
}

fn recorded_diagnostics(song: Option<&Song>, job: Option<&Value>) -> Value {
    song.and_then(|song| song.generation_settings.get("studio_diagnostics"))
        .or_else(|| job.and_then(|job| job.pointer("/generation_settings/studio_diagnostics")))
        .cloned()
        .unwrap_or(Value::Null)
}

fn generation_settings(song: Option<&Song>, job: Option<&Value>) -> Value {
    let mut settings = song
        .map(|song| song.generation_settings.clone())
        .or_else(|| job.and_then(|job| job.get("generation_settings").cloned()))
        .unwrap_or(Value::Null);
    if let Some(object) = settings.as_object_mut() {
        object.remove("studio_diagnostics");
    }
    redacted(&settings)
}

fn timeline(song: Option<&Song>, job: Option<&Value>, diagnostics: &Value) -> Value {
    let mut events = Vec::new();
    if let Some(trace) = diagnostics.get("assistant_trace").and_then(Value::as_array) {
        for entry in trace {
            events.push(json!({
                "kind": "assistant",
                "target": entry.get("target").cloned().unwrap_or(Value::String("not_recorded".into())),
                "status": entry.get("status").cloned().unwrap_or(Value::String("not_recorded".into())),
                "started_at": entry.get("started_at").cloned().unwrap_or(Value::String("not_recorded".into())),
                "completed_at": entry.get("completed_at").cloned().unwrap_or(Value::String("not_recorded".into())),
            }));
        }
    }
    if let Some(job) = job {
        events.push(json!({
            "kind": "generation_job_snapshot",
            "job_id": job.get("id").cloned().unwrap_or(Value::String("not_recorded".into())),
            "status": job.get("status").cloned().unwrap_or(Value::String("not_recorded".into())),
            "phase": job.get("phase").cloned().unwrap_or(Value::String("not_recorded".into())),
            "recorded_at": "not_recorded",
            "note": "The job model stores its current state but no timestamped transition ledger. No times were inferred."
        }));
    }
    if let Some(song) = song {
        events.push(json!({"kind": "library_song_created", "recorded_at": song.created_at}));
        events.push(json!({"kind": "library_song_updated", "recorded_at": song.updated_at}));
    }
    json!({"schema_version": 1, "events": events})
}

fn receipt(job: Option<&Value>, durable: Option<&Value>, diagnostics: &Value) -> Value {
    let assistant_receipts = diagnostics
        .get("assistant_trace")
        .and_then(Value::as_array)
        .map(|trace| {
            trace
                .iter()
                .filter_map(|entry| entry.get("receipt"))
                .map(redacted)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "schema_version": 1,
        "generation_job_id": job.and_then(|job| job.get("id")).cloned().unwrap_or(Value::String("not_recorded".into())),
        "durable_generation": durable.map(redacted).unwrap_or(Value::String("not_recorded".into())),
        "assistant_receipts": if assistant_receipts.is_empty() { Value::String("not_recorded".into()) } else { Value::Array(assistant_receipts) },
        "note": "Only persisted safe receipt fields are included. Provider credentials, durable task tokens and private child handles are excluded."
    })
}

fn missing_file(path: &str, source: &str, reason: &str) -> Value {
    json!({"path": path, "source": source, "exists": false, "availability": reason})
}

pub fn build_song_bundle(library: &Library, input: SongBundleInput) -> Result<Vec<u8>> {
    let song = input.song.as_ref();
    let job = input.job.as_ref();
    let diagnostics = redacted(&recorded_diagnostics(song, job));

    let id = song
        .map(|song| song.id.as_str())
        .or_else(|| job_string(job, "id"))
        .unwrap_or(input.requested_id.as_str());
    let title = song
        .map(|song| song.title.as_str())
        .or_else(|| job_string(job, "title"))
        .unwrap_or("not_recorded");
    let caption = song
        .map(|song| song.caption.as_str())
        .or_else(|| job_string(job, "caption"))
        .unwrap_or("");
    let lyrics = song
        .map(|song| song.lyrics.as_str())
        .or_else(|| job_string(job, "lyrics"))
        .unwrap_or("");
    let task_status = job_string(job, "status").unwrap_or("not_recorded");
    let task_phase = job_string(job, "phase").unwrap_or("not_recorded");

    let mut entries = Vec::<BundleEntry>::new();
    let mut missing = Vec::<Value>::new();

    if let Some(brief) = string_at(&diagnostics, "/form/briefs/song_idea") {
        push_entry(
            &mut entries,
            "input/creative-brief.txt",
            "studio_diagnostics.form.briefs.song_idea",
            brief.as_bytes().to_vec(),
            None,
        )?;
    } else {
        missing.push(missing_file(
            "input/creative-brief.txt",
            "studio_diagnostics.form.briefs.song_idea",
            "not_recorded",
        ));
    }

    if diagnostics
        .get("assistant_trace")
        .and_then(Value::as_array)
        .is_some_and(|trace| !trace.is_empty())
    {
        push_entry(
            &mut entries,
            "input/assistant-activity.json",
            "studio_diagnostics.assistant_trace",
            json_bytes(&diagnostics["assistant_trace"])?,
            None,
        )?;
    } else {
        missing.push(missing_file(
            "input/assistant-activity.json",
            "studio_diagnostics.assistant_trace",
            "not_recorded",
        ));
    }

    if diagnostics
        .pointer("/form/final_copy")
        .is_some_and(Value::is_object)
    {
        push_entry(
            &mut entries,
            "structured/song-plan.json",
            "studio_diagnostics.form.final_copy",
            json_bytes(&diagnostics["form"]["final_copy"])?,
            None,
        )?;
    } else {
        missing.push(missing_file(
            "structured/song-plan.json",
            "studio_diagnostics.form.final_copy",
            "not_recorded",
        ));
    }

    push_entry(
        &mut entries,
        "structured/caption.txt",
        "persisted song/job caption",
        caption.as_bytes().to_vec(),
        None,
    )?;
    push_entry(
        &mut entries,
        "structured/lyrics.txt",
        "persisted song/job lyrics",
        lyrics.as_bytes().to_vec(),
        None,
    )?;

    let generation_request = json!({
        "schema_version": 1,
        "job_id": job.and_then(|job| job.get("id")).cloned().unwrap_or(Value::String("not_recorded".into())),
        "engine_id": song.map(|song| Value::String(song.engine_id.clone())).or_else(|| job.and_then(|job| job.get("engine_id").cloned())).unwrap_or(Value::String("not_recorded".into())),
        "dispatch": job.and_then(|job| job.get("dispatch")).cloned().unwrap_or(Value::String("not_recorded".into())),
        "caption": caption,
        "lyrics": lyrics,
        "requested_duration_seconds": job.and_then(|job| job.get("duration_seconds")).cloned().unwrap_or(Value::String("not_recorded".into())),
        "generation_settings": generation_settings(song, job),
        "source_note": if job.is_some() { "current Studio job snapshot" } else { "persisted library song record" },
    });
    push_entry(
        &mut entries,
        "generation/request.json",
        "persisted library settings and/or current job snapshot",
        json_bytes(&redacted(&generation_request))?,
        None,
    )?;
    push_entry(
        &mut entries,
        "generation/timeline.json",
        "recorded assistant timestamps, job snapshot and library timestamps",
        json_bytes(&timeline(song, job, &diagnostics))?,
        None,
    )?;
    push_entry(
        &mut entries,
        "generation/receipt.json",
        "persisted safe receipts",
        json_bytes(&receipt(job, input.durable.as_ref(), &diagnostics))?,
        None,
    )?;

    if let Some(song) = song {
        let song_record = json!({
            "id": song.id, "title": song.title, "metadata": redacted(&song.metadata),
            "engine_id": song.engine_id, "profile_id": song.profile_id, "source": song.source,
            "created_at": song.created_at, "updated_at": song.updated_at,
            "audio_path": "omitted_internal_path", "audio_codes": "omitted_large_private_generation_state",
        });
        push_entry(
            &mut entries,
            "structured/song-record.json",
            "persisted library song record",
            json_bytes(&song_record)?,
            None,
        )?;
    } else {
        missing.push(missing_file(
            "structured/song-record.json",
            "library song record",
            "not_recorded",
        ));
    }

    let mut audio_manifest = json!({
        "exists": false,
        "availability": if matches!(task_status, "queued" | "running" | "unknown") { "not_available_yet" } else { "missing" },
        "reason": if matches!(task_status, "failed" | "cancelled") { "The persisted job has no imported audio artifact." } else { "No readable audio artifact is present in the Studio media library." },
    });
    if let Some((song, path)) =
        song.and_then(|song| library.media_path_for_song(song).map(|path| (song, path)))
    {
        let bytes = read_media(&path)?;
        let extension = safe_extension(
            &path,
            "bin",
            &["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus"],
        );
        let duration = library::audio_duration_seconds(&bytes, &extension, None)
            .or_else(|| {
                song.metadata
                    .get("actual_duration_seconds")
                    .and_then(Value::as_f64)
            })
            .or_else(|| {
                (song.metadata.get("duration_source").and_then(Value::as_str) == Some("audio_file"))
                    .then(|| {
                        song.metadata
                            .get("duration_seconds")
                            .and_then(Value::as_f64)
                    })
                    .flatten()
            });
        let path_in_zip = format!("artifacts/audio.{extension}");
        let metadata = json!({
            "kind": "audio",
            "duration_seconds": duration.map(Value::from).unwrap_or(Value::String("not_recorded".into())),
            "extension": extension,
        });
        audio_manifest = json!({
            "exists": true, "path": path_in_zip, "bytes": bytes.len(), "sha256": sha256(&bytes),
            "duration_seconds": metadata["duration_seconds"],
        });
        push_entry(
            &mut entries,
            path_in_zip,
            "Studio media library audio file",
            bytes,
            Some(metadata),
        )?;
    } else {
        missing.push(missing_file(
            "artifacts/audio.<ext>",
            "Studio media library audio file",
            audio_manifest["availability"].as_str().unwrap_or("missing"),
        ));
    }

    if let Some((path, media_type)) = song.and_then(|song| library.cover_path_for_song(song)) {
        let bytes = read_media(&path)?;
        let extension = safe_extension(&path, "img", &["png", "jpg", "jpeg", "webp"]);
        push_entry(
            &mut entries,
            format!("artifacts/cover.{extension}"),
            "Studio media library cover file",
            bytes,
            Some(json!({"kind": "cover", "media_type": media_type})),
        )?;
    } else {
        missing.push(missing_file(
            "artifacts/cover.<ext>",
            "Studio media library cover file",
            "not_recorded",
        ));
    }

    if let Some(lrc) = song
        .and_then(|song| song.metadata.get("lrc"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        push_entry(
            &mut entries,
            "artifacts/karaoke.lrc",
            "persisted song metadata.lrc",
            lrc.as_bytes().to_vec(),
            None,
        )?;
    }
    if let Some(song) = song {
        for stem in crate::separation::STEMS {
            let filename = format!("{}-{stem}.wav", song.id);
            if let Some(path) = library.media_file(&filename) {
                push_entry(
                    &mut entries,
                    format!("artifacts/stems/{stem}.wav"),
                    "Studio media library stem file",
                    read_media(&path)?,
                    Some(json!({"kind": "stem", "stem": stem})),
                )?;
            }
        }
    }

    let readme = format!(
        "# Music Maker song diagnostic bundle\n\nSong or job: {title} (`{id}`)\n\nGeneration status: `{task_status}` / `{task_phase}`.\n\nAudio included: {}. {}\n\nEvery included payload is listed in `manifest.json`; `checksums.sha256` covers all payload files except itself and the self-describing manifest. Missing historical data is marked `not_recorded` or `missing` and is never reconstructed. Provider credentials, Authorization/Cookie values, durable task tokens, signed URL queries, internal absolute paths and raw audio token codes are excluded.\n",
        audio_manifest["exists"].as_bool().unwrap_or(false),
        audio_manifest["reason"].as_str().unwrap_or(
            "The readable Studio artifact and its measured metadata are included when present."
        ),
    );
    entries.insert(
        0,
        BundleEntry {
            path: "README.md".into(),
            source: "bundle explanation".into(),
            bytes: readme.into_bytes(),
            metadata: None,
        },
    );

    let mut checksum_lines = entries
        .iter()
        .map(|entry| format!("{}  {}", sha256(&entry.bytes), entry.path))
        .collect::<Vec<_>>();
    checksum_lines.sort();
    let checksums = format!("{}\n", checksum_lines.join("\n")).into_bytes();
    push_entry(
        &mut entries,
        "checksums.sha256",
        "bundle generator",
        checksums,
        Some(json!({"scope": "all included payloads except manifest.json and checksums.sha256"})),
    )?;

    let mut files = entries.iter().map(|entry| json!({
        "path": entry.path, "source": entry.source, "exists": true, "bytes": entry.bytes.len(),
        "sha256": sha256(&entry.bytes), "metadata": entry.metadata,
    })).collect::<Vec<_>>();
    files.extend(missing);
    files.push(json!({"path": "manifest.json", "source": "bundle generator", "exists": true, "sha256": "self_describing_not_applicable"}));

    let manifest = json!({
        "schema_version": 2,
        "kind": "music-maker-song-diagnostic-bundle",
        "requested_id": input.requested_id,
        "song_id": song.map(|song| Value::String(song.id.clone())).unwrap_or(Value::String("not_recorded".into())),
        "job_id": job.and_then(|job| job.get("id")).cloned().unwrap_or(Value::String("not_recorded".into())),
        "title": title,
        "task": {
            "status": task_status, "phase": task_phase,
            "status_source": if job.is_some() { "Studio job snapshot" } else { "not_recorded" },
            "audio": audio_manifest,
        },
        "security": {
            "redacted": ["provider credentials", "Authorization", "Cookie", "tokens", "signed URL queries", "internal absolute paths", "private child handles", "raw audio token codes"],
            "unknown_data_policy": "not_recorded_or_omitted",
        },
        "files": files,
    });
    let manifest_bytes = json_bytes(&manifest)?;

    let cursor = Cursor::new(Vec::new());
    let mut archive = ZipWriter::new(cursor);
    let options = |compression| {
        SimpleFileOptions::default()
            .compression_method(compression)
            .unix_permissions(0o600)
    };
    let readme = entries.remove(0);
    archive.start_file(readme.path, options(CompressionMethod::Deflated))?;
    archive.write_all(&readme.bytes)?;
    archive.start_file("manifest.json", options(CompressionMethod::Deflated))?;
    archive.write_all(&manifest_bytes)?;
    for entry in entries {
        let compression = if entry.path.ends_with(".mp3")
            || entry.path.ends_with(".png")
            || entry.path.ends_with(".jpg")
            || entry.path.ends_with(".jpeg")
            || entry.path.ends_with(".webp")
        {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        archive.start_file(entry.path, options(compression))?;
        archive.write_all(&entry.bytes)?;
    }
    Ok(archive.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::GeneratedSongInput;
    use std::io::Read;

    fn wav_one_second() -> Vec<u8> {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(176_436u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&44_100u32.to_le_bytes());
        wav.extend_from_slice(&176_400u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&176_400u32.to_le_bytes());
        wav.resize(wav.len() + 176_400, 0);
        wav
    }

    fn zip_text(zip: &mut zip::ZipArchive<Cursor<Vec<u8>>>, name: &str) -> String {
        let mut value = String::new();
        zip.by_name(name)
            .unwrap()
            .read_to_string(&mut value)
            .unwrap();
        value
    }

    #[test]
    fn completed_bundle_has_audio_evidence_checksums_and_redaction() {
        let root = std::env::temp_dir().join(format!("music-diagnostics-{}", uuid::Uuid::now_v7()));
        let library = Library::open_at(root.join("library.sqlite"), root.join("media")).unwrap();
        let audio = wav_one_second();
        let imported = library.import_generated_song(GeneratedSongInput {
            title: Some("Trace Song".into()),
            metadata: json!({"api_token": "do-not-export", "internal_path": "/home/leon/private.wav"}),
            caption: "Global Metadata: test".into(), lyrics: "[verse]\nhello".into(),
            generation_settings: json!({
                "callback_url": "https://media.example/audio.wav?signature=private",
                "studio_diagnostics": {
                    "form": {"briefs": {"song_idea": "warm night drive"}, "final_copy": {"title": "Trace Song"}},
                    "assistant_trace": [{"target": "all", "status": "completed", "request": {"instruction": "make it warm"}, "receipt": {"request_id": "safe-request", "task_token": "do-not-export"}}]
                }
            }),
            replay_request: None, audio_codes: Some(json!("large-token-stream")), engine_id: "omnibridge".into(),
            profile_id: None, source: "test".into(), audio_extension: "wav", audio: audio.clone(),
        }).unwrap();

        let bytes = build_song_bundle(&library, SongBundleInput {
            requested_id: imported.song.id.clone(), song: Some(imported.song),
            job: Some(json!({"id": "job-safe", "status": "completed", "phase": "completed", "engine_id": "omnibridge", "generation_settings": {}})),
            durable: Some(json!({"local_job_id": "job-safe", "submit_state": "accepted", "status": "completed"})),
        }).unwrap();
        if let Some(path) = std::env::var_os("MUSIC_DIAGNOSTIC_ACCEPTANCE_PATH") {
            fs::write(&path, &bytes).unwrap();
            eprintln!("diagnostic_acceptance_zip={}", Path::new(&path).display());
        }
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let names = (0..zip.len())
            .map(|index| zip.by_index(index).unwrap().name().to_owned())
            .collect::<Vec<_>>();
        for name in &names {
            assert!(!name.starts_with('/') && !name.contains("..") && !name.contains('\\'));
        }
        for expected in [
            "README.md",
            "manifest.json",
            "input/creative-brief.txt",
            "input/assistant-activity.json",
            "structured/song-plan.json",
            "structured/caption.txt",
            "structured/lyrics.txt",
            "generation/request.json",
            "generation/timeline.json",
            "generation/receipt.json",
            "artifacts/audio.wav",
            "checksums.sha256",
        ] {
            assert!(zip.by_name(expected).is_ok(), "missing {expected}");
        }
        let manifest: Value = serde_json::from_str(&zip_text(&mut zip, "manifest.json")).unwrap();
        assert_eq!(manifest["task"]["audio"]["bytes"], audio.len());
        assert_eq!(manifest["task"]["audio"]["sha256"], sha256(&audio));
        assert!(
            (manifest["task"]["audio"]["duration_seconds"]
                .as_f64()
                .unwrap()
                - 1.0)
                .abs()
                < 0.001
        );
        let request = zip_text(&mut zip, "generation/request.json");
        assert!(!request.contains("private"));
        assert!(!request.contains("/home/leon"));
        let receipt = zip_text(&mut zip, "generation/receipt.json");
        assert!(!receipt.contains("do-not-export"));
        let checksum = zip_text(&mut zip, "checksums.sha256");
        assert!(checksum.contains(&format!("{}  artifacts/audio.wav", sha256(&audio))));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_job_bundle_exists_without_fabricating_audio() {
        let root =
            std::env::temp_dir().join(format!("music-diagnostics-failed-{}", uuid::Uuid::now_v7()));
        let library = Library::open_at(root.join("library.sqlite"), root.join("media")).unwrap();
        let bytes = build_song_bundle(&library, SongBundleInput {
            requested_id: "failed-job".into(), song: None,
            job: Some(json!({
                "id": "failed-job", "status": "failed", "phase": "failed", "dispatch": "omnibridge",
                "caption": "real saved caption", "lyrics": "", "duration_seconds": 60,
                "generation_settings": {"studio_diagnostics": null}, "message": "provider failed"
            })),
            durable: Some(json!({"local_job_id": "failed-job", "submit_state": "accepted", "status": "dead_letter"})),
        }).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let manifest: Value = serde_json::from_str(&zip_text(&mut zip, "manifest.json")).unwrap();
        assert_eq!(manifest["task"]["status"], "failed");
        assert_eq!(manifest["task"]["audio"]["exists"], false);
        assert!(zip.by_name("artifacts/audio.wav").is_err());
        assert_eq!(
            zip_text(&mut zip, "structured/caption.txt"),
            "real saved caption"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn entry_extensions_cannot_escape_the_archive() {
        assert_eq!(
            safe_extension(Path::new("song.../escape"), "bin", &["wav"]),
            "bin"
        );
        assert_eq!(
            safe_extension(Path::new("song.WAV"), "bin", &["wav"]),
            "wav"
        );
    }
}
