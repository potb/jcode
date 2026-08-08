use super::{build_command, BrowserInput};
use serde_json::json;

fn input(value: serde_json::Value) -> BrowserInput {
    serde_json::from_value(value).expect("valid browser input")
}

#[test]
fn open_maps_to_open_command() {
    let plan = build_command(
        "open",
        &input(json!({"action":"open","url":"https://example.com"})),
    )
    .unwrap();
    assert_eq!(plan.args, vec!["open", "https://example.com"]);
}

#[test]
fn open_with_new_tab_uses_tab_new() {
    let plan = build_command(
        "open",
        &input(json!({"action":"open","url":"https://a.test","new_tab":true})),
    )
    .unwrap();
    assert_eq!(plan.args, vec!["tab", "new", "https://a.test"]);
}

#[test]
fn snapshot_and_interactables_map_to_snapshot_variants() {
    let snap = build_command("snapshot", &input(json!({"action":"snapshot"}))).unwrap();
    assert_eq!(snap.args, vec!["snapshot"]);

    let inter = build_command("interactables", &input(json!({"action":"interactables"}))).unwrap();
    assert_eq!(inter.args, vec!["snapshot", "-i"]);
}

#[test]
fn click_supports_selector_and_text() {
    let by_selector = build_command(
        "click",
        &input(json!({"action":"click","selector":"#submit"})),
    )
    .unwrap();
    assert_eq!(by_selector.args, vec!["click", "#submit"]);

    let by_text =
        build_command("click", &input(json!({"action":"click","text":"Sign in"}))).unwrap();
    assert_eq!(by_text.args, vec!["find", "text", "Sign in", "click"]);
}

#[test]
fn click_without_target_errors() {
    let err = build_command("click", &input(json!({"action":"click"}))).unwrap_err();
    assert!(err.to_string().contains("requires selector"));
}

#[test]
fn type_defaults_to_fill_and_honors_clear_false() {
    let default = build_command(
        "type",
        &input(json!({"action":"type","selector":"#q","text":"hi"})),
    )
    .unwrap();
    assert_eq!(default.args, vec!["fill", "#q", "hi"]);

    let append = build_command(
        "type",
        &input(json!({"action":"type","selector":"#q","text":"hi","clear":false})),
    )
    .unwrap();
    assert_eq!(append.args, vec!["type", "#q", "hi"]);
}

#[test]
fn eval_uses_base64_to_avoid_quoting_hazards() {
    let plan = build_command(
        "eval",
        &input(json!({"action":"eval","script":"document.title"})),
    )
    .unwrap();
    assert_eq!(plan.args[0], "eval");
    assert_eq!(plan.args[1], "-b");
    // "document.title" base64-encoded
    assert_eq!(plan.args[2], "ZG9jdW1lbnQudGl0bGU=");
}

#[test]
fn fill_form_batches_fields() {
    let plan = build_command(
        "fill_form",
        &input(json!({
            "action":"fill_form",
            "fields":[
                {"selector":"#email","value":"a@b.c"},
                {"selector":"#tos","checked":true}
            ]
        })),
    )
    .unwrap();
    assert_eq!(plan.args[0], "batch");
    assert!(plan.args[1].contains("fill"));
    assert!(plan.args[2].contains("check"));
}

#[test]
fn press_with_selector_focuses_first() {
    let plan = build_command(
        "press",
        &input(json!({"action":"press","key":"Enter","selector":"#q"})),
    )
    .unwrap();
    assert_eq!(plan.args[0], "batch");
    assert!(plan.args[1].contains("focus"));
    assert!(plan.args[2].contains("press"));

    let bare = build_command("press", &input(json!({"action":"press","key":"Tab"}))).unwrap();
    assert_eq!(bare.args, vec!["press", "Tab"]);
}

#[test]
fn scroll_maps_positions_and_offsets() {
    let bottom = build_command(
        "scroll",
        &input(json!({"action":"scroll","position":"bottom"})),
    )
    .unwrap();
    assert_eq!(bottom.args[0..2], ["scroll", "down"]);

    let into_view = build_command(
        "scroll",
        &input(json!({"action":"scroll","selector":"#footer"})),
    )
    .unwrap();
    assert_eq!(into_view.args, vec!["scrollintoview", "#footer"]);
}

#[test]
fn screenshot_requests_image_attachment() {
    let plan = build_command("screenshot", &input(json!({"action":"screenshot"}))).unwrap();
    assert!(plan.wants_screenshot_image);
    assert_eq!(plan.args, vec!["screenshot"]);
}

