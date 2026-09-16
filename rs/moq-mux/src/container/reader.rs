use bytes::Bytes;
use std::task::Poll;

use crate::{encryption::EncryptionError, error::Error};

pub type Decrypter =
    Box<dyn FrameDecrypter + Send + Sync>;

pub type DecrypterFactory =
    Box<dyn Fn() -> Option<Decrypter> + Send + Sync>;

/// A frame returned by an underlying MoQ frame reader.
#[derive(Debug, Clone)]
pub struct ReadFrame {
    /// Frame number within the current MoQ group.
    pub sequence_number: u32,

    pub timestamp: moq_net::Timestamp,

    /// Encoded payload as received from the underlying stream.
    ///
    /// For a `ProtectedFrame`, this is the decrypted payload.
    pub payload: Bytes,
}

/// Reads encoded payloads from an underlying MoQ frame stream.
pub trait FrameReader {
    type Error;

    /// Polls and consumes the next frame.
    ///
    /// Returns `Poll::Ready(Ok(None))` when the current group has ended.
    fn poll_read_frame(
        &mut self,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<ReadFrame>, Self::Error>>;

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

/// Allows a boxed decrypter to be used as a decrypter.
impl<T> FrameDecrypter for Box<T>
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

    fn poll_read_frame(
        &mut self,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<ReadFrame>, Self::Error>> {
        let frame = match self.inner.poll_read_frame(waiter) {
            Poll::Pending => return Poll::Pending,

            Poll::Ready(Err(error)) => {
                return Poll::Ready(Err(error));
            }

            Poll::Ready(Ok(None)) => {
                return Poll::Ready(Ok(None));
            }

            Poll::Ready(Ok(Some(frame))) => frame,
        };

        let plaintext = match self.decrypter.decrypt(
            u64::from(frame.sequence_number),
            &frame.payload,
        ) {
            Ok(plaintext) => plaintext,

            Err(error) => {
                return Poll::Ready(Err(Error::from(error)));
            }
        };

        Poll::Ready(Ok(Some(ReadFrame {
            payload: plaintext,
            ..frame
        })))
    }

    fn next_sequence_number(&self) -> u32 {
        self.inner.next_sequence_number()
    }
}
