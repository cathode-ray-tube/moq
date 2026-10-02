use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::{SigningKey, VerifyingKey};
use moq_secure::key_store::KeyStore;

use super::EncryptionError;
use crate::container::{FrameDecrypter, FrameEncrypter};

#[allow(dead_code)]
const ENCRYPTION_UNENCRYPTED: u8 = 0;
const ENCRYPTION_CHACHA20_POLY1305: u8 = 1;
const ENCRYPTION_AES_256_GCM: u8 = 2;

impl From<moq_secure::error::MoqSecureError> for EncryptionError {
    fn from(error: moq_secure::error::MoqSecureError) -> Self {
        use moq_secure::error::MoqSecureError;

        match error {
            MoqSecureError::InvalidMagic => Self::InvalidFrame,

            MoqSecureError::UnsupportedVersion(version) => {
                Self::UnsupportedVersion(version)
            }

            MoqSecureError::UnsupportedAlgorithm(algorithm) => {
                Self::UnsupportedAlgorithm(algorithm)
            }

            MoqSecureError::TruncatedFrame => Self::TruncatedFrame,

            MoqSecureError::CiphertextTooShort => {
                Self::CiphertextTooShort
            }

            MoqSecureError::InvalidPadLength => Self::InvalidPadLength,

            MoqSecureError::InvalidSigFlag(flag) => {
                Self::InvalidSigFlag(flag)
            }

            MoqSecureError::AeadAuthFailed => {
                Self::AuthenticationFailed
            }

            MoqSecureError::InvalidSignature => {
                Self::SignatureFailed
            }

            MoqSecureError::SigningMismatch => {
                Self::SigningMismatch
            }

            MoqSecureError::MissingSigSlot => {
                Self::MissingSigSlot
            }

            MoqSecureError::SignatureNotAllowedByNSigned => {
                Self::SignatureNotAllowedByNSigned
            }

            MoqSecureError::DecryptFailed => {
                Self::DecryptionFailed
            }

            MoqSecureError::InvalidKeyId(key_id) => {
                Self::InvalidKeyId(key_id)
            }

            MoqSecureError::ReplayDetected => {
                Self::ReplayDetected
            }
        }
    }
}

/// Encrypts each frame using moq-secure.
pub struct MoqSecureEncrypter {
    pub key_store: Arc<dyn KeyStore>,
    pub signing_key: SigningKey,
    pub key_id: u8,
    pub n_signed: u8,
    pub maybe_sign: bool,
    pub pad_len: u32,
    pub encryption_type: u8,

    /// Independent moq-secure encryption counter.
    ctr: u64,
}

impl MoqSecureEncrypter {
    /// Creates a ChaCha20-Poly1305 encrypter.
    pub fn new(
        key_store: Arc<dyn KeyStore>,
        signing_key: SigningKey,
        key_id: u8,
        n_signed: u8,
        maybe_sign: bool,
        pad_len: u32,
        initial_ctr: u64,
    ) -> Self {
        Self::new_with_encryption_type(
            key_store,
            signing_key,
            key_id,
            n_signed,
            maybe_sign,
            pad_len,
            initial_ctr,
            ENCRYPTION_CHACHA20_POLY1305,
        )
    }

    /// Creates an encrypter using the requested wire-level algorithm.
    pub fn new_with_encryption_type(
        key_store: Arc<dyn KeyStore>,
        signing_key: SigningKey,
        key_id: u8,
        n_signed: u8,
        maybe_sign: bool,
        pad_len: u32,
        initial_ctr: u64,
        encryption_type: u8,
    ) -> Self {
        Self {
            key_store,
            signing_key,
            key_id,
            n_signed,
            maybe_sign,
            pad_len,
            encryption_type,
            ctr: initial_ctr,
        }
    }

    /// Creates an AES-256-GCM encrypter.
    pub fn new_aes256_gcm(
        key_store: Arc<dyn KeyStore>,
        signing_key: SigningKey,
        key_id: u8,
        n_signed: u8,
        maybe_sign: bool,
        pad_len: u32,
        initial_ctr: u64,
    ) -> Self {
        Self::new_with_encryption_type(
            key_store,
            signing_key,
            key_id,
            n_signed,
            maybe_sign,
            pad_len,
            initial_ctr,
            ENCRYPTION_AES_256_GCM,
        )
    }

    pub fn next_counter(&self) -> u64 {
        self.ctr
    }

    pub fn encryption_type(&self) -> u8 {
        self.encryption_type
    }

    fn take_counter(&mut self) -> Result<u64, EncryptionError> {
        let ctr = self.ctr;

        self.ctr = self
            .ctr
            .checked_add(1)
            .ok_or(EncryptionError::CounterExhausted)?;

        Ok(ctr)
    }
}

impl FrameEncrypter for MoqSecureEncrypter {
    fn encrypt(
        &mut self,
        plaintext: &[u8],
    ) -> Result<Bytes, EncryptionError> {
        let ctr = self.take_counter()?;

        let frame = moq_secure::wire::encrypt_frame(
            self.key_store.as_ref(),
            &self.signing_key,
            self.key_id,
            ctr,
            self.n_signed,
            self.maybe_sign,
            self.encryption_type,
            self.pad_len,
            plaintext,
        )?;

        Ok(Bytes::from(frame.serialize()))
    }
}

