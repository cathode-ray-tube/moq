// src/encryption/moq_secure_adapter.rs

use std::sync::Arc;

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

			MoqSecureError::UnsupportedVersion(version) => Self::UnsupportedVersion(version),

			MoqSecureError::TruncatedFrame => Self::TruncatedFrame,

			MoqSecureError::CiphertextTooShort => Self::CiphertextTooShort,

			MoqSecureError::InvalidPadLength => Self::InvalidPadLength,

			MoqSecureError::InvalidEncryptedFlag(flag) => Self::InvalidEncryptedFlag(flag),

			MoqSecureError::InvalidSigFlag(flag) => Self::InvalidSigFlag(flag),

			MoqSecureError::AeadAuthFailed => Self::AuthenticationFailed,

			MoqSecureError::InvalidSignature => Self::SignatureFailed,

			MoqSecureError::SigningMismatch => Self::SigningMismatch,

			MoqSecureError::MissingSigSlot => Self::MissingSigSlot,

			MoqSecureError::SignatureNotAllowedByNSigned => Self::SignatureNotAllowedByNSigned,

			MoqSecureError::DecryptFailed => Self::DecryptionFailed,

			MoqSecureError::InvalidKeyId(key_id) => Self::InvalidKeyId(key_id),
		}
	}
}

/// Encrypts each frame using moq-secure.
///
/// `sequence_number` supplied to `encrypt()` is the frame number within
/// the current group. The independent `ctr` field is incremented by this
/// encrypter once for every frame.
pub struct MoqSecureEncrypter {
	pub key_store: Arc<dyn KeyStore>,
	pub signing_key: SigningKey,
	pub key_id: u8,
	pub n_signed: u8,
	pub maybe_sign: bool,
	pub pad_len: u32,

	/// Independent encryption counter.
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

	/// Returns the counter that will be assigned to the next frame.
	pub fn next_counter(&self) -> u64 {
		self.ctr
	}

	/// Returns the counter that will be assigned to the next frame and
	/// advances it by one.
	fn take_counter(&mut self) -> Result<u64, EncryptionError> {
		let ctr = self.ctr;

		self.ctr = self.ctr.checked_add(1).ok_or(EncryptionError::CounterExhausted)?;

		Ok(ctr)
	}
}

impl FrameEncrypter for MoqSecureEncrypter {
	fn encrypt(&mut self, _sequence_number: u64, plaintext: &[u8]) -> Result<Bytes, EncryptionError> {
		// This counter is intentionally independent of the group frame
		// sequence number passed by ProtectedFrame.
		let ctr = self.take_counter()?;

		let frame = moq_secure::wire::encrypt_frame(
			self.key_store.as_ref(),
			&self.signing_key,
			self.key_id,
			ctr,
			self.n_signed,
			self.maybe_sign,
			1, // encrypted
			self.pad_len,
			plaintext,
		)?;

		Ok(Bytes::from(frame.serialize()))
	}
}
