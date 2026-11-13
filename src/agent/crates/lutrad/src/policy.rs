//! Capability policy for the Lutra agent daemon.
//!
//! Two layers of checking happen here:
//!
//! 1. **Target validation** — reject paths, commands, hosts and package ids that
//!    are unsafe regardless of how many capabilities a token carries
//!    (`/etc/shadow`, `rm`, `127.0.0.1`, `../../`, ...).
//! 2. **Capability derivation** — decide which capabilities a specific
//!    invocation needs. Reading inside the home directory only needs
//!    `fs.read`; reading `/etc` additionally needs `fs.system`.
//!
//! The normalisation helpers are pure functions so the policy can be unit
//! tested without touching the filesystem. The executor re-checks symlinks
//! before touching disk (see `server.rs`).

use lutra_proto::{codes, tools, Capability, Danger, RpcError};
use serde_json::Value;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

/// Longest accepted string argument (paths, urls, commands).
pub const MAX_ARG_LEN: usize = 4096;

/// Paths that may never be read, whatever capabilities a token holds.
const PROTECTED_READ: &[&str] = &[
    "/etc/shadow",
    "/etc/gshadow",
    "/etc/sudoers",
    "/etc/sudoers.d",
    "/etc/ssh/ssh_host_",
    "/root",
    "/proc/kcore",
    "/dev/mem",
    "/dev/kmem",
    "/sys/kernel/security",
];

/// System paths that may never be written by the agent, even with `fs.system`.
/// Packages and humans change these; agents do not.
const PROTECTED_WRITE: &[&str] = &[
    "/boot",
    "/etc",
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/proc",
    "/sys",
    "/dev",
    "/root",
    "/opt/ltr",
    "/var/lib/lutra",
    "/var/log/lutra",
    "/run/lutra",
];

/// Paths inside `$HOME` that grant persistence if written by an agent.
const HOME_PROTECTED: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".bashrc",
    ".profile",
    ".zshrc",
    ".config/autostart",
    ".config/systemd",
    ".config/environment.d",
    ".local/share/systemd",
];

/// Directories the agent may execute binaries from when an absolute path is used.
const EXEC_ROOTS: &[&str] = &[
    "/usr/bin",
    "/bin",
    "/usr/local/bin",
    "/usr/sbin",
    "/sbin",
    "/opt/ltr",
];

/// Commands that are refused outright: shells, privilege tools and anything that
/// can trivially destroy a system. The list is a safety net, not a sandbox.
const EXEC_DENY: &[&str] = &[
    "sh", "bash", "zsh", "dash", "fish", "csh", "ksh", "tcsh", "busybox", "su", "sudo", "doas",
    "pkexec", "rm", "rmdir", "dd", "mkfs", "mkswap", "mount", "umount", "chroot", "chown",
    "chmod", "insmod", "modprobe", "sysctl", "iptables", "nft", "shutdown", "reboot", "poweroff",
    "init", "kill", "killall", "pkill", "useradd", "usermod", "userdel", "passwd", "visudo",
    "dpkg", "apt", "apt-get", "pacman", "wipefs", "fdisk", "parted", "losetup", "cryptsetup",
];

/// Interpreters and generic launchers: allowed, but they additionally require
/// `sys`, because their behaviour cannot be inferred from the command name.
const EXEC_NEEDS_SYS: &[&str] = &[
    "python", "python3", "perl", "ruby", "node", "deno", "bun", "php", "lua", "awk", "gawk",
    "xargs", "env", "nohup",
];

#[derive(Debug, Clone)]
pub struct ToolPlan {
    pub caps: Vec<Capability>,
    pub summary: String,
}

/// A rejection produced by the policy layer.
#[derive(Debug, Clone)]
pub struct PolicyViolation {
    pub cap: Option<Capability>,
    pub code: i32,
    pub reason: String,
}

impl PolicyViolation {
    fn new(code: i32, reason: impl Into<String>) -> Self {
        Self {
            cap: None,
            code,
            reason: reason.into(),
        }
    }

    fn denied(cap: Capability, reason: impl Into<String>) -> Self {
        Self {
            cap: Some(cap),
            code: codes::POLICY_DENIED,
            reason: reason.into(),
        }
    }

