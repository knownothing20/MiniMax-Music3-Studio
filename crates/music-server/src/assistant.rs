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

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The caption contract, transcribed from MiniMax's official demo so the three
/// fields carry exactly the labelled structure the model expects.
/// The two extras every draft carries: a name for the track and a sentence the
/// image model can draw from.
const COPY_EXTRA: &str = "title: a short song title, two to five words, no quotation marks, in the language of the lyrics. cover_prompt: one concise sentence describing a cover image for this track - a scene, not a poster; no text, no lettering, no logos.";
const DURATION_EXTRA: &str = "duration_seconds: a suggested generation ceiling inferred from the completed lyrics and section structure, between 30 and 360 seconds. It is a planning suggestion, not a duration detected from audio.";

/// The cloud API accepts one combined prompt of at most 2,000 characters.
/// The assistant works to a lower product target so a model that counts
/// loosely does not leave the editable draft one sentence away from rejection.
const MAX_CAPTION_CHARS: usize = 1_900;
const TARGET_CAPTION_CHARS: usize = 1_500;
const MAX_LYRICS_CHARS: usize = 3_500;
const CAPTION_HEADING_CHARS: usize = 44;

const STANDARD_LYRICS_VERSION: &str = "standard_lyrics.v1";
const STORY_SONGWRITING_VERSION: &str = "story_songwriting.v1";
const STANDARD_CAPTION_VERSION: &str = "standard_caption.v1";
const CAPTION_REWRITER_VERSION: &str = "music3_caption_rewriter.v1";

const VALIDATION: &str = "Before answering, check your own draft: every explicit user constraint kept, an instrumental request still instrumental, vocal gender not contradicted, every section tag present in its own section, no lyric line quoted or summarised, no song title inside the caption fields, no invented exact BPM or key, no sentence copied from a reference, and the combined structured-caption and lyrics budgets satisfied. Fix what fails, then answer.";

const CAPTION_CONTRACT: &str = r#"The three caption fields follow the exact labeled style the model was trained on, and the rules below are MiniMax's own, from the music-caption-rewriter skill they publish with the model.

Be concrete and musical: describe an energy arc and instrument lifecycles, never a static equipment list or decorative adjectives. Preserve every explicit user constraint - an instrumental request stays instrumental, and a required vocal gender, tempo limit, required instrument or exclusion is never reversed. Do not invent a precise key, BPM, vocal gender or production technique when a broader description is sufficient; use a range or a qualitative tempo instead. Never quote, paraphrase or summarise a lyric line inside the caption, and never include a song title or track id. Write in English unless the user explicitly asks for another language.

CLOUD API CHARACTER BUDGET (count every character including spaces): target <= 1450 characters for the three fields plus their headings, and never exceed the hard ceiling of 1900 characters. The server normalizes the draft to a 1500-character product target before it reaches the form, leaving substantial room below the official 2000-character prompt limit. There is no hard per-field allocation. Distribute the budget according to the song: as a starting point use roughly 350-450 characters for global_metadata, 220-300 for vocal_details, and the remaining space for arrangement; unused space from one field may be used by another. These are flexible guidance, not separate limits. Prefer short clauses, include only details that change the sound, and never repeat the same mood, instrument, vocal trait or section event in multiple fields. When the user supplies prose or copy to be sung as lyrics, keep its narrative in lyrics: the caption describes only the sound and must not retell, quote or paraphrase that prose.

global_metadata: genre and subgenres, tempo, emotional progression, and the overall sonic and production profile, in this order: "Basic Attributes: bpm is <number or range>. key is <letter>, and scale is <major|minor>. <Genre / Subgenre>." then "Global Emotional Progression: <how the emotion evolves from the opening through the final section>." then "Application Scenarios & Imagery: <two or three vivid listening scenarios>." then "Sonics & Production Profile: <soundstage, frequency balance, dynamics, production character>." Include key and scale only when explicit or musically useful.

vocal_details: for vocal music describe the lead configuration, timbre, register, delivery, harmony or backing vocals and restrained vocal effects: "Vocal Gender & Timbre: Singer A (<Male|Female>), <timbre and register>." then "Vocal Style: <delivery, and how it shifts per section>." then "Harmony/Backing Vocals: <where harmonies or doubles appear and their character>." then "Vocal FX: <restrained treatment: reverb, delay, light compression>." For instrumental music state that the piece is instrumental and name the instrument or texture carrying the lead melodic role. Do not invent lyrical subject matter.

arrangement: a compact section timeline: "Instrument Lifecycle Description: <one concise sentence for primary and secondary layers>." then "Groove & Foundation Progression: <one concise sentence>." then "Section Timeline: <at most six short section clauses, grouping sections that behave alike>." then "Textures & Spatial FX: <one concise sentence only when relevant>." State only meaningful entries, exits or intensifications aligned with the lyric tags; do not narrate every bar and do not repeat instrument lists."#;

const STANDARD_CAPTION_CONTRACT: &str = r#"Write a clear standard structured description in the same three fields the Music3 form accepts. This is the project's ordinary description writer, not the Music3 Caption Rewriter skill: do not use its reference-caption router or template examples.

Keep the description concrete, concise and in English unless the user asks otherwise. Preserve explicit genre, tempo, vocal, instrumentation and exclusion constraints. Do not quote lyrics inside the description. The three fields plus headings target at most 1450 characters and must never exceed the same hard ceiling of 1900 characters used by the enhanced path.

global_metadata: summarize genre, tempo, mood progression and overall production character in a compact paragraph.

vocal_details: summarize lead voice, delivery, harmony and restrained vocal effects; for instrumental music state that there are no vocals and identify the lead melodic instrument.

arrangement: summarize the section order, core instrumentation, groove changes and the most important entries, exits or transitions without narrating every bar."#;

/// The lyric rules, likewise transcribed: the tag vocabulary and the structure
/// sizing are what keep the sung result aligned with the requested length.
const LYRICS_RULES: &str = r#"lyrics: singable lyrics using ONLY these section tags, each ALWAYS ALONE on its own line: [intro] [verse] [pre-chorus] [chorus] [post-chorus] [bridge] [instrumental] [solo] [outro]. Never put words on the same line as a tag - the engine keeps the tag and throws that line's words away. The complete lyrics must not exceed 3500 characters. When the user supplies prose or copy and asks to sing it, preserve its meaning and key phrases, reshape only for singability and repetition, and do not expand it with a new plot, viewpoint or commentary. Size the structure to the duration: <=30s: one verse + one chorus; ~60s: verse/pre-chorus/chorus/verse/chorus; >=120s: full structure with bridge and outro. Roughly 12-16 sung words per 10 seconds, and keep neighbouring lines close in length: a line much denser than the one before it gets sung rushed. The engine does not budget time - it sings until the clock runs out and stops there, mid-phrase if it has to - so write slightly less than the duration allows and never leave the song's payoff line for the outro. Musical instructions (tempo, instruments, dynamics) never belong in the lyrics. If the song is instrumental, write the same structure a sung song would have - [intro] [verse] [chorus] [bridge] [outro] - with no words under any of them, and use [instrumental] or [solo] only where a real instrumental passage belongs, the way a band would play one. Alternating [instrumental] with every other tag is not what the tag is for. Write the lyrics in the language the user wrote their request in: a Russian idea gets Russian lyrics, a Japanese one Japanese. The caption fields stay English - that is what the engine reads - but nobody asked for an English song."#;

