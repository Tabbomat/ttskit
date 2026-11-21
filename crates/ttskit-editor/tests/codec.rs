use serde_json::json;
use ttskit_editor::{CodecError, Incoming, Outgoing, ScriptState};

fn ss(guid: &str, script: Option<&str>) -> ScriptState {
    ScriptState {
        name: "n".into(),
        guid: guid.into(),
        script: script.map(Into::into),
        ui: None,
    }
}

#[test]
fn parses_push_object() {
    let m = Incoming::parse(
        r#"{"messageID":0,"scriptStates":[{"name":"Pawn","guid":"db3f06","script":""}]}"#,
    )
    .unwrap();
    assert_eq!(
        m,
        Incoming::PushObject {
            script_states: vec![ScriptState {
                name: "Pawn".into(),
                guid: "db3f06".into(),
                script: Some(String::new()),
                ui: None
            }]
        }
    );
}

#[test]
fn parses_load_game_with_crlf_pretty_json_and_save_path() {
    let text = "{\r\n  \"scriptStates\": [\r\n    {\"name\": \"Global\", \"guid\": \"-1\", \"script\": \"x\", \"ui\": \"<u/>\"}\r\n  ],\r\n  \"savePath\": \"\",\r\n  \"messageID\": 1\r\n}";
    let Incoming::LoadGame {
        script_states,
        save_path,
    } = Incoming::parse(text).unwrap()
    else {
        panic!()
    };
    assert_eq!(script_states[0].ui.as_deref(), Some("<u/>"));
    assert_eq!(save_path.as_deref(), Some(""));
}

#[test]
fn parses_print_error_custom_return_saved_created() {
    assert_eq!(
        Incoming::parse(r#"{"messageID":2,"message":"hi"}"#).unwrap(),
        Incoming::Print {
            message: "hi".into()
        }
    );
    assert_eq!(
        Incoming::parse(
            r#"{"messageID":3,"error":"e","guid":"-1","errorMessagePrefix":"Error in Global Script: "}"#
        )
        .unwrap(),
        Incoming::Error {
            error: "e".into(),
            guid: "-1".into(),
            error_message_prefix: "Error in Global Script: ".into()
        }
    );
    assert_eq!(
        Incoming::parse(r#"{"messageID":4,"customMessage":{"foo":"Hello"}}"#).unwrap(),
        Incoming::Custom {
            custom_message: json!({"foo":"Hello"})
        }
    );
    assert_eq!(
        Incoming::parse(r#"{"messageID":5,"returnValue":true}"#).unwrap(),
        Incoming::Return {
            return_value: json!(true)
        }
    );
    assert_eq!(
        Incoming::parse(r#"{"messageID":6}"#).unwrap(),
        Incoming::GameSaved { save_path: None }
    );
    assert_eq!(
        Incoming::parse(r#"{"messageID":6,"savePath":"a.json"}"#).unwrap(),
        Incoming::GameSaved {
            save_path: Some("a.json".into())
        }
    );
    assert_eq!(
        Incoming::parse(r#"{"messageID":7,"guid":"abcdef"}"#).unwrap(),
        Incoming::ObjectCreated {
            guid: "abcdef".into()
        }
    );
}

#[test]
fn tolerates_unknown_fields_and_ids() {
    assert_eq!(
        Incoming::parse(r#"{"messageID":2,"message":"hi","extra":1}"#).unwrap(),
        Incoming::Print {
            message: "hi".into()
        }
    );
    let Incoming::Unknown { message_id, raw } =
        Incoming::parse(r#"{"messageID":42,"x":1}"#).unwrap()
    else {
        panic!()
    };
    assert_eq!(message_id, 42);
    assert_eq!(raw["x"], json!(1));
}

#[test]
fn rejects_bad_input() {
    assert!(matches!(Incoming::parse("nope"), Err(CodecError::Json(_))));
    assert!(matches!(
        Incoming::parse("[1]"),
        Err(CodecError::NotAnObject)
    ));
    assert!(matches!(
        Incoming::parse(r#"{"x":1}"#),
        Err(CodecError::MissingMessageId)
    ));
    assert!(matches!(
        Incoming::parse(r#"{"messageID":2,"message":5}"#),
        Err(CodecError::Malformed { id: 2, .. })
    ));
}

#[test]
fn serializes_outgoing() {
    assert_eq!(Outgoing::GetScripts.to_value(), json!({"messageID":0}));
    assert_eq!(
        Outgoing::SaveAndPlay {
            script_states: vec![ss("-1", Some("s")), ss("a", None)]
        }
        .to_value(),
        json!({"messageID":1,"scriptStates":[{"name":"n","guid":"-1","script":"s"},{"name":"n","guid":"a"}]})
    );
    assert_eq!(
        Outgoing::Custom {
            custom_message: json!({"a":1})
        }
        .to_value(),
        json!({"messageID":2,"customMessage":{"a":1}})
    );
    assert_eq!(
        Outgoing::ExecuteLua {
            guid: "-1".into(),
            script: "print(1)".into()
        }
        .to_value(),
        json!({"messageID":3,"guid":"-1","script":"print(1)"})
    );
}
