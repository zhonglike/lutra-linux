//! Session and token management for `lutrad`.
//!
//! Tokens are Ed25519-signed, self-describing and short lived (2 h by default).
//! The signing key lives on tmpfs; a restart therefore invalidates every token,
//! which matches the "no permanent authorization" rule in the design docs.
//!
//! `verify()` also consults the in-memory session table: revocation is
//! immediate and escalation rotates the nonce, so an older token cannot be
//! replayed after a capability grant.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use lutra_proto::{
    codes, decode_token, encode_token_bytes, signing_message, Capability, RpcError, SessionInfo,
    TokenClaims, MAX_TOKEN_TTL_SECS, PROTOCOL_VERSION,
};
use rand::rngs::OsRng;
use rand::RngCore;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

struct SessionRecord {
    info: SessionInfo,
    nonce: String,
}

pub struct SessionManager {
    signing: SigningKey,
    verifying: VerifyingKey,
    ttl: u64,
    dir: Option<PathBuf>,
    sessions: HashMap<String, SessionRecord>,
}

impl SessionManager {
    /// `key_path = None` generates an ephemeral key (tests, `--no-key-persist`).
    pub fn new(
        key_path: Option<&Path>,
        dir: Option<PathBuf>,
        ttl: u64,
    ) -> anyhow::Result<Self> {
        let signing = match key_path {
            Some(path) => load_or_create_key(path)?,
            None => SigningKey::generate(&mut OsRng),
        };
        let ttl = ttl.clamp(60, MAX_TOKEN_TTL_SECS);
        Ok(Self {
            verifying: signing.verifying_key(),
            signing,
            ttl,
            dir,
            sessions: HashMap::new(),
        })
    }

    pub fn ttl(&self) -> u64 {
        self.ttl
    }

    pub fn signing_public_key(&self) -> [u8; 32] {
        self.verifying.to_bytes()
    }

    /// Create a session and its first token.
    pub fn create(
        &mut self,
        subject: &str,
        caps: &[Capability],
        ttl_secs: Option<u64>,
        now: u64,
    ) -> anyhow::Result<(SessionInfo, TokenClaims, String)> {
        let session = format!("sess-{}", random_hex(8));
        let caps = dedup_caps(caps);
        let ttl = ttl_secs.unwrap_or(self.ttl).clamp(60, MAX_TOKEN_TTL_SECS);
        let claims = TokenClaims {
            v: PROTOCOL_VERSION,
            session: session.clone(),
            subject: subject.to_string(),
            caps: caps.clone(),
            iat: now,
            exp: now + ttl,
            nonce: random_hex(16),
        };
        let token = self.sign_claims(&claims)?;

        let info = SessionInfo {
            session: session.clone(),
            subject: subject.to_string(),
            caps,
            created_at: now,
            expires_at: claims.exp,
            revoked: false,
        };
        self.sessions.insert(
            session,
            SessionRecord {
                info: info.clone(),
                nonce: claims.nonce.clone(),
            },
        );
        self.persist_token(&claims.session, &token);

        Ok((info, claims, token))
    }

    /// Verify signature, expiry and revocation state.
    pub fn verify(&self, token: &str, now: u64) -> Result<TokenClaims, RpcError> {
        let raw = decode_token(token)?;
        let signature = Signature::from_bytes(&raw.signature);
        self.verifying
            .verify(&signing_message(&raw.payload), &signature)
            .map_err(|_| {
                RpcError::new(
                    codes::SESSION_INVALID,
                    "token signature verification failed",
                )
            })?;

        let claims: TokenClaims = serde_json::from_slice(&raw.payload).map_err(|_| {
            RpcError::new(codes::SESSION_INVALID, "token payload is not valid claims")
        })?;
        if claims.v != PROTOCOL_VERSION {
            return Err(RpcError::new(
                codes::SESSION_INVALID,
                format!("unsupported token version {}", claims.v),
            ));
        }
        if claims.is_expired(now) {
            return Err(RpcError::new(
                codes::SESSION_EXPIRED,
                format!("session token expired at {}", claims.exp),
            ));
        }

        let record = self.sessions.get(&claims.session).ok_or_else(|| {
            RpcError::new(codes::SESSION_INVALID, "unknown or closed session")
        })?;
        if record.info.revoked {
            return Err(RpcError::new(codes::SESSION_INVALID, "session revoked"));
        }
        if record.nonce != claims.nonce {
            return Err(RpcError::new(
                codes::SESSION_INVALID,
                "token has been superseded; use the most recent token",
            ));
        }
        Ok(claims)
    }