const STORY_SONGWRITING_CONTRACT: &str = r#"Story songwriting is an application-owned lyrics method. Treat supplied prose as source material, not an invitation to invent a larger story. Preserve concrete, verifiable details and the author's original point of view; do not add important facts, events, opinions or relationships that are not present. Find one concise hook that carries the central meaning, then build a clear emotional arc toward it. Prefer short, singable lines with natural breath points and restrained repetition. Keep distinctive source phrases when they sing cleanly, but never imitate a named writer, lyricist or artist.

This stage produces only a short title and lyrics. Do not output music direction, genre, tempo, instruments, vocal production, structured-caption fields, external websites, registration steps, audio-generation instructions or hidden reasoning. When the source is a review, essay or personal account, retain its meaning without turning it into a new plot or adding a new conclusion."#;

/// Pronunciation, which the caption cannot reach: the engine reads the lyrics
/// as characters, so the only place to correct a mis-sung word is the word.
const DICTION_RULE: &str = r#"
Diction: the model sings the letters it is given. In Russian write ё as ё rather than е, and mark the stressed vowel with a combining acute - за́мок, замо́к - only where the word would otherwise be read wrong: homographs, rare words, proper names, and a word whose natural stress fights the beat. Never accent every word; a page of accents reads as noise. In other languages do the same locally - respell or transcribe only the individual words that come out wrong, and leave the rest alone."#;

/// Two voices, from a community experiment on the released weights: ~30
/// generations with pinned seeds, one variable at a time. Describing both
/// singers in the caption alone never worked; short tags in the lyrics did.
const DUET_RULE: &str = r#"
Two voices: name both singers in vocal_details ("Singer A (Male), <timbre>. Singer B (Female), <timbre>."), say plainly which one is heard first, and state each section assignment in full. When exactly two voices are wanted, say so as an exclusion: no doubling, no stacked harmonies, no backing choir, never more than two human voices at once. Keep singer assignments in vocal_details and arrangement; lyrics may use only the accepted structural tags and must never introduce speaker tags such as [male vocal], [female vocal] or [duet]. Switch at section or couplet level, never line by line. Describe each voice once, plainly and confidently."#;

/// The failure mode of an instrumental request: vocals creep back in. Naming
/// what carries the melody instead leaves the model something to sing with.
const INSTRUMENTAL_RULE: &str = r#"
Instrumental: state in vocal_details that the piece is instrumental with no sung words, no wordless or sampled vocals and no choir, and name the instrument carrying the lead melodic line in every section that would otherwise have carried a vocal."#;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsStrategy {
    #[default]
    Standard,
    StorySongwriting,
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
    #[serde(default = "default_true")]
    pub use_caption_rewriter: bool,
    #[serde(default)]
    pub lyrics_strategy: LyricsStrategy,
}

fn default_true() -> bool {
    true
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
    /// A name for the track. The model has the words and the mood in front of
    /// it; asking the user to invent one afterwards is asking twice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the cover should show, in one sentence, ready for an image model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_prompt: Option<String>,
    /// How long the song it just wrote should be. The model laid out the
    /// sections, so it is the one that knows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssistAudit {
    pub stage: &'static str,
    pub strategy_name: String,
    pub contract_version: &'static str,
    pub input_summary: Value,
    pub output_summary: Value,
    pub validation: Vec<String>,
    pub compression_actions: Vec<String>,
}

/// The system prompt and the JSON keys the answer must carry.
pub fn instructions(request: &AssistRequest) -> (String, &'static [&'static str]) {
    // Simple and Studio share one persisted choice. Turning the Skill off
    // keeps description generation available through the standard contract.
    let caption_rewriter = request.use_caption_rewriter;
    let references = if caption_rewriter { references_for(request) } else { String::new() };
    let caption_contract = if caption_rewriter { CAPTION_CONTRACT } else { STANDARD_CAPTION_CONTRACT };
    let notes = craft_notes(request);
    match request.target {
        AssistTarget::Lyrics => (
            format!(
                "You are the isolated lyrics-stage role for MiniMax Music 3. Given a lyrics instruction, optional current structured-caption context and a target duration, write a title and lyrics.\n\
                 {LYRICS_RULES}\n\
                 {strategy_contract}{notes}\n\
                 Return only visible final copy. Answer with ONLY a JSON object with keys: title, lyrics.",
                strategy_contract = match request.lyrics_strategy {
                    LyricsStrategy::Standard => "Use the standard project lyrics rules. Do not output structured-caption fields, music direction, external-service instructions or hidden reasoning.",
                    LyricsStrategy::StorySongwriting => STORY_SONGWRITING_CONTRACT,
                }
            ),
            &["title", "lyrics"],
        ),
        AssistTarget::Prompt => (
            format!(
                "You write the structured caption for MiniMax Music 3, a lyrics+description music generation model.\n\
                 Given a sound instruction and/or lyrics, produce global_metadata, vocal_details and arrangement. Build the arrangement timeline around the lyric section tags when lyrics are provided. {caption_contract}{notes}\n\
                 Do not quote, rewrite or return the lyrics. Do not return title, cover_prompt, duration_seconds or hidden reasoning.\n\
                 Answer with ONLY a JSON object with keys: global_metadata, vocal_details, arrangement.
                 {references}"
            ),
            &["global_metadata", "vocal_details", "arrangement"],
        ),
        AssistTarget::All => (
            format!(
                "You write inputs for MiniMax Music 3, a lyrics+description music generation model.\n\
                 Given a song description and a target duration, produce:\n\
                 1. {LYRICS_RULES}\n\
                 2-4. global_metadata, vocal_details, arrangement — a structured caption. {caption_contract}{notes}\n\
                 5-6. {COPY_EXTRA}\n\
                 7. {DURATION_EXTRA}\n\
                 Answer with ONLY a JSON object with keys: lyrics, global_metadata, vocal_details, arrangement, title, cover_prompt, duration_seconds.
                 {VALIDATION}{references}"
            ),
            &["lyrics", "global_metadata", "vocal_details", "arrangement"],
        ),
    }
}

/// Stable application-owned prompt contracts and generation rules.
///
/// This describes current product logic. It is not hidden model reasoning and
/// must not be presented as a verbatim prompt used by a historical run.
pub fn diagnostic_prompt_contracts() -> Value {
    serde_json::json!({
        "schema_version": 2,
        "kind": "application_prompt_contracts",
        "scope": "Stable application-owned prompt contracts and generation rules used by Music Maker.",
        "runtime_prompt_semantics": "This file is not model hidden reasoning and is not evidence of the verbatim runtime prompt for a historical request. Actual user input and persisted assistant activity are exported separately; unrecorded runtime text is not reconstructed.",
        "targets": {
            "all": "lyrics + structured caption + title + cover prompt + suggested duration",
            "prompt": "structured caption + title + cover prompt + suggested duration",
            "lyrics": "lyrics coherent with the current structured caption"
        },
        "caption_contract": CAPTION_CONTRACT,
        "standard_caption_contract": STANDARD_CAPTION_CONTRACT,
        "simple_mode_caption_rewriter_default": true,
        "lyrics_rules": LYRICS_RULES,
        "story_songwriting_contract": STORY_SONGWRITING_CONTRACT,
        "contract_versions": {
            "standard_lyrics": STANDARD_LYRICS_VERSION,
            "story_songwriting": STORY_SONGWRITING_VERSION,
            "standard_caption": STANDARD_CAPTION_VERSION,
            "caption_rewriter": CAPTION_REWRITER_VERSION,
        },
        "copy_output_contract": COPY_EXTRA,
        "duration_output_contract": DURATION_EXTRA,
        "validation_contract": VALIDATION,
        "conditional_rules": {
            "diction": DICTION_RULE,
            "duet": DUET_RULE,
            "instrumental": INSTRUMENTAL_RULE
        }
    })
}

