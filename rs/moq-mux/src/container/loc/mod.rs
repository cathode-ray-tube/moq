//! Low Overhead Container.
//!
//! The IETF draft replacement for hang's Legacy format. Each moq frame
//! holds a small property block (timestamp, optional per-frame
//! timescale) followed by the codec bitstream. Defaults to microsecond
//! timestamps. See [draft-ietf-moq-loc-04](https://www.ietf.org/archive/id/draft-ietf-moq-loc-04.html).

use std::task::Poll;

use moq_net::{Timescale, Timestamp};

use crate::container::{Container, Frame};
use crate::container::writer::{FrameWriter, MoqFrameWriter};

/// LOC's catalog convention: timestamps are in microseconds when no per-frame
/// 0x08 timescale property is present.
const DEFAULT_TIMESCALE: Timescale = Timescale::MICRO;

/// LOC wire format. Each moq frame holds one LOC frame.
#[derive(Default)]
pub struct Wire;

impl Container for Wire {
	type Error = crate::Error;

	fn write(
    &self,
    group: &mut moq_net::group::Producer,
    frames: &[Frame],
) -> Result<(), Self::Error> {
    let mut writer = MoqFrameWriter { group };

    for frame in frames {
        // LOC's wire format omits the per-frame timescale by convention.
        // Convert the timestamp to the catalog's default microsecond scale.
        let timestamp = frame
            .timestamp
            .convert(DEFAULT_TIMESCALE)
            .map_err(hang::Error::from)?;

        let data = moq_loc::encode(timestamp.value(), &frame.payload)?;

        // MoqFrameWriter handles create_frame, write, and finish.
        //
        // Use the original timestamp here because the MoQ frame timestamp
        // should remain in the track's timescale.
        writer.write_frame(frame.timestamp, data)?;
    }

    Ok(())
}


	fn poll_read(
		&self,
		group: &mut moq_net::group::Consumer,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
		use std::task::ready;

		let Some(frame) = ready!(group.poll_read_frame(waiter)?) else {
			return Poll::Ready(Ok(None));
		};

		let loc = moq_loc::decode(frame.payload)?;
		// `loc.timescale == Some(0)` is a malformed wire (caught by moq_loc::decode itself),
		// so any Some(_) we see here is non-zero. Falling back to the catalog default
		// keeps this code path infallible.
		let scale = loc
			.timescale
			.and_then(|s| Timescale::new(s).ok())
			.unwrap_or(DEFAULT_TIMESCALE);
		let timestamp = Timestamp::new(loc.timestamp, scale).map_err(hang::Error::from)?;

		Poll::Ready(Ok(Some(vec![Frame {
			timestamp,
			payload: loc.payload,
			// LOC doesn't carry the keyframe bit on the wire; the
			// wrapping Consumer fills it in from group position.
			keyframe: false,
			// LOC carries no per-frame duration.
			duration: None,
		}])))
	}
}