    pub fn into_rpc(self) -> RpcError {
        let mut error = RpcError::new(self.code, self.reason);
        if let Some(cap) = self.cap {
            error = error.with_data(serde_json::json!({
                "capability": cap,
                "policy": "deny",
            }));
        }
        error
    }
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub home: PathBuf,
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    /// When non-empty, bare command names must appear in this list.
    pub exec_allow: Vec<String>,
    pub exec_deny: Vec<String>,
    /// When non-empty, hosts must match one of these suffixes.
    pub net_host_allow: Vec<String>,
    pub net_host_deny: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self::with_home(default_home())
    }
}

pub fn default_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/home/lutra"))
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

impl Policy {
    pub fn with_home(home: PathBuf) -> Self {
        Self {
            read_roots: vec![
                home.clone(),
                PathBuf::from("/tmp"),
                PathBuf::from("/var/tmp"),
            ],
            write_roots: vec![
                home.clone(),
                PathBuf::from("/tmp"),
                PathBuf::from("/var/tmp"),
            ],
            home,
            exec_allow: Vec::new(),
            exec_deny: EXEC_DENY.iter().map(|s| s.to_string()).collect(),
            net_host_allow: Vec::new(),
            net_host_deny: Vec::new(),
        }
    }

    /// Build from `$HOME` plus optional `/etc/lutra` style env overrides
    /// (`LUTRA_EXEC_ALLOW`, `LUTRA_NET_ALLOW_HOSTS`, `LUTRA_NET_DENY_HOSTS`).
    pub fn from_env() -> Self {
        let mut policy = Self::with_home(default_home());
        if let Ok(value) = std::env::var("LUTRA_EXEC_ALLOW") {
            policy.exec_allow = split_list(&value);
        }
        if let Ok(value) = std::env::var("LUTRA_NET_ALLOW_HOSTS") {
            policy.net_host_allow = split_list(&value);
        }
        if let Ok(value) = std::env::var("LUTRA_NET_DENY_HOSTS") {
            policy.net_host_deny.extend(split_list(&value));
        }
        policy
    }