/// The rules that only apply to some songs.
///
/// Everything here costs prompt room and, on a small local model, attention.
/// A diction rule matters whenever words are being written; the duet rule
/// matters for the few songs that have two singers, and stating it for a solo
/// vocal would only invite one. So each arrives when the request calls for it.
fn craft_notes(request: &AssistRequest) -> String {
    let mut notes = String::new();
    if request.target != AssistTarget::Prompt && !request.instrumental {
        notes.push_str(DICTION_RULE);
    }
    if request.instrumental {
        notes.push_str(INSTRUMENTAL_RULE);
    } else if wants_two_voices(request) {
        notes.push_str(DUET_RULE);
    }
    notes
}

/// Whether the song has two singers, read from whatever the user wrote.
fn wants_two_voices(request: &AssistRequest) -> bool {
    const CUES: &[&str] = &[
        "duet",
        "дуэт",
        "two voices",
        "два голоса",
        "male and female",
        "female and male",
        "мужской и женский",
        "женский и мужской",
        "singer b",
        "call and response",
        "перекличк",
        "вдвоём",
        "вдвоем",
    ];
    let brief = format!(
        "{} {} {} {}",
        request.description, request.instruction, request.vocal_details, request.lyrics
    )
    .to_lowercase();
    CUES.iter().any(|cue| brief.contains(cue))
}

