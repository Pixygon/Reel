//! The agent platform — Reel as a long-lived tool server.
//!
//! Two doors, one implementation, zero drift: both project the SAME
//! `COMMANDS` table that drives the CLI parser, the help and the docs.
//!
//!  * `reel serve` — newline-delimited JSON-RPC 2.0 over stdio. Same verbs
//!    as the CLI, no process-per-command. `{"jsonrpc":"2.0","id":1,
//!    "method":"info","params":{"args":["file.mp4"]}}`.
//!  * `reel mcp` — a Model Context Protocol stdio server: every verb is an
//!    MCP tool with a real input schema, so any MCP-speaking agent runtime
//!    drives Reel natively.
//!
//! Requests run on worker threads (a render must not block a probe);
//! responses carry the request's id and may arrive out of order, which is
//! JSON-RPC's contract anyway.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// Build a `Parsed` for `cmd` out of JSON arguments: positional args by
/// their table name (TARGET, PROJECT…), flags by flag name; `true` on a
/// value-less flag turns the switch on.
fn parsed_from_json(cmd: &crate::cli::Cmd, args: &Value) -> Result<crate::cli::Parsed, String> {
    let mut positional = Vec::new();
    let obj = args.as_object().cloned().unwrap_or_default();
    // Positional either as "args": [..] or by their table names.
    if let Some(list) = obj.get("args").and_then(|v| v.as_array()) {
        for v in list {
            positional.push(value_to_string(v));
        }
    } else {
        for name in cmd.args {
            let key = name.trim_end_matches('?');
            if let Some(v) = obj.get(key).or_else(|| obj.get(&key.to_lowercase())) {
                positional.push(value_to_string(v));
            }
        }
    }
    let required = cmd.args.iter().filter(|a| !a.ends_with('?')).count();
    if positional.len() < required {
        return Err(format!(
            "`{}` needs {}: got {} argument(s)",
            cmd.name,
            cmd.args.join(" "),
            positional.len()
        ));
    }
    let mut values = std::collections::HashMap::new();
    let mut switches = std::collections::HashSet::new();
    let flags = obj
        .get("flags")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_else(|| {
            // Flat style: every non-positional key is a flag.
            let mut m = serde_json::Map::new();
            for (k, v) in &obj {
                let is_positional =
                    k == "args" || cmd.args.iter().any(|a| a.trim_end_matches('?').eq_ignore_ascii_case(k));
                if !is_positional {
                    m.insert(k.clone(), v.clone());
                }
            }
            m
        });
    for (k, v) in flags {
        let Some(flag) = cmd.flags.iter().find(|f| f.name == k) else {
            let known: Vec<&str> = cmd.flags.iter().map(|f| f.name).collect();
            return Err(format!("unknown flag {k:?} for `{}`. Known: {}", cmd.name, known.join(", ")));
        };
        if flag.value.is_some() {
            values.insert(k, value_to_string(&v));
        } else if v.as_bool().unwrap_or(true) {
            switches.insert(k);
        }
    }
    Ok(crate::cli::Parsed { positional, values, switches })
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Execute one verb with JSON arguments. The shared core of both servers.
fn call(method: &str, params: &Value) -> Result<Value, String> {
    let Some(cmd) = crate::cli::COMMANDS.iter().find(|c| c.name == method) else {
        return Err(format!("unknown method {method:?} — `commands` lists them"));
    };
    let parsed = parsed_from_json(cmd, params)?;
    match crate::cli::dispatch(cmd.name, &parsed) {
        Ok(out) => Ok(out.data),
        Err(e) => Err(format!("{e:#}")),
    }
}

fn write_line(msg: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: &Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

// ── reel serve ───────────────────────────────────────────────────────────

pub fn serve() -> i32 {
    eprintln!(
        "reel serve — JSON-RPC 2.0, one message per line. Methods = the CLI verbs \
         (`commands` lists them); params: {{\"args\":[…], \"flags\":{{…}}}}. Ctrl+D ends."
    );
    let stdin = std::io::stdin();
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                write_line(&rpc_error(&Value::Null, -32700, format!("parse error: {e}")));
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = msg.get("method").and_then(|m| m.as_str()).map(String::from) else {
            write_line(&rpc_error(&id, -32600, "no method".into()));
            continue;
        };
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        // Notifications (no id) run without a response, per JSON-RPC.
        let respond = !id.is_null();
        workers.push(std::thread::spawn(move || {
            let out = match call(&method, &params) {
                Ok(v) => rpc_result(&id, v),
                Err(e) => rpc_error(&id, -32000, e),
            };
            if respond {
                write_line(&out);
            }
        }));
        workers.retain(|w| !w.is_finished());
    }
    for w in workers {
        let _ = w.join();
    }
    0
}

// ── reel mcp ─────────────────────────────────────────────────────────────

/// The MCP tool description for one table entry: positional args become
/// required string properties, flags become optional properties (boolean
/// for value-less switches).
fn tool_schema(cmd: &crate::cli::Cmd) -> Value {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for a in cmd.args {
        let optional = a.ends_with('?');
        let key = a.trim_end_matches('?').to_lowercase();
        props.insert(key.clone(), json!({ "type": "string", "description": format!("positional: {a}") }));
        if !optional {
            required.push(Value::String(key));
        }
    }
    for f in cmd.flags {
        let schema = match f.value {
            Some(v) => json!({ "type": "string", "description": format!("{} (value: {v})", f.help) }),
            None => json!({ "type": "boolean", "description": f.help }),
        };
        props.insert(f.name.to_string(), schema);
    }
    json!({ "type": "object", "properties": props, "required": required })
}

pub fn mcp() -> i32 {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        match method {
            "initialize" => write_line(&rpc_result(&id, json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "reel",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "Reel is a native video/audio/image editor. Tools mirror the `reel` CLI: build a .reel project (new/add), edit it (split/trim/effects/audio/captions/…), render it (render/frame/convert). Media paths must be absolute.",
            }))),
            "tools/list" => {
                let tools: Vec<Value> = crate::cli::COMMANDS
                    .iter()
                    // A tool server has no business exposing its own help.
                    .filter(|c| c.name != "commands")
                    .map(|c| json!({
                        "name": c.name,
                        "description": c.help,
                        "inputSchema": tool_schema(c),
                    }))
                    .collect();
                write_line(&rpc_result(&id, json!({ "tools": tools })));
            }
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let (text, is_error) = match call(name, &args) {
                    Ok(v) => (serde_json::to_string_pretty(&v).unwrap_or_default(), false),
                    Err(e) => (e, true),
                };
                write_line(&rpc_result(&id, json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": is_error,
                })));
            }
            "ping" => write_line(&rpc_result(&id, json!({}))),
            // Notifications and unknown methods: MCP clients send
            // notifications/initialized and may probe for optional features.
            _ if id.is_null() => {}
            _ => write_line(&rpc_error(&id, -32601, format!("method not found: {method}"))),
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    fn reel_bin() -> String {
        // Unit tests run inside the bin crate, where CARGO_BIN_EXE_* isn't
        // set — the release binary is built before tests everywhere (CI
        // included).
        format!("{}/target/release/reel", env!("CARGO_MANIFEST_DIR"))
    }

    fn fixture() -> String {
        format!("{}/tests/fixture.mp4", env!("CARGO_MANIFEST_DIR"))
    }

    /// One conversation with a stdio server: send lines, read a response
    /// per request id.
    fn converse(argv: &[&str], requests: &[Value]) -> Vec<Value> {
        let mut child = Command::new(reel_bin())
            .args(argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server");
        let mut stdin = child.stdin.take().unwrap();
        for r in requests {
            writeln!(stdin, "{r}").unwrap();
        }
        drop(stdin); // EOF ends the session
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut got = Vec::new();
        for line in out.lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                got.push(v);
            }
        }
        let _ = child.wait();
        got
    }

    /// `reel serve` answers the same verbs as the CLI, as JSON-RPC — probe
    /// a real file and read its duration back.
    #[test]
    fn serve_speaks_jsonrpc_with_the_cli_verbs() {
        let responses = converse(
            &["serve"],
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"info","params":{"args":[fixture()]}}),
                json!({"jsonrpc":"2.0","id":2,"method":"presets","params":{}}),
                json!({"jsonrpc":"2.0","id":3,"method":"definitely-not-a-verb","params":{}}),
            ],
        );
        assert_eq!(responses.len(), 3, "{responses:?}");
        let by_id = |id: i64| responses.iter().find(|r| r["id"] == json!(id)).unwrap();
        let info = by_id(1);
        assert!((info["result"]["duration"].as_f64().unwrap() - 2.0).abs() < 0.2);
        assert!(by_id(2)["result"]["presets"].is_array());
        let err = by_id(3);
        assert!(err["error"]["message"].as_str().unwrap().contains("unknown method"));
    }

    /// The MCP door: initialize, list tools (every verb, schema'd), call
    /// one against a real file.
    #[test]
    fn mcp_serves_the_command_table_as_tools() {
        let responses = converse(
            &["mcp"],
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"info","arguments":{"media":fixture(),"json":true}}}),
            ],
        );
        let by_id = |id: i64| responses.iter().find(|r| r["id"] == json!(id)).unwrap();
        assert_eq!(by_id(1)["result"]["serverInfo"]["name"], "reel");
        let tools = by_id(2)["result"]["tools"].as_array().unwrap().clone();
        assert!(tools.len() > 30, "every verb is a tool, got {}", tools.len());
        let render = tools.iter().find(|t| t["name"] == "render").expect("render tool");
        assert_eq!(render["inputSchema"]["required"], json!(["project", "output"]));
        let called = by_id(3);
        assert_eq!(called["result"]["isError"], json!(false));
        let text = called["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).expect("tool result is JSON");
        assert!((payload["duration"].as_f64().unwrap() - 2.0).abs() < 0.2);
    }
}
