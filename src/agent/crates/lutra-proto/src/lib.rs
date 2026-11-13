//! Shared wire types for the Lutra intelligent layer (`src/agent`).
//!
//! Everything in this crate is transport agnostic: the daemon, the MCP bridge
//! and the packaging tools all speak the same JSON-RPC 2.0 dialect plus the
//! capability and session-token types defined here.

pub mod audit;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

pub use audit::{AuditEntry, AuditLog, Decision as AuditDecision, VerifyReport};

/// Version of the lutrad JSON-RPC dialect.
pub const PROTOCOL_VERSION: u32 = 1;

/// Default control socket (`tmpfs`, recreated on boot).
pub const DEFAULT_SOCKET: &str = "/run/lutra/lutrad.sock";

/// Directory where per-session token files are written (`tmpfs`).
pub const SESSION_DIR: &str = "/run/lutra/sessions";

/// Directory where the daemon's Ed25519 signing key lives (`tmpfs`).
pub const KEY_DIR: &str = "/run/lutra/keys";

/// Append-only audit chain.
pub const AUDIT_LOG: &str = "/var/log/lutra/audit.jsonl";

/// Session token lifetime: 2 hours, per `docs/02-desktop-and-agent.md`.
pub const DEFAULT_TOKEN_TTL_SECS: u64 = 2 * 60 * 60;

/// Upper bound accepted from clients (8 hours, the largest value offered in settings).
pub const MAX_TOKEN_TTL_SECS: u64 = 8 * 60 * 60;

/// Prefix of an encoded session token.
pub const TOKEN_PREFIX: &str = "ltr1";

/// Size of the raw Ed25519 signature.
pub const SIGNATURE_LEN: usize = 64;

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// Capability enumeration. Serialized in its dotted form (`"fs.read"`), which is
/// also the spelling used by `.ltr` manifests and MCP tool metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "fs.read")]
    FsRead,
    #[serde(rename = "fs.write")]
    FsWrite,
    #[serde(rename = "fs.system")]
    FsSystem,
    #[serde(rename = "exec")]
    Exec,
    #[serde(rename = "net")]
    Net,
    #[serde(rename = "pkg")]
    Pkg,
    #[serde(rename = "sys")]
    Sys,
    #[serde(rename = "display")]
    Display,
    #[serde(rename = "input")]
    Input,
    #[serde(rename = "usb")]
    Usb,
    #[serde(rename = "audio")]
    Audio,
    #[serde(rename = "camera")]
    Camera,
    #[serde(rename = "clipboard")]
    Clipboard,
    #[serde(rename = "sudo")]
    Sudo,
}

/// Rough risk level, used by the guard stub to decide whether a prompt is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Danger {
    Low,
    Medium,
    High,
    Critical,
}

