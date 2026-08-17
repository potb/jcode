//! Curated Anthropic OAuth tool schemas; see `docs/TOOL_INTENT.md`.

use super::*;
use jcode_message_types::ToolDefinition;
use serde_json::json;

fn tool_def(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: format!("{name} description"),
        input_schema: json!({"type":"object","properties":{}}),
    }
}

#[test]
fn oauth_schedule_wakeup_forwards_the_real_schedule_schema() {
    let real_schema = json!({
        "type": "object",
        "properties": {
            "action": {"type": "string"},
            "task": {"type": "string"},
            "wake_in_minutes": {"type": "integer"}
        },
        "required": ["intent"]
    });
    let registry = vec![ToolDefinition {
        name: "schedule".to_string(),
        description: "Schedule, list, or cancel future tasks.".to_string(),
        input_schema: real_schema.clone(),
    }];

    let formatted = format_tools(&registry, true, false);
    let scheduled = formatted
        .iter()
        .find(|t| t.name == "ScheduleWakeup")
        .expect("schedule must be advertised under its OAuth name");

    let props = scheduled.input_schema["properties"]
        .as_object()
        .expect("object schema");
    assert!(props.contains_key("task"), "{props:?}");
    assert!(
        !props.contains_key("delaySeconds"),
        "fabricated schema leaked back in: {props:?}"
    );
    assert_eq!(
        formatted
            .iter()
            .filter(|t| t.name == "ScheduleWakeup")
            .count(),
        1,
        "schedule must not be advertised twice"
    );
}

#[test]
fn oauth_bash_schema_advertises_the_justification_escape_hatch() {
    let formatted = format_tools(&[tool_def("bash")], true, false);
    let bash = formatted
        .iter()
        .find(|t| t.name == "Bash")
        .expect("Bash must be advertised");
    assert!(
        bash.input_schema["properties"]
            .as_object()
            .is_some_and(|p| p.contains_key("justification")),
        "{:?}",
        bash.input_schema
    );
}

/// Without this the model cannot state an intent for the eight most-used tools.
#[test]
fn every_curated_oauth_builtin_accepts_intent() {
    let registry: Vec<ToolDefinition> = [
        "subagent",
        "bash",
        "edit",
        "glob",
        "grep",
        "read",
        "skill_manage",
        "write",
    ]
    .iter()
    .map(|name| tool_def(name))
    .collect();

    let formatted = format_tools(&registry, true, false);

    for name in [
        "Agent", "Bash", "Edit", "Glob", "Grep", "Read", "Skill", "Write",
    ] {
        let tool = formatted
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} must be advertised"));

        let props = tool.input_schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{name} must have an object schema"));
        assert!(
            props.contains_key("intent"),
            "{name} cannot report an intent: {:?}",
            tool.input_schema
        );
        assert_eq!(
            props["intent"]["description"].as_str(),
            Some(CURATED_INTENT_DESCRIPTION),
            "{name} describes intent differently from every other tool"
        );

        let required: Vec<&str> = tool.input_schema["required"]
            .as_array()
            .map(|values| values.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert!(
            required.contains(&"intent"),
            "{name} does not require intent: {required:?}"
        );
    }
}

/// `intent` must not displace a tool's own required arguments.
#[test]
fn curated_intent_preserves_existing_required_arguments() {
    let formatted = format_tools(&[tool_def("read"), tool_def("edit")], true, false);

    let required_of = |name: &str| -> Vec<String> {
        formatted
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.input_schema["required"].as_array().cloned())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };

    let read = required_of("Read");
    assert!(read.contains(&"file_path".to_string()), "{read:?}");
    assert!(read.contains(&"intent".to_string()), "{read:?}");

    let edit = required_of("Edit");
    for argument in ["file_path", "old_string", "new_string", "intent"] {
        assert!(edit.contains(&argument.to_string()), "{edit:?}");
    }
}

/// Applying the helper twice must not duplicate the property or the requirement.
#[test]
fn curated_intent_is_idempotent() {
    let schema = with_curated_intent(with_curated_intent(json!({
        "type": "object",
        "required": ["file_path"],
        "properties": {"file_path": {"type": "string"}}
    })));

    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, vec!["file_path", "intent"]);
}

/// Guards the duplicated description string against drift.
#[test]
fn curated_intent_description_matches_tool_core() {
    let tool_core = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../jcode-tool-core/src/lib.rs"
    ))
    .expect("jcode-tool-core source must be readable");

    assert!(
        tool_core.contains(&format!("{CURATED_INTENT_DESCRIPTION:?}")),
        "TOOL_INTENT_DESCRIPTION changed; update CURATED_INTENT_DESCRIPTION to match"
    );
}

/// The `additionalProperties: false` guard means an accepted `intent` must be
/// declared; this proves the emitted schema would validate a real call.
#[test]
fn curated_read_schema_would_accept_an_intent_bearing_call() {
    let formatted = format_tools(&[tool_def("read")], true, false);
    let read = formatted
        .iter()
        .find(|t| t.name == "Read")
        .expect("Read must be advertised");

    assert_eq!(
        read.input_schema["additionalProperties"],
        json!(false),
        "guard removed; this test no longer proves anything"
    );

    let call = json!({"file_path": "/tmp/x", "intent": "read the file"});
    let declared = read.input_schema["properties"]
        .as_object()
        .expect("object schema");
    for key in call.as_object().expect("object call").keys() {
        assert!(
            declared.contains_key(key),
            "a call passing {key} would be rejected: {declared:?}"
        );
    }
}
