//! Fake LSP server for jcode-lsp integration tests.
//!
//! Hand-rolled, blocking stdio JSON-RPC with `Content-Length` framing. No
//! `async-lsp` dependency needed server-side. Behavior is driven by a
//! scenario, taken from `argv[1]` (preferred) or the `FAKE_LSP_DIAG` /
//! `FAKE_LSP_PULL` / `FAKE_LSP_CRASH` env vars (fallback), so one binary
//! covers every test scenario:
//!
//! - `error`  — publish one error diagnostic on didOpen/didChange.
//! - `clean`  — publish empty diagnostics on didOpen/didChange.
//! - `silent` — publish nothing (exercises the timeout path).
//! - `pull`   — declare pull-diagnostics support; answer
//!   `textDocument/diagnostic` with one warning; no push.
//! - `crash`  — exit(1) immediately after the `initialize` response.
//! - `hang`   — read the `initialize` request and never respond (exercises
//!   the registry's initialize timeout).
//! - `rename` — answer `textDocument/rename` with a `WorkspaceEdit` replacing
//!   line 0, chars 4..12 with the new name; also publishes clean diagnostics
//!   like `clean`. When `argv[2]` is set, every received notification method
//!   is appended to that file (one per line) so tests can observe `didChange`.
//!
//! Always: `initialize`/`initialized`/`shutdown`/`exit` handshake, and
//! `textDocument/definition` answers with a fixed location (the queried
//! file, line 0, column 0).

use std::io::{self, BufRead, Write};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scenario {
    Error,
    Clean,
    Silent,
    Pull,
    Crash,
    Hang,
    Rename,
}

fn scenario() -> Scenario {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "error" => return Scenario::Error,
            "clean" => return Scenario::Clean,
            "silent" => return Scenario::Silent,
            "pull" => return Scenario::Pull,
            "crash" => return Scenario::Crash,
            "hang" => return Scenario::Hang,
            "rename" => return Scenario::Rename,
            _ => {}
        }
    }
    if std::env::var("FAKE_LSP_CRASH").ok().as_deref() == Some("1") {
        return Scenario::Crash;
    }
    if std::env::var("FAKE_LSP_PULL").ok().as_deref() == Some("1") {
        return Scenario::Pull;
    }
    match std::env::var("FAKE_LSP_DIAG").ok().as_deref() {
        Some("error") => Scenario::Error,
        Some("silent") => Scenario::Silent,
        _ => Scenario::Clean,
    }
}

/// Read one `Content-Length`-framed JSON-RPC message body. `Ok(None)` on
/// clean EOF (peer closed stdin).
fn read_message(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed
            .split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
        {
            content_length = rest.1.trim().parse().ok();
        }
    }
    let len = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn write_message(writer: &mut impl Write, body: &str) {
    let _ = write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = writer.flush();
}

fn write_response(writer: &mut impl Write, id: &serde_json::Value, result: serde_json::Value) {
    let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
    write_message(writer, &msg.to_string());
}

fn write_notification(writer: &mut impl Write, method: &str, params: serde_json::Value) {
    let msg = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
    write_message(writer, &msg.to_string());
}

fn build_initialize_result(scenario: Scenario) -> serde_json::Value {
    let mut capabilities = serde_json::json!({ "textDocumentSync": 1 });
    if scenario == Scenario::Pull {
        capabilities["diagnosticProvider"] = serde_json::json!({
            "identifier": "fake",
            "interFileDependencies": false,
            "workspaceDiagnostics": false,
            "workDoneProgress": false,
        });
    }
    serde_json::json!({ "capabilities": capabilities })
}

fn publish_for_scenario(writer: &mut impl Write, scenario: Scenario, uri: &str, version: i64) {
    match scenario {
        Scenario::Error => {
            let diags = serde_json::json!([{
                "range": {
                    "start": {"line": 0, "character": 4},
                    "end": {"line": 0, "character": 5},
                },
                "severity": 1,
                "message": "fake error",
            }]);
            write_notification(
                writer,
                "textDocument/publishDiagnostics",
                serde_json::json!({"uri": uri, "version": version, "diagnostics": diags}),
            );
        }
        Scenario::Clean | Scenario::Rename => {
            write_notification(
                writer,
                "textDocument/publishDiagnostics",
                serde_json::json!({"uri": uri, "version": version, "diagnostics": []}),
            );
        }
        // Silent: publish nothing (timeout path).
        // Pull: no push; answered only via textDocument/diagnostic below.
        Scenario::Silent | Scenario::Pull => {}
        Scenario::Crash => unreachable!("process exits right after initialize"),
        Scenario::Hang => unreachable!("process never answers initialize"),
    }
}

