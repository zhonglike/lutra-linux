//! `.ltr` package container (tar + zstd) implementation.
//!
//! Layout (v1):
//!
//! ```text
//! manifest.json
//! payload/...             -> installed to /opt/ltr/<id>/
//! hooks/pre-install.sh
//! hooks/post-install.sh
//! META-INF/SHA256SUMS     -> "<sha256>  <path>" for manifest, payload and hooks
//! META-INF/SIGNATURE      -> hex Ed25519 signature over release_digest()
//! ```
//!
//! `release_digest = sha256("LTR1\n" || manifest.json || "\n" || SHA256SUMS)`.
//! Signing and verification therefore cover the manifest plus every payload
//! hash; changing a single byte of any file invalidates both the sums and the
//! signature.

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use lutra_proto::audit::{AuditLog, Decision};
use lutra_proto::{Capability, AUDIT_LOG};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use tar::{Archive, Builder, EntryType, Header, HeaderMode};
use zstd::stream::write::Encoder;

use crate::manifest::{self, Manifest};

pub const MANIFEST_FILE: &str = "manifest.json";
pub const PAYLOAD_DIR: &str = "payload";
pub const HOOKS_DIR: &str = "hooks";
pub const META_DIR: &str = "META-INF";
pub const SUMS_FILE: &str = "META-INF/SHA256SUMS";
pub const SUMS_FILE_LEGACY: &str = "META-INF/sha256sums";
pub const SIGNATURE_FILE: &str = "META-INF/SIGNATURE";
pub const INSTALLED_META: &str = ".ltr-manifest.json";

pub const DEFAULT_ROOT: &str = "/opt/ltr";
pub const DEFAULT_PUBKEY: &str = "/etc/lutra/keys/ltr.pub";

pub struct PackOptions {
    pub manifest: PathBuf,
    pub payload: PathBuf,
    pub hooks: Option<PathBuf>,
    pub out: PathBuf,
    pub key: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct PackReport {
    pub out: PathBuf,
    pub id: String,
    pub version: String,
    pub files: usize,
    pub download_size: u64,
    pub installed_size: u64,
    pub digest: String,
    pub public_key: String,
}

pub struct InstallOptions {
    pub package: PathBuf,
    pub root: PathBuf,
    pub pubkey: Option<PathBuf>,
    pub allow_unsigned: bool,
    pub force: bool,
    pub audit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    Verified,
    UnsignedAllowed,
    SkippedNoKey,
}

impl SignatureStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SignatureStatus::Verified => "verified",
            SignatureStatus::UnsignedAllowed => "unsigned (explicitly allowed)",
            SignatureStatus::SkippedNoKey => "skipped (no public key configured)",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InstallReport {
    pub id: String,
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    pub files: usize,
    pub signature: SignatureStatus,
    pub hooks_skipped: bool,
}

#[derive(Debug, Serialize)]
pub struct RemoveReport {
    pub id: String,
    pub path: PathBuf,
    pub freed_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledPackage {
    pub id: String,
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    pub installed_size: u64,
    pub permissions: Vec<Capability>,
}

pub struct VerifyOptions {
    pub package: PathBuf,
    pub pubkey: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct VerifyReport {
    pub id: String,
    pub version: String,
    pub files_checked: usize,
    pub signature: SignatureStatus,
    pub digest: String,
}

// ---------------------------------------------------------------------------
// Pack
// ---------------------------------------------------------------------------

pub fn pack(options: &PackOptions) -> Result<PackReport> {
    let manifest_bytes = fs::read(&options.manifest)
        .with_context(|| format!("cannot read {}", options.manifest.display()))?;
    let manifest = Manifest::from_slice(&manifest_bytes)?;

    if !options.payload.is_dir() {
        bail!("payload directory {} does not exist", options.payload.display());
    }
    let mut files = collect_files(&options.payload, PAYLOAD_DIR)?;
    if files.is_empty() {
        bail!("payload directory {} is empty", options.payload.display());
    }
    if let Some(hooks) = &options.hooks {
        if !hooks.is_dir() {
            bail!("hooks path {} is not a directory", hooks.display());
        }
        files.extend(collect_files(hooks, HOOKS_DIR)?);
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));

