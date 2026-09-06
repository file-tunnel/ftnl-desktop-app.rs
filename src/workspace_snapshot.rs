//! Authenticated, bounded workspace snapshot envelopes.
//!
//! This module is deliberately a codec boundary, not an automatic history
//! store. Callers must provide a 32-byte key from a platform-approved secret
//! provider before opting into persistence. Plaintext clipboard content never
//! appears in errors, logs, or the envelope metadata.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::workspace::{
    CaptureState, ClipboardItem, ClipboardWorkspace, RetentionPolicy, WorkspaceError,
};

pub const SNAPSHOT_ASSOCIATED_DATA: &str = "file-tunnel.desktop-workspace.v1";
pub const SNAPSHOT_ALGORITHM: &str = "xchacha20-poly1305";
pub const SNAPSHOT_MAX_ENVELOPE_BYTES: usize = 8 * 1024 * 1024;
pub const SNAPSHOT_MAX_PLAINTEXT_BYTES: usize = 6 * 1024 * 1024;

const SNAPSHOT_SCHEMA_VERSION: u8 = 1;
const XCHACHA20_KEY_BYTES: usize = 32;
const XCHACHA20_NONCE_BYTES: usize = 24;
const POLY1305_TAG_BYTES: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("the snapshot key is invalid")]
    InvalidKey,
    #[error("the snapshot envelope is invalid")]
    InvalidEnvelope,
    #[error("the snapshot is too large")]
    TooLarge,
    #[error("the snapshot encoding is invalid")]
    InvalidEncoding,
    #[error("the snapshot could not be authenticated")]
    AuthenticationFailed,
    #[error("the workspace snapshot is invalid")]
    InvalidWorkspace,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    document_type: String,
    schema_version: u8,
    algorithm: String,
    associated_data: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotWire {
    document_type: String,
    schema_version: u8,
    revision: u64,
    capture_state: String,
    retention: RetentionWire,
    search_query: String,
    excluded_source_fingerprints: Vec<String>,
    items: Vec<ItemWire>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RetentionWire {
    max_items: usize,
    max_age_days: u16,
    deduplicate: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ItemWire {
    item_id: String,
    content_kind: String,
    text: String,
    content_sha256: String,
    byte_size: usize,
    captured_at: String,
    is_pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_fingerprint: Option<String>,
}

/// Encrypt a workspace snapshot using a freshly generated nonce.
pub fn encrypt_snapshot(
    workspace: &ClipboardWorkspace,
    key: &[u8],
) -> Result<Vec<u8>, SnapshotError> {
    let mut nonce = [0_u8; XCHACHA20_NONCE_BYTES];
    OsRng.fill_bytes(&mut nonce);
    encrypt_snapshot_with_nonce(workspace, key, nonce)
}

/// Encrypt a workspace snapshot with a caller-supplied nonce for deterministic tests.
pub fn encrypt_snapshot_with_nonce(
    workspace: &ClipboardWorkspace,
    key: &[u8],
    nonce: [u8; XCHACHA20_NONCE_BYTES],
) -> Result<Vec<u8>, SnapshotError> {
    let plaintext = encode_snapshot(workspace)?;
    if plaintext.len() > SNAPSHOT_MAX_PLAINTEXT_BYTES {
        return Err(SnapshotError::TooLarge);
    }
    let cipher = cipher_for_key(key)?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: SNAPSHOT_ASSOCIATED_DATA.as_bytes(),
            },
        )
        .map_err(|_| SnapshotError::AuthenticationFailed)?;
    let envelope = Envelope {
        document_type: "encrypted_workspace_snapshot".to_owned(),
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        algorithm: SNAPSHOT_ALGORITHM.to_owned(),
        associated_data: SNAPSHOT_ASSOCIATED_DATA.to_owned(),
        nonce: URL_SAFE_NO_PAD.encode(nonce),
        ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
    };
    let encoded = serde_json::to_vec(&envelope).map_err(|_| SnapshotError::InvalidEncoding)?;
    if encoded.len() > SNAPSHOT_MAX_ENVELOPE_BYTES {
        return Err(SnapshotError::TooLarge);
    }
    Ok(encoded)
}

