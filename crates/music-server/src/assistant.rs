//! The writing assistant: lyrics and the structured caption.
//!
//! MiniMax Music 3 does not write text. Its language model emits audio codes,
//! so every project that offers "a song from one line" uses a separate text
//! model for it — MiniMax's own demo included. This module reproduces that
//! step with the contract the official demo uses, so the caption it produces is
//! the shape the model was trained on.
//!
//! Two providers, both optional, because the manual form is the primary way in:
//!
//! * a local OpenAI-compatible server (llama.cpp, LM Studio, Ollama) — fully
//!   offline, the same approach Dub Studio takes for its own text model;
//! * OpenRouter, chosen from the live catalogue.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The caption contract, transcribed from MiniMax's official demo so the three
/// fields carry exactly the labelled structure the model expects.
const CAPTION_CONTRACT: &str = r#"The three caption fields follow the exact labeled style the model was trained on. Be concrete and musical; describe an energy arc and instrument lifecycles, never a static equipment list or decorative adjectives. Never contradict an explicit user constraint: instrumental stays instrumental; never reverse a required vocal gender, tempo limit, required instrument, or exclusion. Do not quote or paraphrase lyric lines inside the caption. Total caption length roughly 250-400 words.

global_metadata: one paragraph, in order: "Basic Attributes: bpm is <number>. key is <letter>, and scale is <major|minor>. <Genre / Subgenre>." then "Global Emotional Progression: <how the emotion evolves from the opening through the final section>." then "Application Scenarios & Imagery: <two or three vivid listening scenarios>." then "Sonics & Production Profile: <soundstage, frequency balance, dynamics, production character>."

vocal_details: one paragraph: "Vocal Gender & Timbre: Singer A (<Male|Female>), <timbre and register>." then "Vocal Style: <delivery, and how it shifts per section>." then "Harmony/Backing Vocals: <where harmonies or doubles appear and their character>." then "Vocal FX: <restrained treatment: reverb, delay, light compression>." For instrumental pieces write "Instrumental, no vocals." and name the instrument or texture carrying the lead melodic role.

arrangement: one paragraph: "Instrument Lifecycle Description (Primary/Secondary Layering): Primary: <core instruments present start to finish and their role>. Secondary: <instruments that enter, exit or intensify, and in which sections>." then "Groove & Foundation Progression: <how drums, bass and groove develop across sections>." then "Embellishments, Textures & Spatial FX: <fills, textures, transitional gestures, stereo and space treatment where relevant>." State what enters, exits, changes or intensifies for every section of the song, aligned with the lyric section tags."#;

/// The lyric rules, likewise transcribed: the tag vocabulary and the structure
/// sizing are what keep the sung result aligned with the requested length.
const LYRICS_RULES: &str = r#"lyrics: singable lyrics using ONLY these section tags, each ALWAYS ALONE on its own line: [intro] [verse] [pre-chorus] [chorus] [post-chorus] [bridge] [instrumental] [solo] [outro]. Never put words on the same line as a tag. Size the structure to the duration: <=30s: one verse + one chorus; ~60s: verse/pre-chorus/chorus/verse/chorus; >=120s: full structure with bridge and outro. Roughly 12-16 sung words per 10 seconds. Musical instructions (tempo, instruments, dynamics) never belong in the lyrics. If the song is instrumental, use [instrumental] sections with no words."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistTarget {
    /// Write both the lyrics and the three caption fields.
    All,
    /// Rewrite only the lyrics, keeping them coherent with the current caption.
    Lyrics,
    /// Rewrite only the caption, keeping it coherent with the current lyrics.
    Prompt,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistRequest {
    pub target: AssistTarget,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instruction: String,
    #[serde(default)]
    pub lyrics: String,
    #[serde(default)]
    pub global_metadata: String,
    #[serde(default)]
    pub vocal_details: String,
    #[serde(default)]
    pub arrangement: String,
    #[serde(default = "default_duration")]
    pub duration_seconds: f64,
    #[serde(default)]
    pub instrumental: bool,
}

fn default_duration() -> f64 {
    60.0
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AssistDraft {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_metadata: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vocal_details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrangement: Option<String>,
}

/// The system prompt and the JSON keys the answer must carry.
pub fn instructions(request: &AssistRequest) -> (String, &'static [&'static str]) {
    match request.target {
        AssistTarget::Lyrics => (
            format!(
                "You write lyrics for MiniMax Music 3, a lyrics+description music generation model.\n\
                 Given a lyrics instruction, the current structured prompt (global metadata, vocal details, arrangement) and a target duration, write lyrics coherent with that structured prompt.\n\
                 {LYRICS_RULES}\n\
                 Answer with ONLY a JSON object with key: lyrics."
            ),
            &["lyrics"],
        ),
        AssistTarget::Prompt => (
            format!(
                "You write the structured caption for MiniMax Music 3, a lyrics+description music generation model.\n\
                 Given a sound instruction and/or lyrics, produce global_metadata, vocal_details and arrangement. Build the arrangement timeline around the lyric section tags when lyrics are provided. {CAPTION_CONTRACT}\n\
                 Answer with ONLY a JSON object with keys: global_metadata, vocal_details, arrangement."
            ),
            &["global_metadata", "vocal_details", "arrangement"],
        ),
        AssistTarget::All => (
            format!(
                "You write inputs for MiniMax Music 3, a lyrics+description music generation model.\n\
                 Given a song description and a target duration, produce:\n\
                 1. {LYRICS_RULES}\n\
                 2-4. global_metadata, vocal_details, arrangement — a structured caption. {CAPTION_CONTRACT}\n\
                 Answer with ONLY a JSON object with keys: lyrics, global_metadata, vocal_details, arrangement."
            ),
            &["lyrics", "global_metadata", "vocal_details", "arrangement"],
        ),
    }
}