/// The user message, carrying whichever side of the song already exists so the
/// two halves stay coherent.
/// Complete reference captions from MiniMax's own template library, chosen by
/// the skill's genre router. The skill's whole method is to show the model
/// two or three captions from the right family rather than describe the style
/// in the abstract.
fn references_for(request: &AssistRequest) -> String {
    let brief = format!("{} {} {}", request.description, request.instruction, request.global_metadata);
    let references = crate::skill::references(&brief);
    if references.is_empty() {
        return String::new();
    }
    let mut block = String::from("

Reference captions from MiniMax's own library, in the style family this request routes to. Use them for musical identity, section logic and level of detail. Do not copy their sentences, key, bpm, instruments or story - write a new caption for this request.
");
    for (index, reference) in references.iter().enumerate() {
        block.push_str(&format!("
--- reference {} ---
{}
", index + 1, reference.trim()));
    }
    block
}

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
        AssistTarget::All => {
            // Whatever the user already wrote is material, not noise: it goes to
            // the model so the rest is built around it instead of replacing it.
            // Empty fields are simply not mentioned.
            let mut carried = String::new();
            let mut carry = |label: &str, value: &str| {
                let value = value.trim();
                if !value.is_empty() {
                    carried.push_str(&format!("
{label} (the user wrote this - keep it, build around it):
{value}"));
                }
            };
            carry("Lyrics", &request.lyrics);
            carry("Global metadata", &request.global_metadata);
            carry("Vocal details", &request.vocal_details);
            carry("Arrangement", &request.arrangement);
            format!(
                "Song description: {}{carried}{instrumental}",
                if brief.is_empty() { "(none - choose something musical and specific)" } else { brief },
            )
        }
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
    let value: Value = serde_json::from_str(&content[start..=end]).with_context(|| {
        // Naming the failure without showing the answer leaves nothing to act
        // on: the interesting part is what the model actually wrote.
        let sample: String = content.chars().take(220).collect();
        format!("the assistant returned invalid JSON. It answered: {sample}")
    })?;
    // A model answers with what it finds natural: a string for the caption, and
    // very often an array of lines for the lyrics. Both are the same lyric.
    let field = |key: &str| -> Option<String> {
        let text = match value.get(key)? {
            Value::String(text) => text.trim().to_owned(),
            Value::Array(items) => items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join("
")
                .trim()
                .to_owned(),
            _ => return None,
        };
        (!text.is_empty()).then_some(text)
    };

    for key in required {
        if field(key).is_none() {
            bail!("the assistant answer is missing '{key}'");
        }
    }
    let mut draft = AssistDraft {
        lyrics: field("lyrics"),
        global_metadata: field("global_metadata"),
        vocal_details: field("vocal_details"),
        arrangement: field("arrangement"),
        title: field("title"),
        cover_prompt: field("cover_prompt"),
        // Accepted as a number or as the string a model sometimes sends, and
        // kept inside what the engine can render.
        duration_seconds: value
            .get("duration_seconds")
            .and_then(|value| value.as_u64().or_else(|| value.as_str().and_then(|text| text.trim().parse().ok())))
            .map(|seconds| seconds.clamp(10, 360) as u32),
    };

    let within = |label: &str, value: Option<&String>, limit: usize| -> Result<()> {
        if let Some(value) = value {
            let count = value.encode_utf16().count();
            if count > limit {
                bail!("the assistant returned {label} with {count} characters; the limit is {limit}");
            }
        }
        Ok(())
    };
    // MiniMax cloud limits the combined prompt, not three arbitrary slices.
    // Only lyrics have an independent API ceiling.
    within("lyrics", draft.lyrics.as_ref(), MAX_LYRICS_CHARS)?;
    compact_caption(&mut draft);
    let caption_chars = caption_chars(&draft);
    debug_assert!(caption_chars <= TARGET_CAPTION_CHARS);
    if caption_chars > MAX_CAPTION_CHARS {
        bail!("the assistant caption could not be compacted below {MAX_CAPTION_CHARS} characters");
    }
    Ok(draft)
}

/// Parse and validate one visible stage. This keeps the two assistants from
/// leaking fields into one another while retaining parse_draft for legacy callers.
pub fn parse_draft_for_request(
    content: &str,
    required: &[&str],
    request: &AssistRequest,
) -> Result<(AssistDraft, AssistAudit)> {
    let mut draft = parse_draft(content, required)?;
    let mut validation = Vec::new();
    let mut compression_actions = Vec::new();

    match request.target {
        AssistTarget::Lyrics => {
            if draft.global_metadata.is_some()
                || draft.vocal_details.is_some()
                || draft.arrangement.is_some()
                || draft.cover_prompt.is_some()
                || draft.duration_seconds.is_some()
            {
                bail!("the isolated lyrics stage returned structured-caption or generation fields");
            }
            let lyrics = draft.lyrics.take().context("the assistant answer is missing 'lyrics'")?;
            let normalized = normalize_lyrics_tags(&lyrics)?;
            validation.push("lyrics_tags_normalized".to_owned());
            if request.lyrics_strategy == LyricsStrategy::StorySongwriting {
                let visible = format!("{}\n{}", draft.title.as_deref().unwrap_or_default(), normalized).to_lowercase();
                if ["http://", "https://", "www.", "register at", "sign up at", "注册网站"]
                    .iter()
                    .any(|needle| visible.contains(needle))
                {
                    bail!("the story songwriting stage returned prohibited external-service instructions");
                }
                validation.push("story_output_has_no_external_instructions".to_owned());
            }
            draft.lyrics = Some(normalized);
        }
        AssistTarget::Prompt => {
            if draft.lyrics.is_some()
                || draft.title.is_some()
                || draft.cover_prompt.is_some()
                || draft.duration_seconds.is_some()
            {
                bail!("the isolated description stage returned lyrics, title or generation fields");
            }
            reject_lyric_quotes(&draft, &request.lyrics)?;
            validation.push("caption_contains_no_lyric_lines".to_owned());
            if caption_chars(&draft) >= TARGET_CAPTION_CHARS {
                compression_actions.push("deterministic_caption_compaction_checked".to_owned());
            }
        }
        AssistTarget::All => {
            if let Some(lyrics) = draft.lyrics.take() {
                draft.lyrics = Some(normalize_lyrics_tags(&lyrics)?);
                validation.push("lyrics_tags_normalized".to_owned());
            }
        }
    }

    let stage = match request.target {
        AssistTarget::Lyrics => "lyrics",
        AssistTarget::Prompt => "structured_caption",
        AssistTarget::All => "legacy_combined",
    };
    let (strategy_name, contract_version) = match request.target {
        AssistTarget::Lyrics => match request.lyrics_strategy {
            LyricsStrategy::Standard => ("standard", STANDARD_LYRICS_VERSION),
            LyricsStrategy::StorySongwriting => ("story_songwriting", STORY_SONGWRITING_VERSION),
        },
        AssistTarget::Prompt | AssistTarget::All if request.use_caption_rewriter => {
            ("music3_caption_rewriter", CAPTION_REWRITER_VERSION)
        }
        AssistTarget::Prompt | AssistTarget::All => ("standard_description", STANDARD_CAPTION_VERSION),
    };
    validation.push("stage_schema_isolated".to_owned());
    validation.push("visible_output_only".to_owned());

    let audit = AssistAudit {
        stage,
        strategy_name: strategy_name.to_owned(),
        contract_version,
        input_summary: serde_json::json!({
            "instruction_chars": request.instruction.encode_utf16().count(),
            "description_chars": request.description.encode_utf16().count(),
            "lyrics_context_chars": request.lyrics.encode_utf16().count(),
            "duration_seconds": request.duration_seconds,
            "instrumental": request.instrumental,
        }),
        output_summary: serde_json::json!({
            "title": draft.title,
            "lyrics_chars": draft.lyrics.as_deref().unwrap_or_default().encode_utf16().count(),
            "caption_chars": caption_chars(&draft),
            "section_tags": draft.lyrics.as_deref().map(section_tags).unwrap_or_default(),
        }),
        validation,
        compression_actions,
    };
    Ok((draft, audit))
}

fn normalize_lyrics_tags(lyrics: &str) -> Result<String> {
    let mut normalized = Vec::new();
    let mut saw_tag = false;
    for line in lyrics.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if !trimmed.ends_with(']') || trimmed.matches(']').count() != 1 {
                bail!("lyrics section tags must be alone on their line: '{trimmed}'");
            }
            let inner = trimmed[1..trimmed.len() - 1]
                .trim()
                .to_lowercase()
                .replace('_', "-")
                .replace(' ', "-");
            let plain = inner
                .split('-')
                .filter(|part| !part.is_empty() && !part.chars().all(|ch| ch.is_ascii_digit()))
                .filter(|part| !matches!(*part, "final" | "repeat" | "reprise"))
                .collect::<Vec<_>>()
                .join("-");
            let tag = match plain.as_str() {
                "intro" => "intro",
                "verse" => "verse",
                "pre-chorus" | "prechorus" => "pre-chorus",
                "chorus" => "chorus",
                "post-chorus" | "postchorus" => "post-chorus",
                "bridge" => "bridge",
                "instrumental" => "instrumental",
                "solo" => "solo",
                "outro" => "outro",
                _ => bail!("the assistant returned unsupported lyrics tag '{trimmed}'"),
            };
            normalized.push(format!("[{tag}]"));
            saw_tag = true;
        } else {
            normalized.push(line.trim_end().to_owned());
        }
    }
    if !saw_tag {
        bail!("the assistant lyrics contain no supported section tags");
    }
    Ok(normalized.join("\n").trim().to_owned())
}

fn section_tags(lyrics: &str) -> Vec<&str> {
    lyrics
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('[') && line.ends_with(']'))
        .collect()
}

fn reject_lyric_quotes(draft: &AssistDraft, lyrics: &str) -> Result<()> {
    let caption = format!(
        "{} {} {}",
        draft.global_metadata.as_deref().unwrap_or_default(),
        draft.vocal_details.as_deref().unwrap_or_default(),
        draft.arrangement.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    for line in lyrics.lines().map(str::trim).filter(|line| {
        !line.is_empty()
            && !(line.starts_with('[') && line.ends_with(']'))
            && line.chars().count() >= 6
    }) {
        if caption.contains(&line.to_lowercase()) {
            bail!("the structured caption repeats a lyric line");
        }
    }
    Ok(())
}

fn caption_chars(draft: &AssistDraft) -> usize {
    draft.global_metadata.as_deref().unwrap_or_default().encode_utf16().count()
        + draft.vocal_details.as_deref().unwrap_or_default().encode_utf16().count()
        + draft.arrangement.as_deref().unwrap_or_default().encode_utf16().count()
        + CAPTION_HEADING_CHARS
}

/// Hosted models do not always obey a character budget exactly. Compact their
/// answer deterministically instead of asking for another paid generation or
/// returning an error to the form. Short fields are protected first; only the
/// material that can actually shrink shares the required reduction.
fn compact_caption(draft: &mut AssistDraft) {
    let lengths = [
        draft.global_metadata.as_deref().unwrap_or_default().encode_utf16().count(),
        draft.vocal_details.as_deref().unwrap_or_default().encode_utf16().count(),
        draft.arrangement.as_deref().unwrap_or_default().encode_utf16().count(),
    ];
    let content_budget = TARGET_CAPTION_CHARS.saturating_sub(CAPTION_HEADING_CHARS);
    let total: usize = lengths.iter().sum();
    if total <= content_budget {
        return;
    }

    // These are preservation floors, not API field limits. They keep a short
    // field intact and let a verbose field absorb more of the reduction.
    let floors = [
        300_usize.min(lengths[0]),
        220_usize.min(lengths[1]),
        400_usize.min(lengths[2]),
    ];
    let capacities = [
        lengths[0] - floors[0],
        lengths[1] - floors[1],
        lengths[2] - floors[2],
    ];
    let capacity_total: usize = capacities.iter().sum();
    let excess = total - content_budget;
    let mut reductions = [0_usize; 3];
    if capacity_total > 0 {
        for index in 0..3 {
            reductions[index] = excess.saturating_mul(capacities[index]) / capacity_total;
        }
    }
    let mut remaining = excess.saturating_sub(reductions.iter().sum());
    while remaining > 0 {
        let mut progressed = false;
        for index in 0..3 {
            if reductions[index] < capacities[index] {
                reductions[index] += 1;
                remaining -= 1;
                progressed = true;
                if remaining == 0 {
                    break;
                }
            }
        }
        if !progressed {
            break;
        }
    }
    let budgets = [
        lengths[0].saturating_sub(reductions[0]),
        lengths[1].saturating_sub(reductions[1]),
        lengths[2].saturating_sub(reductions[2]),
    ];

    compact_field(
        &mut draft.global_metadata,
        &[
            "Basic Attributes:",
            "Global Emotional Progression:",
            "Application Scenarios & Imagery:",
            "Sonics & Production Profile:",
        ],
        budgets[0],
    );
    compact_field(
        &mut draft.vocal_details,
        &["Vocal Gender & Timbre:", "Vocal Style:", "Harmony/Backing Vocals:", "Vocal FX:"],
        budgets[1],
    );
    compact_field(
        &mut draft.arrangement,
        &[
            "Instrument Lifecycle Description:",
            "Groove & Foundation Progression:",
            "Section Timeline:",
            "Textures & Spatial FX:",
        ],
        budgets[2],
    );
}

fn compact_field(field: &mut Option<String>, labels: &[&str], budget: usize) {
    let Some(value) = field.as_mut() else { return };
    if value.encode_utf16().count() <= budget {
        return;
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.encode_utf16().count() <= budget {
        *value = normalized;
        return;
    }

    let mut found = labels
        .iter()
        .filter_map(|label| normalized.find(label).map(|start| (start, *label)))
        .collect::<Vec<_>>();
    found.sort_by_key(|(start, _)| *start);
    if found.len() < 2 {
        *value = truncate_utf16(&normalized, budget);
        return;
    }

    let mut bodies = Vec::with_capacity(found.len());
    for index in 0..found.len() {
        let (start, label) = found[index];
        let body_start = start + label.len();
        let body_end = found.get(index + 1).map(|(next, _)| *next).unwrap_or(normalized.len());
        bodies.push(normalized[body_start..body_end].trim());
    }
    let label_cost = found.iter().map(|(_, label)| label.encode_utf16().count()).sum::<usize>() + found.len() - 1;
    if label_cost >= budget {
        *value = truncate_utf16(&normalized, budget);
        return;
    }
    let body_budget = budget - label_cost;
    let body_lengths = bodies.iter().map(|body| body.encode_utf16().count()).collect::<Vec<_>>();
    let body_total: usize = body_lengths.iter().sum();
    let mut allocations = vec![0_usize; bodies.len()];
    if body_total > 0 {
        for index in 0..bodies.len() {
            allocations[index] = body_budget.saturating_mul(body_lengths[index]) / body_total;
        }
        let mut remainder = body_budget.saturating_sub(allocations.iter().sum());
        for index in 0..allocations.len() {
            if remainder == 0 {
                break;
            }
            allocations[index] += 1;
            remainder -= 1;
        }
    }

    let mut compacted = String::new();
    for (index, ((_, label), body)) in found.iter().zip(bodies.iter()).enumerate() {
        if index > 0 {
            compacted.push(' ');
        }
        compacted.push_str(label);
        let body = truncate_utf16(body, allocations[index]);
        if !body.is_empty() {
            compacted.push(' ');
            compacted.push_str(&body);
        }
    }
    *value = truncate_utf16(&compacted, budget);
}

fn truncate_utf16(value: &str, budget: usize) -> String {
    if value.encode_utf16().count() <= budget {
        return value.to_owned();
    }
    if budget == 0 {
        return String::new();
    }
    let text_budget = budget.saturating_sub(1);
    let mut prefix = String::new();
    let mut used = 0_usize;
    for ch in value.chars() {
        let units = ch.len_utf16();
        if used + units > text_budget {
            break;
        }
        prefix.push(ch);
        used += units;
    }
    let minimum = text_budget / 2;
    let mut boundary = None;
    for (index, ch) in prefix.char_indices().rev() {
        let end = if matches!(ch, '.' | '!' | '?' | ';' | '。' | '！' | '？' | '；') {
            index + ch.len_utf8()
        } else if ch.is_whitespace() {
            index
        } else {
            continue;
        };
        if prefix[..end].encode_utf16().count() >= minimum {
            boundary = Some(end);
            break;
        }
    }
    prefix.truncate(boundary.unwrap_or(prefix.len()));
    let prefix = prefix.trim_end();
    format!("{prefix}…")
}

/// The shape the answer must have, as a schema the server can enforce.
///
/// llama-server turns this into grammar rules and applies them while sampling,
/// so a local model cannot answer with prose, with a fenced block, or with a
/// list where a string belongs - the three ways it used to come back unusable.
pub fn draft_schema(required: &[&str]) -> Value {
    // A minimum length, not just a type: "required" only forces the key to be
    // present, and a model that answers with an empty string satisfies that
    // while leaving the field blank on screen.
    // Grammar generation can bound each field by the shared ceiling, while
    // `parse_draft` enforces their actual combined size.
    let global_metadata = serde_json::json!({ "type": "string", "minLength": 40, "maxLength": MAX_CAPTION_CHARS });
    let vocal_details = serde_json::json!({ "type": "string", "minLength": 40, "maxLength": MAX_CAPTION_CHARS });
    let arrangement = serde_json::json!({ "type": "string", "minLength": 40, "maxLength": MAX_CAPTION_CHARS });
    let lyric = serde_json::json!({ "type": "string", "minLength": 20, "maxLength": MAX_LYRICS_CHARS });
    let short = serde_json::json!({ "type": "string", "minLength": 3 });
    serde_json::json!({
        "type": "object",
        "properties": {
            "lyrics": lyric,
            "global_metadata": global_metadata,
            "vocal_details": vocal_details,
            "arrangement": arrangement,
            "title": short,
            "cover_prompt": short,
            "duration_seconds": { "type": "number" },
        },
        "required": required,
        "additionalProperties": false,
    })
}

/// An OpenAI-compatible chat request. Both providers speak this shape, so the
/// only difference between them is the endpoint and the credential.
pub fn chat_body(model: &str, system: &str, user: &str) -> Value {
    chat_body_with_reasoning(model, system, user, None)
}

/// The same body, asking the model to think harder.
///
/// `effort` is OpenRouter's unified reasoning control - "minimal" through
/// "max" - which they translate per provider. A local OpenAI-compatible server
/// has no such parameter, so nothing is sent there and the model decides for
/// itself; its thinking is read back out of `reasoning_content` either way.
pub fn chat_body_with_reasoning(model: &str, system: &str, user: &str, effort: Option<&str>) -> Value {
    chat_body_full(model, system, user, effort, None)
}

/// The request as it goes out, with the model's own sampling when it has any.
///
/// OpenRouter publishes `default_parameters` per model, and 83 of them fill it
/// in. Sending one hardcoded temperature to every model overrides what the
/// model asks for; the studio's own value is only a fallback for models that
/// publish nothing.
pub fn chat_body_full(
    model: &str,
    system: &str,
    user: &str,
    effort: Option<&str>,
    defaults: Option<&Value>,
) -> Value {
    chat_body_constrained(model, system, user, effort, defaults, None)
}

/// The same request, with the answer's shape enforced where the server can do
/// it. Asking politely for JSON in the prompt is a hope; a schema is a rule.
pub fn chat_body_constrained(
    model: &str,
    system: &str,
    user: &str,
    effort: Option<&str>,
    defaults: Option<&Value>,
    schema: Option<Value>,
) -> Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "stream": false,
    });

    let mut published = false;
    if let Some(Value::Object(map)) = defaults {
        for (key, value) in map {
            if value.is_null() {
                continue;
            }
            body[key] = value.clone();
            published = true;
        }
    }
    if !published {
        // Nothing published: a little warmth, because these are lyrics.
        body["temperature"] = Value::from(0.8);
    }
    if let Some(effort) = effort.filter(|value| !value.trim().is_empty() && *value != "off") {
        // The draft is what is wanted, not the thinking: exclude keeps the
        // response small and the parser looking in one place.
        //
        // Only for a model that says it takes this. OpenRouter publishes
        // `supported_parameters` for every model and 182 of the 468 do not
        // list reasoning; sending it to those is asking for something they
        // never offered.
        body["reasoning"] = serde_json::json!({ "effort": effort, "exclude": true });
    }
    if let Some(schema) = schema {
        body["response_format"] = serde_json::json!({ "type": "json_schema", "schema": schema });
    }

    body
}

/// Reads the answer out of a chat completion.
///
/// Reasoning models served by llama.cpp put their visible answer in
/// `content` and their thinking in `reasoning_content` - but with several
/// Gemma builds `content` comes back empty and everything, the JSON draft
/// included, arrives in `reasoning_content`. Reading only `content` there
/// looks exactly like a model that answered nothing.
pub fn content_of(response: &Value) -> Result<String> {
    let message = response
        .pointer("/choices/0/message")
        .context("the assistant response contained no message")?;
    // OpenRouter calls it `reasoning`, llama.cpp `reasoning_content`; both
    // appear when a model answers with its thinking and an empty content.
    for field in ["content", "reasoning_content", "reasoning"] {
        if let Some(text) = message.get(field).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return Ok(text.to_owned());
            }
        }
    }
    Err(anyhow!("the assistant response contained no message content"))
}

