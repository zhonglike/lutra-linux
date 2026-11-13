//! `lutra-audit` — inspect and verify the agent audit hash chain.
//!
//! The daemon only appends; this tool only reads. `verify` walks the whole
//! chain and exits non-zero as soon as a line fails parsing, sequencing,
//! linkage or self-hash validation.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use lutra_proto::audit::{tail_lines, verify_chain};
use serde_json::json;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "lutra-audit",
    version,
    about = "Verify or tail the Lutra agent audit log (hash chain)"
)]
struct Cli {
    /// Audit log path.
    #[arg(long, global = true, default_value = "/var/log/lutra/audit.jsonl")]
    path: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Verify genesis, sequence continuity and every entry hash.
    Verify {
        /// Emit a machine readable JSON report.
        #[arg(long)]
        json: bool,
    },
    /// Print the last N raw entries.
    Tail {
        #[arg(short = 'n', long, default_value_t = 20)]
        lines: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Verify { json } => verify(&cli.path, json),
        Command::Tail { lines } => tail(&cli.path, lines),
    }
}

fn verify(path: &Path, json_output: bool) -> Result<()> {
    let file = File::open(path)
        .with_context(|| format!("cannot open audit log {}", path.display()))?;

    match verify_chain(BufReader::new(file)) {
        Ok(report) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "path": path.display().to_string(),
                        "entries": report.entries,
                        "last_hash": report.last_hash,
                    }))?
                );
            } else {
                println!(
                    "OK  {} entries  last_hash={}",
                    report.entries, report.last_hash
                );
            }
            Ok(())
        }
        Err(error) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": false,
                        "path": path.display().to_string(),
                        "line": error.line,
                        "error": error.message,
                    }))?
                );
            } else {
                eprintln!("FAILED  line {}: {}", error.line, error.message);
            }
            std::process::exit(1);
        }
    }
}

fn tail(path: &Path, count: usize) -> Result<()> {
    let file = File::open(path)
        .with_context(|| format!("cannot open audit log {}", path.display()))?;
    let lines = tail_lines(BufReader::new(file), count)
        .with_context(|| format!("cannot read audit log {}", path.display()))?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}