fn build_pull_result(scenario: Scenario) -> serde_json::Value {
    if scenario == Scenario::Pull {
        serde_json::json!({
            "kind": "full",
            "resultId": null,
            "items": [{
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 1},
                },
                "severity": 2,
                "message": "fake warning",
            }],
        })
    } else {
        serde_json::json!({"kind": "full", "resultId": null, "items": []})
    }
}

fn main() {
    let scenario = scenario();
    // Optional notification log (argv[2]): every received notification method
    // is appended, one per line, so tests can observe e.g. didChange.
    let notif_log = std::env::args().nth(2);
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    loop {
        let body = match read_message(&mut reader) {
            Ok(Some(b)) => b,
            Ok(None) => break,
            Err(_) => break,
        };
        let msg: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(serde_json::Value::Null);

        if id.is_none()
            && !method.is_empty()
            && let Some(log) = &notif_log
            && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log)
        {
            let _ = writeln!(f, "{method}");
        }

        match method {
            "initialize" => {
                if scenario == Scenario::Hang {
                    // Never respond; keep reading so the pipe stays open.
                    continue;
                }
                let result = build_initialize_result(scenario);
                if let Some(id) = &id {
                    write_response(&mut writer, id, result);
                }
                if scenario == Scenario::Crash {
                    let _ = writer.flush();
                    std::process::exit(1);
                }
            }
            "initialized" => {}
            "shutdown" => {
                if let Some(id) = &id {
                    write_response(&mut writer, id, serde_json::Value::Null);
                }
            }
            "exit" => {
                std::process::exit(0);
            }
            "textDocument/didOpen" | "textDocument/didChange" => {
                let uri = params.pointer("/textDocument/uri").and_then(|v| v.as_str());
                let version = params
                    .pointer("/textDocument/version")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1);
                if let Some(uri) = uri {
                    publish_for_scenario(&mut writer, scenario, uri, version);
                }
            }
            "textDocument/diagnostic" => {
                if let Some(id) = &id {
                    let result = build_pull_result(scenario);
                    write_response(&mut writer, id, result);
                }
            }
            "textDocument/rename" => {
                if let Some(id) = &id {
                    let uri = params
                        .pointer("/textDocument/uri")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let new_name = params
                        .pointer("/newName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("renamed");
                    // Replace line 0, chars 4..12 ("old_name" in the standard
                    // test file content) with the new name.
                    let result = serde_json::json!({
                        "changes": {
                            uri: [{
                                "range": {
                                    "start": {"line": 0, "character": 4},
                                    "end": {"line": 0, "character": 12},
                                },
                                "newText": new_name,
                            }],
                        },
                    });
                    write_response(&mut writer, id, result);
                }
            }
            "textDocument/definition" => {
                if let Some(id) = &id {
                    let uri = params
                        .pointer("/textDocument/uri")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let result = serde_json::json!({
                        "uri": uri,
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 0},
                        },
                    });
                    write_response(&mut writer, id, result);
                }
            }
            "workspace/symbol" => {
                if let Some(id) = &id {
                    let query = params
                        .pointer("/query")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let result = serde_json::json!([{
                        "name": format!("fake_symbol_{query}"),
                        "kind": 12,
                        "location": {
                            "uri": "file:///tmp/fake_symbol.rs",
                            "range": {
                                "start": {"line": 3, "character": 0},
                                "end": {"line": 3, "character": 10},
                            },
                        },
                    }]);
                    write_response(&mut writer, id, result);
                }
            }
            _ => {
                // Unknown request: answer null so the client never hangs.
                // Unknown notification: ignore.
                if let Some(id) = &id {
                    write_response(&mut writer, id, serde_json::Value::Null);
                }
            }
        }
    }
}
