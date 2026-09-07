use bytes::Bytes;

use crate::{
    encryption::EncryptionError,
    error::Error,
};

/// Writes an encoded payload to an underlying MoQ frame stream.
pub trait FrameWriter {
    type Error;

    fn write_frame(
        &mut self,
        timestamp: moq_net::Timestamp,
        payload: Bytes,
    ) -> Result<(), Self::Error>;

    /// Returns the sequence number that will be assigned to the next frame.
    fn next_sequence_number(&self) -> u32;
}

/// Transforms a plaintext media payload before it is written.
///
/// Despite the historical `FrameEncrypter` name, an implementation may
/// encrypt, authenticate, and sign the payload.
pub trait FrameEncrypter {
    fn encrypt(
        &mut self,
        sequence_number: u64,
        plaintext: &[u8],
    ) -> Result<Bytes, EncryptionError>;
}

/// A FrameWriter decorator that protects each payload before forwarding it.
pub struct ProtectedFrame<W, E> {
    pub inner: W,
    pub encrypter: E,
}

impl<W, E> Sframe<W, E> {
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

impl<W, E> FrameWriter for Sframe<W, E>
where
    W: FrameWriter<Error = Error>,
    E: FrameEncrypter,
{
    type Error = Error;

    fn write_frame(
        &mut self,
        timestamp: moq_net::Timestamp,
        payload: Bytes,
    ) -> Result<(), Self::Error> {
        // MoqFrameWriter uses the underlying group's frame_count() as
        // the sequence number for the next frame.
        let sequence_number = self.inner.next_sequence_number() as u64;

        let protected_payload = self
            .encrypter
            .encrypt(sequence_number, &payload)
            .map_err(Error::from)?;

        self.inner
            .write_frame(timestamp, protected_payload)
    }

    fn next_sequence_number(&self) -> u32 {
        self.inner.next_sequence_number()
    }
}

/// Writes payloads into a moq_net group.
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