#[cfg(test)]
mod tests {
    /// The skill is 6 MB on disk and none of it may travel: only the routed
    /// reference captions do, and there are at most three.
    #[test]
    fn the_prompt_stays_small_enough_to_send() {
        let request = super::AssistRequest {
            target: super::AssistTarget::All,
            description: String::new(),
            instruction: "symphonic metal with orchestral choirs, female vocal".into(),
            lyrics: String::new(),
            global_metadata: String::new(),
            vocal_details: String::new(),
            arrangement: String::new(),
            duration_seconds: 60.0,
            instrumental: false,
            use_caption_rewriter: true,
            lyrics_strategy: LyricsStrategy::Standard,
        };
        let (system, _) = super::instructions(&request);
        println!("system prompt: {} characters, {} reference blocks", system.len(), system.matches("--- reference").count());
        assert!(system.len() < 24_000, "the prompt grew to {} characters", system.len());
        assert!(system.matches("--- reference").count() <= 3);
    }

    /// The skill's own reference captions were selected and then never used:
    /// the routing existed, the prompt did not carry it.
    #[test]
    fn the_caption_prompt_carries_the_skill_references() {
        let request = super::AssistRequest {
            target: super::AssistTarget::All,
            description: String::new(),
            instruction: "a dark synthwave night drive, female vocal".into(),
            lyrics: String::new(),
            global_metadata: String::new(),
            vocal_details: String::new(),
            arrangement: String::new(),
            duration_seconds: 60.0,
            instrumental: false,
            use_caption_rewriter: true,
            lyrics_strategy: LyricsStrategy::Standard,
        };
        let (system, _) = super::instructions(&request);
        assert!(system.contains("Reference captions from MiniMax"), "the skill's references are missing from the prompt");
        assert!(system.contains("Global Metadata"), "a reference caption is not in the prompt");
    }

