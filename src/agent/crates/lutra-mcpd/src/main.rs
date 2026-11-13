//! `lutra-mcpd` — stdio MCP server that forwards tools to `lutrad`.
//!
//! MCP is newline-delimited JSON-RPC 2.0 on stdin/stdout. `lutrad` is spoken to
//! over the Unix socket using the same dialect. stdout is reserved for MCP
//! frames; diagnostics go to stderr.
//!
//! Session bootstrap:
//!   1. reuse `LUTRA_SESSION_TOKEN` when set, otherwise
//!   2. call `session.create` with `LUTRA_SESSION_CAPS` (default: the seven
//!      capabilities needed by a normal agent).
//!
//! The guard stub may deny the request; that failure is surfaced to the MCP
//! client rather than silently downgraded.

mod tools;

use anyhow::Result;
use clap::Parser;
use lutra_proto::{codes, Capability, Request, Response, RpcError};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::time::Duration;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const DEFAULT_CAPS: &str = "fs.read,fs.write,exec,net,pkg,sys,display";

#[derive(Debug, Parser)]
#[command(
    name = "lutra-mcpd",
    version,
    about = "Lutra MCP server: exposes fs/exec/net/pkg tools backed by lutrad"
)]
struct Cli {
    /// lutrad control socket.
    #[arg(long, default_value = "/run/lutra/lutrad.sock")]
    socket: PathBuf,

    /// Session subject reported to lutrad (defaults to $USER).
    #[arg(long)]
    subject: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = RpcClient::new(cli.socket);
    let subject = cli
        .subject
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "mcp:lutra-mcpd".to_string());

    let token = match std::env::var("LUTRA_SESSION_TOKEN") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => create_session(&client, &subject)?,
    };
    let fingerprint = token.get(..16).unwrap_or(token.as_str());
    eprintln!("lutra-mcpd: session ready ({fingerprint}…)");

    serve(client, token)
}

fn create_session(client: &RpcClient, subject: &str) -> Result<String> {
    let caps = session_caps();
    eprintln!(
        "lutra-mcpd: requesting caps [{}]",
        caps.iter()
            .map(|cap| cap.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let result = client
        .call("session.create", json!({ "subject": subject, "caps": caps }))
        .map_err(|error| anyhow::anyhow!("session.create failed: {error}"))?;
    result
        .get("token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("session.create returned no token"))
}

fn session_caps() -> Vec<Capability> {
    let raw = std::env::var("LUTRA_SESSION_CAPS").unwrap_or_else(|_| DEFAULT_CAPS.to_string());
    let mut caps = Vec::new();
    for item in raw.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        match Capability::parse(item) {
            Some(cap) => caps.push(cap),
            None => eprintln!("lutra-mcpd: ignoring unknown capability `{item}`"),
        }
    }
    caps
}

fn serve(client: RpcClient, token: String) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    let mut line = String::new();

    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Request = match serde_json::from_str(trimmed) {
            Ok(request) => request,
            Err(error) => {
                write_frame(
                    &mut writer,
                    &Response::error(
                        None,
                        RpcError::new(codes::PARSE_ERROR, format!("invalid JSON: {error}")),
                    ),
                )?;
                continue;
            }
        };

        let is_notification = request.id.is_none();
        let id = request.id.clone();
        let response = match handle(&client, &token, &request.method, request.params) {
            Ok(result) => Response::ok(id, result),
            Err(error) => Response::error(id, error),
        };
        if is_notification {
            continue;
        }
        write_frame(&mut writer, &response)?;
    }
    Ok(())
}

fn write_frame<W: Write>(writer: &mut W, response: &Response) -> Result<()> {
    serde_json::to_writer(&mut *writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn handle(
    client: &RpcClient,
    token: &str,
    method: &str,
    params: Option<Value>,
) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": "lutra-mcpd",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "Tools are enforced by lutrad: every call needs a capability in the \
                             session token. Escalate through guard.authorize when a tool is refused."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools::list_result()),
        "tools/call" => tools_call(client, token, params),
        // Notifications are fire-and-forget; handled before this match for id-less frames.
        "notifications/initialized" | "notifications/cancelled" => Ok(Value::Null),
        other => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("unknown MCP method `{other}`"),
        )),
    }
}

#[derive(Debug, Deserialize)]
struct CallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
}

fn tools_call(client: &RpcClient, token: &str, params: Option<Value>) -> Result<Value, RpcError> {
    let params: CallParams = serde_json::from_value(params.unwrap_or(Value::Null))
        .map_err(|error| RpcError::invalid_params(format!("invalid tools/call params: {error}")))?;

    if !tools::is_known(&params.name) {
        return Err(RpcError::invalid_params(format!(
            "unknown tool `{}`",
            params.name
        )));
    }

    let arguments = if params.arguments.is_null() {
        json!({})
    } else {
        params.arguments
    };

    match client.call(
        "tool.call",
        json!({
            "token": token,
            "tool": params.name,
            "arguments": arguments,
        }),
    ) {
        Ok(value) => Ok(tool_result_ok(&value)),
        // Tool failures are ordinary MCP results with isError=true, not protocol errors.
        Err(error) => Ok(tool_result_err(&error)),
    }
}

fn tool_result_ok(value: &Value) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    })
}

fn tool_result_err(error: &RpcError) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": format!("lutrad error {}: {}", error.code, error.message)
        }],
        "isError": true
    })
}

// ---------------------------------------------------------------------------
// lutrad client
// ---------------------------------------------------------------------------

struct RpcClient {
    socket: PathBuf,
}

impl RpcClient {
    fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    #[cfg(unix)]
    fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(&self.socket).map_err(|error| {
            RpcError::new(
                codes::IO_ERROR,
                format!("cannot connect to {}: {error}", self.socket.display()),
            )
        })?;
        stream
            .set_read_timeout(Some(Duration::from_secs(330)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .ok();

        let mut reader = BufReader::new(stream.try_clone().map_err(|error| {
            RpcError::new(codes::IO_ERROR, format!("cannot clone socket: {error}"))
        })?);

        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let mut frame = serde_json::to_vec(&request)
            .map_err(|error| RpcError::internal(format!("cannot encode request: {error}")))?;
        frame.push(b'\n');
        stream.write_all(&frame).map_err(|error| {
            RpcError::new(codes::IO_ERROR, format!("cannot write request: {error}"))
        })?;
        stream.flush().ok();

        let mut response_line = String::new();
        reader.read_line(&mut response_line).map_err(|error| {
            RpcError::new(
                codes::IO_ERROR,
                format!("cannot read response from lutrad: {error}"),
            )
        })?;
        if response_line.trim().is_empty() {
            return Err(RpcError::new(
                codes::INTERNAL_ERROR,
                "lutrad closed the connection without a response",
            ));
        }

        let response: Response = serde_json::from_str(response_line.trim()).map_err(|error| {
            RpcError::new(
                codes::INTERNAL_ERROR,
                format!("cannot decode lutrad response: {error}"),
            )
        })?;
        if let Some(error) = response.error {
            return Err(error);
        }
        response.result.ok_or_else(|| {
            RpcError::new(codes::INTERNAL_ERROR, "lutrad returned an empty result")
        })
    }

    #[cfg(not(unix))]
    fn call(&self, _method: &str, _params: Value) -> Result<Value, RpcError> {
        Err(RpcError::new(
            codes::NOT_IMPLEMENTED,
            "lutra-mcpd requires Unix domain sockets",
        ))
    }
}
