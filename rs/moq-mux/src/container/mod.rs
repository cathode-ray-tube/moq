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

pub use crate::error::Error;

pub use writer::{
    FrameEncrypter,
    FrameWriter,
    MoqFrameWriter,
    ProtectedFrame,
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
    /// Each container picks its own native scale: fmp4 uses the source
    /// `mdhd.timescale`, mkv uses nanoseconds, legacy is fixed at microseconds.
    /// LOC defaults to microseconds but a decoded frame keeps whatever
    /// per-frame timescale the wire carried.
    pub timestamp: moq_net::Timestamp,

	/// Sample duration in the frame's own scale, when the container reports it.
	///
	/// CMAF carries a per-sample duration (trun sample-duration). Legacy and LOC
	/// can fill it from a duration marker when reading a fetched group. Streaming
	/// muxers receive the later endpoint separately, so media stays immediately available.
	/// The [`Consumer`] adds it to `timestamp` to learn how far a group has
	/// presented, so it can advance to a newer group as soon as the gap is
	/// covered instead of waiting out the max age budget.
	pub duration: Option<moq_net::Timestamp>,

    /// Encoded codec payload.
    pub payload: Bytes,

	/// Whether this frame opens a group, or is a video keyframe.
	///
	/// Containers that carry the bit on the wire (CMAF reads it from
	/// trun sample-flags) set it for video; containers that don't (Legacy,
	/// LOC) leave it `false`. The wrapping [`Consumer`] still asserts
	/// "first frame in a group is a keyframe" as a fallback, so the
	/// Legacy/LOC case lands correctly without anyone having to know. For
	/// audio, whose samples are all independently decodable, that fallback
	/// is the only source: the bit marks the group boundary the publisher
	/// drew, never a per-sample sync flag.
	pub keyframe: bool,
}

/// Stamp `frame` with the duration that ends at `bound`, unless it already has one.
pub(crate) fn close_duration(frame: &mut Frame, bound: moq_net::Timestamp) {
	if frame.duration.is_some() {
		return;
	}
	let Some(delta) = bound.as_micros().checked_sub(frame.timestamp.as_micros()) else {
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

	/// Container-specific error. Must be convertible from [`moq_net::Error`]
	/// (so IO errors propagate), [`MissingKeyframe`], and [`InvalidEnd`]
	/// (so the producer can reject invalid group boundaries).
	type Error: std::error::Error
		+ Send
		+ Sync
		+ Unpin
		+ From<moq_net::Error>
		+ From<MissingKeyframe>
		+ From<InvalidEnd>;

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

	/// Return the endpoint timestamp when `frame` carries empty-payload metadata.
	///
	/// For video this bounds the preceding frame; for audio it bounds source samples
	/// before terminal codec packets. A consumer never submits the marker to a decoder.
	/// Formats without endpoint metadata use the default.
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

	/// Async wrapper around [`Self::poll_read`]. Carries the same contract: only
	/// `Ok(None)` ends the group, and `Ok(Some(batch))` may hand back an empty
	/// `batch` (poll again for more), so a caller loop must key completion off
	/// `None`, not an empty batch.
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