    let available: Vec<&str> = files.iter().map(|(_, name)| name.as_str()).collect();
    for (label, hook) in [
        ("pre_install", manifest.hooks.pre_install.as_deref()),
        ("post_install", manifest.hooks.post_install.as_deref()),
        ("pre_remove", manifest.hooks.pre_remove.as_deref()),
    ] {
        if let Some(hook) = hook {
            if !available.contains(&hook) {
                bail!("manifest declares hooks.{label} = `{hook}` but the file was not supplied");
            }
        }
    }

    let mut sums = String::new();
    let mut installed_size = 0u64;
    sums.push_str(&format!(
        "{}  {}\n",
        hex::encode(Sha256::digest(&manifest_bytes)),
        MANIFEST_FILE
    ));
    for (path, name) in &files {
        let (digest, size) = hash_file(path)?;
        installed_size += size;
        sums.push_str(&format!("{digest}  {name}\n"));
    }
    let sums_bytes = sums.into_bytes();

    let signing_key = load_signing_key(&options.key)?;
    let digest = release_digest(&manifest_bytes, &sums_bytes);
    let signature = signing_key.sign(&digest);
    let signature_hex = hex::encode(signature.to_bytes());

    if let Some(parent) = options
        .out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let output = File::create(&options.out)
        .with_context(|| format!("cannot create {}", options.out.display()))?;
    let encoder = Encoder::new(output, 9).context("cannot initialise zstd encoder")?;
    let mut builder = Builder::new(encoder);
    builder.mode(HeaderMode::Deterministic);

    append_bytes(&mut builder, MANIFEST_FILE, &manifest_bytes)?;
    for (path, name) in &files {
        builder
            .append_path_with_name(path, name)
            .with_context(|| format!("cannot add {name} to archive"))?;
    }
    append_bytes(&mut builder, SUMS_FILE, &sums_bytes)?;
    append_bytes(
        &mut builder,
        SIGNATURE_FILE,
        format!("{signature_hex}\n").as_bytes(),
    )?;

    builder.finish().context("cannot finish tar stream")?;
    let encoder = builder.into_inner().context("cannot flush tar builder")?;
    let output = encoder.finish().context("cannot finish zstd stream")?;
    output.sync_data().ok();

