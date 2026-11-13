//! Append-only, hash-chained audit log.
//!
//! Every line is a JSON object whose `hash` covers `prev_hash` and the
//! canonical JSON of the entry body:
//!
//! ```text
//! hash = sha256(prev_hash || "\n" || json(body_without_hash))
//! ```
//!
//! Deleting, reordering or editing any entry breaks the chain at that line,
//! which `lutra-audit verify` reports.

use crate::{now_rfc3339, Capability};
use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Lines, Write};
use std::path::{Path, PathBuf};

/// `prev_hash` of the first entry.
pub const GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

pub const AUDIT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny,
}

impl Decision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
        }
    }
}

/// One audit record as stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub v: u32,
    pub seq: u64,
    pub ts: String,
    pub session: String,
    pub subject: String,
    pub tool: String,
    pub caps: Vec<Capability>,
    pub decision: Decision,
    pub reason: String,
    pub prev_hash: String,
    pub hash: String,
}

/// Canonical hashing view. Field order is part of the on-disk contract.
#[derive(Serialize)]
struct AuditBody<'a> {
    v: u32,
    seq: u64,
    ts: &'a str,
    session: &'a str,
    subject: &'a str,
    tool: &'a str,
    caps: &'a [Capability],
    decision: Decision,
    reason: &'a str,
    prev_hash: &'a str,
}

impl AuditEntry {
    fn body(&self) -> AuditBody<'_> {
        AuditBody {
            v: self.v,
            seq: self.seq,
            ts: &self.ts,
            session: &self.session,
            subject: &self.subject,
            tool: &self.tool,
            caps: &self.caps,
            decision: self.decision,
            reason: &self.reason,
            prev_hash: &self.prev_hash,
        }
    }

    /// Recompute the entry hash from its contents.
    pub fn computed_hash(&self) -> String {
        let body = serde_json::to_string(&self.body()).expect("audit body serializes");
        entry_hash(&self.prev_hash, &body)
    }

    pub fn verify_hash(&self) -> bool {
        self.computed_hash() == self.hash
    }
}

/// `hash = sha256(prev_hash || "\n" || body_json)`.
pub fn entry_hash(prev_hash: &str, body_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(b"\n");
    hasher.update(body_json.as_bytes());
    hex::encode(hasher.finalize())
}

/// Append-only writer that keeps the chain head in memory.
pub struct AuditLog {
    path: PathBuf,
    file: File,
    seq: u64,
    prev_hash: String,
}

impl AuditLog {
    /// Open (or create) the log. Refuses to append when the tail does not
    /// validate, so a tampered chain cannot be silently extended.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create audit directory {}", parent.display()))?;
        }

        let mut seq = 0;
        let mut prev_hash = GENESIS_HASH.to_string();

        if path.exists() {
            let file = File::open(&path)
                .with_context(|| format!("cannot read audit log {}", path.display()))?;
            let mut last: Option<AuditEntry> = None;
            for (index, line) in BufReader::new(file).lines().enumerate() {
                let line = line.with_context(|| format!("cannot read audit log line {}", index + 1))?;
                if line.trim().is_empty() {
                    continue;
                }
                let entry: AuditEntry = serde_json::from_str(&line).with_context(|| {
                    format!(
                        "audit log {} line {} is not a valid entry",
                        path.display(),
                        index + 1
                    )
                })?;
                last = Some(entry);
            }
            if let Some(entry) = last {
                if !entry.verify_hash() {
                    return Err(anyhow!(
                        "audit log {} has a broken tail hash; refusing to append",
                        path.display()
                    ));
                }
                seq = entry.seq;
                prev_hash = entry.hash;
            }
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("cannot open audit log {} for append", path.display()))?;

        Ok(Self {
            path,
            file,
            seq,
            prev_hash,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn last_hash(&self) -> &str {
        &self.prev_hash
    }

    pub fn next_seq(&self) -> u64 {
        self.seq + 1
    }

    /// Append one decision and return the entry that was written.
    pub fn append(
        &mut self,
        session: &str,
        subject: &str,
        tool: &str,
        caps: &[Capability],
        decision: Decision,
        reason: &str,
    ) -> anyhow::Result<AuditEntry> {
        let seq = self.seq + 1;
        let mut entry = AuditEntry {
            v: AUDIT_VERSION,
            seq,
            ts: now_rfc3339(),
            session: session.to_string(),
            subject: subject.to_string(),
            tool: tool.to_string(),
            caps: caps.to_vec(),
            decision,
            reason: reason.to_string(),
            prev_hash: self.prev_hash.clone(),
            hash: String::new(),
        };
        let body = serde_json::to_string(&entry.body()).expect("audit body serializes");
        entry.hash = entry_hash(&entry.prev_hash, &body);

        let line = serde_json::to_string(&entry).expect("audit entry serializes");
        writeln!(self.file, "{line}")
            .with_context(|| format!("cannot append to {}", self.path.display()))?;
        self.file.flush().ok();
        // fsync keeps the chain durable across crashes; failure is logged by callers.
        self.file.sync_data().ok();

        self.seq = seq;
        self.prev_hash = entry.hash.clone();
        Ok(entry)
    }
}

/// Summary produced by a successful verification.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub entries: u64,
    pub last_hash: String,
}

/// Chain verification failure.
#[derive(Debug, Clone)]
pub struct ChainError {
    /// 1-based line number; 0 for I/O level errors.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "audit chain error: {}", self.message)
        } else {
            write!(f, "audit chain broken at line {}: {}", self.line, self.message)
        }
    }
}

impl std::error::Error for ChainError {}

fn broken(line: usize, message: impl Into<String>) -> ChainError {
    ChainError {
        line,
        message: message.into(),
    }
}

