//! `lutrad` request server: JSON-RPC 2.0 over a newline-delimited Unix socket.
//!
//! Flow for every `tool.call`:
//!
//! ```text
//! token verification -> policy evaluation -> capability coverage
//!     -> execution -> audit append (allow)  |  audit append (deny) + error
//! ```
//!
//! The daemon never runs as root and never silently widens a grant: anything
//! the token does not cover must first go through `guard.authorize`.

use crate::policy::{self, Approver, EnvGuard, Policy};
use crate::session::SessionManager;
use lutra_proto::audit::{AuditLog, Decision};
use lutra_proto::{
    codes, now_unix, tools, Capability, Request, Response, RpcError,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;
const MAX_WRITE_BYTES: usize = 32 * 1024 * 1024;
const MAX_EXEC_OUTPUT: u64 = 1024 * 1024;
const MAX_FETCH_BYTES: u64 = 2 * 1024 * 1024;

pub struct ServerConfig {
    pub socket: PathBuf,
    pub audit_path: PathBuf,
    pub key_path: Option<PathBuf>,
    pub session_dir: Option<PathBuf>,
    pub token_ttl: u64,
}

pub struct Ctx {
    pub sessions: Mutex<SessionManager>,
    pub audit: Mutex<AuditLog>,
    pub policy: Policy,
    pub guard: EnvGuard,
    pub ttl: u64,
    pub started: Instant,
    pub audit_path: PathBuf,
    pub socket: PathBuf,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Ctx {
    fn record(
        &self,
        session: &str,
        subject: &str,
        tool: &str,
        caps: &[Capability],
        decision: Decision,
        reason: &str,
    ) {
        let mut audit = lock(&self.audit);
        if let Err(error) = audit.append(session, subject, tool, caps, decision, reason) {
            error!(error = %error, "audit append failed");
        }
    }
}

fn build(config: ServerConfig) -> anyhow::Result<Arc<Ctx>> {
    let ServerConfig {
        socket,
        audit_path,
        key_path,
        session_dir,
        token_ttl,
    } = config;

    let sessions = SessionManager::new(key_path.as_deref(), session_dir, token_ttl)?;
    info!(
        key = %hex::encode(sessions.signing_public_key()),
        "session signing key ready"
    );
    let audit = AuditLog::open(&audit_path)?;
    info!(path = %audit_path.display(), "audit chain opened");

    Ok(Arc::new(Ctx {
        sessions: Mutex::new(sessions),
        audit: Mutex::new(audit),
        policy: Policy::from_env(),
        guard: EnvGuard::from_env(),
        ttl: token_ttl.clamp(60, lutra_proto::MAX_TOKEN_TTL_SECS),
        started: Instant::now(),
        audit_path,
        socket,
    }))
}

#[cfg(unix)]
pub fn run(config: ServerConfig) -> anyhow::Result<()> {
    let ctx = build(config)?;
    unix_listener::serve(Arc::clone(&ctx), &ctx.socket)
}

#[cfg(not(unix))]
pub fn run(config: ServerConfig) -> anyhow::Result<()> {
    let _ = build(config)?;
    anyhow::bail!("lutrad requires Unix domain sockets; this host is not supported")
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Handle one parsed request. Returns `None` for notifications.
pub fn dispatch(ctx: &Arc<Ctx>, request: Request) -> Option<Response> {
    let id = request.id.clone();
    let is_notification = id.is_none();
    let result = handle(ctx, &request.method, request.params);
    if is_notification {
        if let Err(error) = &result {
            warn!(method = %request.method, error = %error, "notification failed");
        }
        return None;
    }
    Some(match result {
        Ok(value) => Response::ok(id, value),
        Err(error) => Response::error(id, error),
    })
}

fn handle(ctx: &Ctx, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
    match method {
        "health" => Ok(json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": lutra_proto::PROTOCOL_VERSION,
            "uptime_secs": ctx.started.elapsed().as_secs(),
            "audit_log": ctx.audit_path.display().to_string(),
            "guard": "stub",
            "default_ttl_secs": ctx.ttl,
        })),
        "session.create" => session_create(ctx, parse_params(params)?),
        "session.revoke" => session_revoke(ctx, parse_params(params)?),
        "session.list" => Ok(json!({ "sessions": lock(&ctx.sessions).list(now_unix()) })),
        "session.verify" => session_verify(ctx, parse_params(params)?),
        "guard.authorize" => guard_authorize(ctx, parse_params(params)?),
        "policy.explain" => policy_explain(ctx, parse_params(params)?),
        "tool.call" => tool_call(ctx, parse_params(params)?),
        other => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("unknown method `{other}`"),
        )),
    }
}

fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T, RpcError> {
    serde_json::from_value(params.unwrap_or(Value::Null))
        .map_err(|error| RpcError::invalid_params(format!("invalid params: {error}")))
}

// ---------------------------------------------------------------------------
// Session methods
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateParams {
    subject: String,
    #[serde(default)]
    caps: Vec<Capability>,
    #[serde(default)]
    ttl_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RevokeParams {
    session: String,
}

#[derive(Debug, Deserialize)]
struct VerifyParams {
    token: String,
}

#[derive(Debug, Deserialize)]
struct AuthorizeParams {
    token: String,
    #[serde(default)]
    caps: Vec<Capability>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExplainParams {
    tool: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    token: String,
    tool: String,
    #[serde(default)]
    arguments: Value,
}

fn session_create(ctx: &Ctx, params: CreateParams) -> Result<Value, RpcError> {
    let subject = params.subject.trim().to_string();
    if subject.is_empty() || subject.len() > 128 {
        return Err(RpcError::invalid_params(
            "`subject` must be 1..=128 visible characters",
        ));
    }

    let approval = ctx.guard.approve(&subject, &params.caps);
    if !approval.granted {
        ctx.record(
            "-",
            &subject,
            "session.create",
            &params.caps,
            Decision::Deny,
            &approval.reason,
        );
        return Err(RpcError::new(codes::PERMISSION_DENIED, approval.reason));
    }

    let now = now_unix();
    let (info, claims, token) = {
        let mut sessions = lock(&ctx.sessions);
        sessions
            .create(&subject, &params.caps, params.ttl_secs, now)
            .map_err(|error| RpcError::internal(format!("cannot create session: {error}")))?
    };

    ctx.record(
        &info.session,
        &info.subject,
        "session.create",
        &info.caps,
        Decision::Allow,
        &format!(
            "token issued, ttl={}s, nonce={}, guard={}",
            claims.exp.saturating_sub(claims.iat),
            claims.nonce,
            approval.reason
        ),
    );

    Ok(json!({
        "session": info.session,
        "subject": info.subject,
        "caps": info.caps,
        "token": token,
        "issued_at": claims.iat,
        "expires_at": claims.exp,
        "nonce": claims.nonce,
    }))
}

fn session_revoke(ctx: &Ctx, params: RevokeParams) -> Result<Value, RpcError> {
    let revoked = {
        let mut sessions = lock(&ctx.sessions);
        sessions.revoke(&params.session)
    };
    if !revoked {
        return Err(RpcError::new(
            codes::SESSION_INVALID,
            format!("unknown session `{}`", params.session),
        ));
    }
    ctx.record(
        &params.session,
        "-",
        "session.revoke",
        &[],
        Decision::Allow,
        "session revoked by client",
    );
    Ok(json!({ "session": params.session, "revoked": true }))
}

fn session_verify(ctx: &Ctx, params: VerifyParams) -> Result<Value, RpcError> {
    let claims = {
        let sessions = lock(&ctx.sessions);
        sessions.verify(&params.token, now_unix())?
    };
    Ok(json!({
        "valid": true,
        "session": claims.session,
        "subject": claims.subject,
        "caps": claims.caps,
        "issued_at": claims.iat,
        "expires_at": claims.exp,
    }))
}

fn guard_authorize(ctx: &Ctx, params: AuthorizeParams) -> Result<Value, RpcError> {
    let claims = {
        let sessions = lock(&ctx.sessions);
        sessions.verify(&params.token, now_unix())?
    };

    let approval = ctx.guard.approve(&claims.subject, &params.caps);
    if !approval.granted {
        ctx.record(
            &claims.session,
            &claims.subject,
            "guard.authorize",
            &params.caps,
            Decision::Deny,
            &approval.reason,
        );
        return Err(RpcError::new(codes::PERMISSION_DENIED, approval.reason));
    }

    let (token, new_claims) = {
        let mut sessions = lock(&ctx.sessions);
        sessions.escalate(&params.token, &params.caps, now_unix())?
    };

    let note = params.reason.unwrap_or_default();
    ctx.record(
        &new_claims.session,
        &new_claims.subject,
        "guard.authorize",
        &params.caps,
        Decision::Allow,
        &format!(
            "escalated by guard ({}){}",
            approval.reason,
            if note.is_empty() {
                String::new()
            } else {
                format!(", client note: {note}")
            }
        ),
    );

    Ok(json!({
        "token": token,
        "caps": new_claims.caps,
        "expires_at": new_claims.exp,
    }))
}

fn policy_explain(ctx: &Ctx, params: ExplainParams) -> Result<Value, RpcError> {
    match ctx.policy.evaluate(&params.tool, &params.arguments) {
        Ok(plan) => Ok(json!({
            "tool": params.tool,
            "caps": plan.caps,
            "summary": plan.summary,
        })),
        Err(violation) => Err(violation.into_rpc()),
    }
}

// ---------------------------------------------------------------------------
// Tool calls
// ---------------------------------------------------------------------------

fn tool_call(ctx: &Ctx, params: ToolCallParams) -> Result<Value, RpcError> {
    let claims = {
        let sessions = lock(&ctx.sessions);
        sessions.verify(&params.token, now_unix())?
    };
    let arguments = if params.arguments.is_null() {
        json!({})
    } else {
        params.arguments
    };

    let plan = match ctx.policy.evaluate(&params.tool, &arguments) {
        Ok(plan) => plan,
        Err(violation) => {
            ctx.record(
                &claims.session,
                &claims.subject,
                &params.tool,
                &[],
                Decision::Deny,
                &violation.reason,
            );
            return Err(violation.into_rpc());
        }
    };

    let missing = claims.missing(&plan.caps);
    if !missing.is_empty() {
        let reason = format!(
            "session lacks {}; call guard.authorize to request it",
            join_caps(&missing)
        );
        ctx.record(
            &claims.session,
            &claims.subject,
            &params.tool,
            &plan.caps,
            Decision::Deny,
            &reason,
        );
        return Err(
            RpcError::new(codes::PERMISSION_DENIED, reason).with_data(json!({
                "missing": missing,
                "required": plan.caps,
            })),
        );
    }

    match execute(ctx, &params.tool, &arguments) {
        Ok(result) => {
            ctx.record(
                &claims.session,
                &claims.subject,
                &params.tool,
                &plan.caps,
                Decision::Allow,
                &format!("ok: {}", plan.summary),
            );
            Ok(result)
        }
        Err(error) => {
            ctx.record(
                &claims.session,
                &claims.subject,
                &params.tool,
                &plan.caps,
                Decision::Allow,
                &format!("error: {}", error.message),
            );
            Err(error)
        }
    }
}

fn join_caps(caps: &[Capability]) -> String {
    caps.iter()
        .map(|cap| cap.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn execute(ctx: &Ctx, tool: &str, args: &Value) -> Result<Value, RpcError> {
    match tool {
        tools::FS_READ => fs_read(args),
        tools::FS_WRITE => fs_write(args),
        tools::EXEC => exec(args),
        tools::NET_FETCH => net_fetch(args),
        tools::PKG_INSTALL => pkg_install(args),
        tools::STATUS => Ok(status(ctx)),
        other => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("unknown tool `{other}`"),
        )),
    }
}

// -- filesystem -------------------------------------------------------------

fn fs_read(args: &Value) -> Result<Value, RpcError> {
    let path = arg_path(args)?;
    let metadata = std::fs::symlink_metadata(&path).map_err(io_error)?;
    if metadata.file_type().is_symlink() {
        return Err(RpcError::new(
            codes::POLICY_DENIED,
            format!("refusing to read through symlink {}", path.display()),
        ));
    }
    if !metadata.is_file() {
        return Err(RpcError::new(
            codes::INVALID_PARAMS,
            format!("{} is not a regular file", path.display()),
        ));
    }

    let file = std::fs::File::open(&path).map_err(io_error)?;
    let mut buffer = Vec::new();
    file.take(MAX_READ_BYTES)
        .read_to_end(&mut buffer)
        .map_err(io_error)?;
    let truncated = metadata.len() > MAX_READ_BYTES;

    let (content, encoding) = match String::from_utf8(buffer.clone()) {
        Ok(text) => (Some(text), "utf-8"),
        Err(_) => (None, "binary"),
    };

    Ok(json!({
        "path": path.display().to_string(),
        "size": metadata.len(),
        "truncated": truncated,
        "sha256": sha256_hex(&buffer),
        "encoding": encoding,
        "content": content,
    }))
}

fn fs_write(args: &Value) -> Result<Value, RpcError> {
    let path = arg_path(args)?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing string argument `content`"))?;
    if content.len() > MAX_WRITE_BYTES {
        return Err(RpcError::invalid_params("content exceeds 32 MiB"));
    }
    let append = args.get("append").and_then(Value::as_bool).unwrap_or(false);

    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() {
            return Err(RpcError::new(
                codes::POLICY_DENIED,
                format!("refusing to write through symlink {}", path.display()),
            ));
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_error)?;
    }

    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    let mut file = options.open(&path).map_err(io_error)?;
    file.write_all(content.as_bytes()).map_err(io_error)?;
    file.sync_all().ok();

    Ok(json!({
        "path": path.display().to_string(),
        "bytes_written": content.len(),
        "append": append,
        "sha256": sha256_hex(content.as_bytes()),
    }))
}

// -- process execution ------------------------------------------------------

fn exec(args: &Value) -> Result<Value, RpcError> {
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing string argument `command`"))?;
    let argv: Vec<String> = match args.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    RpcError::invalid_params("`args` must contain only strings")
                })
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(RpcError::invalid_params("`args` must be an array")),
    };
    let timeout_secs = args
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 300);

    let mut process = Command::new(command);
    process
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = args.get("cwd").and_then(Value::as_str) {
        let dir = PathBuf::from(cwd);
        if !dir.is_absolute() {
            return Err(RpcError::invalid_params("`cwd` must be absolute"));
        }
        process.current_dir(dir);
    }

    let mut child = process.spawn().map_err(|error| {
        RpcError::new(
            codes::EXEC_FAILED,
            format!("cannot spawn `{command}`: {error}"),
        )
    })?;
    let stdout_handle = child
        .stdout
        .take()
        .map(|pipe| std::thread::spawn(move || read_limited(pipe, MAX_EXEC_OUTPUT)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|pipe| std::thread::spawn(move || read_limited(pipe, MAX_EXEC_OUTPUT)));

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().map_err(|error| {
                        RpcError::new(codes::EXEC_FAILED, format!("wait failed: {error}"))
                    })?;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(RpcError::new(
                    codes::EXEC_FAILED,
                    format!("wait failed: {error}"),
                ))
            }
        }
    };

    let stdout = stdout_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();

    Ok(json!({
        "command": command,
        "args": argv,
        "exit_code": status.code(),
        "timed_out": timed_out,
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
    }))
}