    let download_size = fs::metadata(&options.out)?.len();
    Ok(PackReport {
        out: options.out.clone(),
        id: manifest.id,
        version: manifest.version,
        files: files.len(),
        download_size,
        installed_size,
        digest: hex::encode(digest),
        public_key: hex::encode(signing_key.verifying_key().to_bytes()),
    })
}

fn append_bytes<W: Write>(builder: &mut Builder<W>, name: &str, data: &[u8]) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    builder
        .append_data(&mut header, name, data)
        .with_context(|| format!("cannot add {name} to archive"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

pub fn install(options: &InstallOptions) -> Result<InstallReport> {
    fs::create_dir_all(&options.root)
        .with_context(|| format!("cannot create install root {}", options.root.display()))?;

    let staging = options
        .root
        .join(format!(".staging-{}-{}", std::process::id(), lutra_proto::now_unix()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;

    let result = install_inner(options, &staging);
    let _ = fs::remove_dir_all(&staging);
    result
}

fn install_inner(options: &InstallOptions, staging: &Path) -> Result<InstallReport> {
    extract_archive(&options.package, staging)?;

    let manifest_path = staging.join(MANIFEST_FILE);
    let manifest_bytes = fs::read(&manifest_path)
        .context("package is missing manifest.json")?;
    let manifest = Manifest::from_slice(&manifest_bytes)?;

    verify_extracted_checksums(staging)?;

    let sums_bytes = read_sums_bytes(staging)?;
    let signature = read_signature(staging)?;
    let digest = release_digest(&manifest_bytes, &sums_bytes);

    let signature_status = match resolve_public_key(options.pubkey.as_deref())? {
        Some(key) => {
            key.verify(&digest, &signature)
                .context("signature verification failed")?;
            SignatureStatus::Verified
        }
        None if options.allow_unsigned => SignatureStatus::UnsignedAllowed,
        None => bail!(
            "no public key configured (pass --pubkey, set LUTRA_PACKAGE_PUBKEY, \
             or install {} and retry, or use --allow-unsigned)",
            DEFAULT_PUBKEY
        ),
    };

    let arch = std::env::consts::ARCH;
    if !manifest.arch.is_empty() && !manifest.arch.iter().any(|value| value == arch) {
        bail!(
            "package supports {:?}, this host is {arch}",
            manifest.arch
        );
    }

    let target = options.root.join(&manifest.id);
    if target.exists() {
        if !options.force {
            bail!(
                "{} {} is already installed at {} (use --force to replace)",
                manifest.id,
                manifest.version,
                target.display()
            );
        }
        fs::remove_dir_all(&target)
            .with_context(|| format!("cannot remove existing {}", target.display()))?;
    }

    let payload = staging.join(PAYLOAD_DIR);
    if !payload.is_dir() {
        bail!("package has no payload/ directory");
    }
    fs::rename(&payload, &target).with_context(|| {
        format!("cannot move payload to {}", target.display())
    })?;
    fs::write(target.join(INSTALLED_META), &manifest_bytes)?;

    let files = count_files(&target).unwrap_or(0);
    if options.audit {
        audit_event(
            "pkg.install",
            &manifest.permissions,
            &format!(
                "installed {} {} to {} (signature: {})",
                manifest.id,
                manifest.version,
                target.display(),
                signature_status.as_str()
            ),
        );
    }

    Ok(InstallReport {
        id: manifest.id.clone(),
        name: manifest.name,
        version: manifest.version,
        path: target,
        files,
        signature: signature_status,
        // Executing hooks without the systemd-run sandbox would defeat the
        // package's permission model, so this build skips them.
        hooks_skipped: manifest.hooks.pre_install.is_some()
            || manifest.hooks.post_install.is_some(),
    })
}

// ---------------------------------------------------------------------------
// Remove / list
// ---------------------------------------------------------------------------

pub fn remove(root: &Path, id: &str, audit: bool) -> Result<RemoveReport> {
    manifest::validate_id(id)?;
    let target = root.join(id);
    if !target.is_dir() {
        bail!("{id} is not installed under {}", root.display());
    }

    let manifest = fs::read(target.join(INSTALLED_META))
        .ok()
        .and_then(|bytes| Manifest::from_slice(&bytes).ok());
    let freed_bytes = dir_size(&target).unwrap_or(0);
    fs::remove_dir_all(&target)
        .with_context(|| format!("cannot remove {}", target.display()))?;

    if audit {
        audit_event(
            "pkg.remove",
            manifest.as_ref().map(|m| m.permissions.as_slice()).unwrap_or(&[]),
            &format!("removed {id} ({freed_bytes} bytes freed)"),
        );
    }

    Ok(RemoveReport {
        id: id.to_string(),
        path: target,
        freed_bytes,
    })
}

pub fn list(root: &Path) -> Result<Vec<InstalledPackage>> {
    let mut packages = Vec::new();
    if !root.is_dir() {
        return Ok(packages);
    }

    for entry in fs::read_dir(root)
        .with_context(|| format!("cannot read {}", root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let hidden = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with('.'))
            .unwrap_or(true);
        if hidden {
            continue;
        }

        let Ok(bytes) = fs::read(path.join(INSTALLED_META)) else {
            continue;
        };
        let Ok(manifest) = Manifest::from_slice(&bytes) else {
            continue;
        };

        packages.push(InstalledPackage {
            id: manifest.id,
            name: manifest.name,
            version: manifest.version,
            installed_size: dir_size(&path).unwrap_or(0),
            permissions: manifest.permissions,
            path,
        });
    }

    packages.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(packages)
}

// ---------------------------------------------------------------------------
// Verify
// ---------------------------------------------------------------------------

pub fn verify(options: &VerifyOptions) -> Result<VerifyReport> {
    let file = File::open(&options.package)
        .with_context(|| format!("cannot open {}", options.package.display()))?;
    let decoder = zstd::stream::read::Decoder::new(BufReader::new(file))
        .context("not a zstd-compressed archive")?;
    let mut archive = Archive::new(decoder);

    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut sums: Option<BTreeMap<String, String>> = None;
    let mut sums_bytes: Option<Vec<u8>> = None;
    let mut signature: Option<Signature> = None;
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    let meta_prefix = format!("{META_DIR}/");
    let payload_prefix = format!("{PAYLOAD_DIR}/");
    let hooks_prefix = format!("{HOOKS_DIR}/");

    for entry in archive.entries().context("cannot read tar stream")? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let name = path.to_string_lossy().replace('\\', "/");
        let is_dir = entry.header().entry_type().is_dir();

        match name.as_str() {
            MANIFEST_FILE => {
                let mut buffer = Vec::new();
                entry.read_to_end(&mut buffer)?;
                manifest_bytes = Some(buffer);
            }
            SUMS_FILE | SUMS_FILE_LEGACY => {
                let mut buffer = Vec::new();
                entry.read_to_end(&mut buffer)?;
                let text = String::from_utf8(buffer.clone())
                    .context("SHA256SUMS is not valid UTF-8")?;
                sums = Some(parse_sums(&text)?);
                sums_bytes = Some(buffer);
            }
            SIGNATURE_FILE => {
                let mut text = String::new();
                entry.read_to_string(&mut text)?;
                signature = Some(parse_signature(text.trim())?);
            }
            _ if name.starts_with(meta_prefix.as_str()) => {}
            _ if (name.starts_with(payload_prefix.as_str())
                || name.starts_with(hooks_prefix.as_str()))
                && !is_dir =>
            {
                let (digest, _) = hash_reader(&mut entry)?;
                found.insert(name, digest);
            }
            _ => {}
        }
    }

    let manifest_bytes = manifest_bytes.context("package is missing manifest.json")?;
    let manifest = Manifest::from_slice(&manifest_bytes)?;
    let sums = sums.context("package is missing META-INF/SHA256SUMS")?;
    let sums_bytes = sums_bytes.expect("sums bytes exist when sums exist");
    let signature = signature.context("package is missing META-INF/SIGNATURE")?;

    if let Some(expected) = sums.get(MANIFEST_FILE) {
        let actual = hex::encode(Sha256::digest(&manifest_bytes));
        if expected != &actual {
            bail!("checksum mismatch for {MANIFEST_FILE}");
        }
    }

    for (name, digest) in &found {
        match sums.get(name) {
            Some(expected) if expected == digest => {}
            Some(_) => bail!("checksum mismatch for {name}"),
            None => bail!("{name} is not listed in {SUMS_FILE}"),
        }
    }
    for name in sums.keys() {
        if name == MANIFEST_FILE {
            continue;
        }
        if !found.contains_key(name) {
            bail!("{SUMS_FILE} lists {name} but the package does not contain it");
        }
    }

    let digest = release_digest(&manifest_bytes, &sums_bytes);
    let signature_status = match resolve_public_key(options.pubkey.as_deref())? {
        Some(key) => {
            key.verify(&digest, &signature)
                .context("signature verification failed")?;
            SignatureStatus::Verified
        }
        None => SignatureStatus::SkippedNoKey,
    };

    Ok(VerifyReport {
        id: manifest.id,
        version: manifest.version,
        files_checked: found.len(),
        signature: signature_status,
        digest: hex::encode(digest),
    })
}

// ---------------------------------------------------------------------------
// Crypto helpers
// ---------------------------------------------------------------------------

/// Digest that the package signature covers.
pub fn release_digest(manifest_bytes: &[u8], sums_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"LTR1\n");
    hasher.update(manifest_bytes);
    hasher.update(b"\n");
    hasher.update(sums_bytes);
    hasher.finalize().into()
}

pub fn load_signing_key(path: &Path) -> Result<SigningKey> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("cannot read signing key {}", path.display()))?;
    let bytes = hex::decode(text.trim())
        .with_context(|| format!("{} is not hex", path.display()))?;
    let key_bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("{} must contain exactly 32 bytes", path.display()))?;
    Ok(SigningKey::from_bytes(&key_bytes))
}

fn load_verifying_key(path: &Path) -> Result<VerifyingKey> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("cannot read public key {}", path.display()))?;
    let bytes = hex::decode(text.trim())
        .with_context(|| format!("{} is not hex", path.display()))?;
    let key_bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("{} must contain exactly 32 bytes", path.display()))?;
    VerifyingKey::from_bytes(&key_bytes)
        .map_err(|error| anyhow!("invalid Ed25519 public key: {error}"))
}

