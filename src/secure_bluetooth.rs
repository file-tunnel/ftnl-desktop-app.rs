//! Application-layer security for an untrusted Bluetooth transport.
//!
//! The operating-system Bluetooth link is treated as an attacker-controlled
//! byte stream.  A fresh X25519 handshake, transcript-bound HKDF-SHA256 key
//! derivation, and an explicit short-authentication-string (SAS) comparison
//! are required before application payloads can be exchanged.  Platform
//! adapters belong outside this module and may only transport the canonical
//! hello and encrypted-frame bytes.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use hkdf::Hkdf;
use rand_core::{CryptoRng, OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use thiserror::Error;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

const PROTOCOL_LABEL: &[u8] = b"ftnl-secure-bluetooth-v1";
const HELLO_MAGIC: &[u8; 4] = b"FTBH";
const FRAME_MAGIC: &[u8; 4] = b"FTBE";
const VERSION: u8 = 1;
const HELLO_FIXED_LEN: usize = 4 + 1 + 1 + 16 + 16 + 1 + 32;
const FRAME_HEADER_LEN: usize = 4 + 1 + 1 + 8 + 2;
const AEAD_TAG_LEN: usize = 16;
const KEY_MATERIAL_LEN: usize = 32 + 32 + 4 + 4 + 32;
const MIN_DEVICE_ID_LEN: usize = 8;
const MAX_DEVICE_ID_LEN: usize = 64;

/// Maximum plaintext carried by one encrypted proximity frame.
pub const MAX_BLUETOOTH_PLAINTEXT_LEN: usize = 16 * 1024;
/// Maximum frames in either direction before a fresh handshake is required.
pub const MAX_BLUETOOTH_FRAMES_PER_DIRECTION: u64 = 4096;
/// A user must compare the SAS within this window.
pub const BLUETOOTH_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// Confirmed sessions are short-lived and never resume from disk.
pub const BLUETOOTH_SESSION_LIFETIME: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BluetoothRole {
    Initiator = 0,
    Responder = 1,
}

impl BluetoothRole {
    fn from_byte(value: u8) -> Result<Self, SecureBluetoothError> {
        match value {
            0 => Ok(Self::Initiator),
            1 => Ok(Self::Responder),
            _ => Err(SecureBluetoothError::InvalidHello),
        }
    }

    fn opposite(self) -> Self {
        match self {
            Self::Initiator => Self::Responder,
            Self::Responder => Self::Initiator,
        }
    }
}

/// Message classes allowed on an active proximity session.
///
/// The payload remains opaque to this layer.  Callers map these classes to the
/// JSON-Schema payloads for Shared Auth relay, peer information, and signed
/// update-manifest offers.  No bearer credential or binary update is allowed
/// to be placed in an advertisement or hello.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BluetoothMessageType {
    SharedAuth = 1,
    PeerInfo = 2,
    UpdateManifest = 3,
}