// -- network ----------------------------------------------------------------

fn net_fetch(args: &Value) -> Result<Value, RpcError> {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing string argument `url`"))?;
    let target = policy::parse_url(url).map_err(|violation| violation.into_rpc())?;

    if target.scheme == "https" {
        return Err(RpcError::new(
            codes::NOT_IMPLEMENTED,
            "TLS is not implemented in this build; only http:// fetches are supported",
        ));
    }

    let addresses = (target.host.as_str(), target.port)
        .to_socket_addrs()
        .map_err(|error| {
            RpcError::new(
                codes::EXEC_FAILED,
                format!("cannot resolve {}: {error}", target.host),
            )
        })?;

    let mut stream = None;
    for address in addresses {
        if !policy::is_public_ip(address.ip()) {
            return Err(RpcError::new(
                codes::POLICY_DENIED,
                format!(
                    "{} resolves to {} which is not a public address",
                    target.host,
                    address.ip()
                ),
            ));
        }
        if let Ok(candidate) =
            TcpStream::connect_timeout(&address, Duration::from_secs(5))
        {
            stream = Some(candidate);
            break;
        }
    }
    let mut stream = stream.ok_or_else(|| {
        RpcError::new(
            codes::EXEC_FAILED,
            format!("cannot connect to {}:{}", target.host, target.port),
        )
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .ok();

    let host_header = if target.port == 80 {
        target.host.clone()
    } else {
        format!("{}:{}", target.host, target.port)
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: lutra-mcpd/{}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        target.path,
        host_header,
        env!("CARGO_PKG_VERSION")
    );
    stream.write_all(request.as_bytes()).map_err(io_error)?;
    stream.flush().ok();

    let mut response = Vec::new();
    stream
        .take(MAX_FETCH_BYTES)
        .read_to_end(&mut response)
        .map_err(io_error)?;
    let text = String::from_utf8_lossy(&response).to_string();
    let (head, body) = split_http_response(&text);
    let status_line = head.lines().next().unwrap_or_default().to_string();

    Ok(json!({
        "url": url,
        "status_line": status_line,
        "headers": head,
        "body": body,
        "truncated": response.len() as u64 >= MAX_FETCH_BYTES,
        "tls": false,
    }))
}

fn split_http_response(text: &str) -> (String, String) {
    if let Some(index) = text.find("\r\n\r\n") {
        (text[..index].to_string(), text[index + 4..].to_string())
    } else if let Some(index) = text.find("\n\n") {
        (text[..index].to_string(), text[index + 2..].to_string())
    } else {
        (text.to_string(), String::new())
    }
}

// -- packages ---------------------------------------------------------------

fn pkg_install(args: &Value) -> Result<Value, RpcError> {
    let Some(raw_path) = args.get("path").and_then(Value::as_str) else {
        return Err(RpcError::new(
            codes::NOT_IMPLEMENTED,
            "installing by package id needs the Lutra Store; pass a local .ltr `path` instead",
        ));
    };
    let package = policy::normalize_path(raw_path).map_err(|violation| violation.into_rpc())?;
    let ltr = std::env::var("LUTRA_LTR_BIN").unwrap_or_else(|_| "/usr/bin/ltr".to_string());

    let output = Command::new(&ltr)
        .arg("install")
        .arg(&package)
        .output()
        .map_err(|error| {
            RpcError::new(
                codes::EXEC_FAILED,
                format!("cannot run `{ltr} install`: {error}"),
            )
        })?;

    Ok(json!({
        "ltr": ltr,
        "package": package.display().to_string(),
        "exit_code": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
    }))
}

// -- status -----------------------------------------------------------------

fn status(ctx: &Ctx) -> Value {
    let sessions = lock(&ctx.sessions).list(now_unix());
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": lutra_proto::PROTOCOL_VERSION,
        "uptime_secs": ctx.started.elapsed().as_secs(),
        "audit_log": ctx.audit_path.display().to_string(),
        "socket": ctx.socket.display().to_string(),
        "default_ttl_secs": ctx.ttl,
        "guard": "stub",
        "sessions": sessions,
        "tools": tools::ALL,
    })
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn arg_path(args: &Value) -> Result<PathBuf, RpcError> {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing string argument `path`"))?;
    policy::normalize_path(raw).map_err(|violation| violation.into_rpc())
}

fn io_error(error: std::io::Error) -> RpcError {
    RpcError::new(codes::IO_ERROR, error.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn read_limited<R: Read>(mut reader: R, limit: u64) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = reader.by_ref().take(limit).read_to_end(&mut buffer);
    buffer
}

// ---------------------------------------------------------------------------
// Unix socket front end
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod unix_listener {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use tracing::{info, warn};

    pub fn serve(ctx: Arc<Ctx>, socket: &Path) -> anyhow::Result<()> {
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o750));
        }
        match std::fs::remove_file(socket) {
            Ok(()) => info!(path = %socket.display(), "removed stale socket"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let listener = UnixListener::bind(socket)?;
        let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660));
        info!(path = %socket.display(), "lutrad listening");

        for incoming in listener.incoming() {
            match incoming {
                Ok(stream) => {
                    let ctx = Arc::clone(&ctx);
                    std::thread::spawn(move || {
                        if let Err(error) = handle_connection(stream, ctx) {
                            warn!(error = %error, "connection failed");
                        }
                    });
                }
                Err(error) => warn!(error = %error, "accept failed"),
            }
        }
        Ok(())
    }

    fn handle_connection(stream: UnixStream, ctx: Arc<Ctx>) -> anyhow::Result<()> {
        let reader = BufReader::new(stream.try_clone()?);
        let mut writer = std::io::BufWriter::new(stream);

        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let request: Request = match serde_json::from_str(trimmed) {
                Ok(request) => request,
                Err(error) => {
                    let response = Response::error(
                        None,
                        RpcError::new(codes::PARSE_ERROR, format!("invalid JSON: {error}")),
                    );
                    write_response(&mut writer, &response)?;
                    continue;
                }
            };

            if let Some(response) = super::dispatch(&ctx, request) {
                write_response(&mut writer, &response)?;
            }
        }
        Ok(())
    }

    fn write_response<W: Write>(writer: &mut W, response: &Response) -> anyhow::Result<()> {
        serde_json::to_writer(&mut *writer, response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_http_response_handles_crlf_and_missing_body() {
        let (head, body) = split_http_response("HTTP/1.1 200 OK\r\nA: b\r\n\r\nhello");
        assert!(head.starts_with("HTTP/1.1 200"));
        assert_eq!(body, "hello");

        let (head, body) = split_http_response("HTTP/1.1 204 No Content");
        assert_eq!(body, "");
        assert_eq!(head, "HTTP/1.1 204 No Content");
    }

    #[test]
    fn read_limited_stops_at_the_limit() {
        let data = vec![b'a'; 4096];
        assert_eq!(read_limited(data.as_slice(), 1024).len(), 1024);
    }
}