/// Resolution order: explicit flag, `LUTRA_PACKAGE_PUBKEY`, then the system
/// trust anchor. `None` means "no key configured".
fn resolve_public_key(explicit: Option<&Path>) -> Result<Option<VerifyingKey>> {
    if let Some(path) = explicit {
        return Ok(Some(load_verifying_key(path)?));
    }
    if let Some(value) = std::env::var_os("LUTRA_PACKAGE_PUBKEY") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(Some(load_verifying_key(&path)?));
        }
    }
    let default = Path::new(DEFAULT_PUBKEY);
    if default.is_file() {
        return Ok(Some(load_verifying_key(default)?));
    }
    Ok(None)
}

fn parse_signature(text: &str) -> Result<Signature> {
    let bytes = hex::decode(text).context("META-INF/SIGNATURE must be hex")?;
    let signature_bytes: [u8; 64] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("META-INF/SIGNATURE must be 64 bytes"))?;
    Ok(Signature::from_bytes(&signature_bytes))
}

// ---------------------------------------------------------------------------
// Archive helpers
// ---------------------------------------------------------------------------

fn collect_files(root: &Path, prefix: &str) -> Result<Vec<(PathBuf, String)>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for entry in
            fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                let relative = path.strip_prefix(root)?;
                let name = relative.to_string_lossy().replace('\\', "/");
                files.push((path, format!("{prefix}/{name}")));
            } else {
                bail!(
                    "symlinks and special files are not supported in v1 packages: {}",
                    path.display()
                );
            }
        }
    }

    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(files)
}

