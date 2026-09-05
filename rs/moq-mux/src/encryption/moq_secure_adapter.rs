// src/encryption/moq_secure_adapter.rs

use bytes::Bytes;
use ed25519_dalek::SigningKey;
use moq_secure::key_store::KeyStore;

use super::EncryptionError;
use crate::container::FrameEncrypter;

impl From<moq_secure::error::MoqSecureError> for EncryptionError {
    fn from(error: moq_secure::error::MoqSecureError) -> Self {
        use moq_secure::error::MoqSecureError;

        match error {
            MoqSecureError::InvalidMagic => Self::InvalidFrame,

            MoqSecureError::UnsupportedVersion(version) => {
                Self::UnsupportedVersion(version)
            }

            MoqSecureError::TruncatedFrame => Self::TruncatedFrame,

            MoqSecureError::CiphertextTooShort => {
                Self::CiphertextTooShort
            }

            MoqSecureError::InvalidPadLength => Self::InvalidPadLength,

            MoqSecureError::InvalidEncryptedFlag(flag) => {
                Self::InvalidEncryptedFlag(flag)
            }

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
        }
    }
}

pub struct MoqSecureEncrypter<'a> {
    pub key_store: &'a dyn KeyStore,
    pub signing_key: &'a SigningKey,
    pub key_id: u8,
    pub n_signed: u8,
    pub maybe_sign: bool,
    pub pad_len: u32,
}

impl<'a> MoqSecureEncrypter<'a> {
    pub fn new(
        key_store: &'a dyn KeyStore,
        signing_key: &'a SigningKey,
        key_id: u8,
        n_signed: u8,
        maybe_sign: bool,
        pad_len: u32,
    ) -> Self {
        Self {
            key_store,
            signing_key,
            key_id,
            n_signed,
            maybe_sign,
            pad_len,
        }
    }
}

impl FrameEncrypter for MoqSecureEncrypter<'_> {
    fn encrypt(
        &mut self,
        sequence_number: u64,
        plaintext: &[u8],
    ) -> Result<Bytes, EncryptionError> {
        let frame = moq_secure::wire::encrypt_frame(
            self.key_store,
            self.signing_key,
            self.key_id,
            sequence_number,
            self.n_signed,
            self.maybe_sign,
            1,
            self.pad_len,
            plaintext,
        )
        .map_err(EncryptionError::from)?;

        Ok(Bytes::from(frame.serialize()))
    }
}