#[test]
fn wait_supports_selector_and_contains() {
    let by_sel = build_command(
        "wait",
        &input(json!({"action":"wait","selector":"#done"})),
    )
    .unwrap();
    assert_eq!(by_sel.args, vec!["wait", "#done"]);

    let by_text = build_command(
        "wait",
        &input(json!({"action":"wait","contains":"Welcome"})),
    )
    .unwrap();
    assert_eq!(by_text.args, vec!["wait", "--text", "Welcome"]);
}

#[test]
fn tab_actions_map_to_tab_subcommands() {
    assert_eq!(
        build_command("list_tabs", &input(json!({"action":"list_tabs"})))
            .unwrap()
            .args,
        vec!["tab", "list"]
    );
    assert_eq!(
        build_command("select_tab", &input(json!({"action":"select_tab","tab_id":2})))
            .unwrap()
            .args,
        vec!["tab", "t2"]
    );
}

#[test]
fn upload_requires_selector_and_path() {
    let plan = build_command(
        "upload",
        &input(json!({"action":"upload","selector":"input[type=file]","path":"/tmp/a.png"})),
    )
    .unwrap();
    assert_eq!(plan.args, vec!["upload", "input[type=file]", "/tmp/a.png"]);

    assert!(
        build_command("upload", &input(json!({"action":"upload","path":"/tmp/a"}))).is_err()
    );
}

#[test]
fn every_jcode_action_has_a_mapping() {
    // The tool schema advertises these; none may fall through to "unsupported".
    let cases = vec![
        json!({"action":"list_tabs"}),
        json!({"action":"new_tab"}),
        json!({"action":"select_tab","tab_id":1}),
        json!({"action":"get_active_tab"}),
        json!({"action":"list_frames"}),
        json!({"action":"open","url":"https://a.test"}),
        json!({"action":"snapshot"}),
        json!({"action":"get_content"}),
        json!({"action":"interactables"}),
        json!({"action":"click","selector":"a"}),
        json!({"action":"type","selector":"a","text":"b"}),
        json!({"action":"fill_form","fields":[{"selector":"a","value":"b"}]}),
        json!({"action":"select","selector":"a","text":"b"}),
        json!({"action":"wait","selector":"a"}),
        json!({"action":"screenshot"}),
        json!({"action":"eval","script":"1"}),
        json!({"action":"scroll","position":"top"}),
        json!({"action":"upload","selector":"a","path":"/tmp/x"}),
        json!({"action":"press","key":"Enter"}),
        json!({"action":"provider_command","provider_action":"doctor"}),
    ];
    for case in cases {
        let action = case["action"].as_str().unwrap().to_string();
        let plan = build_command(&action, &input(case.clone()));
        assert!(plan.is_ok(), "action {action} failed: {:?}", plan.err());
        assert!(
            !plan.unwrap().args.is_empty(),
            "action {action} produced no args"
        );
    }
}

#[test]
fn tab_list_renders_new_and_old_tab_id_shapes() {
    // agent-browser >=0.30 shape: stable string handles.
    let new_shape = json!({"tabs":[
        {"active":true,"tabId":"t1","title":"A","url":"https://a.test"},
        {"active":false,"tabId":"t2","title":"B","url":"https://b.test"}
    ]});
    let rendered = super::format_tabs(&new_shape);
    assert!(rendered.contains("t1"), "{rendered}");
    assert!(rendered.contains("t2"), "{rendered}");
    assert!(rendered.starts_with('*'), "active tab must be marked: {rendered}");

    // Older shape: positional index only.
    let old_shape = json!({"tabs":[
        {"active":false,"index":0,"title":"A","url":"https://a.test"},
        {"active":true,"index":1,"title":"B","url":"https://b.test"}
    ]});
    let rendered = super::format_tabs(&old_shape);
    assert!(rendered.contains("t0"), "{rendered}");
    assert!(rendered.contains("t1"), "{rendered}");
}

#[test]
fn select_tab_always_uses_t_prefixed_handle() {
    // Bare integers are rejected by agent-browser >=0.30.
    let plan = build_command("select_tab", &input(json!({"action":"select_tab","tab_id":3})))
        .unwrap();
    assert_eq!(plan.args, vec!["tab", "t3"]);
}

#[test]
fn get_content_html_and_title_map_to_get_subcommands() {
    let html = build_command(
        "get_content",
        &input(json!({"action":"get_content","format":"html","selector":"#main"})),
    )
    .unwrap();
    assert_eq!(html.args, vec!["get", "html", "#main"]);

    let title = build_command(
        "get_content",
        &input(json!({"action":"get_content","format":"title"})),
    )
    .unwrap();
    assert_eq!(title.args, vec!["get", "title"]);

    // Default format falls back to body text.
    let text = build_command("get_content", &input(json!({"action":"get_content"}))).unwrap();
    assert_eq!(text.args, vec!["get", "text", "body"]);
}