    /// Map a tool call to its capabilities, or reject it.
    pub fn evaluate(&self, tool: &str, args: &Value) -> Result<ToolPlan, PolicyViolation> {
        match tool {
            tools::FS_READ => self.fs_read(args),
            tools::FS_WRITE => self.fs_write(args),
            tools::EXEC => self.exec(args),
            tools::NET_FETCH => self.net_fetch(args),
            tools::PKG_INSTALL => self.pkg_install(args),
            tools::STATUS => Ok(ToolPlan {
                caps: Vec::new(),
                summary: "daemon status".to_string(),
            }),
            other => Err(PolicyViolation::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown tool `{other}`"),
            )),
        }
    }

    // -- filesystem ---------------------------------------------------------

    fn fs_read(&self, args: &Value) -> Result<ToolPlan, PolicyViolation> {
        let path = normalize_path(str_arg(args, "path")?)?;
        self.check_protected_read(&path)?;

        let mut caps = vec![Capability::FsRead];
        if !in_roots(&path, &self.read_roots) {
            caps.push(Capability::FsSystem);
        }
        Ok(ToolPlan {
            caps,
            summary: format!("read {}", path.display()),
        })
    }

    fn fs_write(&self, args: &Value) -> Result<ToolPlan, PolicyViolation> {
        let path = normalize_path(str_arg(args, "path")?)?;

        for protected in PROTECTED_WRITE {
            if path.starts_with(protected) {
                return Err(PolicyViolation::denied(
                    Capability::FsWrite,
                    format!("writing {} is refused by policy", protected),
                ));
            }
        }
        for relative in HOME_PROTECTED {
            if path.starts_with(self.home.join(relative)) {
                return Err(PolicyViolation::denied(
                    Capability::FsWrite,
                    format!(
                        "writing {} is refused: it controls login or autostart behaviour",
                        path.display()
                    ),
                ));
            }
        }

        let mut caps = vec![Capability::FsWrite];
        if !in_roots(&path, &self.write_roots) {
            caps.push(Capability::FsSystem);
        }
        Ok(ToolPlan {
            caps,
            summary: format!("write {}", path.display()),
        })
    }

    fn check_protected_read(&self, path: &Path) -> Result<(), PolicyViolation> {
        for protected in PROTECTED_READ {
            if path.starts_with(protected) {
                return Err(PolicyViolation::denied(
                    Capability::FsRead,
                    format!("reading {} is refused by policy", path.display()),
                ));
            }
        }
        Ok(())
    }

    // -- process execution --------------------------------------------------

    fn exec(&self, args: &Value) -> Result<ToolPlan, PolicyViolation> {
        let command = str_arg(args, "command")?;
        if command.chars().any(char::is_whitespace) {
            return Err(PolicyViolation::new(
                codes::INVALID_PARAMS,
                "`command` must be a single executable name, put arguments in `args`",
            ));
        }

        let path = Path::new(command);
        let base = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| PolicyViolation::new(codes::INVALID_PARAMS, "invalid command name"))?
            .to_ascii_lowercase();

        if self.exec_deny.iter().any(|denied| denied == &base) {
            return Err(PolicyViolation::denied(
                Capability::Exec,
                format!("command `{base}` is on the exec denylist"),
            ));
        }
        if !self.exec_allow.is_empty() && !self.exec_allow.iter().any(|allowed| allowed == &base) {
            return Err(PolicyViolation::denied(
                Capability::Exec,
                format!("command `{base}` is not in the exec allowlist"),
            ));
        }

        if command.contains('/') {
            let resolved = normalize_path(command)?;
            if !EXEC_ROOTS.iter().any(|root| resolved.starts_with(root)) {
                return Err(PolicyViolation::denied(
                    Capability::Exec,
                    format!(
                        "binary {} is outside the executable roots ({})",
                        resolved.display(),
                        EXEC_ROOTS.join(", ")
                    ),
                ));
            }
        }

        if let Some(argv) = args.get("args") {
            let argv = argv.as_array().ok_or_else(|| {
                PolicyViolation::new(codes::INVALID_PARAMS, "`args` must be an array of strings")
            })?;
            for item in argv {
                let value = item.as_str().ok_or_else(|| {
                    PolicyViolation::new(codes::INVALID_PARAMS, "`args` must contain only strings")
                })?;
                if value.len() > MAX_ARG_LEN {
                    return Err(PolicyViolation::new(
                        codes::INVALID_PARAMS,
                        "exec argument is too long",
                    ));
                }
                if value.contains('\0') || value == "--no-preserve-root" {
                    return Err(PolicyViolation::denied(
                        Capability::Exec,
                        "refused exec argument",
                    ));
                }
            }
        }

        let mut caps = vec![Capability::Exec];
        if EXEC_NEEDS_SYS.iter().any(|name| base.starts_with(*name))
            || command.starts_with("/usr/sbin/")
            || command.starts_with("/sbin/")
        {
            caps.push(Capability::Sys);
        }

        if let Some(cwd) = args.get("cwd").and_then(Value::as_str) {
            let dir = normalize_path(cwd)?;
            if !in_roots(&dir, &self.read_roots) {
                caps.push(Capability::FsSystem);
            }
        }

        Ok(ToolPlan {
            caps,
            summary: format!("exec {command}"),
        })
    }

    // -- network ------------------------------------------------------------

    fn net_fetch(&self, args: &Value) -> Result<ToolPlan, PolicyViolation> {
        let url = str_arg(args, "url")?;
        let target = parse_url(url)?;
        let host = target.host.to_ascii_lowercase();

        if host.is_empty() {
            return Err(PolicyViolation::new(codes::INVALID_PARAMS, "url has no host"));
        }
        if is_blocked_hostname(&host) {
            return Err(PolicyViolation::denied(
                Capability::Net,
                format!("host `{host}` is blocked (local/metadata names)"),
            ));
        }
        if let Ok(ip) = host.parse::<IpAddr>() {
            if !is_public_ip(ip) {
                return Err(PolicyViolation::denied(
                    Capability::Net,
                    format!("address {ip} is not a public unicast address"),
                ));
            }
        }
        if self
            .net_host_deny
            .iter()
            .any(|pattern| host_matches(&host, pattern))
        {
            return Err(PolicyViolation::denied(
                Capability::Net,
                format!("host `{host}` is on the net denylist"),
            ));
        }
        if !self.net_host_allow.is_empty()
            && !self
                .net_host_allow
                .iter()
                .any(|pattern| host_matches(&host, pattern))
        {
            return Err(PolicyViolation::denied(
                Capability::Net,
                format!("host `{host}` is not in the net allowlist"),
            ));
        }

        Ok(ToolPlan {
            caps: vec![Capability::Net],
            summary: format!("fetch {}", target.redacted()),
        })
    }

    // -- packages -----------------------------------------------------------

    fn pkg_install(&self, args: &Value) -> Result<ToolPlan, PolicyViolation> {
        if let Some(id) = args.get("id").and_then(Value::as_str) {
            validate_package_id(id)?;
            return Ok(ToolPlan {
                caps: vec![Capability::Pkg, Capability::Net],
                summary: format!("install package {id} from the store"),
            });
        }
        if let Some(raw) = args.get("path").and_then(Value::as_str) {
            let path = normalize_path(raw)?;
            if path.extension().and_then(|e| e.to_str()) != Some("ltr") {
                return Err(PolicyViolation::new(
                    codes::INVALID_PARAMS,
                    "package path must end with .ltr",
                ));
            }
            let allowed = self.home.clone();
            let local_dirs = [
                allowed,
                PathBuf::from("/tmp"),
                PathBuf::from("/var/tmp"),
                PathBuf::from("/var/lib/lutra/store"),
            ];
            if !in_roots(&path, &local_dirs) {
                return Err(PolicyViolation::denied(
                    Capability::Pkg,
                    format!(
                        "package {} must come from the home directory, /tmp or the store cache",
                        path.display()
                    ),
                ));
            }
            return Ok(ToolPlan {
                caps: vec![Capability::Pkg, Capability::FsRead],
                summary: format!("install local package {}", path.display()),
            });
        }
        Err(PolicyViolation::new(
            codes::INVALID_PARAMS,
            "lutra_pkg_install requires either `id` or `path`",
        ))
    }
}

