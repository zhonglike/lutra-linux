//! `ltr` — Lutra `.ltr` package tool.
//!
//! Subcommands map directly to the v1 package lifecycle described in
//! `docs/02-desktop-and-agent.md` section 6.

mod manifest;
mod package;

use anyhow::Result;
use clap::{Parser, Subcommand};
use package::{InstallOptions, PackOptions, VerifyOptions};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "ltr",
    version,
    about = "Lutra package tool: pack / install / remove / list / verify (.ltr = tar + zstd)"
)]
struct Cli {
    /// Install root. Each package lives in <root>/<id>/.
    #[arg(long, global = true, default_value = "/opt/ltr")]
    root: PathBuf,

    /// Do not append entries to the audit log.
    #[arg(long, global = true)]
    no_audit: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build a signed .ltr package.
    Pack {
        /// Path to manifest.json.
        #[arg(long)]
        manifest: PathBuf,
        /// Directory whose contents become payload/.
        #[arg(long)]
        payload: PathBuf,
        /// Optional directory whose contents become hooks/.
        #[arg(long)]
        hooks: Option<PathBuf>,
        /// Output .ltr file.
        #[arg(long)]
        out: PathBuf,
        /// Ed25519 signing key (32 hex-encoded bytes).
        #[arg(long)]
        key: PathBuf,
    },
    /// Verify checksums and signature, then unpack into the install root.
    Install {
        /// Path to the .ltr file.
        package: PathBuf,
        /// Ed25519 public key (32 hex-encoded bytes). Defaults to
        /// `$LUTRA_PACKAGE_PUBKEY` or /etc/lutra/keys/ltr.pub.
        #[arg(long)]
        pubkey: Option<PathBuf>,
        /// Allow installation when no public key is configured.
        #[arg(long)]
        allow_unsigned: bool,
        /// Replace an existing installation.
        #[arg(long)]
        force: bool,
    },
    /// Remove an installed package directory.
    Remove {
        /// Reverse-DNS package id.
        id: String,
    },
    /// List installed packages.
    List {
        /// Machine readable output.
        #[arg(long)]
        json: bool,
    },
    /// Verify structure, checksums and signature without installing.
    Verify {
        /// Path to the .ltr file.
        package: PathBuf,
        /// Ed25519 public key (32 hex-encoded bytes).
        #[arg(long)]
        pubkey: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Pack {
            manifest,
            payload,
            hooks,
            out,
            key,
        } => {
            let report = package::pack(&PackOptions {
                manifest,
                payload,
                hooks,
                out,
                key,
            })?;
            println!("packed {}", report.out.display());
            println!("  id           {}", report.id);
            println!("  version      {}", report.version);
            println!("  files        {}", report.files);
            println!(
                "  sizes        download={} installed={}",
                report.download_size, report.installed_size
            );
            println!("  digest       {}", report.digest);
            println!("  public key   {}", report.public_key);
            println!(
                "  note: distribute the public key out of band; it is the signature trust anchor"
            );
            Ok(())
        }
        Command::Install {
            package: path,
            pubkey,
            allow_unsigned,
            force,
        } => {
            let report = package::install(&InstallOptions {
                package: path,
                root: cli.root,
                pubkey,
                allow_unsigned,
                force,
                audit: !cli.no_audit,
            })?;
            println!("installed {} {}", report.id, report.version);
            println!("  name         {}", report.name);
            println!("  path         {}", report.path.display());
            println!("  files        {}", report.files);
            println!("  signature    {}", report.signature.as_str());
            if report.hooks_skipped {
                eprintln!(
                    "  warning: hooks were not executed (requires the systemd-run sandbox, \
                     not implemented in this build)"
                );
            }
            Ok(())
        }
        Command::Remove { id } => {
            let report = package::remove(&cli.root, &id, !cli.no_audit)?;
            println!(
                "removed {} from {} ({} bytes freed)",
                report.id,
                report.path.display(),
                report.freed_bytes
            );
            Ok(())
        }
        Command::List { json } => {
            let packages = package::list(&cli.root)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&packages)?);
                return Ok(());
            }
            if packages.is_empty() {
                println!("no packages installed under {}", cli.root.display());
                return Ok(());
            }
            println!(
                "{:<32} {:<12} {:>12}  {:<30} {}",
                "ID", "VERSION", "SIZE", "PERMISSIONS", "NAME"
            );
            for package in packages {
                let permissions: Vec<&str> = package
                    .permissions
                    .iter()
                    .map(|capability| capability.as_str())
                    .collect();
                println!(
                    "{:<32} {:<12} {:>12}  {:<30} {}",
                    package.id,
                    package.version,
                    package.installed_size,
                    permissions.join(","),
                    package.name
                );
            }
            Ok(())
        }
        Command::Verify { package: path, pubkey } => {
            let report = package::verify(&VerifyOptions {
                package: path,
                pubkey,
            })?;
            println!("OK {} {}", report.id, report.version);
            println!("  files        {}", report.files_checked);
            println!("  digest       {}", report.digest);
            println!("  signature    {}", report.signature.as_str());
            if report.signature == package::SignatureStatus::SkippedNoKey {
                eprintln!(
                    "  warning: no public key configured; checksums verified but authenticity is unproven"
                );
            }
            Ok(())
        }
    }
}