    /// Mark a session revoked and drop its token file. Returns false if unknown.
    pub fn revoke(&mut self, session: &str) -> bool {
        match self.sessions.get_mut(session) {
            Some(record) => {
                record.info.revoked = true;
                if let Some(dir) = &self.dir {
                    let _ = fs::remove_file(dir.join(format!("{session}.token")));
                }
                true
            }
            None => false,
        }
    }

    pub fn list(&mut self, now: u64) -> Vec<SessionInfo> {
        self.purge_expired(now);
        let mut sessions: Vec<SessionInfo> =
            self.sessions.values().map(|r| r.info.clone()).collect();
        sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.session.cmp(&b.session)));
        sessions
    }

    pub fn purge_expired(&mut self, now: u64) -> usize {
        let expired: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, record)| record.info.expires_at <= now || record.info.revoked)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            if let Some(record) = self.sessions.remove(id) {
                if let Some(dir) = &self.dir {
                    let _ = fs::remove_file(dir.join(format!("{id}.token")));
                }
                if !record.info.revoked {
                    info!(session = %id, "session expired");
                }
            }
        }
        expired.len()
    }

    /// Grant additional capabilities, returning a fresh token. The old token's
    /// nonce stops matching, so it can no longer be used.
    pub fn escalate(
        &mut self,
        token: &str,
        extra: &[Capability],
        now: u64,
    ) -> Result<(String, TokenClaims), RpcError> {
        let claims = self.verify(token, now)?;
        let mut caps = claims.caps.clone();
        for cap in dedup_caps(extra) {
            if !caps.contains(&cap) {
                caps.push(cap);
            }
        }

        let new_claims = TokenClaims {
            v: PROTOCOL_VERSION,
            session: claims.session.clone(),
            subject: claims.subject.clone(),
            caps: caps.clone(),
            iat: now,
            exp: claims.exp,
            nonce: random_hex(16),
        };
        let new_token = self.sign_claims(&new_claims)?;

        let record = self
            .sessions
            .get_mut(&claims.session)
            .ok_or_else(|| RpcError::new(codes::SESSION_INVALID, "unknown session"))?;
        record.info.caps = caps;
        record.nonce = new_claims.nonce.clone();
        self.persist_token(&new_claims.session, &new_token);

        Ok((new_token, new_claims))
    }

    fn sign_claims(&self, claims: &TokenClaims) -> anyhow::Result<String> {
        let payload = serde_json::to_vec(claims)?;
        let signature = self.signing.sign(&signing_message(&payload));
        Ok(encode_token_bytes(&payload, &signature.to_bytes()))
    }

    fn persist_token(&self, session: &str, token: &str) {
        let Some(dir) = &self.dir else { return };
        if let Err(error) = fs::create_dir_all(dir) {
            warn!(error = %error, dir = %dir.display(), "cannot create session directory");
            return;
        }
        let path = dir.join(format!("{session}.token"));
        if let Err(error) = fs::write(&path, token) {
            warn!(error = %error, path = %path.display(), "cannot write session token");
            return;
        }
        restrict_permissions(&path);
    }
}

