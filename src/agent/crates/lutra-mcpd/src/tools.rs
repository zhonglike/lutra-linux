//! MCP tool catalogue.
//!
//! The tool set is deliberately small and mirrors the six capabilities the
//! daemon exposes. Each entry is forwarded 1:1 as a `tool.call` JSON-RPC
//! request; the daemon (not this process) decides whether it is allowed.

use lutra_proto::tools;
use serde_json::{json, Value};

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: tools::FS_READ,
            description: "Read a file inside the session's allowed roots. Requires fs.read; \
                          paths outside the home directory additionally require fs.system.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path, no '..' segments"
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDef {
            name: tools::FS_WRITE,
            description: "Create or overwrite a file in the home directory, /tmp or /var/tmp. \
                          Requires fs.write. System paths and login/autostart files are refused.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path" },
                    "content": { "type": "string", "description": "UTF-8 content, max 32 MiB" },
                    "append": { "type": "boolean", "description": "Append instead of truncating" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        },
        ToolDef {
            name: tools::EXEC,
            description: "Run a binary without a shell. Requires exec; interpreters (python, node, ...) \
                          additionally require sys. A denylist blocks shells and system-modifying tools.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Executable name or absolute path" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Argument vector, passed verbatim"
                    },
                    "cwd": { "type": "string", "description": "Absolute working directory" },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 300,
                        "default": 30
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        },
        ToolDef {
            name: tools::NET_FETCH,
            description: "HTTP GET to a public host. Requires net. Private, loopback and metadata \
                          addresses are refused. TLS is not implemented in this build (http only).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "http:// URL with a public host" }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
        },
        ToolDef {
            name: tools::PKG_INSTALL,
            description: "Install a .ltr package. Requires pkg (plus fs.read for a local file or net \
                          for store ids). The daemon delegates to `ltr install`.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to a .ltr file in the home directory, /tmp or the store cache"
                    },
                    "id": {
                        "type": "string",
                        "description": "Reverse-DNS package id (store resolution is not implemented)"
                    }
                },
                "additionalProperties": false
            }),
        },
        ToolDef {
            name: tools::STATUS,
            description: "Daemon status: version, uptime, sessions and the audit log path. No capability required.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
    ]
}

/// Payload for the MCP `tools/list` result.
pub fn list_result() -> Value {
    let tools: Vec<Value> = definitions()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            })
        })
        .collect();
    json!({ "tools": tools })
}

pub fn is_known(name: &str) -> bool {
    definitions().iter().any(|tool| tool.name == name)
}