impl BluetoothMessageType {
    fn from_byte(value: u8) -> Result<Self, SecureBluetoothError> {
        match value {
            1 => Ok(Self::SharedAuth),
            2 => Ok(Self::PeerInfo),
            3 => Ok(Self::UpdateManifest),
            _ => Err(SecureBluetoothError::InvalidFrame),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SecureBluetoothError {
    #[error("invalid Bluetooth handshake message")]
    InvalidHello,
    #[error("Bluetooth peer does not match this handshake")]
    InvalidPeer,
    #[error("Bluetooth key agreement failed")]
    KeyAgreement,
    #[error("Bluetooth session key derivation failed")]
    KeyDerivation,
    #[error("Bluetooth authentication numbers did not match")]
    AuthenticationMismatch,
    #[error("Bluetooth confirmation window expired")]
    ConfirmationExpired,
    #[error("Bluetooth session expired")]
    SessionExpired,
    #[error("Bluetooth session frame limit reached")]
    FrameLimit,
    #[error("Bluetooth message is too large")]
    MessageTooLarge,
    #[error("invalid encrypted Bluetooth frame")]
    InvalidFrame,
    #[error("replayed or out-of-order Bluetooth frame")]
    ReplayOrOutOfOrder,
    #[error("Bluetooth frame encryption failed")]
    Encrypt,
    #[error("Bluetooth frame authentication failed")]
    Decrypt,
}

/// Public, non-secret handshake contribution suitable for transport over BLE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothHello {
    role: BluetoothRole,
    session_id: [u8; 16],
    contribution: [u8; 16],
    device_id: String,
    public_key: [u8; 32],
}

impl BluetoothHello {
    pub fn role(&self) -> BluetoothRole {
        self.role
    }

    pub fn session_id(&self) -> &[u8; 16] {
        &self.session_id
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }

    /// Canonical binary encoding for transport and transcript hashing.
    pub fn encode(&self) -> Vec<u8> {
        let id = self.device_id.as_bytes();
        let mut out = Vec::with_capacity(HELLO_FIXED_LEN + id.len());
        out.extend_from_slice(HELLO_MAGIC);
        out.push(VERSION);
        out.push(self.role as u8);
        out.extend_from_slice(&self.session_id);
        out.extend_from_slice(&self.contribution);
        out.push(id.len() as u8);
        out.extend_from_slice(id);
        out.extend_from_slice(&self.public_key);
        out
    }

    /// Parse an untrusted peer hello with exact length and character bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, SecureBluetoothError> {
        if bytes.len() < HELLO_FIXED_LEN
            || bytes.get(..4) != Some(HELLO_MAGIC)
            || bytes[4] != VERSION
        {
            return Err(SecureBluetoothError::InvalidHello);
        }
        let role = BluetoothRole::from_byte(bytes[5])?;
        let mut session_id = [0u8; 16];
        session_id.copy_from_slice(&bytes[6..22]);
        let mut contribution = [0u8; 16];
        contribution.copy_from_slice(&bytes[22..38]);
        let id_len = bytes[38] as usize;
        if !(MIN_DEVICE_ID_LEN..=MAX_DEVICE_ID_LEN).contains(&id_len)
            || bytes.len() != HELLO_FIXED_LEN + id_len
        {
            return Err(SecureBluetoothError::InvalidHello);
        }
        let id_end = 39 + id_len;
        let device_id = std::str::from_utf8(&bytes[39..id_end])
            .map_err(|_| SecureBluetoothError::InvalidHello)?
            .to_owned();
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(&bytes[id_end..id_end + 32]);
        let hello = Self {
            role,
            session_id,
            contribution,
            device_id,
            public_key,
        };
        hello.validate()?;
        Ok(hello)
    }

    fn validate(&self) -> Result<(), SecureBluetoothError> {
        if !device_id_is_valid(&self.device_id)
            || self.session_id.ct_eq(&[0u8; 16]).into()
            || self.contribution.ct_eq(&[0u8; 16]).into()
            || self.public_key.ct_eq(&[0u8; 32]).into()
        {
            return Err(SecureBluetoothError::InvalidHello);
        }
        Ok(())
    }
}

/// One-use local handshake state.  The private key is consumed by `derive`.
pub struct BluetoothPairing {
    hello: BluetoothHello,
    secret: Option<StaticSecret>,
}

impl BluetoothPairing {
    pub fn initiator(device_id: impl Into<String>) -> Result<Self, SecureBluetoothError> {
        let mut rng = OsRng;
        let mut session_id = [0u8; 16];
        rng.fill_bytes(&mut session_id);
        Self::new(
            BluetoothRole::Initiator,
            device_id.into(),
            session_id,
            &mut rng,
        )
    }

    pub fn responder(
        device_id: impl Into<String>,
        initiator: &BluetoothHello,
    ) -> Result<Self, SecureBluetoothError> {
        initiator.validate()?;
        if initiator.role != BluetoothRole::Initiator {
            return Err(SecureBluetoothError::InvalidPeer);
        }
        let mut rng = OsRng;
        Self::new(
            BluetoothRole::Responder,
            device_id.into(),
            initiator.session_id,
            &mut rng,
        )
    }

    fn new<R: RngCore + CryptoRng>(
        role: BluetoothRole,
        device_id: String,
        session_id: [u8; 16],
        rng: &mut R,
    ) -> Result<Self, SecureBluetoothError> {
        if !device_id_is_valid(&device_id) || session_id.ct_eq(&[0u8; 16]).into() {
            return Err(SecureBluetoothError::InvalidHello);
        }
        let secret = StaticSecret::random_from_rng(&mut *rng);
        let public_key = PublicKey::from(&secret).to_bytes();
        let mut contribution = [0u8; 16];
        rng.fill_bytes(&mut contribution);
        if contribution.ct_eq(&[0u8; 16]).into() {
            return Err(SecureBluetoothError::KeyAgreement);
        }
        Ok(Self {
            hello: BluetoothHello {
                role,
                session_id,
                contribution,
                device_id,
                public_key,
            },
            secret: Some(secret),
        })
    }

