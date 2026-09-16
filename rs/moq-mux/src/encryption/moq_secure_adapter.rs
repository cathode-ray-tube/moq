use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::{SigningKey, VerifyingKey};
use moq_secure::key_store::KeyStore;

use super::EncryptionError;
use crate::container::{FrameDecrypter, FrameEncrypter};

impl From<moq_secure::error::MoqSecureError> for EncryptionError {
	fn from(error: moq_secure::error::MoqSecureError) -> Self {
		use moq_secure::error::MoqSecureError;

		match error {
			MoqSecureError::InvalidMagic => Self::InvalidFrame,

			MoqSecureError::UnsupportedVersion(version) => {
				Self::UnsupportedVersion(version)
			}

			MoqSecureError::TruncatedFrame => Self::TruncatedFrame,

			MoqSecureError::CiphertextTooShort => Self::CiphertextTooShort,

			MoqSecureError::InvalidPadLength => Self::InvalidPadLength,

			MoqSecureError::InvalidEncryptedFlag(flag) => {
				Self::InvalidEncryptedFlag(flag)
			}

			MoqSecureError::InvalidSigFlag(flag) => Self::InvalidSigFlag(flag),

			MoqSecureError::AeadAuthFailed => Self::AuthenticationFailed,

			MoqSecureError::InvalidSignature => Self::SignatureFailed,

			MoqSecureError::SigningMismatch => Self::SigningMismatch,

			MoqSecureError::MissingSigSlot => Self::MissingSigSlot,

			MoqSecureError::SignatureNotAllowedByNSigned => {
				Self::SignatureNotAllowedByNSigned
			}

			MoqSecureError::DecryptFailed => Self::DecryptionFailed,

			MoqSecureError::InvalidKeyId(key_id) => Self::InvalidKeyId(key_id),
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

	/// Independent moq-secure encryption counter.
	ctr: u64,
}

impl MoqSecureEncrypter {
	pub fn new(
		key_store: Arc<dyn KeyStore>,
		signing_key: SigningKey,
		key_id: u8,
		n_signed: u8,
		maybe_sign: bool,
		pad_len: u32,
		initial_ctr: u64,
	) -> Self {
		Self {
			key_store,
			signing_key,
			key_id,
			n_signed,
			maybe_sign,
			pad_len,
			ctr: initial_ctr,
		}
	}

	pub fn next_counter(&self) -> u64 {
		self.ctr
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
		_sequence_number: u64,
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
			1,
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

/// Decrypts and verifies each frame using moq-secure.
pub struct MoqSecureDecrypter {
	pub key_store: Arc<dyn KeyStore>,
	pub broadcaster_public_key: VerifyingKey,

	/// Number of unsigned frames remaining in the current signature lease.
	pub lease_remaining: u8,
}

impl MoqSecureDecrypter {
	pub fn new(config: &MoqSecureDecryptionConfig) -> Self {
        Self {
            key_store: Arc::clone(&config.key_store),
            broadcaster_public_key: config.broadcaster_public_key.clone(),
			lease_remaining: 0,
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
			lease_remaining,
		}
	}

	pub fn lease_remaining(&self) -> u8 {
		self.lease_remaining
	}

	pub fn reset_lease(&mut self) {
		self.lease_remaining = 0;
	}
}

impl FrameDecrypter for MoqSecureDecrypter {
	fn decrypt(
		&mut self,
		_sequence_number: u64,
		ciphertext: &[u8],
	) -> Result<Bytes, EncryptionError> {
		let plaintext = moq_secure::wire::decrypt_frame(
			self.key_store.as_ref(),
			&self.broadcaster_public_key,
			&mut self.lease_remaining,
			ciphertext,
		)?;

		Ok(Bytes::from(plaintext))
	}
}
