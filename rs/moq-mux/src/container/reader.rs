use bytes::Bytes;

use crate::{encryption::EncryptionError, error::Error};

/// A frame returned by an underlying MoQ frame reader.
#[derive(Debug, Clone)]
pub struct ReadFrame {
	/// Frame number within the current MoQ group.
	pub sequence_number: u32,

	pub timestamp: moq_net::Timestamp,

	/// Encoded payload as received from the underlying stream.
	pub payload: Bytes,
}

/// Reads encoded payloads from an underlying MoQ frame stream.
pub trait FrameReader {
	type Error;

	/// Reads and consumes the next frame.
	///
	/// Returns `Ok(None)` when the current group has ended.
	fn read_frame(&mut self) -> Result<Option<ReadFrame>, Self::Error>;

	/// Returns the sequence number that will be assigned to the next frame.
	fn next_sequence_number(&self) -> u32;
}

/// Transforms an encoded payload after it is read.
pub trait FrameDecrypter {
	/// `sequence_number` is the MoQ frame number within the current group.
	///
	/// A decrypter may ignore it when the protected wire format contains
	/// its own authenticated counter.
	fn decrypt(
		&mut self,
		sequence_number: u64,
		ciphertext: &[u8],
	) -> Result<Bytes, EncryptionError>;
}

/// Allows a mutable reference to be used as a decrypter.
impl<T> FrameDecrypter for &mut T
where
	T: FrameDecrypter + ?Sized,
{
	fn decrypt(
		&mut self,
		sequence_number: u64,
		ciphertext: &[u8],
	) -> Result<Bytes, EncryptionError> {
		(**self).decrypt(sequence_number, ciphertext)
	}
}

/// A [`FrameReader`] decorator that decrypts each payload before returning it.
pub struct ProtectedFrame<R, D> {
	pub inner: R,
	pub decrypter: D,
}

impl<R, D> ProtectedFrame<R, D> {
	pub fn new(inner: R, decrypter: D) -> Self {
		Self {
			inner,
			decrypter,
		}
	}

	pub fn into_inner(self) -> R {
		self.inner
	}

	pub fn into_parts(self) -> (R, D) {
		(self.inner, self.decrypter)
	}
}

impl<R, D> FrameReader for ProtectedFrame<R, D>
where
	R: FrameReader<Error = Error>,
	D: FrameDecrypter,
{
	type Error = Error;

	fn read_frame(&mut self) -> Result<Option<ReadFrame>, Self::Error> {
		let Some(mut frame) = self.inner.read_frame()? else {
			return Ok(None);
		};

		let plaintext = self
			.decrypter
			.decrypt(
				u64::from(frame.sequence_number),
				&frame.payload,
			)
			.map_err(Error::from)?;

		frame.payload = plaintext;

		Ok(Some(frame))
	}

	fn next_sequence_number(&self) -> u32 {
		self.inner.next_sequence_number()
	}
}