    pub fn hello(&self) -> &BluetoothHello {
        &self.hello
    }

    /// Consume the ephemeral secret and derive a pending, unconfirmed session.
    pub fn derive(
        mut self,
        peer: &BluetoothHello,
    ) -> Result<PendingBluetoothSession, SecureBluetoothError> {
        self.hello.validate()?;
        peer.validate()?;
        if peer.role != self.hello.role.opposite()
            || peer.session_id != self.hello.session_id
            || peer.device_id == self.hello.device_id
            || peer.public_key == self.hello.public_key
        {
            return Err(SecureBluetoothError::InvalidPeer);
        }

        let secret = self
            .secret
            .take()
            .ok_or(SecureBluetoothError::KeyAgreement)?;
        let shared = secret.diffie_hellman(&PublicKey::from(peer.public_key));
        if shared.as_bytes().ct_eq(&[0u8; 32]).into() {
            return Err(SecureBluetoothError::KeyAgreement);
        }

        let (initiator, responder) = match self.hello.role {
            BluetoothRole::Initiator => (&self.hello, peer),
            BluetoothRole::Responder => (peer, &self.hello),
        };
        let transcript = handshake_transcript(initiator, responder);
        let transcript_hash: [u8; 32] = Sha256::digest(&transcript).into();
        let hkdf = Hkdf::<Sha256>::new(Some(&transcript_hash), shared.as_bytes());
        let mut material = Zeroizing::new([0u8; KEY_MATERIAL_LEN]);
        hkdf.expand(PROTOCOL_LABEL, &mut *material)
            .map_err(|_| SecureBluetoothError::KeyDerivation)?;

        let mut initiator_key = Zeroizing::new([0u8; 32]);
        initiator_key.copy_from_slice(&material[..32]);
        let mut responder_key = Zeroizing::new([0u8; 32]);
        responder_key.copy_from_slice(&material[32..64]);
        let mut initiator_prefix = [0u8; 4];
        initiator_prefix.copy_from_slice(&material[64..68]);
        let mut responder_prefix = [0u8; 4];
        responder_prefix.copy_from_slice(&material[68..72]);
        let sas_number =
            u32::from_be_bytes(material[72..76].try_into().expect("fixed slice")) % 1_000_000;
        let sas = format!("{sas_number:06}");

        let (tx_key, rx_key, tx_prefix, rx_prefix) = match self.hello.role {
            BluetoothRole::Initiator => (
                initiator_key,
                responder_key,
                initiator_prefix,
                responder_prefix,
            ),
            BluetoothRole::Responder => (
                responder_key,
                initiator_key,
                responder_prefix,
                initiator_prefix,
            ),
        };

        Ok(PendingBluetoothSession {
            local_role: self.hello.role,
            session_id: self.hello.session_id,
            local_device_id: self.hello.device_id,
            peer_device_id: peer.device_id.clone(),
            transcript_hash,
            tx_key,
            rx_key,
            tx_prefix,
            rx_prefix,
            sas,
            created_at: Instant::now(),
        })
    }

    #[cfg(test)]
    fn fixed(
        role: BluetoothRole,
        device_id: &str,
        session_id: [u8; 16],
        contribution: [u8; 16],
        private_key: [u8; 32],
    ) -> Self {
        let secret = StaticSecret::from(private_key);
        let public_key = PublicKey::from(&secret).to_bytes();
        Self {
            hello: BluetoothHello {
                role,
                session_id,
                contribution,
                device_id: device_id.to_owned(),
                public_key,
            },
            secret: Some(secret),
        }
    }
}

/// Derived keys that cannot encrypt or decrypt until the user confirms SAS.
pub struct PendingBluetoothSession {
    local_role: BluetoothRole,
    session_id: [u8; 16],
    local_device_id: String,
    peer_device_id: String,
    transcript_hash: [u8; 32],
    tx_key: Zeroizing<[u8; 32]>,
    rx_key: Zeroizing<[u8; 32]>,
    tx_prefix: [u8; 4],
    rx_prefix: [u8; 4],
    sas: String,
    created_at: Instant,
}

impl PendingBluetoothSession {
    pub fn local_device_id(&self) -> &str {
        &self.local_device_id
    }

    pub fn peer_device_id(&self) -> &str {
        &self.peer_device_id
    }