/// Decrypt and strictly validate a workspace snapshot. Retention is re-applied at load time.
pub fn decrypt_snapshot(
    encoded: &[u8],
    key: &[u8],
    now: SystemTime,
) -> Result<ClipboardWorkspace, SnapshotError> {
    if encoded.len() > SNAPSHOT_MAX_ENVELOPE_BYTES {
        return Err(SnapshotError::TooLarge);
    }
    let envelope: Envelope =
        serde_json::from_slice(encoded).map_err(|_| SnapshotError::InvalidEnvelope)?;
    if envelope.document_type != "encrypted_workspace_snapshot"
        || envelope.schema_version != SNAPSHOT_SCHEMA_VERSION
        || envelope.algorithm != SNAPSHOT_ALGORITHM
        || envelope.associated_data != SNAPSHOT_ASSOCIATED_DATA
    {
        return Err(SnapshotError::InvalidEnvelope);
    }
    let nonce = decode_exact::<XCHACHA20_NONCE_BYTES>(&envelope.nonce)?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(envelope.ciphertext.as_bytes())
        .map_err(|_| SnapshotError::InvalidEncoding)?;
    if ciphertext.len() < POLY1305_TAG_BYTES
        || ciphertext.len() > SNAPSHOT_MAX_PLAINTEXT_BYTES + POLY1305_TAG_BYTES
    {
        return Err(SnapshotError::TooLarge);
    }
    let cipher = cipher_for_key(key)?;
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: SNAPSHOT_ASSOCIATED_DATA.as_bytes(),
            },
        )
        .map_err(|_| SnapshotError::AuthenticationFailed)?;
    if plaintext.len() > SNAPSHOT_MAX_PLAINTEXT_BYTES {
        return Err(SnapshotError::TooLarge);
    }
    decode_snapshot(&plaintext, now)
}

fn cipher_for_key(key: &[u8]) -> Result<XChaCha20Poly1305, SnapshotError> {
    if key.len() != XCHACHA20_KEY_BYTES {
        return Err(SnapshotError::InvalidKey);
    }
    Ok(XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(
        key,
    )))
}

fn encode_snapshot(workspace: &ClipboardWorkspace) -> Result<Vec<u8>, SnapshotError> {
    let wire = SnapshotWire {
        document_type: "workspace_snapshot".to_owned(),
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        revision: workspace.revision(),
        capture_state: match workspace.capture_state() {
            CaptureState::Active => "active".to_owned(),
            CaptureState::Paused => "paused".to_owned(),
        },
        retention: RetentionWire {
            max_items: workspace.retention().max_items,
            max_age_days: workspace.retention().max_age_days,
            deduplicate: workspace.retention().deduplicate,
        },
        search_query: workspace.search_query().to_owned(),
        excluded_source_fingerprints: workspace
            .excluded_source_fingerprints()
            .iter()
            .cloned()
            .collect(),
        items: workspace
            .items()
            .iter()
            .map(|item| {
                Ok(ItemWire {
                    item_id: item.item_id.to_string(),
                    content_kind: "text".to_owned(),
                    text: item.text.clone(),
                    content_sha256: item.content_sha256.clone(),
                    byte_size: item.byte_size,
                    captured_at: format_timestamp(item.captured_at)?,
                    is_pinned: item.is_pinned,
                    source_fingerprint: item.source_fingerprint.clone(),
                })
            })
            .collect::<Result<_, SnapshotError>>()?,
    };
    serde_json::to_vec(&wire).map_err(|_| SnapshotError::InvalidEncoding)
}

fn decode_snapshot(encoded: &[u8], now: SystemTime) -> Result<ClipboardWorkspace, SnapshotError> {
    let wire: SnapshotWire =
        serde_json::from_slice(encoded).map_err(|_| SnapshotError::InvalidEnvelope)?;
    if wire.document_type != "workspace_snapshot" || wire.schema_version != SNAPSHOT_SCHEMA_VERSION
    {
        return Err(SnapshotError::InvalidEnvelope);
    }
    let capture_state = match wire.capture_state.as_str() {
        "active" => CaptureState::Active,
        "paused" => CaptureState::Paused,
        _ => return Err(SnapshotError::InvalidWorkspace),
    };
    let exclusions = wire
        .excluded_source_fingerprints
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if exclusions.len() != wire.excluded_source_fingerprints.len() {
        return Err(SnapshotError::InvalidWorkspace);
    }
    let items = wire
        .items
        .into_iter()
        .map(|item| {
            if item.content_kind != "text" {
                return Err(SnapshotError::InvalidWorkspace);
            }
            let item_id =
                Uuid::parse_str(&item.item_id).map_err(|_| SnapshotError::InvalidWorkspace)?;
            if item_id.to_string() != item.item_id {
                return Err(SnapshotError::InvalidWorkspace);
            }
            Ok(ClipboardItem {
                item_id,
                text: item.text,
                content_sha256: item.content_sha256,
                byte_size: item.byte_size,
                captured_at: parse_timestamp(&item.captured_at)?,
                is_pinned: item.is_pinned,
                source_fingerprint: item.source_fingerprint,
            })
        })
        .collect::<Result<Vec<_>, SnapshotError>>()?;
    ClipboardWorkspace::from_snapshot_parts(
        wire.revision,
        capture_state,
        RetentionPolicy {
            max_items: wire.retention.max_items,
            max_age_days: wire.retention.max_age_days,
            deduplicate: wire.retention.deduplicate,
        },
        wire.search_query,
        exclusions,
        items,
        now,
    )
    .map_err(|error| match error {
        WorkspaceError::InvalidSnapshot => SnapshotError::InvalidWorkspace,
        _ => SnapshotError::InvalidWorkspace,
    })
}

