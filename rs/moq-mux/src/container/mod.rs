//! Container formats.
//!
//! A container decides how one or more media samples are laid out inside a
//! moq-lite frame, including framing overhead and optional file-format
//! compatibility.
//!
//! Each submodule implements one format. Wire-level formats implement the
//! [`Container`] trait, so [`Producer<C>`] and [`Consumer<C>`] can be generic
//! over the choice. The catalog announces a container per track;
//! [`catalog::hang::Container`](crate::catalog::hang::Container) dispatches
//! the appropriate implementation at runtime.

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
    ///
    /// Each container picks its own native scale: fMP4 uses the source
    /// `mdhd.timescale`, Matroska uses nanoseconds, Legacy is fixed at
    /// microseconds, and LOC defaults to microseconds while preserving any
    /// per-frame timescale carried on the wire.
    pub timestamp: moq_net::Timestamp,

    /// Sample duration, normalized to microseconds, when the container reports
    /// one.
    ///
    /// CMAF carries a per-sample duration. Legacy and LOC can fill this from
    /// a duration marker when reading a fetched group. Streaming muxers
    /// receive the later endpoint separately, so media remains immediately
    /// available.
    ///
    /// The [`Consumer`] adds this duration to `timestamp` to learn how far a
    /// group has been presented. It can then advance to a newer group as soon
    /// as the gap is covered instead of waiting out the maximum-age budget.
    pub duration: Option<moq_net::Timestamp>,

    /// Encoded codec payload.
    pub payload: Bytes,

    /// Whether this frame opens a group, or is a video keyframe.
    ///
    /// Containers that carry the bit on the wire, such as CMAF, set it from
    /// the container metadata. Containers without keyframe metadata, such as
    /// Legacy and LOC, return `false`; the wrapping [`Consumer`] promotes the
    /// first decoded frame in each group to a keyframe and validates the group
    /// boundary.
    ///
    /// For audio, whose samples are independently decodable, the group
    /// boundary is the only source of this information.
    pub keyframe: bool,
}

/// Stamp `frame` with the duration ending at `bound`, unless it already has
/// one.
///
/// Durations are normalized to microseconds, matching the historical behavior
/// of this module. The timestamp itself may use a different native timescale.
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
    type Error: std::error::Error
        + Send
        + Sync
        + Unpin
        + From<moq_net::Error>
        + From<MissingKeyframe>
        + From<InvalidEnd>;

    fn write<W>(
        &self,
        output: &mut W,
        frames: &[Frame],
    ) -> Result<(), Self::Error>
    where
        W: FrameWriter<Error = Self::Error>;

    /// Existing unprotected compatibility path.
    fn poll_read(
        &self,
        group: &mut moq_net::group::Consumer,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<Vec<Frame>>, Self::Error>>;

    /// Read through an already-created frame reader.
    ///
    /// The reader may be a `ProtectedFrame`, in which case each payload has
    /// already been decrypted before the container parses it.
    fn poll_read_frames<R>(
        &self,
        reader: &mut R,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<Vec<Frame>>, Self::Error>>
    where
    R: FrameReader<Error = crate::error::Error>,

    fn end(&self, _frame: &Frame) -> Option<moq_net::Timestamp> {
        None
    }

    fn kind(&self) -> Kind {
        Kind::Data
    }

    fn finish_group<W>(
        &self,
        _output: &mut W,
        _end: Option<moq_net::Timestamp>,
    ) -> Result<(), Self::Error>
    where
        W: FrameWriter<Error = Self::Error>,
    {
        Ok(())
    }

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