/// Verify a JSONL audit file: genesis anchor, sequence continuity, previous
/// hash linkage and self-hash of every entry.
pub fn verify_chain<R: BufRead>(reader: R) -> Result<VerifyReport, ChainError> {
    let mut expected_seq = 1u64;
    let mut prev_hash = GENESIS_HASH.to_string();
    let mut entries = 0u64;
    let mut saw_any = false;

    for (index, line) in reader.lines().enumerate() {
        let line_no = index + 1;
        let line = line.map_err(|e| broken(line_no, format!("read error: {e}")))?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: AuditEntry = serde_json::from_str(&line)
            .map_err(|e| broken(line_no, format!("invalid JSON entry: {e}")))?;

        if !saw_any {
            if entry.prev_hash != GENESIS_HASH {
                return Err(broken(
                    line_no,
                    "chain does not start at genesis (prev_hash is not all-zero)",
                ));
            }
            if entry.seq != 1 {
                return Err(broken(
                    line_no,
                    format!("chain does not start at seq 1 (found seq {})", entry.seq),
                ));
            }
            saw_any = true;
        }

        if entry.seq != expected_seq {
            return Err(broken(
                line_no,
                format!("expected seq {expected_seq}, found {}", entry.seq),
            ));
        }
        if entry.prev_hash != prev_hash {
            return Err(broken(
                line_no,
                format!(
                    "prev_hash does not match the previous entry (expected {prev_hash}, found {})",
                    entry.prev_hash
                ),
            ));
        }
        if !entry.verify_hash() {
            return Err(broken(line_no, "entry hash does not match its contents"));
        }

        prev_hash = entry.hash;
        expected_seq += 1;
        entries += 1;
    }

    if !saw_any {
        return Err(broken(0, "audit log is empty"));
    }

    Ok(VerifyReport {
        entries,
        last_hash: prev_hash,
    })
}

/// Convenience wrapper for callers that already have a path.
pub fn verify_file(path: impl AsRef<Path>) -> anyhow::Result<VerifyReport> {
    let file = File::open(path.as_ref())
        .with_context(|| format!("cannot open {}", path.as_ref().display()))?;
    verify_chain(BufReader::new(file)).map_err(|e| anyhow!(e))
}

fn stream_lines<R: BufRead>(reader: R) -> Lines<R> {
    reader.lines()
}

/// Return the last `count` raw lines (used by `lutra-audit tail`).
pub fn tail_lines<R: BufRead>(reader: R, count: usize) -> io::Result<Vec<String>> {
    let mut buffer: Vec<String> = Vec::new();
    for line in stream_lines(reader) {
        buffer.push(line?);
    }
    let start = buffer.len().saturating_sub(count);
    Ok(buffer.split_off(start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Capability;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_log_path(tag: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "lutra-audit-{}-{}-{}.jsonl",
            std::process::id(),
            unique,
            tag
        ))
    }

    fn write_chain(path: &Path, count: usize) {
        let mut log = AuditLog::open(path).unwrap();
        for i in 0..count {
            log.append(
                &format!("sess-{i}"),
                "agent:test",
                "lutra_fs_read",
                &[Capability::FsRead],
                Decision::Allow,
                "unit test",
            )
            .unwrap();
        }
    }

    #[test]
    fn chain_appends_and_verifies() {
        let path = temp_log_path("ok");
        write_chain(&path, 3);

        let file = File::open(&path).unwrap();
        let report = verify_chain(BufReader::new(file)).unwrap();
        assert_eq!(report.entries, 3);
        assert_eq!(report.last_hash.len(), 64);

        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 3);
        let first: AuditEntry = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(first.prev_hash, GENESIS_HASH);
        assert!(first.verify_hash());

        fs::remove_file(&path).ok();
    }

    #[test]
    fn tampered_entry_is_detected() {
        let path = temp_log_path("tamper");
        write_chain(&path, 3);

        let text = fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines[1] = lines[1].replace("\"allow\"", "\"deny\"");
        fs::write(&path, lines.join("\n") + "\n").unwrap();

        let file = File::open(&path).unwrap();
        let err = verify_chain(BufReader::new(file)).unwrap_err();
        assert_eq!(err.line, 2);
        assert!(err.message.contains("hash"));

        fs::remove_file(&path).ok();
    }

    #[test]
    fn deleted_entry_is_detected() {
        let path = temp_log_path("delete");
        write_chain(&path, 4);

        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let kept = [lines[0], lines[2], lines[3]].join("\n") + "\n";
        fs::write(&path, kept).unwrap();

        let file = File::open(&path).unwrap();
        let err = verify_chain(BufReader::new(file)).unwrap_err();
        assert_eq!(err.line, 2);
        assert!(err.message.contains("expected seq 2"));

        fs::remove_file(&path).ok();
    }

    #[test]
    fn non_genesis_chain_is_rejected() {
        let path = temp_log_path("genesis");
        write_chain(&path, 1);
        let text = fs::read_to_string(&path).unwrap();
        let mut entry: AuditEntry = serde_json::from_str(text.trim()).unwrap();
        entry.seq = 2;
        let body = serde_json::to_string(&entry.body()).unwrap();
        entry.hash = entry_hash(&entry.prev_hash, &body);
        fs::write(&path, serde_json::to_string(&entry).unwrap() + "\n").unwrap();

        let file = File::open(&path).unwrap();
        let err = verify_chain(BufReader::new(file)).unwrap_err();
        assert!(err.message.contains("seq 1"));

        fs::remove_file(&path).ok();
    }

    #[test]
    fn tail_returns_last_lines() {
        let known = "a\nb\nc\nd\n";
        let lines = tail_lines(Cursor::new(known), 2).unwrap();
        assert_eq!(lines, vec!["c", "d"]);
    }
}