    /// The form's default was quoted into the prompt, and every answer came
    /// back as sixty seconds. Nothing may put a length in front of the model
    /// when it is writing the whole song.
    #[test]
    fn the_whole_song_request_carries_no_target_length() {
        let request = super::AssistRequest {
            target: super::AssistTarget::All,
            description: String::new(),
            instruction: "club progressive house".into(),
            lyrics: String::new(),
            global_metadata: String::new(),
            vocal_details: String::new(),
            arrangement: String::new(),
            duration_seconds: 60.0,
            instrumental: false,
            use_caption_rewriter: true,
            lyrics_strategy: LyricsStrategy::Standard,
        };
        let message = super::user_message(&request);
        assert!(!message.contains("60"), "the prompt still carries the default: {message}");
        assert!(!message.to_lowercase().contains("duration"), "the prompt still names a duration: {message}");
    }

    use super::*;

    /// The published rules must reach the model itself, whichever provider is
    /// answering: the same system message is what `chat_body` sends to a local
    /// sidecar, to a server the user runs, or to OpenRouter.
    #[test]
    fn minimax_own_caption_rules_are_in_the_request_that_goes_out() {
        for target in [AssistTarget::All, AssistTarget::Prompt] {
            let mut sample = request(target);
            sample.target = target;
            let (system, _) = instructions(&sample);
            assert!(system.contains("music-caption-rewriter"), "the skill is not cited for {target:?}");
            assert!(system.contains("Global Emotional Progression"), "caption shape missing for {target:?}");
            assert!(system.contains("target <= 1450 characters"), "safe target missing for {target:?}");
            assert!(system.contains("hard ceiling of 1900 characters"), "hard ceiling missing for {target:?}");

            let body = chat_body("any-model", &system, "idea");
            let sent = body["messages"][0]["content"].as_str().unwrap_or_default();
            assert!(sent.contains("Instrument Lifecycle Description"), "the contract never reached the body");
        }
    }

    #[test]
    fn simple_mode_switch_changes_only_the_description_contract() {
        let enhanced = request(AssistTarget::All);
        let mut standard = enhanced.clone();
        standard.use_caption_rewriter = false;

        let (enhanced_system, enhanced_required) = instructions(&enhanced);
        let (standard_system, standard_required) = instructions(&standard);

        assert!(enhanced_system.contains("music-caption-rewriter"));
        assert!(enhanced_system.contains("Reference captions from MiniMax"));
        assert!(standard_system.contains("ordinary description writer"));
        assert!(!standard_system.contains("Reference captions from MiniMax"));
        assert!(!standard_system.contains("music-caption-rewriter"));
        assert!(enhanced_system.contains(LYRICS_RULES));
        assert!(standard_system.contains(LYRICS_RULES));
        assert_eq!(enhanced_required, standard_required);
        assert!(standard_required.contains(&"lyrics"));
        assert!(enhanced_system.contains("hard ceiling of 1900 characters"));
        assert!(standard_system.contains("hard ceiling of 1900 characters"));
    }

