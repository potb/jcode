//! Does an upgrade invalidate the picker's warm-start disk cache?
//!
//! `SessionInfo` gained `is_ambient` (issue #26) and the cache file at
//! `~/.jcode/cache/session-picker-list-v1.json` serializes `SessionInfo`
//! directly, with `SESSION_LIST_DISK_CACHE_VERSION` unchanged at 1. If the new
//! field has no serde default, every cache written by a previous build fails to
//! deserialize and the first `/resume` after upgrading silently loses its warm
//! start (`load_cached_sessions_grouped` swallows the error with `.ok()?`).

#[test]
fn cache_entry_written_before_is_ambient_still_loads() {
    // Shaped exactly as an older build wrote it: no `is_ambient` key at all.
    let legacy_session = serde_json::json!({
        "id": "session_legacy",
        "parent_id": null,
        "short_name": "legacy",
        "icon": "🧪",
        "title": "Legacy cached session",
        "message_count": 2,
        "user_message_count": 1,
        "assistant_message_count": 1,
        "created_at": "2026-08-01T00:00:00Z",
        "last_message_time": "2026-08-01T00:00:00Z",
        "last_active_at": null,
        "working_dir": "/tmp",
        "model": null,
        "provider_key": null,
        "is_canary": false,
        "is_debug": false,
        "saved": false,
        "save_label": null,
        "status": "Closed",
        "needs_catchup": false,
        "estimated_tokens": 10,
        "first_user_prompt": null,
        "messages_preview": [],
        "search_index": "legacy",
        "server_name": null,
        "server_icon": null,
        "source": "Jcode",
        "resume_target": { "JcodeSession": { "session_id": "session_legacy" } },
        "external_path": null
    });

    let session: jcode_tui_session_picker::SessionInfo = serde_json::from_value(legacy_session)
        .expect(
            "a cache entry written before `is_ambient` existed must still load, \
             otherwise the first /resume after an upgrade loses its warm start",
        );
    assert!(!session.is_ambient, "a missing flag must default to false");
}
