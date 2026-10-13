//! The agents' characters: a name, an optional short profile, a look, plus the reply-language preference and
//! whether reviewers talk to each other by name.
//!
//! Stored as one small owner-only JSON file next to the interface preferences. The name and the look are
//! only for the interface; the name, the profile, the language and the conversational flag are also written
//! into the prompts of the calls that write prose (see `crate::style` for exactly where and what). Nothing
//! here widens what an agent may do, and every text goes through the same cleaning and `redact_secrets` as
//! anything else this program stores.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Body, S};
use crate::store::redact_secrets;
use crate::style::{Character, MAX_NAME_CHARS, MAX_PERSONALITY_CHARS, PromptStyle, ReplyLanguage};
use crate::web::AppState;
use crate::web::error::{ApiError, ApiResult};

pub const ACCESSORIES: [&str; 4] = ["none", "glasses", "cap", "headphones"];
pub const PALETTES: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    /// 0 is the agent's own colours; 1 and 2 are alternatives.
    pub palette: u8,
    /// One of [`ACCESSORIES`].
    pub accessory: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CharacterEntry {
    pub name: String,
    pub personality: String,
    pub appearance: Appearance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CharactersFile {
    /// `auto` (the language of the question), `es` or `en`.
    pub reply_language: String,
    /// Reviewers address the others by name.
    pub conversational: bool,
    /// By provider id.
    pub characters: BTreeMap<String, CharacterEntry>,
}

impl Default for CharactersFile {
    fn default() -> Self {
        // Replying in the language of the question is what stops mixed-language answers, so it is on from the start.
        Self {
            reply_language: "auto".into(),
            conversational: true,
            characters: BTreeMap::new(),
        }
    }
}

pub fn load(state: &AppState) -> CharactersFile {
    state
        .characters_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// The prompt preferences for a new consultation, from the saved characters. `None` when there is nothing to add.
pub fn style_for(state: &AppState) -> Option<Arc<PromptStyle>> {
    let file = load(state);
    let language = ReplyLanguage::parse(&file.reply_language).unwrap_or_default();
    let style = PromptStyle {
        language,
        conversational: file.conversational,
        characters: file
            .characters
            .into_iter()
            .map(|(id, e)| {
                (
                    id,
                    Character {
                        name: e.name,
                        personality: e.personality,
                    },
                )
            })
            .collect(),
    };
    // Even plain `auto` adds the language rule (it is what stops answers that mix two languages).
    Some(Arc::new(style))
}

fn validate(state: &AppState, file: &CharactersFile) -> ApiResult<CharactersFile> {
    ReplyLanguage::parse(&file.reply_language)
        .map_err(|e| ApiError::validation(e.to_string()).field("reply_language"))?;
    let mut out = CharactersFile {
        reply_language: ReplyLanguage::parse(&file.reply_language)
            .unwrap_or_default()
            .as_str()
            .into(),
        conversational: file.conversational,
        characters: BTreeMap::new(),
    };
    for (key, e) in &file.characters {
        let provider =
            state.orch.registry().resolve(key).map_err(|e| {
                ApiError::validation(e.to_string()).field(&format!("characters.{key}"))
            })?;
        let id = provider.id().to_string();
        let cleaned = Character {
            name: e.name.clone(),
            personality: e.personality.clone(),
        }
        .cleaned()
        .map_err(|er| ApiError::validation(er.to_string()).field(&format!("characters.{key}")))?;
        if e.appearance.palette >= PALETTES {
            return Err(
                ApiError::validation(format!("palette must be 0 to {}", PALETTES - 1))
                    .field(&format!("characters.{key}.appearance.palette")),
            );
        }
        let accessory = if e.appearance.accessory.is_empty() {
            "none"
        } else {
            e.appearance.accessory.as_str()
        };
        if !ACCESSORIES.contains(&accessory) {
            return Err(ApiError::validation(format!(
                "accessory must be one of: {}",
                ACCESSORIES.join(", ")
            ))
            .field(&format!("characters.{key}.appearance.accessory")));
        }
        out.characters.insert(
            id,
            CharacterEntry {
                name: redact_secrets(&cleaned.name),
                personality: redact_secrets(&cleaned.personality),
                appearance: Appearance {
                    palette: e.appearance.palette,
                    accessory: accessory.to_string(),
                },
            },
        );
    }
    Ok(out)
}

pub async fn get(State(state): S) -> Json<Value> {
    let file = load(&state);
    Json(json!({
        "settings": file,
        "limits": {"name_chars": MAX_NAME_CHARS, "personality_chars": MAX_PERSONALITY_CHARS, "palettes": PALETTES, "accessories": ACCESSORIES},
        "note": "The name and the profile are written into the prompt of the calls that write prose (not the search plan or the claim checks). A profile shapes tone and emphasis only; it cannot change what an agent is allowed to do, and it can change what an answer says.",
        "writable": state.characters_path.is_some(),
    }))
}

pub async fn put(State(state): S, Body(file): Body<CharactersFile>) -> ApiResult<Json<Value>> {
    let path = state
        .characters_path
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("the preferences location is unknown"))?;
    let valid = validate(&state, &file)?;
    {
        use std::io::Write as _;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| ApiError::internal(e.to_string()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .map_err(|e| ApiError::internal(e.to_string()))?;
        f.write_all(
            serde_json::to_string_pretty(&valid)
                .unwrap_or_default()
                .as_bytes(),
        )
        .map_err(|e| ApiError::internal(e.to_string()))?;
        f.sync_all()
            .map_err(|e| ApiError::internal(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| ApiError::internal(e.to_string()))?;
    }
    Ok(Json(json!({"settings": valid})))
}