// ---------------------------------------------------------------------------
// Guard stub
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Approval {
    pub granted: bool,
    pub reason: String,
}

/// Confirmation backend. The real implementation talks D-Bus to `lutra-guard`;
/// this build ships an environment-driven stub so the daemon is usable headless.
pub trait Approver: Send + Sync {
    fn approve(&self, subject: &str, caps: &[Capability]) -> Approval;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardMode {
    /// Approve only low-risk capabilities plus an explicit `LUTRA_GUARD_APPROVE` list.
    Env,
    /// Approve everything (development only, logged as such).
    AllowAll,
    DenyAll,
}

#[derive(Debug, Clone)]
pub struct EnvGuard {
    pub mode: GuardMode,
    pub approved: Vec<Capability>,
}

impl EnvGuard {
    pub fn from_env() -> Self {
        let mode = match std::env::var("LUTRA_GUARD_MODE").ok().as_deref() {
            Some("allow") => GuardMode::AllowAll,
            Some("deny") => GuardMode::DenyAll,
            _ => GuardMode::Env,
        };
        let approved = std::env::var("LUTRA_GUARD_APPROVE")
            .unwrap_or_default()
            .split(',')
            .filter_map(|item| Capability::parse(item.trim()))
            .collect();
        Self { mode, approved }
    }
}

impl Approver for EnvGuard {
    fn approve(&self, subject: &str, caps: &[Capability]) -> Approval {
        match self.mode {
            GuardMode::AllowAll => Approval {
                granted: true,
                reason: "guard stub: LUTRA_GUARD_MODE=allow (development only)".to_string(),
            },
            GuardMode::DenyAll => Approval {
                granted: false,
                reason: "guard stub: LUTRA_GUARD_MODE=deny".to_string(),
            },
            GuardMode::Env => {
                let missing: Vec<&Capability> = caps
                    .iter()
                    .filter(|cap| cap.danger() != Danger::Low && !self.approved.contains(cap))
                    .collect();
                if missing.is_empty() {
                    Approval {
                        granted: true,
                        reason: format!("guard stub: approved for {subject}"),
                    }
                } else {
                    let names: Vec<&str> = missing.iter().map(|cap| cap.as_str()).collect();
                    Approval {
                        granted: false,
                        reason: format!(
                            "lutra-guard UI is not available in this build; \
                             missing approval for {} (development: set LUTRA_GUARD_APPROVE={})",
                            names.join(", "),
                            names.join(",")
                        ),
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Normalise an absolute path lexically. `..` is always rejected rather than
/// resolved, so a policy decision can never be widened by traversal.
pub fn normalize_path(raw: &str) -> Result<PathBuf, PolicyViolation> {
    if raw.len() > MAX_ARG_LEN {
        return Err(PolicyViolation::new(
            codes::INVALID_PARAMS,
            "path is too long",
        ));
    }
    if !raw.starts_with('/') {
        return Err(PolicyViolation::new(
            codes::INVALID_PARAMS,
            format!("path `{raw}` must be absolute"),
        ));
    }

    let mut normalized = PathBuf::from("/");
    for segment in raw.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                return Err(PolicyViolation::denied(
                    Capability::FsRead,
                    format!("path traversal is not allowed: {raw}"),
                ))
            }
            other => {
                if other.contains('\0') {
                    return Err(PolicyViolation::new(
                        codes::INVALID_PARAMS,
                        "path contains a NUL byte",
                    ));
                }
                normalized.push(other);
            }
        }
    }
    Ok(normalized)
}

fn in_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, PolicyViolation> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            PolicyViolation::new(
                codes::INVALID_PARAMS,
                format!("missing string argument `{key}`"),
            )
        })
}

pub fn validate_package_id(id: &str) -> Result<(), PolicyViolation> {
    if id.len() < 3 || id.len() > 128 {
        return Err(PolicyViolation::new(
            codes::INVALID_PARAMS,
            "package id length must be 3..=128",
        ));
    }
    if !id.contains('.') {
        return Err(PolicyViolation::new(
            codes::INVALID_PARAMS,
            "package id must be reverse-DNS, e.g. com.example.app",
        ));
    }
    for segment in id.split('.') {
        if segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(PolicyViolation::new(
                codes::INVALID_PARAMS,
                format!("invalid package id `{id}` (lowercase alphanumerics, '-' and '.' only)"),
            ));
        }
    }
    Ok(())
}