impl Capability {
    pub const ALL: [Capability; 14] = [
        Capability::FsRead,
        Capability::FsWrite,
        Capability::FsSystem,
        Capability::Exec,
        Capability::Net,
        Capability::Pkg,
        Capability::Sys,
        Capability::Display,
        Capability::Input,
        Capability::Usb,
        Capability::Audio,
        Capability::Camera,
        Capability::Clipboard,
        Capability::Sudo,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Capability::FsRead => "fs.read",
            Capability::FsWrite => "fs.write",
            Capability::FsSystem => "fs.system",
            Capability::Exec => "exec",
            Capability::Net => "net",
            Capability::Pkg => "pkg",
            Capability::Sys => "sys",
            Capability::Display => "display",
            Capability::Input => "input",
            Capability::Usb => "usb",
            Capability::Audio => "audio",
            Capability::Camera => "camera",
            Capability::Clipboard => "clipboard",
            Capability::Sudo => "sudo",
        }
    }

    /// Parse the dotted name; returns `None` for unknown capabilities so callers
    /// can reject rather than silently widen a grant.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|cap| cap.as_str() == value)
    }

    pub const fn danger(self) -> Danger {
        match self {
            Capability::FsRead => Danger::Low,
            Capability::FsWrite => Danger::Medium,
            Capability::FsSystem => Danger::High,
            Capability::Exec => Danger::High,
            Capability::Net => Danger::Medium,
            Capability::Pkg => Danger::High,
            Capability::Sys => Danger::Critical,
            Capability::Display => Danger::Medium,
            Capability::Input => Danger::High,
            Capability::Usb => Danger::High,
            Capability::Audio => Danger::Medium,
            Capability::Camera => Danger::Medium,
            Capability::Clipboard => Danger::Medium,
            Capability::Sudo => Danger::Critical,
        }
    }

    /// Low-risk capabilities are granted without a guard prompt.
    pub const fn requires_confirmation(self) -> bool {
        match self.danger() {
            Danger::Low => false,
            _ => true,
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Session tokens
// ---------------------------------------------------------------------------

/// Claims carried by an Ed25519-signed session token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenClaims {
    /// Token format version.
    pub v: u32,
    /// Session id this token belongs to.
    pub session: String,
    /// Human readable actor (`agent:claude-code`, `mcp:lutra-mcpd`, ...).
    pub subject: String,
    /// Granted capabilities.
    pub caps: Vec<Capability>,
    /// Issued at (unix seconds).
    pub iat: u64,
    /// Expiry (unix seconds).
    pub exp: u64,
    /// Random nonce; rotating it invalidates superseded tokens.
    pub nonce: String,
}

impl TokenClaims {
    pub fn has_cap(&self, cap: Capability) -> bool {
        self.caps.contains(&cap)
    }

    pub fn missing(&self, required: &[Capability]) -> Vec<Capability> {
        required
            .iter()
            .copied()
            .filter(|cap| !self.has_cap(*cap))
            .collect()
    }

    pub fn covers(&self, required: &[Capability]) -> bool {
        required.iter().all(|cap| self.has_cap(*cap))
    }

    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.exp
    }
}

/// Decoded but not yet verified token.
#[derive(Debug, Clone)]
pub struct RawToken {
    pub payload: Vec<u8>,
    pub signature: [u8; SIGNATURE_LEN],
}

/// Message actually signed: `"ltr1." + hex(json claims)`.
pub fn signing_message(payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(TOKEN_PREFIX.len() + 1 + payload.len() * 2);
    message.extend_from_slice(TOKEN_PREFIX.as_bytes());
    message.push(b'.');
    message.extend_from_slice(hex::encode(payload).as_bytes());
    message
}

/// Encode an already signed payload (`ltr1.<hex payload>.<hex signature>`).
pub fn encode_token_bytes(payload: &[u8], signature: &[u8; SIGNATURE_LEN]) -> String {
    format!(
        "{}.{}.{}",
        TOKEN_PREFIX,
        hex::encode(payload),
        hex::encode(signature)
    )
}

/// Serialize claims and encode them with the given signature.
pub fn encode_token(claims: &TokenClaims, signature: &[u8; SIGNATURE_LEN]) -> String {
    let payload = serde_json::to_vec(claims).expect("token claims are always serializable");
    encode_token_bytes(&payload, signature)
}

/// Split and hex-decode a token. Does **not** verify the signature.
pub fn decode_token(token: &str) -> Result<RawToken, RpcError> {
    let mut parts = token.split('.');
    let prefix = parts.next();
    let payload_hex = parts.next();
    let signature_hex = parts.next();

    if prefix != Some(TOKEN_PREFIX) || parts.next().is_some() {
        return Err(RpcError::new(
            codes::SESSION_INVALID,
            "malformed session token (expected ltr1.<payload>.<signature>)",
        ));
    }
    let payload_hex = payload_hex.ok_or_else(|| {
        RpcError::new(codes::SESSION_INVALID, "session token has no payload")
    })?;
    let signature_hex = signature_hex.ok_or_else(|| {
        RpcError::new(codes::SESSION_INVALID, "session token has no signature")
    })?;

    let payload = hex::decode(payload_hex)
        .map_err(|_| RpcError::new(codes::SESSION_INVALID, "session token payload is not hex"))?;
    let signature = hex::decode(signature_hex)
        .map_err(|_| RpcError::new(codes::SESSION_INVALID, "session token signature is not hex"))?;
    let signature: [u8; SIGNATURE_LEN] = signature.as_slice().try_into().map_err(|_| {
        RpcError::new(codes::SESSION_INVALID, "session token signature has the wrong length")
    })?;

    Ok(RawToken { payload, signature })
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0
// ---------------------------------------------------------------------------

/// Stable error codes. The first block mirrors JSON-RPC 2.0, the second is
/// Lutra specific and exposed verbatim to MCP clients.
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;

    /// Generic capability failure (token does not cover the request).
    pub const PERMISSION_DENIED: i32 = -32001;
    pub const UNKNOWN_CAPABILITY: i32 = -32002;
    pub const SESSION_INVALID: i32 = -32003;
    pub const SESSION_EXPIRED: i32 = -32004;
    /// Target rejected by policy regardless of capabilities.
    pub const POLICY_DENIED: i32 = -32005;
    pub const EXEC_FAILED: i32 = -32006;
    pub const CHAIN_BROKEN: i32 = -32007;
    pub const PACKAGE_INVALID: i32 = -32008;
    /// Feature intentionally absent from this build.
    pub const NOT_IMPLEMENTED: i32 = -32009;
    pub const IO_ERROR: i32 = -32010;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes::INTERNAL_ERROR, message)
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, message)
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    /// Notifications omit the id; responses echo it back.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, error: RpcError) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

