//! Runtime adapter for generated `Code System Graph` host hooks.

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use code_system_graph_hooks::{HostKind, RoutingRequest, route};
use serde_json::{Value, json};

const INPUT_LIMIT: u64 = 1024 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or_else(|| "expected `route` command".to_owned())?;
    if command != "route" {
        return Err(format!("unknown command `{command}`"));
    }

    let mut host = None;
    let mut root = None;
    let mut codegraph_enabled = None;
    let mut marker_seen = false;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for `{flag}`"))?;
        match flag.as_str() {
            "--host" => {
                host = Some(HostKind::from_str(&value).map_err(|error| error.to_string())?);
            }
            "--root" => root = Some(PathBuf::from(value)),
            "--codegraph-enabled" => {
                codegraph_enabled = Some(
                    value
                        .parse::<bool>()
                        .map_err(|_| "`--codegraph-enabled` must be `true` or `false`".to_owned())?,
                );
            }
            "--marker" => {
                if value != "code-system-graph-hooks:v1" {
                    return Err("unsupported generated hook marker".to_owned());
                }
                marker_seen = true;
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    let host = host.ok_or_else(|| "missing `--host`".to_owned())?;
    let root = root.ok_or_else(|| "missing `--root`".to_owned())?;
    let codegraph_enabled =
        codegraph_enabled.ok_or_else(|| "missing `--codegraph-enabled`".to_owned())?;
    if !marker_seen {
        return Err("missing `--marker`".to_owned());
    }

    let mut input = Vec::new();
    io::stdin()
        .take(INPUT_LIMIT + 1)
        .read_to_end(&mut input)
        .map_err(|error| format!("failed to read hook input: {error}"))?;
    if u64::try_from(input.len()).unwrap_or(u64::MAX) > INPUT_LIMIT {
        eprintln!("Code System Graph routing hook failed open: input exceeded 1 MiB");
        print_output(&neutral_output(host))?;
        return Ok(());
    }
    let event: Value = match serde_json::from_slice(&input) {
        Ok(event) => event,
        Err(error) => {
            eprintln!("Code System Graph routing hook failed open: {error}");
            print_output(&neutral_output(host))?;
            return Ok(());
        }
    };
    let response = match route(&RoutingRequest {
        host,
        root,
        event,
        codegraph_enabled,
        ttl_seconds: 300,
    }) {
        Ok(response) => response,
        Err(error) => {
            eprintln!("Code System Graph routing hook failed open: {error}");
            print_output(&neutral_output(host))?;
            return Ok(());
        }
    };
    let output = response.guidance.as_deref().map_or_else(
        || neutral_output(host),
        |guidance| guidance_output(host, guidance),
    );
    print_output(&output)
}

fn neutral_output(host: HostKind) -> Value {
    match host {
        HostKind::Gemini => json!({"continue": true, "decision": "allow"}),
        HostKind::Antigravity => json!({}),
        _ => json!({"continue": true}),
    }
}

fn guidance_output(host: HostKind, guidance: &str) -> Value {
    match host {
        HostKind::ClaudeCode | HostKind::Codex => json!({
            "continue": true,
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": guidance
            }
        }),
        HostKind::Gemini => json!({
            "continue": true,
            "decision": "allow",
            "hookSpecificOutput": {
                "hookEventName": "BeforeAgent",
                "additionalContext": guidance
            }
        }),
        _ => neutral_output(host),
    }
}

fn print_output(value: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string(value)
            .map_err(|error| format!("failed to serialize hook output: {error}"))?
    );
    Ok(())
}