/// Parsed `http(s)://` URL, kept deliberately small (no query normalisation).
#[derive(Debug, Clone)]
pub struct UrlTarget {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl UrlTarget {
    fn redacted(&self) -> String {
        format!("{}://{}:{}{}", self.scheme, self.host, self.port, self.path)
    }
}

pub fn parse_url(raw: &str) -> Result<UrlTarget, PolicyViolation> {
    if raw.len() > MAX_ARG_LEN {
        return Err(PolicyViolation::new(codes::INVALID_PARAMS, "url is too long"));
    }
    let (scheme_raw, rest) = raw.split_once("://").ok_or_else(|| {
        PolicyViolation::new(codes::INVALID_PARAMS, "url must include a scheme")
    })?;
    let scheme = scheme_raw.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(PolicyViolation::denied(
            Capability::Net,
            format!("scheme `{scheme}` is not allowed (http/https only)"),
        ));
    }

    let (authority, mut path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if let Some(fragment) = path.find('#') {
        path = &path[..fragment];
    }
    if path.is_empty() {
        path = "/";
    }
    if path.len() > MAX_ARG_LEN
        || path
            .chars()
            .any(|c| c == '\r' || c == '\n' || c == ' ' || c == '\0')
    {
        return Err(PolicyViolation::denied(
            Capability::Net,
            "url path contains characters that would break HTTP framing",
        ));
    }

    // Strip userinfo; it must not influence host policy.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.is_empty() {
        return Err(PolicyViolation::new(codes::INVALID_PARAMS, "url has no host"));
    }

    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let (host, tail) = stripped.split_once(']').ok_or_else(|| {
            PolicyViolation::new(codes::INVALID_PARAMS, "unterminated IPv6 literal")
        })?;
        let port = match tail.strip_prefix(':') {
            Some(value) => parse_port(value, &scheme)?,
            None if tail.is_empty() => default_port(&scheme),
            None => {
                return Err(PolicyViolation::new(
                    codes::INVALID_PARAMS,
                    "malformed IPv6 authority",
                ))
            }
        };
        (host.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, value)) if !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()) => {
                (host.to_string(), parse_port(value, &scheme)?)
            }
            _ => (authority.to_string(), default_port(&scheme)),
        }
    };

    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
    {
        return Err(PolicyViolation::new(
            codes::INVALID_PARAMS,
            "url host contains invalid characters",
        ));
    }

    Ok(UrlTarget {
        scheme,
        host,
        port,
        path: path.to_string(),
    })
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" {
        443
    } else {
        80
    }
}

fn parse_port(value: &str, scheme: &str) -> Result<u16, PolicyViolation> {
    value.parse::<u16>().map_err(|_| {
        PolicyViolation::new(
            codes::INVALID_PARAMS,
            format!("invalid port `{value}` for {scheme}"),
        )
    })
}