fn jsonrpc_version() -> String {
    "2.0".to_string()
}

// ---------------------------------------------------------------------------
// Session metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session: String,
    pub subject: String,
    pub caps: Vec<Capability>,
    pub created_at: u64,
    pub expires_at: u64,
    pub revoked: bool,
}

// ---------------------------------------------------------------------------
// Tool names (wire + MCP)
// ---------------------------------------------------------------------------

pub mod tools {
    pub const FS_READ: &str = "lutra_fs_read";
    pub const FS_WRITE: &str = "lutra_fs_write";
    pub const EXEC: &str = "lutra_exec";
    pub const NET_FETCH: &str = "lutra_net_fetch";
    pub const PKG_INSTALL: &str = "lutra_pkg_install";
    pub const STATUS: &str = "lutra_status";

    pub const ALL: [&str; 6] = [FS_READ, FS_WRITE, EXEC, NET_FETCH, PKG_INSTALL, STATUS];
}

// ---------------------------------------------------------------------------
// Time helpers (no chrono dependency)
// ---------------------------------------------------------------------------

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// RFC 3339 / ISO 8601 UTC timestamp, used by the audit log.
pub fn now_rfc3339() -> String {
    format_rfc3339(now_unix())
}

pub fn format_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days` (days since 1970-01-01).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let shifted = if z >= 0 { z } else { z - 146_096 };
    let era = shifted / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_round_trip() {
        for cap in Capability::ALL {
            assert_eq!(Capability::parse(cap.as_str()), Some(cap));
        }
        assert_eq!(Capability::parse("fs.nope"), None);
    }

    #[test]
    fn capability_serde_uses_dotted_names() {
        let json = serde_json::to_string(&[Capability::FsRead, Capability::FsSystem]).unwrap();
        assert_eq!(json, r#"["fs.read","fs.system"]"#);
    }

    #[test]
    fn token_encoding_round_trip() {
        let claims = TokenClaims {
            v: PROTOCOL_VERSION,
            session: "sess-1".into(),
            subject: "agent:test".into(),
            caps: vec![Capability::FsRead],
            iat: 100,
            exp: 200,
            nonce: "abcd".into(),
        };
        let token = encode_token(&claims, &[7u8; SIGNATURE_LEN]);
        let raw = decode_token(&token).unwrap();
        assert_eq!(raw.signature, [7u8; SIGNATURE_LEN]);
        let decoded: TokenClaims = serde_json::from_slice(&raw.payload).unwrap();
        assert_eq!(decoded, claims);
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        assert!(decode_token("nope").is_err());
        assert!(decode_token("ltr1.zz.zz").is_err());
        assert!(decode_token("ltr1.abcd.abcd").is_err());
    }

    #[test]
    fn rfc3339_formatting() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn claims_coverage() {
        let claims = TokenClaims {
            v: 1,
            session: "s".into(),
            subject: "x".into(),
            caps: vec![Capability::FsRead, Capability::Net],
            iat: 0,
            exp: 10,
            nonce: "n".into(),
        };
        assert!(claims.covers(&[Capability::Net]));
        assert_eq!(claims.missing(&[Capability::Net, Capability::Exec]), vec![Capability::Exec]);
        assert!(claims.is_expired(10));
        assert!(!claims.is_expired(9));
    }
}