    pub fn short_authentication_string(&self) -> &str {
        &self.sas
    }

    /// Confirm the user-observed SAS.  Wrong, malformed, or late values fail.
    pub fn confirm(
        self,
        observed_sas: &str,
    ) -> Result<SecureBluetoothSession, SecureBluetoothError> {
        if self.created_at.elapsed() > BLUETOOTH_CONFIRMATION_TIMEOUT {
            return Err(SecureBluetoothError::ConfirmationExpired);
        }
        let syntax_ok = observed_sas.len() == 6 && observed_sas.bytes().all(|b| b.is_ascii_digit());
        if !syntax_ok || !bool::from(self.sas.as_bytes().ct_eq(observed_sas.as_bytes())) {
            return Err(SecureBluetoothError::AuthenticationMismatch);
        }
        Ok(SecureBluetoothSession {
            local_role: self.local_role,
            session_id: self.session_id,
            transcript_hash: self.transcript_hash,
            tx_key: self.tx_key,
            rx_key: self.rx_key,
            tx_prefix: self.tx_prefix,
            rx_prefix: self.rx_prefix,
            tx_counter: 0,
            rx_counter: 0,
            created_at: Instant::now(),
        })
    }
}

/// Confirmed, short-lived encrypted session layered over untrusted Bluetooth.
pub struct SecureBluetoothSession {
    local_role: BluetoothRole,
    session_id: [u8; 16],
    transcript_hash: [u8; 32],
    tx_key: Zeroizing<[u8; 32]>,
    rx_key: Zeroizing<[u8; 32]>,
    tx_prefix: [u8; 4],
    rx_prefix: [u8; 4],
    tx_counter: u64,
    rx_counter: u64,
    created_at: Instant,
}

impl SecureBluetoothSession {
    pub fn local_role(&self) -> BluetoothRole {
        self.local_role
    }

    /// Encrypt one opaque application payload into a canonical frame.
    pub fn encrypt(
        &mut self,
        message_type: BluetoothMessageType,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, SecureBluetoothError> {
        self.ensure_active()?;
        if plaintext.len() > MAX_BLUETOOTH_PLAINTEXT_LEN {
            return Err(SecureBluetoothError::MessageTooLarge);
        }
        let counter = self
            .tx_counter
            .checked_add(1)
            .ok_or(SecureBluetoothError::FrameLimit)?;
        if counter > MAX_BLUETOOTH_FRAMES_PER_DIRECTION {
            return Err(SecureBluetoothError::FrameLimit);
        }
        let ciphertext_len = plaintext
            .len()
            .checked_add(AEAD_TAG_LEN)
            .ok_or(SecureBluetoothError::MessageTooLarge)?;
        let header = frame_header(message_type, counter, ciphertext_len)?;
        let nonce = nonce(self.tx_prefix, counter);
        let aad = associated_data(&self.transcript_hash, &self.session_id, &header);
        let cipher = ChaCha20Poly1305::new((&*self.tx_key).into());
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| SecureBluetoothError::Encrypt)?;
        self.tx_counter = counter;

        let mut frame = Vec::with_capacity(header.len() + ciphertext.len());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(&ciphertext);
        Ok(frame)
    }

    /// Decrypt one complete frame.  Counters must be exact and advance only
    /// after authentication succeeds, preventing replay and reordering.
    pub fn decrypt(
        &mut self,
        frame: &[u8],
    ) -> Result<(BluetoothMessageType, Zeroizing<Vec<u8>>), SecureBluetoothError> {
        self.ensure_active()?;
        if frame.len() < FRAME_HEADER_LEN + AEAD_TAG_LEN
            || frame.get(..4) != Some(FRAME_MAGIC)
            || frame[4] != VERSION
        {
            return Err(SecureBluetoothError::InvalidFrame);
        }
        let message_type = BluetoothMessageType::from_byte(frame[5])?;
        let counter = u64::from_be_bytes(
            frame[6..14]
                .try_into()
                .map_err(|_| SecureBluetoothError::InvalidFrame)?,
        );
        let ciphertext_len = u16::from_be_bytes(
            frame[14..16]
                .try_into()
                .map_err(|_| SecureBluetoothError::InvalidFrame)?,
        ) as usize;
        if counter != self.rx_counter.saturating_add(1) {
            return Err(SecureBluetoothError::ReplayOrOutOfOrder);
        }
        if counter > MAX_BLUETOOTH_FRAMES_PER_DIRECTION {
            return Err(SecureBluetoothError::FrameLimit);
        }
        if ciphertext_len < AEAD_TAG_LEN
            || ciphertext_len > MAX_BLUETOOTH_PLAINTEXT_LEN + AEAD_TAG_LEN
            || frame.len() != FRAME_HEADER_LEN + ciphertext_len
        {
            return Err(SecureBluetoothError::InvalidFrame);
        }
        let header = &frame[..FRAME_HEADER_LEN];
        let ciphertext = &frame[FRAME_HEADER_LEN..];
        let nonce = nonce(self.rx_prefix, counter);
        let aad = associated_data(&self.transcript_hash, &self.session_id, header);
        let cipher = ChaCha20Poly1305::new((&*self.rx_key).into());
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| SecureBluetoothError::Decrypt)?;
        self.rx_counter = counter;
        Ok((message_type, Zeroizing::new(plaintext)))
    }