#[derive(Clone)]
pub struct MoqSecureDecryptionConfig {
    pub key_store: Arc<dyn KeyStore>,
    pub broadcaster_public_key: VerifyingKey,
}

impl MoqSecureDecryptionConfig {
    pub fn new(
        key_store: Arc<dyn KeyStore>,
        broadcaster_public_key: VerifyingKey,
    ) -> Self {
        Self {
            key_store,
            broadcaster_public_key,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackMode {
    Live,
    Rewind,
}

#[derive(Debug, Default)]
struct CounterState {
    /// Highest counter accepted while in live mode.
    live_max_ctr: Option<u64>,

    /// Highest counter accepted during the current rewind invocation.
    playback_ctr: Option<u64>,
}

#[derive(Debug)]
struct DecryptionState {
    /// Shared signing lease.
    ///
    /// This is deliberately not stored per encryption key. The signing
    /// lease permits unsigned frames after a signed frame and is separate
    /// from encryption-key replay tracking.
    lease_remaining: u8,

    /// Encryption replay state, tracked independently for each key ID.
    counters: HashMap<u8, CounterState>,

    mode: PlaybackMode,
}

impl DecryptionState {
    fn new(lease_remaining: u8) -> Self {
        Self {
            lease_remaining,
            counters: HashMap::new(),
            mode: PlaybackMode::Live,
        }
    }

    fn begin_live(&mut self) {
        self.mode = PlaybackMode::Live;
    }

    fn begin_rewind(&mut self) {
        self.mode = PlaybackMode::Rewind;

        // Each rewind invocation gets a fresh playback cursor.
        //
        // live_max_ctr is intentionally retained so that live replay
        // protection survives mode changes.
        for state in self.counters.values_mut() {
            state.playback_ctr = None;
        }
    }

    fn check_counter(
        &self,
        key_id: u8,
        ctr: u64,
    ) -> Result<(), EncryptionError> {
        let Some(state) = self.counters.get(&key_id) else {
            return Ok(());
        };

        let replayed = match self.mode {
            PlaybackMode::Live => {
                state
                    .live_max_ctr
                    .is_some_and(|max| ctr <= max)
            }

            PlaybackMode::Rewind => {
                state
                    .playback_ctr
                    .is_some_and(|cursor| ctr <= cursor)
            }
        };

        if replayed {
            return Err(EncryptionError::ReplayDetected);
        }

        Ok(())
    }

    fn record_counter(&mut self, key_id: u8, ctr: u64) {
        let state = self.counters.entry(key_id).or_default();

        match self.mode {
            PlaybackMode::Live => {
                state.live_max_ctr = Some(
                    state.live_max_ctr.map_or(ctr, |max| max.max(ctr)),
                );
            }

            PlaybackMode::Rewind => {
                state.playback_ctr = Some(ctr);
            }
        }
    }
}

/// Decrypts and verifies each frame using moq-secure.
pub struct MoqSecureDecrypter {
    pub key_store: Arc<dyn KeyStore>,
    pub broadcaster_public_key: VerifyingKey,

    state: DecryptionState,
}

impl MoqSecureDecrypter {
    pub fn new(config: &MoqSecureDecryptionConfig) -> Self {
        Self {
            key_store: Arc::clone(&config.key_store),
            broadcaster_public_key: config.broadcaster_public_key,
            state: DecryptionState::new(0),
        }
    }

    pub fn with_lease(
        key_store: Arc<dyn KeyStore>,
        broadcaster_public_key: VerifyingKey,
        lease_remaining: u8,
    ) -> Self {
        Self {
            key_store,
            broadcaster_public_key,
            state: DecryptionState::new(lease_remaining),
        }
    }

    pub fn lease_remaining(&self) -> u8 {
        self.state.lease_remaining
    }

    pub fn reset_lease(&mut self) {
        self.state.lease_remaining = 0;
    }

    pub fn playback_mode(&self) -> PlaybackMode {
        self.state.mode
    }

    /// Signals that the player has entered live playback.
    ///
    /// Live counter maxima are retained.
    pub fn begin_live(&mut self) {
        self.state.begin_live();
    }

    /// Signals the beginning of a new rewind invocation.
    ///
    /// The rewind playback cursor is reset for every encryption key.
    pub fn begin_rewind(&mut self) {
        self.state.begin_rewind();
    }
}

impl FrameDecrypter for MoqSecureDecrypter {
    fn decrypt(
        &mut self,
        ciphertext: &[u8],
    ) -> Result<Bytes, EncryptionError> {
        // key_id and ctr are available in the unencrypted header.
        // Frame::parse also validates the header structure.
        let frame = moq_secure::wire::Frame::parse(ciphertext)?;

        let key_id = frame.header.key_id;
        let ctr = frame.header.ctr;

        // Do not modify replay state during this check.
        self.state.check_counter(key_id, ctr)?;

        // decrypt_frame continues to manage the shared signing lease.
        // The implementation updates the lease only after signature
        // verification, decryption, and padding validation succeed.
        let plaintext = moq_secure::wire::decrypt_frame(
            self.key_store.as_ref(),
            &self.broadcaster_public_key,
            &mut self.state.lease_remaining,
            ciphertext,
        )?;

        // Commit the counter only after successful authentication and
        // plaintext validation.
        self.state.record_counter(key_id, ctr);

        Ok(Bytes::from(plaintext))
    }
}
