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

#[cfg(test)]
pub(crate) mod test_util;

pub mod flv;
pub mod fmp4;
pub mod legacy;
pub mod loc;
pub mod mkv;
pub mod reader;
pub mod source;
pub mod ts;
pub mod writer;

pub use consumer::Consumer;
pub use group::GroupConsumer;
pub use producer::Producer;
pub(crate) use source::ExportSource;

pub use crate::error::Error;

pub use reader::{
    FrameDecrypter,
    FrameReader,
    ProtectedFrame as ProtectedReadFrame,
    ReadFrame,
};

pub use writer::{
    FrameEncrypter,
    FrameWriter,
    MoqFrameWriter,
    ProtectedFrame as ProtectedWriteFrame,
};

/// The media role that determines how a container represents frame durations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Audio packets carry codec-defined durations.
    Audio,

    /// Video frames can need a duration marker at group end.
    Video,

    /// Opaque data has no sample-duration semantics.
    Data,
}

/// A decoded media frame: timestamp, payload bytes, keyframe flag.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Presentation timestamp.
    pub timestamp: moq_net::Timestamp,

    /// Sample duration in the frame's own scale, when the container reports it.
    pub duration: Option<moq_net::Timestamp>,

    /// Encoded codec payload.
    pub payload: Bytes,

    /// Whether this frame opens a group, or is a video keyframe.
    pub keyframe: bool,
}

/// Stamp `frame` with the duration that ends at `bound`, unless it already has one.
pub(crate) fn close_duration(frame: &mut Frame, bound: moq_net::Timestamp) {
    if frame.duration.is_some() {
        return;
    }

    let Some(delta) = bound
        .as_micros()
        .checked_sub(frame.timestamp.as_micros())
    else {
        return;
    };

    if delta == 0 {
        return;
    }

    let Ok(delta) = u64::try_from(delta) else {
        return;
    };

    if let Ok(duration) = moq_net::Timestamp::from_micros(delta) {
        frame.duration = Some(duration);
    }
}

/// A non-keyframe frame arrived with no open group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("missing keyframe: a group must open on a keyframe")]
pub struct MissingKeyframe;

/// An explicit endpoint precedes the last frame of an ordered video group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("video group endpoint precedes its last frame")]
pub struct InvalidEnd;

/// Encode and decode media frames over a moq-lite group.
pub trait Container {
    /// Container-specific error.
    type Error: std::error::Error
        + Send
        + Sync
        + Unpin
        + From<moq_net::Error>
        + From<MissingKeyframe>
        + From<InvalidEnd>;

    /// Encode one or more frames and send them through `output`.
    fn write<W>(&self, output: &mut W, frames: &[Frame]) -> Result<(), Self::Error>
    where
        W: FrameWriter<Error = Self::Error>;

    /// Poll the next MoQ frame from `group` and decode it into media frames.
    ///
    /// Container implementations should obtain their `FrameReader` and call
    /// `poll_read_frame(waiter)`. If the reader is a `ProtectedReadFrame`,
    /// its returned payload has already been decrypted.
    fn poll_read(
        &self,
        group: &mut moq_net::group::Consumer,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<Vec<Frame>>, Self::Error>>;

    /// Return the endpoint timestamp when `frame` carries empty-payload metadata.
    fn end(&self, _frame: &Frame) -> Option<moq_net::Timestamp> {
        None
    }

    /// The media role, when this format represents an audio or video track.
    fn kind(&self) -> Kind {
        Kind::Data
    }

    /// Write any format-specific endpoint before the producer closes the group.
    fn finish_group(
        &self,
        _group: &mut moq_net::group::Producer,
        _end: Option<moq_net::Timestamp>,
    ) -> Result<(), Self::Error> {
        Ok(())
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