    fn ensure_active(&self) -> Result<(), SecureBluetoothError> {
        if self.created_at.elapsed() > BLUETOOTH_SESSION_LIFETIME {
            Err(SecureBluetoothError::SessionExpired)
        } else {
            Ok(())
        }
    }
}

fn device_id_is_valid(value: &str) -> bool {
    (MIN_DEVICE_ID_LEN..=MAX_DEVICE_ID_LEN).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn handshake_transcript(initiator: &BluetoothHello, responder: &BluetoothHello) -> Vec<u8> {
    let initiator_bytes = initiator.encode();
    let responder_bytes = responder.encode();
    let mut transcript = Vec::with_capacity(
        PROTOCOL_LABEL.len() + 2 + initiator_bytes.len() + 2 + responder_bytes.len(),
    );
    transcript.extend_from_slice(PROTOCOL_LABEL);
    transcript.extend_from_slice(&(initiator_bytes.len() as u16).to_be_bytes());
    transcript.extend_from_slice(&initiator_bytes);
    transcript.extend_from_slice(&(responder_bytes.len() as u16).to_be_bytes());
    transcript.extend_from_slice(&responder_bytes);
    transcript
}

fn nonce(prefix: [u8; 4], counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(&prefix);
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

fn associated_data(transcript_hash: &[u8; 32], session_id: &[u8; 16], header: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(transcript_hash.len() + session_id.len() + header.len());
    aad.extend_from_slice(transcript_hash);
    aad.extend_from_slice(session_id);
    aad.extend_from_slice(header);
    aad
}

fn frame_header(
    message_type: BluetoothMessageType,
    counter: u64,
    ciphertext_len: usize,
) -> Result<[u8; FRAME_HEADER_LEN], SecureBluetoothError> {
    let ciphertext_len =
        u16::try_from(ciphertext_len).map_err(|_| SecureBluetoothError::MessageTooLarge)?;
    let mut header = [0u8; FRAME_HEADER_LEN];
    header[..4].copy_from_slice(FRAME_MAGIC);
    header[4] = VERSION;
    header[5] = message_type as u8;
    header[6..14].copy_from_slice(&counter.to_be_bytes());
    header[14..16].copy_from_slice(&ciphertext_len.to_be_bytes());
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INITIATOR_ID: &str = "desktop-a";
    const RESPONDER_ID: &str = "desktop-b";

    fn pending_pair() -> (PendingBluetoothSession, PendingBluetoothSession) {
        let initiator = BluetoothPairing::initiator(INITIATOR_ID).unwrap();
        let initiator_hello = initiator.hello().clone();
        let responder = BluetoothPairing::responder(RESPONDER_ID, &initiator_hello).unwrap();
        let responder_hello = responder.hello().clone();
        (
            initiator.derive(&responder_hello).unwrap(),
            responder.derive(&initiator_hello).unwrap(),
        )
    }

    #[test]
    fn hello_round_trips_and_rejects_trailing_bytes() {
        let pairing = BluetoothPairing::initiator(INITIATOR_ID).unwrap();
        let encoded = pairing.hello().encode();
        assert_eq!(BluetoothHello::decode(&encoded).unwrap(), *pairing.hello());

        let mut trailing = encoded;
        trailing.push(0);
        assert_eq!(
            BluetoothHello::decode(&trailing),
            Err(SecureBluetoothError::InvalidHello)
        );
    }

    #[test]
    fn sas_confirmation_is_required_before_frames() {
        let (initiator_pending, responder_pending) = pending_pair();
        let sas = responder_pending.short_authentication_string().to_owned();
        assert!(matches!(
            initiator_pending.confirm("000000"),
            Err(SecureBluetoothError::AuthenticationMismatch)
        ));
        let _ = responder_pending.confirm(&sas).unwrap();
    }

    #[test]
    fn peers_encrypt_in_both_directions_with_exact_counters() {
        let (initiator_pending, responder_pending) = pending_pair();
        let sas = initiator_pending.short_authentication_string().to_owned();
        let mut initiator = initiator_pending.confirm(&sas).unwrap();
        let mut responder = responder_pending.confirm(&sas).unwrap();

        let outbound = initiator
            .encrypt(BluetoothMessageType::PeerInfo, b"bounded peer info")
            .unwrap();
        let (kind, plaintext) = responder.decrypt(&outbound).unwrap();
        assert_eq!(kind, BluetoothMessageType::PeerInfo);
        assert_eq!(&*plaintext, b"bounded peer info");

        let reply = responder
            .encrypt(BluetoothMessageType::SharedAuth, b"opaque request")
            .unwrap();
        let (kind, plaintext) = initiator.decrypt(&reply).unwrap();
        assert_eq!(kind, BluetoothMessageType::SharedAuth);
        assert_eq!(&*plaintext, b"opaque request");

        assert_eq!(
            responder.decrypt(&outbound),
            Err(SecureBluetoothError::ReplayOrOutOfOrder)
        );
    }

    #[test]
    fn tampering_does_not_advance_receive_counter() {
        let (initiator_pending, responder_pending) = pending_pair();
        let sas = initiator_pending.short_authentication_string().to_owned();
        let mut initiator = initiator_pending.confirm(&sas).unwrap();
        let mut responder = responder_pending.confirm(&sas).unwrap();
        let valid = initiator
            .encrypt(BluetoothMessageType::UpdateManifest, b"signed metadata")
            .unwrap();
        let mut tampered = valid.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_eq!(
            responder.decrypt(&tampered),
            Err(SecureBluetoothError::Decrypt)
        );

        assert_eq!(
            responder.decrypt(&valid).unwrap().1.as_slice(),
            b"signed metadata"
        );
    }

    #[test]
    fn bounds_and_unknown_values_fail_closed() {
        let (initiator_pending, responder_pending) = pending_pair();
        let sas = initiator_pending.short_authentication_string().to_owned();
        let mut initiator = initiator_pending.confirm(&sas).unwrap();
        let mut responder = responder_pending.confirm(&sas).unwrap();
        assert_eq!(
            initiator.encrypt(
                BluetoothMessageType::PeerInfo,
                &[0u8; MAX_BLUETOOTH_PLAINTEXT_LEN + 1]
            ),
            Err(SecureBluetoothError::MessageTooLarge)
        );

        let mut frame = initiator
            .encrypt(BluetoothMessageType::PeerInfo, b"payload")
            .unwrap();
        frame[5] = 255;
        assert_eq!(
            responder.decrypt(&frame),
            Err(SecureBluetoothError::InvalidFrame)
        );
    }

    #[test]
    fn fixed_vector_is_stable_for_dart_conformance() {
        let initiator = BluetoothPairing::fixed(
            BluetoothRole::Initiator,
            "desktop-a",
            [1; 16],
            [2; 16],
            [3; 32],
        );
        let responder = BluetoothPairing::fixed(
            BluetoothRole::Responder,
            "desktop-b",
            [1; 16],
            [4; 16],
            [5; 32],
        );
        let initiator_hello = initiator.hello().clone();
        let responder_hello = responder.hello().clone();
        let initiator_pending = initiator.derive(&responder_hello).unwrap();
        let responder_pending = responder.derive(&initiator_hello).unwrap();
        assert_eq!(
            initiator_pending.short_authentication_string(),
            responder_pending.short_authentication_string()
        );
        let sas = initiator_pending.short_authentication_string().to_owned();
        let mut initiator = initiator_pending.confirm(&sas).unwrap();
        let mut responder = responder_pending.confirm(&sas).unwrap();
        let frame = initiator
            .encrypt(BluetoothMessageType::PeerInfo, b"cross-language")
            .unwrap();
        assert_eq!(
            responder.decrypt(&frame).unwrap().1.as_slice(),
            b"cross-language"
        );
        assert_eq!(
            hex_encode(&frame),
            "4654424501020000000000000001001e27099d19814472695338d697c059063622849adfb50bde364537bd94e35c"
        );
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