fn decode_exact<const N: usize>(value: &str) -> Result<[u8; N], SnapshotError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value.as_bytes())
        .map_err(|_| SnapshotError::InvalidEncoding)?;
    bytes.try_into().map_err(|_| SnapshotError::InvalidEnvelope)
}

fn format_timestamp(value: SystemTime) -> Result<String, SnapshotError> {
    let duration = value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SnapshotError::InvalidWorkspace)?;
    let nanos =
        i128::from(duration.as_secs()) * 1_000_000_000 + i128::from(duration.subsec_nanos());
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .map_err(|_| SnapshotError::InvalidWorkspace)?
        .format(&Rfc3339)
        .map_err(|_| SnapshotError::InvalidWorkspace)
}

fn parse_timestamp(value: &str) -> Result<SystemTime, SnapshotError> {
    let timestamp = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| SnapshotError::InvalidWorkspace)?
        .unix_timestamp_nanos();
    if timestamp < 0 {
        return Err(SnapshotError::InvalidWorkspace);
    }
    let seconds =
        u64::try_from(timestamp / 1_000_000_000).map_err(|_| SnapshotError::InvalidWorkspace)?;
    let nanos =
        u32::try_from(timestamp % 1_000_000_000).map_err(|_| SnapshotError::InvalidWorkspace)?;
    UNIX_EPOCH
        .checked_add(Duration::new(seconds, nanos))
        .ok_or(SnapshotError::InvalidWorkspace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{content_sha256_hex, WorkspaceAction};

    fn at(day: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(day * 24 * 60 * 60)
    }

    fn workspace() -> ClipboardWorkspace {
        let mut workspace = ClipboardWorkspace::default();
        workspace
            .apply(Uuid::new_v4(), 0, WorkspaceAction::ResumeCapture, at(1))
            .unwrap();
        workspace
            .apply(
                Uuid::new_v4(),
                1,
                WorkspaceAction::IngestText {
                    item_id: Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap(),
                    text: "hello".to_owned(),
                    content_sha256: content_sha256_hex("hello"),
                    byte_size: 5,
                    captured_at: at(1),
                    source_fingerprint: None,
                },
                at(1),
            )
            .unwrap();
        workspace
    }

    #[test]
    fn encrypted_snapshot_round_trips_and_reapplies_retention() {
        let key = [0x11_u8; 32];
        let encoded = encrypt_snapshot_with_nonce(&workspace(), &key, [0x22_u8; 24]).unwrap();
        let decoded = decrypt_snapshot(&encoded, &key, at(1)).unwrap();
        assert_eq!(decoded.items().len(), 1);
        assert_eq!(decoded.items()[0].text, "hello");
        assert_eq!(decoded.revision(), 2);
    }

    #[test]
    fn wrong_key_and_tampering_never_reveal_plaintext() {
        let key = [0x11_u8; 32];
        let mut encoded = encrypt_snapshot_with_nonce(&workspace(), &key, [0x22_u8; 24]).unwrap();
        assert_eq!(
            decrypt_snapshot(&encoded, &[0x33_u8; 32], at(1))
                .unwrap_err()
                .to_string(),
            "the snapshot could not be authenticated"
        );
        let last = encoded.len() - 1;
        encoded[last] = if encoded[last] == b'a' { b'b' } else { b'a' };
        assert!(matches!(
            decrypt_snapshot(&encoded, &key, at(1)),
            Err(SnapshotError::InvalidEnvelope)
                | Err(SnapshotError::InvalidEncoding)
                | Err(SnapshotError::AuthenticationFailed)
        ));
        assert!(!String::from_utf8_lossy(&encoded).contains("hello"));
    }

    #[test]
    fn keys_and_envelopes_are_bounded() {
        assert!(matches!(
            encrypt_snapshot(&workspace(), &[0_u8; 31]),
            Err(SnapshotError::InvalidKey)
        ));
        assert!(matches!(
            decrypt_snapshot(b"{}", &[0_u8; 32], at(1)),
            Err(SnapshotError::InvalidEnvelope)
        ));
    }
}