/// The user message, carrying whichever side of the song already exists so the
/// two halves stay coherent.
pub fn user_message(request: &AssistRequest) -> String {
    let instruction = request.instruction.trim();
    let description = request.description.trim();
    let brief = if !instruction.is_empty() { instruction } else { description };
    let instrumental = if request.instrumental { "\nThis piece is instrumental: no sung words." } else { "" };

    match request.target {
        AssistTarget::Lyrics => format!(
            "Lyrics instruction: {}\nCurrent structured prompt, keep the lyrics coherent with it:\nGlobal metadata: {}\nVocal details: {}\nArrangement: {}\nTarget duration: {} seconds.{instrumental}",
            if brief.is_empty() { "(none — write lyrics that fit the structured prompt)" } else { brief },
            request.global_metadata.trim(),
            request.vocal_details.trim(),
            request.arrangement.trim(),
            request.duration_seconds.round() as i64,
        ),
        AssistTarget::Prompt => format!(
            "Sound instruction: {}\nCurrent lyrics, keep the structured prompt coherent with them:\n{}{instrumental}",
            if brief.is_empty() { "(none — describe a sound that fits the lyrics)" } else { brief },
            request.lyrics.trim(),
        ),
        AssistTarget::All => format!(
            "Song description: {}\nTarget duration: {} seconds.{instrumental}",
            if brief.is_empty() { "(none — choose something musical and specific)" } else { brief },
            request.duration_seconds.round() as i64,
        ),
    }
}

/// Extracts the answer, tolerating a model that wraps its JSON in prose or a
/// code fence — a local 4B model does that more often than a hosted one.
pub fn parse_draft(content: &str, required: &[&str]) -> Result<AssistDraft> {
    let start = content.find('{').context("the assistant returned no JSON object")?;
    let end = content.rfind('}').context("the assistant returned no JSON object")?;
    if end <= start {
        bail!("the assistant returned no JSON object");
    }
    let value: Value = serde_json::from_str(&content[start..=end]).context("the assistant returned invalid JSON")?;
    let field = |key: &str| value.get(key).and_then(Value::as_str).map(|text| text.trim().to_owned()).filter(|text| !text.is_empty());

    for key in required {
        if field(key).is_none() {
            bail!("the assistant answer is missing '{key}'");
        }
    }
    Ok(AssistDraft {
        lyrics: field("lyrics"),
        global_metadata: field("global_metadata"),
        vocal_details: field("vocal_details"),
        arrangement: field("arrangement"),
    })
}

/// An OpenAI-compatible chat request. Both providers speak this shape, so the
/// only difference between them is the endpoint and the credential.
pub fn chat_body(model: &str, system: &str, user: &str) -> Value {
    serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "temperature": 0.8,
        "stream": false,
    })
}

pub fn content_of(response: &Value) -> Result<String> {
    response
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("the assistant response contained no message content")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(target: AssistTarget) -> AssistRequest {
        AssistRequest {
            target,
            description: "a night drive synth pop song".into(),
            instruction: String::new(),
            lyrics: "[verse]\nneon".into(),
            global_metadata: "Basic Attributes: bpm is 110.".into(),
            vocal_details: "Vocal Gender & Timbre: Singer A (Female)".into(),
            arrangement: "Primary: synths".into(),
            duration_seconds: 90.0,
            instrumental: false,
        }
    }

    /// Each target must ask for exactly the fields it will write back, otherwise
    /// a partial answer would silently blank a pane the user had filled in.
    #[test]
    fn every_target_declares_the_fields_it_writes() {
        assert_eq!(instructions(&request(AssistTarget::Lyrics)).1, &["lyrics"]);
        assert_eq!(instructions(&request(AssistTarget::Prompt)).1, &["global_metadata", "vocal_details", "arrangement"]);
        assert_eq!(instructions(&request(AssistTarget::All)).1.len(), 4);
    }

    #[test]
    fn the_other_half_of_the_song_travels_as_context() {
        let lyrics_message = user_message(&request(AssistTarget::Lyrics));
        assert!(lyrics_message.contains("Basic Attributes: bpm is 110."));
        assert!(lyrics_message.contains("90 seconds"));

        let prompt_message = user_message(&request(AssistTarget::Prompt));
        assert!(prompt_message.contains("[verse]"));
    }

    #[test]
    fn an_instrumental_request_says_so_to_the_model() {
        let mut instrumental = request(AssistTarget::All);
        instrumental.instrumental = true;
        assert!(user_message(&instrumental).contains("instrumental"));
    }

    #[test]
    fn json_survives_a_code_fence_and_surrounding_prose() {
        let draft = parse_draft(
            "Sure!\n```json\n{\"lyrics\": \"[verse]\\nline\"}\n```\nHope that helps.",
            &["lyrics"],
        )
        .unwrap();
        assert_eq!(draft.lyrics.unwrap(), "[verse]\nline");
    }

    #[test]
    fn a_missing_required_field_is_an_error_rather_than_a_blank_pane() {
        assert!(parse_draft("{\"lyrics\": \"x\"}", &["global_metadata"]).is_err());
        assert!(parse_draft("no json here", &["lyrics"]).is_err());
    }
}