fn dedup_caps(caps: &[Capability]) -> Vec<Capability> {
    let mut out = Vec::new();
    for cap in caps {
        if !out.contains(cap) {
            out.push(*cap);
        }
    }
    out
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buffer);
    hex::encode(buffer)
}

fn load_or_create_key(path: &Path) -> anyhow::Result<SigningKey> {
    if path.exists() {
        let text = fs::read_to_string(path)?;
        let bytes = hex::decode(text.trim())
            .map_err(|_| anyhow::anyhow!("{} is not a hex-encoded Ed25519 key", path.display()))?;
        let key_bytes: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("{} must contain exactly 32 bytes", path.display()))?;
        return Ok(SigningKey::from_bytes(&key_bytes));
    }

    let key = SigningKey::generate(&mut OsRng);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(path, hex::encode(key.to_bytes())) {
        Ok(()) => {
            restrict_permissions(path);
            info!(path = %path.display(), "generated session signing key");
        }
        Err(error) => warn!(
            error = %error,
            path = %path.display(),
            "cannot persist signing key; using an ephemeral key"
        ),
    }
    Ok(key)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use lutra_proto::DEFAULT_TOKEN_TTL_SECS;

    fn manager() -> SessionManager {
        SessionManager::new(None, None, DEFAULT_TOKEN_TTL_SECS).unwrap()
    }

    #[test]
    fn token_round_trip_carries_caps() {
        let mut manager = manager();
        let (info, claims, token) = manager
            .create("agent:test", &[Capability::FsRead, Capability::Net], None, 1000)
            .unwrap();
        assert_eq!(info.expires_at, 1000 + DEFAULT_TOKEN_TTL_SECS);
        assert_eq!(claims.subject, "agent:test");

        let verified = manager.verify(&token, 1001).unwrap();
        assert_eq!(verified.session, info.session);
        assert!(verified.has_cap(Capability::Net));
    }

    #[test]
    fn expired_tokens_are_rejected() {
        let mut manager = manager();
        let (_, claims, token) = manager
            .create("agent:test", &[Capability::FsRead], Some(60), 1000)
            .unwrap();
        let err = manager.verify(&token, claims.exp + 1).unwrap_err();
        assert_eq!(err.code, codes::SESSION_EXPIRED);
    }

    #[test]
    fn tampered_claims_break_the_signature() {
        let mut manager = manager();
        let (_, _, token) = manager
            .create("agent:test", &[Capability::FsRead], None, 1000)
            .unwrap();

        let raw = decode_token(&token).unwrap();
        let mut claims: TokenClaims = serde_json::from_slice(&raw.payload).unwrap();
        claims.subject = "agent:evil".into();
        let payload = serde_json::to_vec(&claims).unwrap();
        let forged = encode_token_bytes(&payload, &raw.signature);

        let err = manager.verify(&forged, 1001).unwrap_err();
        assert_eq!(err.code, codes::SESSION_INVALID);
    }

    #[test]
    fn revoked_sessions_are_rejected() {
        let mut manager = manager();
        let (info, _, token) = manager
            .create("agent:test", &[Capability::FsRead], None, 1000)
            .unwrap();
        assert!(manager.revoke(&info.session));
        let err = manager.verify(&token, 1001).unwrap_err();
        assert_eq!(err.code, codes::SESSION_INVALID);
    }

    #[test]
    fn escalation_adds_caps_and_rotates_nonce() {
        let mut manager = manager();
        let (info, _, token) = manager
            .create("agent:test", &[Capability::FsRead], None, 1000)
            .unwrap();

        let (new_token, new_claims) = manager
            .escalate(&token, &[Capability::Exec], 1100)
            .unwrap();
        assert!(new_claims.has_cap(Capability::Exec));
        assert_eq!(new_claims.session, info.session);
        assert!(manager.verify(&new_token, 1101).is_ok());

        let err = manager.verify(&token, 1101).unwrap_err();
        assert_eq!(err.code, codes::SESSION_INVALID);
    }
}
