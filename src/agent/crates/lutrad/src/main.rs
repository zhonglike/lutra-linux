//! `lutrad` — Lutra agent daemon.
//!
//! Owns sessions, capability decisions, tool execution and the audit chain.
//! Listens on a newline-delimited JSON-RPC 2.0 Unix socket.

mod policy;
mod server;
mod session;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "lutrad",
    version,
    about = "Lutra agent daemon: sessions, capability policy, tool execution, audit"
)]
struct Cli {
    /// Control socket (tmpfs).
    #[arg(long, default_value = "/run/lutra/lutrad.sock")]
    socket: PathBuf,

    /// Hash-chained audit log (JSONL, append-only).
    #[arg(long, default_value = "/var/log/lutra/audit.jsonl")]
    audit: PathBuf,

    /// Directory for per-session token files (tmpfs).
    #[arg(long, default_value = "/run/lutra/sessions")]
    session_dir: PathBuf,

    /// Ed25519 signing key (hex encoded, 32 bytes). Created on first start.
    #[arg(long, default_value = "/run/lutra/keys/lutrad.ed25519")]
    key: PathBuf,

    /// Default session token TTL in seconds (clamped to 60..=28800).
    #[arg(long, default_value_t = lutra_proto::DEFAULT_TOKEN_TTL_SECS)]
    token_ttl: u64,

    /// Do not persist the signing key (ephemeral, tokens die with the process).
    #[arg(long)]
    no_key_persist: bool,

    /// Do not write session token files to `--session-dir`.
    #[arg(long)]
    no_session_files: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let config = server::ServerConfig {
        socket: cli.socket,
        audit_path: cli.audit,
        key_path: if cli.no_key_persist {
            None
        } else {
            Some(cli.key)
        },
        session_dir: if cli.no_session_files {
            None
        } else {
            Some(cli.session_dir)
        },
        token_ttl: cli.token_ttl,
    };

    server::run(config)
}
