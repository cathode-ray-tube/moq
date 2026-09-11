// src/container/writer.rs

use bytes::Bytes;

use crate::{encryption::EncryptionError, error::Error};

/// Writes an encoded payload to an underlying MoQ frame stream.
pub trait FrameWriter {
	type Error;

	fn write_frame(&mut self, timestamp: moq_net::Timestamp, payload: Bytes) -> Result<(), Self::Error>;

	/// Returns the frame number that will be assigned to the next frame
	/// in the current group.
	fn next_sequence_number(&self) -> u32;
}

/// Transforms a plaintext media payload before it is written.
///
/// The sequence number passed to the encrypter is the frame number within
/// the current group. It is independent of the encryption counter.
pub trait FrameEncrypter {
	fn encrypt(&mut self, sequence_number: u64, plaintext: &[u8]) -> Result<Bytes, EncryptionError>;
}

/// Forwards encryption through a mutable reference.
///
/// This allows `&mut dyn FrameEncrypter` to be used as the encrypter in
/// `ProtectedFrame`.
impl<T> FrameEncrypter for &mut T
where
	T: FrameEncrypter + ?Sized,
{
	fn encrypt(&mut self, sequence_number: u64, plaintext: &[u8]) -> Result<Bytes, EncryptionError> {
		(**self).encrypt(sequence_number, plaintext)
	}
}

/// A [`FrameWriter`] decorator that protects each payload before forwarding it.
///
/// `sequence_number` is obtained from the underlying writer and identifies
/// the frame within the current group. The encrypter owns and increments its
/// own encryption counter independently.
pub struct ProtectedFrame<W, E> {
	pub inner: W,
	pub encrypter: E,
}

impl<W, E> ProtectedFrame<W, E> {
	pub fn new(inner: W, encrypter: E) -> Self {
		Self { inner, encrypter }
	}

	pub fn into_inner(self) -> W {
		self.inner
	}

	pub fn into_parts(self) -> (W, E) {
		(self.inner, self.encrypter)
	}
}

impl<W, E> FrameWriter for ProtectedFrame<W, E>
where
	W: FrameWriter<Error = Error>,
	E: FrameEncrypter,
{
	type Error = Error;

	fn write_frame(&mut self, timestamp: moq_net::Timestamp, payload: Bytes) -> Result<(), Self::Error> {
		// This is the frame number within the current group.
		// It is not the encryption counter.
		let sequence_number = u64::from(self.inner.next_sequence_number());

		let protected_payload = self.encrypter.encrypt(sequence_number, &payload).map_err(Error::from)?;

		self.inner.write_frame(timestamp, protected_payload)
	}

	fn next_sequence_number(&self) -> u32 {
		self.inner.next_sequence_number()
	}
}

/// Writes payloads into a `moq_net` group.
pub struct MoqFrameWriter<'a> {
	pub group: &'a mut moq_net::group::Producer,
}

impl FrameWriter for MoqFrameWriter<'_> {
	type Error = Error;

	fn write_frame(&mut self, timestamp: moq_net::Timestamp, payload: Bytes) -> Result<(), Self::Error> {
		let info = moq_net::frame::Info {
			size: payload.len() as u64,
			timestamp,
		};

		let mut frame = self.group.create_frame(info)?;
		frame.write(payload)?;
		frame.finish()?;

		Ok(())
	}

	fn next_sequence_number(&self) -> u32 {
		self.group.frame_count() as u32
	}
}
