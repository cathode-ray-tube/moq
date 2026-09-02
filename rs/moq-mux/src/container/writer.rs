use bytes::Bytes;

use super::FrameWriter;
use crate::Error;

/// Adapter that writes encoded container payloads into a MoQ producer.
pub struct MoqFrameWriter<'a> {
	pub group: &'a mut moq_net::group::Producer,
}

impl FrameWriter for MoqFrameWriter<'_> {
	type Error = Error;

	fn write_frame(
		&mut self,
		timestamp: moq_net::Timestamp,
		payload: Bytes,
	) -> Result<(), Self::Error> {
		let info = moq_net::frame::Info {
			size: payload.len() as u64,
			timestamp,
		};

		let mut frame = self.group.create_frame(info)?;
		frame.write(payload)?;
		frame.finish()?;

		Ok(())
	}
}

/// Frame-writer decorator for encrypted payloads.
pub struct Sframe<W> {
	pub inner: W,
}

impl<W> FrameWriter for Sframe<W>
where
	W: FrameWriter<Error = Error>,
{
	type Error = Error;

	fn write_frame(
		&mut self,
		timestamp: moq_net::Timestamp,
		payload: Bytes,
	) -> Result<(), Self::Error> {
		let encrypted = encrypt(&payload)?;
		self.inner.write_frame(timestamp, encrypted)
	}
}

fn encrypt(payload: &Bytes) -> Result<Bytes, Error> {
	// Replace this with the project's actual SFrame encoder.
	//
	// `todo!()` is intentional until the encryption key, nonce, authentication
	// tag, and wire-format configuration are available.
	let _ = payload;
	todo!("implement SFrame payload encryption")
}
