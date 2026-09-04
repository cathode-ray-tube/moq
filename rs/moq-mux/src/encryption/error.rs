// src/encryption/error.rs

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EncryptionError {
    #[error("encryption is not implemented")]
    NotImplemented,

    #[error("encryption counter exhausted")]
    CounterExhausted,

    #[error("invalid encrypted frame")]
    InvalidFrame,

    #[error("encrypted frame is truncated")]
    TruncatedFrame,

    #[error("ciphertext is too short")]
    CiphertextTooShort,

    #[error("invalid padding length")]
    InvalidPadLength,

    #[error("invalid encryption flag: {0}")]
    InvalidEncryptedFlag(u8),

    #[error("invalid signature flag: {0}")]
    InvalidSigFlag(u8),

    #[error("authentication failed")]
    AuthenticationFailed,

    #[error("signature verification failed")]
    SignatureFailed,

    #[error("signing configuration does not match frame flags")]
    SigningMismatch,

    #[error("signature slot is missing or zero")]
    MissingSigSlot,

    #[error("signature is not allowed when nSigned is zero")]
    SignatureNotAllowedByNSigned,

    #[error("decryption failed")]
    DecryptionFailed,

    #[error("invalid encryption key")]
    InvalidKey,

    #[error("invalid key identifier: {0}")]
    InvalidKeyId(u8),

    #[error("unsupported encryption version: {0}")]
    UnsupportedVersion(u8),

    #[error("encryption backend error: {0}")]
    Backend(String),
}