fn extract_archive(package: &Path, dest: &Path) -> Result<()> {
    let file = File::open(package)
        .with_context(|| format!("cannot open {}", package.display()))?;
    let decoder = zstd::stream::read::Decoder::new(BufReader::new(file))
        .context("not a zstd-compressed archive")?;
    let mut archive = Archive::new(decoder);

    for entry in archive.entries().context("cannot read tar stream")? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().replace('\\', "/");
        let target = safe_join(dest, &name)?;
        let entry_type = entry.header().entry_type();

        if entry_type.is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mode = entry.header().mode().unwrap_or(0o644);
            let mut output = File::create(&target)
                .with_context(|| format!("cannot create {}", target.display()))?;
            io::copy(&mut entry, &mut output)?;
            apply_mode(&target, mode);
        } else if entry_type.is_symlink() {
            let link = entry
                .link_name()?
                .ok_or_else(|| anyhow!("symlink {name} has no target"))?;
            if link.is_absolute() {
                bail!("absolute symlink target rejected: {name} -> {}", link.display());
            }
            if link
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            {
                bail!(
                    "symlink target may not contain `..`: {name} -> {}",
                    link.display()
                );
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            create_symlink(&link, &target)?;
        } else {
            bail!("unsupported tar entry type for {name}: {entry_type:?}");
        }
    }

    Ok(())
}