fn is_blocked_hostname(host: &str) -> bool {
    matches!(
        host,
        "localhost" | "metadata" | "metadata.google.internal" | "instance-data"
    ) || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".home.arpa")
}

fn host_matches(host: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    let suffix = format!(".{pattern}");
    host == pattern || host.ends_with(suffix.as_str())
}

/// True for globally routable unicast addresses. Loopback, private, link-local,
/// CGNAT, documentation and multicast ranges are rejected so `net` cannot be
/// used to reach the host itself or cloud metadata services.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1])))
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_public_v4(mapped);
    }
    let segments = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (segments[0] & 0xffc0) == 0xfe80) // link local fe80::/10
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy() -> Policy {
        Policy::with_home(PathBuf::from("/home/tester"))
    }

    fn caps(result: Result<ToolPlan, PolicyViolation>) -> Vec<Capability> {
        result.expect("policy should allow").caps
    }

    fn deny(result: Result<ToolPlan, PolicyViolation>) -> String {
        result.expect_err("policy should deny").reason
    }

    #[test]
    fn read_inside_home_needs_only_fs_read() {
        let plan = policy()
            .evaluate(tools::FS_READ, &json!({"path": "/home/tester/notes.txt"}))
            .unwrap();
        assert_eq!(plan.caps, vec![Capability::FsRead]);
    }

    #[test]
    fn read_outside_home_requires_fs_system() {
        let plan = policy()
            .evaluate(tools::FS_READ, &json!({"path": "/etc/os-release"}))
            .unwrap();
        assert_eq!(plan.caps, vec![Capability::FsRead, Capability::FsSystem]);
    }

    #[test]
    fn protected_files_are_denied_even_with_fs_system() {
        let policy = policy();
        assert!(deny(policy.evaluate(
            tools::FS_READ,
            &json!({"path": "/etc/shadow"})
        ))
        .contains("refused"));
        assert!(deny(policy.evaluate(
            tools::FS_WRITE,
            &json!({"path": "/etc/sudoers.d/agent", "content": "x"})
        ))
        .contains("refused"));
    }

    #[test]
    fn traversal_is_rejected() {
        let err = deny(policy().evaluate(
            tools::FS_READ,
            &json!({"path": "/home/tester/../../etc/shadow"}),
        ));
        assert!(err.contains("traversal"));
    }

    #[test]
    fn relative_paths_are_rejected() {
        let err = deny(policy().evaluate(tools::FS_READ, &json!({"path": "notes.txt"})));
        assert!(err.contains("absolute"));
    }

    #[test]
    fn write_inside_home_and_refuses_system_paths() {
        let policy = policy();
        let plan = policy
            .evaluate(
                tools::FS_WRITE,
                &json!({"path": "/home/tester/doc.md", "content": "hi"}),
            )
            .unwrap();
        assert_eq!(plan.caps, vec![Capability::FsWrite]);

        assert!(deny(policy.evaluate(
            tools::FS_WRITE,
            &json!({"path": "/usr/bin/evil", "content": "hi"})
        ))
        .contains("refused"));
        assert!(deny(policy.evaluate(
            tools::FS_WRITE,
            &json!({"path": "/home/tester/.ssh/authorized_keys", "content": "key"})
        ))
        .contains("login"));
    }

    #[test]
    fn exec_denylist_and_allowlist() {
        let mut policy = policy();
        assert_eq!(
            caps(policy.evaluate(tools::EXEC, &json!({"command": "ls"}))),
            vec![Capability::Exec]
        );
        assert!(deny(policy.evaluate(tools::EXEC, &json!({"command": "bash"}))).contains("denylist"));
        assert!(deny(policy.evaluate(tools::EXEC, &json!({"command": "rm"}))).contains("denylist"));
        assert!(deny(policy.evaluate(
            tools::EXEC,
            &json!({"command": "/tmp/evil"})
        ))
        .contains("executable roots"));

        policy.exec_allow = vec!["ls".to_string()];
        assert!(deny(policy.evaluate(tools::EXEC, &json!({"command": "cat"}))).contains("allowlist"));
    }

    #[test]
    fn interpreters_require_sys() {
        let plan = policy()
            .evaluate(
                tools::EXEC,
                &json!({"command": "python3", "args": ["-c", "print(1)"]}),
            )
            .unwrap();
        assert_eq!(plan.caps, vec![Capability::Exec, Capability::Sys]);
    }

    #[test]
    fn exec_rejects_shell_operators_in_command_field() {
        let err = deny(policy().evaluate(
            tools::EXEC,
            &json!({"command": "ls -la"}),
        ));
        assert!(err.contains("single executable"));
        let err = deny(policy().evaluate(
            tools::EXEC,
            &json!({"command": "ls", "args": ["--no-preserve-root"]}),
        ));
        assert!(err.contains("refused"));
    }

    #[test]
    fn net_rejects_private_and_metadata_targets() {
        let policy = policy();
        for url in [
            "http://127.0.0.1/admin",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://localhost:8080/",
            "http://[::1]/",
            "ftp://example.com/file",
        ] {
            assert!(
                policy
                    .evaluate(tools::NET_FETCH, &json!({"url": url}))
                    .is_err(),
                "{url} should be rejected"
            );
        }
    }

    #[test]
    fn net_allows_public_https_and_honours_allowlist() {
        let mut policy = policy();
        assert_eq!(
            caps(policy.evaluate(
                tools::NET_FETCH,
                &json!({"url": "https://example.com/index.html"})
            )),
            vec![Capability::Net]
        );

        policy.net_host_allow = vec!["example.com".to_string()];
        assert!(policy
            .evaluate(tools::NET_FETCH, &json!({"url": "https://api.example.com/x"}))
            .is_ok());
        assert!(deny(policy.evaluate(
            tools::NET_FETCH,
            &json!({"url": "https://evil.test/x"})
        ))
        .contains("allowlist"));
    }

    #[test]
    fn url_parsing_handles_ports_and_ipv6() {
        let target = parse_url("http://example.com:8080/a?b=1").unwrap();
        assert_eq!(target.host, "example.com");
        assert_eq!(target.port, 8080);
        assert_eq!(target.path, "/a?b=1");

        let target = parse_url("https://[::1]/").unwrap();
        assert_eq!(target.host, "::1");
        assert_eq!(target.port, 443);

        assert!(parse_url("https://example.com/\r\nX: y").is_err());
    }

    #[test]
    fn package_ids_and_paths_are_validated() {
        let policy = policy();
        assert_eq!(
            caps(policy.evaluate(
                tools::PKG_INSTALL,
                &json!({"id": "com.artcraft.photocraft"})
            )),
            vec![Capability::Pkg, Capability::Net]
        );
        assert!(deny(policy.evaluate(
            tools::PKG_INSTALL,
            &json!({"id": "Com.Artcraft/Evil"})
        ))
        .contains("invalid package id"));

        assert_eq!(
            caps(policy.evaluate(
                tools::PKG_INSTALL,
                &json!({"path": "/home/tester/app.ltr"})
            )),
            vec![Capability::Pkg, Capability::FsRead]
        );
        assert!(deny(policy.evaluate(
            tools::PKG_INSTALL,
            &json!({"path": "/etc/app.ltr"})
        ))
        .contains("store cache"));
        assert!(deny(policy.evaluate(
            tools::PKG_INSTALL,
            &json!({"path": "/home/tester/app.deb"})
        ))
        .contains(".ltr"));
    }

    #[test]
    fn status_needs_no_capabilities_and_unknown_tools_fail() {
        assert!(caps(policy().evaluate(tools::STATUS, &json!({}))).is_empty());
        assert!(policy().evaluate("lutra_rm_rf", &json!({})).is_err());
    }

    #[test]
    fn public_ip_classification() {
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("10.1.2.3".parse().unwrap()));
        assert!(!is_public_ip("169.254.169.254".parse().unwrap()));
        assert!(!is_public_ip("100.64.0.1".parse().unwrap()));
        assert!(!is_public_ip("::1".parse().unwrap()));
        assert!(!is_public_ip("fc00::1".parse().unwrap()));
        assert!(!is_public_ip("fe80::1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn guard_stub_denies_unapproved_medium_caps() {
        let guard = EnvGuard {
            mode: GuardMode::Env,
            approved: vec![Capability::FsWrite],
        };
        assert!(guard.approve("agent:test", &[Capability::FsRead]).granted);
        assert!(guard.approve("agent:test", &[Capability::FsWrite]).granted);
        assert!(!guard.approve("agent:test", &[Capability::Exec]).granted);
    }
}
