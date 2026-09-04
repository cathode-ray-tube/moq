//! Container formats.
//!
//! A container decides how a media frame is laid out inside a moq-lite
//! frame: framing overhead, whether multiple samples can share one moq
//! frame, and whether the same encoding doubles as a file format on disk.
//!
//! Each submodule implements one format. The wire-level ones implement
//! the [`Container`] trait, so [`Producer<C>`] and [`Consumer<C>`] can be
//! generic over the choice. The catalog announces a container per
//! track; [`catalog::hang::Container`](crate::catalog::hang::Container)
//! dispatches the right implementation at runtime.

use bytes::Bytes;
use std::task::Poll;

mod consumer;
mod group;
mod producer;
mod source;

#[cfg(test)]
pub(crate) mod test_util;

pub mod writer;

pub mod flv;
pub mod fmp4;
pub mod legacy;
pub mod loc;
pub mod mkv;
pub mod ts;

pub use consumer::Consumer;
pub use group::GroupConsumer;
pub use producer::Producer;
pub(crate) use source::ExportSource;

pub use error::ContainerError;

pub use writer::{
    FrameEncrypter,
    FrameWriter,
    MoqFrameWriter,
    Sframe,
};

/// A decoded media frame: timestamp, payload bytes, keyframe flag.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Presentation timestamp.
    ///
    /// Each container picks its own native scale: fmp4 uses the source
    /// `mdhd.timescale`, mkv uses nanoseconds, legacy is fixed at microseconds.
    /// LOC defaults to microseconds but a decoded frame keeps whatever
    /// per-frame timescale the wire carried.
    pub timestamp: moq_net::Timestamp,

    /// Sample duration in the frame's own scale, when reported.
    pub duration: Option<moq_net::Timestamp>,

    /// Encoded codec payload.
    pub payload: Bytes,

    /// Whether this frame is a keyframe.
    pub keyframe: bool,
}

/// A non-keyframe frame arrived with no open group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("missing keyframe: a group must open on a keyframe")]
pub struct MissingKeyframe;

/// Encode and decode media frames over a moq-lite group.
pub trait Container {
    /// Container-specific error. Must be convertible from [`moq_net::Error`]
    /// and [`MissingKeyframe`].
    type Error: std::error::Error
        + Send
        + Sync
        + Unpin
        + From<moq_net::Error>
        + From<MissingKeyframe>;

    /// Encode one or more frames and send them through `output`.
    fn write<W>(
        &self,
        output: &mut W,
        frames: &[Frame],
    ) -> Result<(), Self::Error>
    where
        W: FrameWriter<Error = Self::Error>;

    /// Poll the next moq-lite frame from `group` and decode it into media
    /// frames.
    fn poll_read(
        &self,
        group: &mut moq_net::group::Consumer,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<Vec<Frame>>, Self::Error>>;

    /// Return the exclusive media endpoint when `frame` is a container marker.
    fn end(&self, _frame: &Frame) -> Option<moq_net::Timestamp> {
        None
    }

    /// Async wrapper around [`Self::poll_read`].
    fn read(
        &self,
        group: &mut moq_net::group::Consumer,
    ) -> impl std::future::Future<Output = Result<Option<Vec<Frame>>, Self::Error>>
    where
        Self: Sync,
    {
        async { kio::wait(|waiter| self.poll_read(group, waiter)).await }
    }
}
