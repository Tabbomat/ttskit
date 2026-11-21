use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Errors from decoding a message.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("message is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("message is not a JSON object")]
    NotAnObject,
    #[error("message has no numeric `messageID`")]
    MissingMessageId,
    #[error("message with ID {id} is malformed: {source}")]
    Malformed { id: u64, source: serde_json::Error },
}

/// One object's scripts. `script` and `ui` are omitted from the JSON when `None`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ScriptState {
    #[serde(default)]
    pub name: String,
    pub guid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<String>,
}

/// Messages sent by the game to the editor. Unknown IDs are kept as `Unknown`.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// ID 0: a new object was pushed to the editor.
    PushObject { script_states: Vec<ScriptState> },
    /// ID 1: a game was loaded, or a reply to "get scripts".
    LoadGame {
        script_states: Vec<ScriptState>,
        save_path: Option<String>,
    },
    /// ID 2: `print` output.
    Print { message: String },
    /// ID 3: a Lua error.
    Error {
        error: String,
        guid: String,
        error_message_prefix: String,
    },
    /// ID 4: a custom message sent from Lua.
    Custom { custom_message: Value },
    /// ID 5: the value returned by executed Lua.
    Return { return_value: Value },
    /// ID 6: the game was saved.
    GameSaved { save_path: Option<String> },
    /// ID 7: an object was created.
    ObjectCreated { guid: String },
    /// Any other ID; the full object is preserved.
    Unknown {
        message_id: u64,
        raw: Map<String, Value>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScriptStatesBody {
    #[serde(default)]
    script_states: Vec<ScriptState>,
    #[serde(default)]
    save_path: Option<String>,
}

#[derive(Deserialize)]
struct PrintBody {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    guid: String,
    #[serde(default)]
    error_message_prefix: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CustomBody {
    #[serde(default)]
    custom_message: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReturnBody {
    #[serde(default)]
    return_value: Value,
}

#[derive(Deserialize)]
struct GuidBody {
    #[serde(default)]
    guid: String,
}

impl Incoming {
    /// Parse one message from JSON text. Unknown fields are ignored.
    pub fn parse(text: &str) -> Result<Self, CodecError> {
        let Value::Object(raw) = serde_json::from_str(text)? else {
            return Err(CodecError::NotAnObject);
        };
        let id = raw
            .get("messageID")
            .and_then(Value::as_u64)
            .ok_or(CodecError::MissingMessageId)?;
        let value = Value::Object(raw.clone());
        let bad = |source| CodecError::Malformed { id, source };
        Ok(match id {
            0 => Self::PushObject {
                script_states: serde_json::from_value::<ScriptStatesBody>(value)
                    .map_err(bad)?
                    .script_states,
            },
            1 => {
                let b: ScriptStatesBody = serde_json::from_value(value).map_err(bad)?;
                Self::LoadGame {
                    script_states: b.script_states,
                    save_path: b.save_path,
                }
            }
            2 => Self::Print {
                message: serde_json::from_value::<PrintBody>(value)
                    .map_err(bad)?
                    .message,
            },
            3 => {
                let b: ErrorBody = serde_json::from_value(value).map_err(bad)?;
                Self::Error {
                    error: b.error,
                    guid: b.guid,
                    error_message_prefix: b.error_message_prefix,
                }
            }
            4 => Self::Custom {
                custom_message: serde_json::from_value::<CustomBody>(value)
                    .map_err(bad)?
                    .custom_message,
            },
            5 => Self::Return {
                return_value: serde_json::from_value::<ReturnBody>(value)
                    .map_err(bad)?
                    .return_value,
            },
            6 => Self::GameSaved {
                save_path: serde_json::from_value::<ScriptStatesBody>(value)
                    .map_err(bad)?
                    .save_path,
            },
            7 => Self::ObjectCreated {
                guid: serde_json::from_value::<GuidBody>(value).map_err(bad)?.guid,
            },
            _ => Self::Unknown {
                message_id: id,
                raw,
            },
        })
    }
}

/// Messages sent by the editor to the game.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    /// ID 0: ask the game to send all scripts (answered with `Incoming::LoadGame`).
    GetScripts,
    /// ID 1: update scripts and reload the game.
    SaveAndPlay { script_states: Vec<ScriptState> },
    /// ID 2: custom message; the game only delivers it if it is a JSON object.
    Custom { custom_message: Value },
    /// ID 3: execute Lua in the context of `guid` (`"-1"` is Global).
    ExecuteLua { guid: String, script: String },
}

impl Outgoing {
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        let id = match self {
            Self::GetScripts => 0,
            Self::SaveAndPlay { script_states } => {
                m.insert(
                    "scriptStates".into(),
                    serde_json::to_value(script_states).expect("script states serialize"),
                );
                1
            }
            Self::Custom { custom_message } => {
                m.insert("customMessage".into(), custom_message.clone());
                2
            }
            Self::ExecuteLua { guid, script } => {
                m.insert("guid".into(), guid.clone().into());
                m.insert("script".into(), script.clone().into());
                3
            }
        };
        m.insert("messageID".into(), id.into());
        Value::Object(m)
    }

    pub fn to_json(&self) -> String {
        self.to_value().to_string()
    }
}