fn safe_join(root: &Path, name: &str) -> Result<PathBuf> {
    let relative = Path::new(name);
    if relative.is_absolute() {
        bail!("absolute path in archive: {name}");
    }
    let mut joined = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => joined.push(part),
            Component::CurDir => {}
            _ => bail!("unsafe path in archive: {name}"),
        }
    }
    Ok(joined)
}

#[cfg(unix)]
fn apply_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777));
}

#[cfg(not(unix))]
fn apply_mode(_path: &Path, _mode: u32) {}

#[cfg(unix)]
fn create_symlink(link: &Path, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(link, target)
        .with_context(|| format!("cannot create symlink {}", target.display()))
}

#[cfg(not(unix))]
fn create_symlink(_link: &Path, _target: &Path) -> Result<()> {
    bail!("symlinks in .ltr packages require a Unix host")
}

fn read_sums_bytes(staging: &Path) -> Result<Vec<u8>> {
    let primary = staging.join(SUMS_FILE);
    if primary.is_file() {
        return Ok(fs::read(primary)?);
    }
    let legacy = staging.join(SUMS_FILE_LEGACY);
    if legacy.is_file() {
        return Ok(fs::read(legacy)?);
    }
    bail!("package is missing {SUMS_FILE}")
}

fn read_signature(staging: &Path) -> Result<Signature> {
    let path = staging.join(SIGNATURE_FILE);
    let text = fs::read_to_string(&path)
        .with_context(|| format!("package is missing {SIGNATURE_FILE}"))?;
    parse_signature(text.trim())
}

fn verify_extracted_checksums(staging: &Path) -> Result<()> {
    let text = String::from_utf8(read_sums_bytes(staging)?)
        .context("SHA256SUMS is not valid UTF-8")?;
    let sums = parse_sums(&text)?;

    for (name, expected) in &sums {
        let path = safe_join(staging, name)?;
        if !path.is_file() {
            bail!("{SUMS_FILE} lists {name} but it is missing from the package");
        }
        let (actual, _) = hash_file(&path)?;
        if &actual != expected {
            bail!("checksum mismatch for {name}");
        }
    }

    for dir in [PAYLOAD_DIR, HOOKS_DIR] {
        let base = staging.join(dir);
        if !base.is_dir() {
            continue;
        }
        for (_, name) in collect_files(&base, dir)? {
            if !sums.contains_key(&name) {
                bail!("{name} is not covered by {SUMS_FILE}");
            }
        }
    }
    Ok(())
}

fn parse_sums(text: &str) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let digest = parts.next().unwrap_or_default();
        let name = parts
            .next()
            .map(|value| value.trim_start())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("malformed checksum line {}", index + 1))?;
        if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("malformed checksum on line {}", index + 1);
        }
        map.insert(name.to_string(), digest.to_ascii_lowercase());
    }
    Ok(map)
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    let file = File::open(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    hash_reader(file).with_context(|| format!("cannot hash {}", path.display()))
}

fn hash_reader<R: Read>(mut reader: R) -> io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((hex::encode(hasher.finalize()), total))
}

fn count_files(root: &Path) -> Result<usize> {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn dir_size(root: &Path) -> Result<u64> {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                total += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            }
        }
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// Audit integration
// ---------------------------------------------------------------------------

fn audit_event(tool: &str, caps: &[Capability], reason: &str) {
    let subject = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".to_string());

    match AuditLog::open(AUDIT_LOG) {
        Ok(mut log) => {
            if let Err(error) = log.append("ltr-cli", &subject, tool, caps, Decision::Allow, reason)
            {
                eprintln!("ltr: audit append failed: {error}");
            }
        }
        Err(error) => eprintln!("ltr: audit log unavailable ({error}); continuing"),
    }
}
