use bytes::Bytes;

use super::FrameWriter;
use crate::Error;

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

	fn next_sequence_number(&self) -> u32 {
		self.group.frame_count() as u32
	}
}

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

	fn next_sequence_number(&self) -> u32 {
		self.inner.next_sequence_number()
	}
}

fn encrypt(payload: &Bytes) -> Result<Bytes, Error> {
	let _ = payload;
	todo!("implement SFrame payload encryption")
}