    #[test]
    fn studio_caption_assistant_follows_the_shared_skill_choice() {
        let enhanced = request(AssistTarget::Prompt);
        let mut standard = enhanced.clone();
        standard.use_caption_rewriter = false;

        let (enhanced_system, enhanced_required) = instructions(&enhanced);
        let (standard_system, standard_required) = instructions(&standard);

        assert!(enhanced_system.contains("music-caption-rewriter"));
        assert!(enhanced_system.contains("Reference captions from MiniMax"));
        assert!(standard_system.contains("ordinary description writer"));
        assert!(!standard_system.contains("music-caption-rewriter"));
        assert!(!standard_system.contains("Reference captions from MiniMax"));
        assert_eq!(enhanced_required, &["global_metadata", "vocal_details", "arrangement"]);
        assert_eq!(standard_required, enhanced_required);
        assert!(standard_system.contains("hard ceiling of 1900 characters"));
    }

    #[test]
    fn omitted_caption_rewriter_choice_defaults_to_enabled() {
        let request: AssistRequest = serde_json::from_value(serde_json::json!({
            "target": "all",
            "instruction": "night drive"
        }))
        .expect("request");
        assert!(request.use_caption_rewriter);
    }

    #[test]
    fn an_answer_that_arrives_as_reasoning_is_still_an_answer() {
        let response = serde_json::json!({
            "choices": [{ "message": { "content": "", "reasoning_content": "{\"lyrics\": \"[verse]\"}" } }]
        });
        assert_eq!(content_of(&response).unwrap(), "{\"lyrics\": \"[verse]\"}");

        let empty = serde_json::json!({ "choices": [{ "message": { "content": "  " } }] });
        assert!(content_of(&empty).is_err());
    }

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
            use_caption_rewriter: true,
            lyrics_strategy: LyricsStrategy::Standard,
        }
    }

    /// Each target must ask for exactly the fields it will write back, otherwise
    /// a partial answer would silently blank a pane the user had filled in.
    #[test]
    fn every_target_declares_the_fields_it_writes() {
        assert_eq!(instructions(&request(AssistTarget::Lyrics)).1, &["title", "lyrics"]);
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
    fn all_four_strategy_combinations_keep_two_independent_contracts() {
        for strategy in [LyricsStrategy::Standard, LyricsStrategy::StorySongwriting] {
            for caption_rewriter in [false, true] {
                let mut lyrics_request = request(AssistTarget::Lyrics);
                lyrics_request.lyrics_strategy = strategy;
                lyrics_request.use_caption_rewriter = caption_rewriter;
                let (lyrics_system, lyrics_fields) = instructions(&lyrics_request);
                assert_eq!(lyrics_fields, &["title", "lyrics"]);
                assert!(!lyrics_system.contains("global_metadata, vocal_details and arrangement"));
                assert_eq!(lyrics_system.contains("application-owned lyrics method"), strategy == LyricsStrategy::StorySongwriting);

                let mut caption_request = lyrics_request.clone();
                caption_request.target = AssistTarget::Prompt;
                let (caption_system, caption_fields) = instructions(&caption_request);
                assert_eq!(caption_fields, &["global_metadata", "vocal_details", "arrangement"]);
                assert_eq!(caption_system.contains("Reference captions from MiniMax"), caption_rewriter);
                assert!(!caption_system.contains("application-owned lyrics method"));
            }
        }
    }

    #[test]
    fn story_stage_normalizes_safe_tag_aliases_and_records_audit() {
        let mut request = request(AssistTarget::Lyrics);
        request.lyrics_strategy = LyricsStrategy::StorySongwriting;
        let answer = serde_json::json!({
            "title": "灵魂的回声",
            "lyrics": "[Verse 1]\n灵魂本就孤独\n[Pre Chorus]\n世界喧哗\n[Final Chorus]\n同频的人看见光\n[Outro]\n人间有了回声"
        });
        let (draft, audit) = parse_draft_for_request(&answer.to_string(), &["title", "lyrics"], &request).unwrap();
        let lyrics = draft.lyrics.unwrap();
        assert!(lyrics.contains("[verse]"));
        assert!(lyrics.contains("[pre-chorus]"));
        assert!(lyrics.contains("[chorus]"));
        assert!(!lyrics.contains("Verse 1"));
        assert_eq!(audit.strategy_name, "story_songwriting");
        assert_eq!(audit.contract_version, STORY_SONGWRITING_VERSION);
        assert!(audit.validation.contains(&"stage_schema_isolated".to_owned()));
    }

    #[test]
    fn unknown_tags_and_cross_stage_fields_are_rejected() {
        let lyrics_request = request(AssistTarget::Lyrics);
        let unknown = serde_json::json!({ "title": "x", "lyrics": "[narration]\nhello" });
        assert!(parse_draft_for_request(&unknown.to_string(), &["title", "lyrics"], &lyrics_request).is_err());
        let inline = serde_json::json!({ "title": "x", "lyrics": "[verse] words on the tag line" });
        assert!(parse_draft_for_request(&inline.to_string(), &["title", "lyrics"], &lyrics_request).is_err());
        let polluted = serde_json::json!({
            "title": "x", "lyrics": "[verse]\nhello", "global_metadata": "music"
        });
        assert!(parse_draft_for_request(&polluted.to_string(), &["title", "lyrics"], &lyrics_request).is_err());

        let prompt_request = request(AssistTarget::Prompt);
        let prompt_polluted = serde_json::json!({
            "global_metadata": "g", "vocal_details": "v", "arrangement": "a", "lyrics": "[verse]\nhello"
        });
        assert!(parse_draft_for_request(
            &prompt_polluted.to_string(),
            &["global_metadata", "vocal_details", "arrangement"],
            &prompt_request,
        ).is_err());
    }

    #[test]
    fn caption_stage_rejects_a_quoted_lyric_line() {
        let mut prompt_request = request(AssistTarget::Prompt);
        prompt_request.lyrics = "[verse]\n同频的人才能看见彼此灵魂深处的光".into();
        let answer = serde_json::json!({
            "global_metadata": "同频的人才能看见彼此灵魂深处的光",
            "vocal_details": "warm female voice",
            "arrangement": "verse then chorus"
        });
        assert!(parse_draft_for_request(
            &answer.to_string(),
            &["global_metadata", "vocal_details", "arrangement"],
            &prompt_request,
        ).is_err());
    }

    #[test]
    fn a_longer_section_is_accepted_when_the_combined_caption_fits() {
        let answer = serde_json::json!({
            "global_metadata": "g".repeat(777),
            "vocal_details": "v".repeat(80),
            "arrangement": "a".repeat(120),
        });
        parse_draft(
            &answer.to_string(),
            &["global_metadata", "vocal_details", "arrangement"],
        )
        .expect("the cloud contract has no 520-character field limit");
    }

    #[test]
    fn an_oversized_combined_caption_is_compacted_before_it_reaches_the_form() {
        let answer = serde_json::json!({
            "global_metadata": "g".repeat(900),
            "vocal_details": "v".repeat(500),
            "arrangement": "a".repeat(600),
        });
        let draft = parse_draft(
            &answer.to_string(),
            &["global_metadata", "vocal_details", "arrangement"],
        )
        .expect("an overlong model answer should be repaired without another model call");
        assert!(caption_chars(&draft) <= TARGET_CAPTION_CHARS);
        assert!(draft.global_metadata.unwrap().ends_with('…'));
    }

    #[test]
    fn a_caption_below_the_api_ceiling_is_still_compacted_to_the_product_target() {
        let answer = serde_json::json!({
            "global_metadata": "g".repeat(700),
            "vocal_details": "v".repeat(350),
            "arrangement": "a".repeat(500),
        });
        let draft = parse_draft(
            &answer.to_string(),
            &["global_metadata", "vocal_details", "arrangement"],
        )
        .unwrap();
        assert!(caption_chars(&draft) <= TARGET_CAPTION_CHARS);
        assert!(draft.global_metadata.unwrap().ends_with('…'));
    }

    #[test]
    fn compaction_preserves_structural_labels_and_unicode_boundaries() {
        let repeat = "温柔而有磁性的细节 ".repeat(90);
        let answer = serde_json::json!({
            "global_metadata": format!("Basic Attributes: {repeat} Global Emotional Progression: {repeat} Application Scenarios & Imagery: {repeat} Sonics & Production Profile: {repeat}"),
            "vocal_details": format!("Vocal Gender & Timbre: {repeat} Vocal Style: {repeat} Harmony/Backing Vocals: {repeat} Vocal FX: {repeat}"),
            "arrangement": format!("Instrument Lifecycle Description: {repeat} Groove & Foundation Progression: {repeat} Section Timeline: {repeat} Textures & Spatial FX: {repeat}"),
        });
        let draft = parse_draft(
            &answer.to_string(),
            &["global_metadata", "vocal_details", "arrangement"],
        )
        .unwrap();
        assert!(caption_chars(&draft) <= TARGET_CAPTION_CHARS);
        let joined = format!(
            "{} {} {}",
            draft.global_metadata.unwrap(),
            draft.vocal_details.unwrap(),
            draft.arrangement.unwrap()
        );
        for label in [
            "Basic Attributes:",
            "Global Emotional Progression:",
            "Vocal Gender & Timbre:",
            "Vocal FX:",
            "Instrument Lifecycle Description:",
            "Section Timeline:",
            "Textures & Spatial FX:",
        ] {
            assert!(joined.contains(label), "compaction dropped {label}");
        }
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

    /// Gemma writes lyrics as a list of lines about as often as it writes them
    /// as one string, and both are the same song.
    #[test]
    fn lyrics_may_arrive_as_a_list_of_lines() {
        let answer = r#"{"lyrics": ["[intro]", "[verse]", "neon on the wet road"], "global_metadata": "g", "vocal_details": "v", "arrangement": "a"}"#;
        let draft = super::parse_draft(answer, &["lyrics"]).expect("a list of lines is a lyric");
        assert_eq!(draft.lyrics.as_deref(), Some("[intro]
[verse]
neon on the wet road"));
    }

    /// A Russian idea used to come back as an English song: the caption rule
    /// ("write in English") had quietly swallowed the lyrics as well.
    #[test]
    fn the_lyrics_follow_the_language_of_the_request() {
        let request = super::AssistRequest {
            target: super::AssistTarget::All,
            description: String::new(),
            instruction: "панк-рок про ёжика в бункере".into(),
            lyrics: String::new(),
            global_metadata: String::new(),
            vocal_details: String::new(),
            arrangement: String::new(),
            duration_seconds: 60.0,
            instrumental: false,
            use_caption_rewriter: true,
            lyrics_strategy: LyricsStrategy::Standard,
        };
        let (system, _) = super::instructions(&request);
        assert!(system.contains("language the user wrote their request in"));
    }

    /// A stress mark is the only lever there is on pronunciation: the caption
    /// never reaches the singing, the letters do.
    #[test]
    fn a_sung_request_carries_the_diction_rule() {
        let request = super::AssistRequest {
            target: super::AssistTarget::All,
            description: "песня про замок на горе".into(),
            instruction: String::new(),
            lyrics: String::new(),
            global_metadata: String::new(),
            vocal_details: String::new(),
            arrangement: String::new(),
            duration_seconds: 90.0,
            instrumental: false,
            use_caption_rewriter: true,
            lyrics_strategy: LyricsStrategy::Standard,
        };
        let (system, _) = super::instructions(&request);
        assert!(system.contains("combining acute"), "nothing tells the model how to fix a stress");
        assert!(system.contains("ё"), "the ё rule is missing");
    }

    /// Two singers need rules a solo song must not see: told to a one-voice
    /// song, they would invite a second voice that nobody asked for.
    #[test]
    fn the_duet_rules_arrive_only_for_two_voices() {
        let mut solo = request(AssistTarget::All);
        solo.description = "a night drive synth pop song".into();
        solo.vocal_details = String::new();
        solo.lyrics = String::new();
        let (system, _) = instructions(&solo);
        assert!(!system.contains("[male vocal]"), "a solo song was given duet rules");

        let mut duet = solo.clone();
        duet.description = "дуэт мужского и женского голоса, поп-баллада".into();
        let (system, _) = instructions(&duet);
        assert!(system.contains("[male vocal]"), "the duet has no voice tags to use");
        assert!(system.contains("no backing choir"), "the anti-choir clause is missing");
    }

    /// An instrumental gets the opposite instruction, and no diction rule:
    /// there is nothing to pronounce.
    #[test]
    fn an_instrumental_is_told_what_carries_the_melody() {
        let mut instrumental = request(AssistTarget::All);
        instrumental.instrumental = true;
        let (system, _) = instructions(&instrumental);
        assert!(system.contains("lead melodic line"), "nothing replaces the missing vocal");
        assert!(!system.contains("combining acute"), "an instrumental was given a diction rule");
    }

    /// "required" alone let the model answer with an empty string and still
    /// satisfy the schema, which is how a blank description came back.
    /// Two things this request has to get right about a model it did not
    /// choose: use what the model published for itself, and ask for thinking
    /// only where thinking is offered.
    #[test]
    fn a_model_that_published_nothing_gets_the_studio_s_own_warmth() {
        let plain = chat_body_constrained("m", "s", "u", None, None, None);
        assert_eq!(plain["temperature"], serde_json::json!(0.8));

        let published = serde_json::json!({ "temperature": 0.6, "top_p": 0.95 });
        let honoured = chat_body_constrained("m", "s", "u", None, Some(&published), None);
        assert_eq!(honoured["temperature"], serde_json::json!(0.6), "the model's own figure wins");
        assert_eq!(honoured["top_p"], serde_json::json!(0.95));
    }

    #[test]
    fn reasoning_is_only_asked_of_models_that_take_it() {
        let asked = chat_body_constrained("m", "s", "u", Some("high"), None, None);
        assert_eq!(asked["reasoning"], serde_json::json!({ "effort": "high", "exclude": true }));

        // The caller passes None for a model whose supported_parameters do not
        // list reasoning, and for "off".
        let quiet = chat_body_constrained("m", "s", "u", None, None, None);
        assert!(quiet.get("reasoning").is_none(), "nothing is asked of a model that does not offer it");
        let switched_off = chat_body_constrained("m", "s", "u", Some("off"), None, None);
        assert!(switched_off.get("reasoning").is_none());
    }

    #[test]
    fn the_schema_asks_for_content_not_just_a_key() {
        let schema = super::draft_schema(&["global_metadata"]);
        assert_eq!(schema["properties"]["global_metadata"]["minLength"], 40);
        assert_eq!(schema["properties"]["global_metadata"]["maxLength"], MAX_CAPTION_CHARS);
        assert_eq!(schema["required"][0], "global_metadata");
    }
}
