//! `.ltr` manifest v1 (`manifest.json`).
//!
//! Field names follow `docs/02-desktop-and-agent.md` section 6.2. Unknown
//! fields are ignored by serde so newer producers remain installable, but the
//! fields below are always validated before a package is packed or installed.

use anyhow::{bail, Context, Result};
use lutra_proto::Capability;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;

pub const MANIFEST_FILE: &str = "manifest.json";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub arch: Vec<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub maintainer: Option<String>,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<Capability>,
    #[serde(default)]
    pub entrypoints: Entrypoints,
    #[serde(default)]
    pub size: Size,
    #[serde(default)]
    pub hooks: Hooks,
    #[serde(default)]
    pub source: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entrypoints {
    #[serde(default)]
    pub gui: Option<String>,
    #[serde(default)]
    pub cli: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Size {
    #[serde(default)]
    pub download: u64,
    #[serde(default)]
    pub installed: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hooks {
    #[serde(default)]
    pub pre_install: Option<String>,
    #[serde(default)]
    pub post_install: Option<String>,
    #[serde(default)]
    pub pre_remove: Option<String>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("cannot read manifest {}", path.display()))?;
        Self::from_slice(&bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let manifest: Manifest =
            serde_json::from_slice(bytes).context("manifest.json is not valid JSON")?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA_VERSION {
            bail!(
                "unsupported manifest schema {} (expected {})",
                self.schema,
                SCHEMA_VERSION
            );
        }
        validate_id(&self.id)?;
        if self.name.trim().is_empty() {
            bail!("manifest `name` must not be empty");
        }
        if self.version.trim().is_empty() || self.version.len() > 64 {
            bail!("manifest `version` must be 1..=64 characters");
        }
        if !self
            .version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
        {
            bail!("manifest `version` contains invalid characters");
        }
        for arch in &self.arch {
            if arch.trim().is_empty() {
                bail!("manifest `arch` contains an empty entry");
            }
        }
        for (label, entry) in [
            ("entrypoints.gui", self.entrypoints.gui.as_deref()),
            ("entrypoints.cli", self.entrypoints.cli.as_deref()),
        ] {
            if let Some(path) = entry {
                validate_absolute_path(label, path)?;
            }
        }
        for (label, hook) in [
            ("hooks.pre_install", self.hooks.pre_install.as_deref()),
            ("hooks.post_install", self.hooks.post_install.as_deref()),
            ("hooks.pre_remove", self.hooks.pre_remove.as_deref()),
        ] {
            if let Some(path) = hook {
                validate_relative_path(label, path)?;
            }
        }
        Ok(())
    }
}

/// Reverse-DNS id: lowercase alphanumerics plus `-`, at least two segments.
pub fn validate_id(id: &str) -> Result<()> {
    if id.len() < 3 || id.len() > 128 {
        bail!("package id must be 3..=128 characters");
    }
    if !id.contains('.') {
        bail!("package id must be reverse-DNS, e.g. com.example.app");
    }
    for segment in id.split('.') {
        if segment.is_empty() {
            bail!("package id `{id}` contains an empty segment");
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            bail!(
                "package id `{id}` may only contain lowercase letters, digits, '-' and '.'"
            );
        }
        if segment.starts_with('-') || segment.ends_with('-') {
            bail!("package id `{id}` has a segment starting or ending with '-'");
        }
    }
    Ok(())
}

fn validate_absolute_path(label: &str, path: &str) -> Result<()> {
    if !path.starts_with('/') {
        bail!("{label} must be an absolute path, got `{path}`");
    }
    if path.split('/').any(|segment| segment == "..") {
        bail!("{label} must not contain `..`");
    }
    Ok(())
}

fn validate_relative_path(label: &str, path: &str) -> Result<()> {
    if path.starts_with('/') {
        bail!("{label} must be relative to the package root, got `{path}`");
    }
    if path.split('/').any(|segment| segment == ".." || segment == ".") {
        bail!("{label} must not contain `.` or `..` segments");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            schema: 1,
            id: "com.example.app".into(),
            name: "Example".into(),
            version: "1.2.3".into(),
            arch: vec!["x86_64".into()],
            summary: String::new(),
            homepage: None,
            license: None,
            maintainer: None,
            depends: vec![],
            conflicts: vec![],
            permissions: vec![Capability::FsRead],
            entrypoints: Entrypoints::default(),
            size: Size::default(),
            hooks: Hooks::default(),
            source: None,
        }
    }

    #[test]
    fn sample_manifest_is_valid() {
        sample().validate().unwrap();
    }

    #[test]
    fn bad_ids_are_rejected() {
        for id in ["com", "Com.Example", "com..app", "com.example/app", "-com.example"] {
            assert!(validate_id(id).is_err(), "{id} should be rejected");
        }
        assert!(validate_id("com.example.app").is_ok());
    }

    #[test]
    fn absolute_entrypoints_and_relative_hooks_are_enforced() {
        let mut manifest = sample();
        manifest.entrypoints.gui = Some("bin/app".into());
        assert!(manifest.validate().is_err());

        manifest.entrypoints.gui = Some("/opt/ltr/com.example.app/bin/app".into());
        manifest.hooks.post_install = Some("/tmp/hook.sh".into());
        assert!(manifest.validate().is_err());

        manifest.hooks.post_install = Some("hooks/post-install.sh".into());
        manifest.validate().unwrap();
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let json = br#"{
            "schema": 1,
            "id": "com.example.app",
            "name": "Example",
            "version": "1.0.0",
            "future_field": {"a": 1}
        }"#;
        let manifest = Manifest::from_slice(json).unwrap();
        assert_eq!(manifest.id, "com.example.app");
    }
}
