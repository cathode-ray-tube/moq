use std::collections::VecDeque;
use std::task::{ready, Poll};

use moq_net::Timestamp;

use super::{Container, Frame};

use crate::container::{Decrypter, FrameDecrypter};
use crate::container::group::GroupReader;
use crate::container::reader::ProtectedFrame;

/// Media and clean group boundaries in delivery order.
pub(crate) enum Event {
    Frame(Frame),
    FrameEnd(Timestamp),
    GroupEnd,
}

/// Decode a moq-lite track into a stream of media [`Frame`]s in age-bounded
/// presentation order.
pub struct Consumer<F: Container> {
    track: moq_net::track::Subscriber,

    format: F,

    /// The current group that we want to read from.
    current: u64,

    /// Groups that we are monitoring, sorted by sequence ascending.
    pending: VecDeque<GroupBuffer>,

    /// Latches the cursor onto the publisher's first served group when the
    /// subscription names no start.
    startup: bool,

    /// How far we may drift from the live edge before skipping a group.
    max_age: std::time::Duration,

    /// Timeline-discontinuity tracking.
    rewind: Rewind,

    /// Exclusive audio endpoint delivered before terminal codec packets.
    end: Option<Timestamp>,

    /// Optional decrypter, persistent across reads and group transitions.
    decrypter: Option<Decrypter>,

}

/// Live state for detecting timeline rewinds and classifying out-of-order groups.
#[derive(Default)]
struct Rewind {
    /// Largest timestamp delivered so far and the group that carried it.
    live_edge: Option<(u64, Timestamp)>,

    /// Active rewind boundary.
    boundary: Option<Reset>,

    /// Number of discontinuities observed.
    discontinuity: u64,
}

/// A recorded rewind boundary.
#[derive(Clone, Copy)]
struct Reset {
    /// Highest-sequence old-epoch group seen at detection.
    prev_max: u64,

    /// Group whose backwards timestamp triggered detection.
    group: u64,

    /// Timestamp that triggered detection.
    timestamp: Timestamp,
}

impl Reset {
    /// `Some(true)` means old/drop.
    ///
    /// `Some(false)` means new/keep.
    ///
    /// `None` means ambiguous and requires timestamp classification.
    fn by_sequence(&self, sequence: u64) -> Option<bool> {
        if sequence <= self.prev_max {
            Some(true)
        } else if sequence >= self.group {
            Some(false)
        } else {
            None
        }
    }

    /// Returns whether a group belongs to the reneged old epoch.
    fn is_stale(&self, sequence: u64, timestamp: Timestamp) -> bool {
        self.by_sequence(sequence)
            .unwrap_or(timestamp >= self.timestamp)
    }
}

impl<F: Container<Error = crate::error::Error>> Consumer<F> {
    /// Create a consumer wrapping the given subscriber and container.
	    pub fn new(track: moq_net::track::Subscriber, format: F) -> Self {
		let subscription = track.subscription();
		let start = subscription.start.map(|position| position.group);
		let max_age = subscription.max_age.min(track.info().max_age);
	
		Self {
			track,
			format,
			current: start.unwrap_or(0),
			pending: VecDeque::new(),
			startup: start.is_none(),
			max_age,
			rewind: Rewind::default(),
			end: None,
			decrypter: None,
		}
	}
	pub fn with_decrypter(mut self, decrypter: Decrypter) -> Self {
    self.decrypter = Some(decrypter);
    self
}

    /// A counter that increments each time the consumer reaches a declared
    /// discontinuity or detects a timeline rewind.
    pub fn discontinuity(&self) -> u64 {
        self.rewind.discontinuity
    }

    /// The exclusive audio endpoint delivered before terminal codec packets.
    pub fn end(&self) -> Option<Timestamp> {
        self.end
    }

    /// Read the next frame from the track.
    pub async fn read(&mut self) -> Result<Option<Frame>, F::Error> {
        kio::wait(|waiter| self.poll_read(waiter)).await
    }

    /// Poll-based implementation of the read loop.
    pub fn poll_read(
        &mut self,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<Frame>, F::Error>> {
        loop {
            match ready!(self.poll_event(waiter))? {
                Some(Event::Frame(frame)) => {
                    return Poll::Ready(Ok(Some(frame)));
                }

                Some(Event::FrameEnd(end)) => {
                    if self.format.kind() == super::Kind::Audio {
                        self.end = Some(end);
                    }
                }

                Some(Event::GroupEnd) => {}

                None => {
                    return Poll::Ready(Ok(None));
                }
            }
        }
    }

   
/// Read media or a clean group boundary without waiting for a successor group.
pub(crate) fn poll_event(
    &mut self,
    waiter: &kio::Waiter,
) -> Poll<Result<Option<Event>, F::Error>> {
    let finished = self.poll_read_finish(waiter)?.is_ready();

    if self.startup {
        let mut found = None;

        for index in 0..self.pending.len() {
            if matches!(
                self.poll_min_timestamp(index, waiter),
                Poll::Ready(Ok(_))
            ) {
                found = Some(index);
                break;
            }
        }

        if found.is_some() {
            self.current = self
                .pending
                .front()
                .expect("a group has a frame")
                .sequence;

            self.startup = false;
        }
    }

    let current = self.current;

    self.pending.retain_mut(|group| {
        group.sequence <= current
            || !group.buffered.is_empty()
            || !group.poll_aborted(waiter)
    });

    'read: loop {
        if self.poll_reset(waiter)? {
            continue;
        }

        self.poll_classify(waiter)?;

        if let Some(group) = self.pending.front()
            && group.sequence <= self.current
        {
            match self.poll_read_group(0, waiter) {
                Poll::Ready(Ok(Some(Event::Frame(frame)))) => {
                    let sequence = self.pending[0].group.sequence;
                    let timestamp = frame.timestamp;

                    if self
                        .rewind
                        .live_edge
                        .is_none_or(|(_, high)| timestamp > high)
                    {
                        self.rewind.live_edge = Some((sequence, timestamp));
                    }

                    return Poll::Ready(Ok(Some(Event::Frame(frame))));
                }

                Poll::Ready(Ok(Some(event))) => {
                    return Poll::Ready(Ok(Some(event)));
                }

                Poll::Ready(Ok(None)) => {
                    self.pending.pop_front();

                    self.current = self
                        .pending
                        .front()
                        .map_or(self.current + 1, |group| group.sequence);

                    continue 'read;
                }

                Poll::Pending => {}

                Poll::Ready(Err(error)) => {
                    let aborted = self.pending[0].poll_aborted(waiter);

                    if !aborted {
                        return Poll::Ready(Err(error));
                    }

                    tracing::warn!(
                        error = ?error,
                        "current group evicted; skipping to next buffered group"
                    );

                    self.pending.pop_front();

                    self.current = self
                        .pending
                        .front()
                        .map_or(self.current + 1, |group| group.sequence);

                    continue 'read;
                }
            }
        }

        let (oldest_timestamp, current_end) =
            if let Some(index) = self
                .pending
                .iter()
                .position(|group| group.sequence <= self.current)
            {
                match self.poll_min_timestamp(index, waiter) {
                    Poll::Ready(Ok(timestamp)) => {
                        let end = self.pending[index].max_end;

                        (
                            Some(std::time::Duration::from(timestamp)),
                            end,
                        )
                    }

                    _ => (None, None),
                }
            } else {
                (None, None)
            };

        let mut next_group = None;

        for index in 0..self.pending.len() {
            if self.pending[index].sequence <= self.current {
                continue;
            }

            if let Poll::Ready(Ok(timestamp)) =
                self.poll_min_timestamp(index, waiter)
            {
                next_group = Some((
                    index,
                    std::time::Duration::from(timestamp),
                ));
                break;
            }
        }

        let mut max_timestamp = std::time::Duration::ZERO;

        for index in (0..self.pending.len()).rev() {
            if self.pending[index].sequence <= self.current {
                break;
            }

            if let Poll::Ready(Ok(timestamp)) =
                self.poll_max_timestamp(index, waiter)
            {
                max_timestamp = max_timestamp.max(timestamp.into());
                break;
            }
        }

        if let Some(front_sequence) =
            self.pending.front().map(|group| group.sequence)
            && front_sequence > self.current
            && let Some((_, next_start)) = next_group
            && (finished
                || max_timestamp.saturating_sub(next_start)
                    >= self.max_age)
        {
            self.current = front_sequence;
            continue;
        }

        let should_skip = if let Some((_, next_start)) = next_group {
            if let Some(oldest) = oldest_timestamp {
                let over_max_age =
                    max_timestamp.saturating_sub(oldest) >= self.max_age;

                let covered =
                    current_end.is_some_and(|end| end >= next_start);

                over_max_age || covered
            } else {
                max_timestamp.saturating_sub(next_start) >= self.max_age
            }
        } else {
            false
        };

        if let Some((new_index, _)) = next_group
            && should_skip
        {
            let mut discontinuities = 0;

            if self.rewind.live_edge.is_some() {
                for index in 0..new_index {
                    match self.poll_empty(index, waiter) {
                        Poll::Ready(true) => {
                            discontinuities += 1;
                        }

                        Poll::Ready(false) => {}

                        Poll::Pending => {
                            return Poll::Pending;
                        }
                    }
                }
            }

            self.pending.drain(0..new_index);
            self.mark_discontinuities(discontinuities);

            let new_current = self
                .pending
                .front()
                .expect("skip target exists")
                .sequence;

            tracing::debug!(
                old = self.current,
                new = new_current,
                "skipping slow groups"
            );

            self.current = new_current;
            continue;
        }

        if finished
            && let Some(index) = self
                .pending
                .iter()
                .position(|group| group.sequence > self.current)
            && matches!(self.poll_empty(index, waiter), Poll::Ready(true))
        {
            self.current = self.pending[index].sequence;
            continue;
        }

        if finished && self.pending.is_empty() {
            return Poll::Ready(Ok(None));
        }

        return Poll::Pending;
    }
}

    fn mark_discontinuities(&mut self, count: u64) {
        if count == 0 {
            return;
        }

        self.rewind.discontinuity += count;
        self.end = None;
        self.rewind.live_edge = None;
        self.rewind.boundary = None;
    }

    /// Read newly available groups from the track.
    fn poll_read_finish(
        &mut self,
        waiter: &kio::Waiter,
    ) -> Poll<Result<(), F::Error>> {
        loop {
            let Some(group) = ready!(self.track.poll_recv_group(waiter)?) else {
                return Poll::Ready(Ok(()));
            };

            let reader = GroupBuffer::new(group);
            let sequence = reader.group.sequence;

            let drop = match &self.rewind.boundary {
                Some(reset) => match reset.by_sequence(sequence) {
                    Some(true) => true,
                    Some(false) => sequence < self.current,
                    None => false,
                },

                None => sequence < self.current,
            };

            if drop {
                tracing::debug!(
                    old = ?sequence,
                    current = ?self.current,
                    "skipping old group"
                );

                continue;
            }

            let index = self
                .pending
                .partition_point(|group| group.sequence < sequence);

            self.pending.insert(index, reader);
        }
    }

    /// Detect a publisher rewind.
    fn poll_reset(
        &mut self,
        waiter: &kio::Waiter,
    ) -> Result<bool, F::Error> {
        let Some((previous_max, live_edge)) = self.rewind.live_edge else {
            return Ok(false);
        };

        let reset = {
            let mut found = None;

            for index in (0..self.pending.len()).rev() {
                if self.pending[index].sequence <= previous_max {
                    break;
                }

                let timestamp = match self.poll_min_timestamp(index, waiter) {
                    Poll::Ready(Ok(timestamp)) => timestamp,
                    _ => continue,
                };

                if timestamp < live_edge {
                    found = Some(Reset {
                        prev_max: previous_max,
                        group: self.pending[index].sequence,
                        timestamp,
                    });

                    break;
                }
            }

            let Some(reset) = found else {
                return Ok(false);
            };

            reset
        };

        self.pending.retain(|group| {
            match reset.by_sequence(group.sequence) {
                Some(stale) => !stale,

                None => group
                    .min_timestamp
                    .is_none_or(|timestamp| {
                        !reset.is_stale(group.sequence, timestamp)
                    }),
            }
        });

        self.rewind.discontinuity += 1;
        self.end = None;

        tracing::debug!(
            prev_max = reset.prev_max,
            group = reset.group,
            discontinuity = self.rewind.discontinuity,
            "buffer reset: group timestamps rewound"
        );

        self.rewind.boundary = Some(reset);

        self.current = self
            .pending
            .front()
            .map_or(reset.group, |group| group.sequence);

        self.rewind.live_edge = Some((reset.group, reset.timestamp));

        Ok(true)
    }

    /// Resolve groups left ambiguous by a reset.
    fn poll_classify(
        &mut self,
        waiter: &kio::Waiter,
    ) -> Result<(), F::Error> {
        let Some(reset) = self.rewind.boundary else {
            return Ok(());
        };

        let mut index = 0;

        while index < self.pending.len() {
            let sequence = self.pending[index].sequence;

            if reset.by_sequence(sequence).is_some() {
                index += 1;
                continue;
            }

            match self.poll_min_timestamp(index, waiter) {
                Poll::Ready(Ok(timestamp))
                    if reset.is_stale(sequence, timestamp) =>
                {
                    self.pending.remove(index);
                }

                _ => {
                    index += 1;
                }
            }
        }

        Ok(())
    }

    /// Set the maximum age mid-stream.
    pub fn set_max_age(&mut self, max_age: std::time::Duration) {
        self.max_age = max_age.min(self.track.info().max_age);

        let subscription = self
            .track
            .subscription()
            .with_max_age(max_age);

        let _ = self.track.update(subscription);
    }

    /*
     * GroupBuffer adapter methods.
     *
     * The decrypter is temporarily moved out of `self` so it can be mutably
     * borrowed at the same time as an individual GroupBuffer.
     */

    fn poll_read_group(
        &mut self,
        index: usize,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<Event>, F::Error>> {
        let decrypter = self.decrypter.take();

        let result = self.pending[index].poll_read(
            waiter,
            &self.format,
            decrypter,
        );

        self.decrypter = decrypter;
        result
    }

    fn poll_min_timestamp(
        &mut self,
        index: usize,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Timestamp, F::Error>> {
        let decrypter = self.decrypter.take();

        let result = self.pending[index].poll_min_timestamp(
            waiter,
            &self.format,
            decrypter,
        );

        self.decrypter = decrypter;
        result
    }

    fn poll_max_timestamp(
        &mut self,
        index: usize,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Timestamp, F::Error>> {
        let decrypter = self.decrypter.take();

        let result = self.pending[index].poll_max_timestamp(
            waiter,
            &self.format,
            decrypter,
        );

        self.decrypter = decrypter;
        result
    }

    fn poll_empty(
        &mut self,
        index: usize,
        waiter: &kio::Waiter,
    ) -> Poll<bool> {
        self.pending[index].poll_empty(waiter)
    }
}

/// Internal reader for a group of frames.
///
/// Handles two-phase frame reading, timestamp parsing, and min/max timestamp
/// tracking for age decisions.
struct GroupBuffer {
    group: moq_net::group::Consumer,

    /// Current frame index within the group.
    index: usize,

    /// Whether the group has carried any wire frame.
    ///
    /// A cleanly finished group with no wire frames is an explicit
    /// discontinuity marker.
    empty: bool,

    /// Read frames that have not yet been consumed by the caller.
    buffered: VecDeque<Frame>,

    /// Frame-end markers indexed by delivered-frame count.
    markers: VecDeque<(usize, Timestamp)>,

    /// Number of media frames delivered to the caller.
    delivered: usize,

    /// Minimum timestamp in the group.
    min_timestamp: Option<Timestamp>,

    /// Maximum timestamp in the group.
    max_timestamp: Option<Timestamp>,

    /// Furthest presentation point reached so far.
    ///
    /// Stored as a wall-clock duration so cross-scale comparisons are cheap.
    max_end: Option<std::time::Duration>,
}

impl GroupBuffer {
    fn new(group: moq_net::group::Consumer) -> Self {
        Self {
            group,
            index: 0,
            empty: true,
            buffered: VecDeque::new(),
            markers: VecDeque::new(),
            delivered: 0,
            max_timestamp: None,
            min_timestamp: None,
            max_end: None,
        }
    }

    /// Poll for the next frame or boundary event from this group.
    fn poll_read<F: Container<Error = crate::error::Error>>(
        &mut self,
        waiter: &kio::Waiter,
        format: &F,
        mut decrypter: Option<&mut dyn FrameDecrypter>,
    ) -> Poll<Result<Option<Event>, F::Error>> {
        loop {
            if self
                .markers
                .front()
                .is_some_and(|(index, _)| *index <= self.delivered)
            {
                let (_, end) = self.markers.pop_front().unwrap();

                return Poll::Ready(Ok(Some(Event::FrameEnd(end))));
            }

            if let Some(frame) = self.buffered.pop_front() {
                self.delivered += 1;

                return Poll::Ready(Ok(Some(Event::Frame(frame))));
            }

            if !ready!(
                self.buffer_once(
                    waiter,
                    format,
                    decrypter.as_deref_mut(),
                )?
            ) {
                return Poll::Ready(Ok(None));
            }
        }
    }

    /// Add one more wire frame to the buffer if possible.
    ///
    /// Returns `false` if the group is finished.
    fn buffer_once<F: Container<Error = crate::error::Error>>(
        &mut self,
        waiter: &kio::Waiter,
        format: &F,
        decrypter: Option<&mut dyn FrameDecrypter>,
    ) -> Poll<Result<bool, F::Error>> {
        let frames = if let Some(decrypter) = decrypter {
            let raw_reader = GroupReader::new(&mut self.group);
            let mut reader = ProtectedFrame::new(raw_reader, decrypter);

            ready!(format.poll_read_frames(&mut reader, waiter))?
        } else {
            let mut reader = GroupReader::new(&mut self.group);

            ready!(format.poll_read_frames(&mut reader, waiter))?
        };

        let Some(frames) = frames else {
            return Poll::Ready(Ok(false));
        };

        self.empty = false;

        for mut frame in frames {
            if let Some(bound) = format.end(&frame) {
                self.note_end(bound);
                self.markers.push_back((self.index, bound));
                continue;
            }

            self.min_timestamp = Some(match self.min_timestamp {
                Some(existing) => existing.min(frame.timestamp),
                None => frame.timestamp,
            });

            self.max_timestamp = Some(match self.max_timestamp {
                Some(existing) => existing.max(frame.timestamp),
                None => frame.timestamp,
            });

            self.note_end(frame.timestamp);

            if let Some(duration) = frame.duration {
                let end = std::time::Duration::from(frame.timestamp)
                    + std::time::Duration::from(duration);

                self.max_end = Some(
                    self.max_end
                        .map_or(end, |existing| existing.max(end)),
                );
            }

            // The first frame of a group is always a keyframe by protocol
            // invariant. Preserve container-provided keyframe flags thereafter.
            frame.keyframe = frame.keyframe || self.index == 0;
            self.index += 1;

            self.buffered.push_back(frame);
        }

        Poll::Ready(Ok(true))
    }

    /// Ensure at least one media frame is buffered.
    fn buffer_one<F: Container<Error = crate::error::Error>>(
        &mut self,
        waiter: &kio::Waiter,
        format: &F,
        mut decrypter: Option<&mut dyn FrameDecrypter>,
    ) -> Poll<Result<bool, F::Error>> {
        loop {
            if !self.buffered.is_empty() {
                return Poll::Ready(Ok(true));
            }

            if !ready!(
                self.buffer_once(
                    waiter,
                    format,
                    decrypter.as_deref_mut(),
                )?
            ) {
                return Poll::Ready(Ok(false));
            }

            // The wire frame decoded to no media frames. Continue reading.
        }
    }

    /// Read all remaining wire frames.
    fn buffer_all<F: Container<Error = crate::error::Error>>(
        &mut self,
        waiter: &kio::Waiter,
        format: &F,
        mut decrypter: Option<&mut dyn FrameDecrypter>,
    ) -> Poll<Result<(), F::Error>> {
        while ready!(
            self.buffer_once(
                waiter,
                format,
                decrypter.as_deref_mut(),
            )?
        ) {}

        Poll::Ready(Ok(()))
    }

    /// Poll for the maximum timestamp in this group.
    fn poll_max_timestamp<F: Container<Error = crate::error::Error>>(
        &mut self,
        waiter: &kio::Waiter,
        format: &F,
        mut decrypter: Option<&mut dyn FrameDecrypter>,
    ) -> Poll<Result<Timestamp, F::Error>> {
        let _ = self.buffer_all(
            waiter,
            format,
            decrypter.as_deref_mut(),
        )?;

        if let Some(max) = self.max_timestamp {
            return Poll::Ready(Ok(max));
        }

        if let Poll::Ready(_frames) = self.group.poll_finished(waiter)? {
            return Poll::Ready(Err(
                moq_net::Error::Decode(moq_net::DecodeError::Short).into()
            ));
        }

        Poll::Pending
    }

    /// Poll for the minimum timestamp in this group.
    fn poll_min_timestamp<F: Container<Error = crate::error::Error>>(
        &mut self,
        waiter: &kio::Waiter,
        format: &F,
        mut decrypter: Option<&mut dyn FrameDecrypter>,
    ) -> Poll<Result<Timestamp, F::Error>> {
        let _ = self.buffer_one(
            waiter,
            format,
            decrypter.as_deref_mut(),
        )?;

        if let Some(min) = self.min_timestamp {
            return Poll::Ready(Ok(min));
        }

        if let Poll::Ready(_frames) = self.group.poll_finished(waiter)? {
            return Poll::Ready(Err(
                moq_net::Error::Decode(moq_net::DecodeError::Short).into()
            ));
        }

        Poll::Pending
    }

    /// True if the transport can no longer deliver the frame at which this
    /// group stopped.
    fn poll_aborted(&mut self, waiter: &kio::Waiter) -> bool {
        matches!(
            self.group.poll_finished(waiter),
            Poll::Ready(Err(_))
        )
    }

    fn note_end(&mut self, timestamp: Timestamp) {
        let end = std::time::Duration::from(timestamp);

        self.max_end = Some(
            self.max_end
                .map_or(end, |existing| existing.max(end)),
        );
    }

    /// Returns whether this is a clean empty group.
    ///
    /// This does not parse payload data and therefore does not need the
    /// decrypter.
    fn poll_empty(&mut self, waiter: &kio::Waiter) -> Poll<bool> {
        if !self.empty {
            return Poll::Ready(false);
        }

        match self.group.poll_finished(waiter) {
            Poll::Ready(Ok(_)) => Poll::Ready(true),
            Poll::Ready(Err(_)) => Poll::Ready(false),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl std::ops::Deref for GroupBuffer {
    type Target = moq_net::group::Consumer;

    fn deref(&self) -> &Self::Target {
        &self.group
    }
}


#[cfg(test)]
mod tests {
	use super::Container as ContainerTrait;
	use super::*;
	use crate::catalog::hang::Container;
	use std::time::Duration;

	use bytes::Bytes;

	/// Mint a standalone track for tests via a throwaway broadcast, since tracks are
	/// born from their broadcast (no public `track::Producer::new`).
	fn track_producer(
		name: impl Into<std::sync::Arc<str>>,
		info: impl Into<Option<moq_net::track::Info>>,
	) -> moq_net::track::Producer {
		moq_net::broadcast::Info::new()
			.produce()
			.create_track(name, info)
			.unwrap()
	}

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	/// Test-only container that round-trips a per-sample duration on the wire, so the
	/// duration-based skip can be exercised without building a real CMAF init segment.
	/// Each frame is `[timestamp_us: u64 LE][duration_us: u64 LE][payload]`.
	struct DurationWire;

	/// Encode a `[timestamp][duration][payload]` DurationWire frame.
	fn encode_duration_frame(timestamp: Timestamp, duration: Timestamp) -> Vec<u8> {
		let mut buf = Vec::with_capacity(18);
		buf.extend_from_slice(&(timestamp.as_micros() as u64).to_le_bytes());
		buf.extend_from_slice(&(duration.as_micros() as u64).to_le_bytes());
		buf.extend_from_slice(&[0xDE, 0xAD]);
		buf
	}

	impl ContainerTrait for DurationWire {
		type Error = crate::Error;

		fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			// The duration tests write frames directly via `write_duration_frame`;
			// this path just preserves the timestamp with an unknown duration.
			for frame in frames {
				group.write_frame(frame.timestamp, encode_duration_frame(frame.timestamp, ts(0)))?;
			}
			Ok(())
		}

		fn poll_read(
			&self,
			group: &mut moq_net::group::Consumer,
			waiter: &kio::Waiter,
		) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
			use bytes::Buf;

			let Some(mut data) = ready!(group.poll_read_frame(waiter)?).map(|f| f.payload) else {
				return Poll::Ready(Ok(None));
			};

			let timestamp = ts(data.get_u64_le());
			let duration = ts(data.get_u64_le());
			let payload = data.copy_to_bytes(data.remaining());

			Poll::Ready(Ok(Some(vec![Frame {
				timestamp,
				payload,
				keyframe: false,
				duration: Some(duration),
			}])))
		}
	}

	/// Write one DurationWire frame (timestamp and duration in µs) into a group.
	fn write_duration_frame(group: &mut moq_net::group::Producer, timestamp: Timestamp, duration: Timestamp) {
		group
			.write_frame(timestamp, encode_duration_frame(timestamp, duration))
			.unwrap();
	}

	/// Write a finished group with explicit sequence and timestamps (Container::Legacy(crate::container::Kind::Data) format).
	fn write_group(track: &mut moq_net::track::Producer, sequence: u64, timestamps: &[Timestamp]) {
		let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
		for &timestamp in timestamps {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(&mut group, &[frame])
				.unwrap();
		}
		group.finish().unwrap();
	}

	/// Drain all available frames with a per-read timeout.
	async fn read_all(consumer: &mut Consumer<Container>) -> Result<Vec<Frame>, crate::Error> {
		let mut frames = Vec::new();
		loop {
			match tokio::time::timeout(Duration::from_millis(200), consumer.read()).await {
				Ok(Ok(Some(frame))) => frames.push(frame),
				Ok(Ok(None)) => break,
				Ok(Err(e)) => return Err(e),
				Err(_) => panic!(
					"read_all: Consumer::read timed out after 200ms ({} frames collected so far)",
					frames.len()
				),
			}
		}
		Ok(frames)
	}

	/// Wrap `track` so only the container consumer performs age skips. The
	/// transport gets the media track's full retention window and hands over every
	/// retained group; what the container does with them is what the test measures.
	///
	/// Both layers enforce the same budget, and normally should: the transport skipping
	/// a group the consumer was going to skip anyway just saves the bandwidth. These
	/// tests are the exception, since they are about the consumer's half of it.
	fn container_max_age_only(track: moq_net::track::Subscriber, max_age: std::time::Duration) -> Consumer<Container> {
		let control = track.control();
		let mut consumer = Consumer::new(track, Container::Legacy(crate::container::Kind::Data));
		consumer.set_max_age(max_age);
		control
			.update(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(30)))
			.unwrap();
		consumer
	}

	// ---- Basic Reading ----

	#[test]
	fn new_inherits_the_initial_subscription_latency() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let max_age = Duration::from_millis(250);
		let subscriber = track.subscribe(moq_net::track::Subscription::default().with_max_age(max_age));

		let consumer = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));

		assert_eq!(consumer.max_age, max_age);
		assert_eq!(consumer.track.subscription().max_age, max_age);
	}

	#[tokio::test]
	async fn read_single_group() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);

		// Next read returns None (track ended)
		assert!(consumer.read().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn empty_group_declares_a_discontinuity() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(2)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		track
			.create_group(moq_net::group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		write_group(&mut track, 2, &[ts(1_000_000)]);
		track.finish().unwrap();

		assert_eq!(consumer.read().await.unwrap().unwrap().timestamp, ts(0));
		assert_eq!(consumer.discontinuity(), 0);
		assert_eq!(consumer.read().await.unwrap().unwrap().timestamp, ts(1_000_000));
		assert_eq!(consumer.discontinuity(), 1);
	}

	#[tokio::test]
	async fn latency_skip_preserves_empty_group_discontinuity() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(2)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));
		// Keep transport filtering out of this test so it isolates the mux skip logic.
		consumer.max_age = Duration::ZERO;

		write_group(&mut track, 0, &[ts(0)]);
		let mut marker = track.create_group(moq_net::group::Info { sequence: 2 }).unwrap();
		write_group(&mut track, 3, &[ts(1_000_000)]);

		assert_eq!(consumer.read().await.unwrap().unwrap().timestamp, ts(0));
		assert_eq!(consumer.discontinuity(), 0);
		assert!(
			tokio::time::timeout(Duration::from_millis(20), consumer.read())
				.await
				.is_err()
		);

		marker.finish().unwrap();
		track.finish().unwrap();
		assert_eq!(consumer.read().await.unwrap().unwrap().timestamp, ts(1_000_000));
		assert_eq!(consumer.discontinuity(), 1);
	}

	#[tokio::test]
	async fn read_multiple_frames_single_group() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0), ts(33_000), ts(66_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));
		assert_eq!(frames[2].timestamp, ts(66_000));

		assert!(frames[0].keyframe);
	}

	#[tokio::test]
	async fn read_multiple_groups_within_latency() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// 5 groups, 20ms spacing. Total span = 80ms, well within the 500ms max age.
		for i in 0..5u64 {
			write_group(&mut track, i, &[ts(i * 20_000)]);
		}
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 5);
	}

	// ---- Age Skipping ----

	#[tokio::test]
	async fn latency_skip_delivers_recent_groups() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0: 5 frames, NOT finished (blocks consumer)
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for f in 0..5u64 {
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group0,
					&[Frame {
						timestamp: ts(f * 2_000),
						payload: Bytes::from_static(&[0xDE, 0xAD]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
		}

		// Groups 1-19: finished, 15ms spacing, 5 frames each
		for g in 1..20u64 {
			let timestamps: Vec<_> = (0..5).map(|f| ts(g * 15_000 + f * 2_000)).collect();
			write_group(&mut track, g, &timestamps);
		}
		track.finish().unwrap();

		// Finish group 0 after consumer has had time to accumulate pending groups
		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		// Group 0's 5 frames + some later groups (earlier ones skipped by age)
		assert!(frames.len() >= 25, "Expected >= 25 frames, got {}", frames.len());
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn zero_latency_skips_aggressively() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_age_only(consumer_track, Duration::ZERO);

		// Group 0 at ts 0 keeps timestamps monotonic with sequence (groups 1-9 follow at
		// g*50 ms), so the test exercises age skipping and not rewind detection.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		for g in 1..10u64 {
			let timestamps: Vec<_> = (0..3).map(|f| ts(g * 50_000 + f * 5_000)).collect();
			write_group(&mut track, g, &timestamps);
		}
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 28, "Expected group 0 frame + groups 1-9");
		assert!(!frames.is_empty(), "Expected at least some frames");
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn latency_skip_correctness() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_age_only(consumer_track, Duration::from_millis(100));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		for g in 1..10u64 {
			write_group(&mut track, g, &[ts(g * 30_000)]);
		}
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty(), "Expected at least some frames");
		assert_eq!(frames.len(), 10, "Expected group 0 frame + groups 1-9");
		assert_eq!(frames[0].timestamp, ts(0));

		for i in 1..10u64 {
			assert_eq!(frames[i as usize].timestamp, ts(i * 30_000));
		}
		finisher.await.expect("finisher task panicked");
	}

	// ---- Rewind / reneg ----

	/// The reset boundary classifies out-of-order groups by `(sequence, timestamp)`.
	/// Old epoch peaked at group 55 (ts 100); group 58 rewound to ts 90.
	#[test]
	fn reset_classifies_out_of_order_groups() {
		let reset = Reset {
			prev_max: 55,
			group: 58,
			timestamp: ts(90),
		};

		// Late new-epoch gap-filler: sequence in (55, 58), ts below the rewind. Keep.
		assert!(!reset.is_stale(57, ts(88)));
		// Old straggler from before the peak (low sequence). Drop, even though its ts (86)
		// is below the rewind — sequence is what separates it from group 57.
		assert!(reset.is_stale(52, ts(86)));
		// Old straggler in the gap whose higher ts hadn't arrived at detection. Drop.
		assert!(reset.is_stale(56, ts(105)));
		// At or after the rewound group: new epoch. Keep.
		assert!(!reset.is_stale(58, ts(90)));
		assert!(!reset.is_stale(59, ts(92)));
		// At or before the old peak: old epoch. Drop.
		assert!(reset.is_stale(55, ts(100)));
	}

	#[tokio::test]
	async fn rewind_at_the_cursor_signals_the_first_frame() {
		tokio::time::pause();
		for drained in [false, true] {
			let mut track = track_producer(
				"cursor-rewind",
				hang::container::track_info(hang::catalog::PRIORITY.video),
			);
			let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
			write_group(&mut track, 0, &[ts(600_000_000)]);
			assert_eq!(consumer.read().await.unwrap().unwrap().timestamp, ts(600_000_000));
			if drained {
				assert!(
					tokio::time::timeout(Duration::from_millis(1), consumer.read())
						.await
						.is_err()
				);
			}
			// One new group, so no higher-sequence successor can reveal the reset.
			write_group(&mut track, 1, &[ts(0), ts(100_000)]);
			assert_eq!(consumer.read().await.unwrap().unwrap().timestamp, ts(0));
			assert_eq!(consumer.discontinuity(), 1, "first rewound frame, drained={drained}");
			assert_eq!(consumer.read().await.unwrap().unwrap().timestamp, ts(100_000));
			assert_eq!(consumer.discontinuity(), 1, "no duplicate reset within a group");
		}
	}

	/// A new-epoch group that arrives out of order *below* the resume point is kept and
	/// played, not dropped — the bug a plain "floor = detection group" would have.
	#[tokio::test]
	async fn reset_keeps_out_of_order_new_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Old epoch, played forward until the live edge passes the rewind point.
		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(100_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);
		// New epoch's later group (seq 5, ts 3 ms) arrives first and triggers the reset.
		write_group(&mut track, 5, &[ts(3_000)]);

		// Its earlier gap-fillers (seq 3, 4) land after the reset, below the resume point.
		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			write_group(&mut track, 3, &[ts(1_000)]);
			write_group(&mut track, 4, &[ts(2_000)]);
			track.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();

		// Old epoch played before the reset, and all three new-epoch groups survived —
		// including the two out-of-order gap-fillers that arrived below the resume point.
		assert!(micros.contains(&100_000), "old epoch played before the reset");
		assert!(
			micros.contains(&1_000) && micros.contains(&2_000) && micros.contains(&3_000),
			"out-of-order new-epoch groups kept, got {micros:?}"
		);
		assert_eq!(consumer.discontinuity(), 1, "one rewind detected");
		finisher.await.expect("finisher task panicked");
	}

	/// A rewind is detected even when a higher-sequence group has already caught back up past
	/// the live edge (so the newest pending group looks forward). Scanning only `back()` would
	/// miss the lower-sequence rewound group and play the reneged tail without a discontinuity.
	#[tokio::test]
	async fn reset_detected_behind_forward_newest_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Old timeline, played to a live edge of 200 ms.
		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(100_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);
		// Group 6 (highest sequence) is forward of the live edge, masking...
		write_group(&mut track, 6, &[ts(250_000)]);
		// ...group 5, a lower-sequence group that rewound below it.
		write_group(&mut track, 5, &[ts(50_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();

		assert_eq!(
			consumer.discontinuity(),
			1,
			"rewind detected behind a forward newest group"
		);
		assert!(micros.contains(&50_000), "resumed at the rewound group, got {micros:?}");
		assert!(
			!micros.contains(&200_000),
			"the reneged tail was dropped, got {micros:?}"
		);
	}

	/// A newer group whose timestamps jump backwards past the buffered tail drops the
	/// reneged groups and resumes from the rewound group. Models a voice agent that
	/// runs ahead of playback and then interrupts to start a new utterance.
	#[tokio::test]
	async fn backwards_timestamp_resets_buffer() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10)));
		// Large max age so the slow-group skip never fires; isolate the rewind path.
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Publisher runs ahead: groups 0-4 at 0, 100, 200, 300, 400 ms.
		for i in 0..5u64 {
			write_group(&mut track, i, &[ts(i * 100_000)]);
		}
		// Then it reneges and rewinds: group 5 restarts the timeline at 0 ms.
		write_group(&mut track, 5, &[ts(0), ts(20_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let timestamps: Vec<_> = frames.iter().map(|f| f.timestamp).collect();

		// We play forward until the live edge passes the rewind point (through 100 ms), then
		// the rewind drops the buffered-ahead groups (200/300/400 ms) and resumes at group 5.
		assert_eq!(timestamps, vec![ts(0), ts(100_000), ts(0), ts(20_000)]);
		assert_eq!(consumer.discontinuity(), 1);
	}

	/// Rewind detection is always on: a backwards group timestamp resets the buffer with no
	/// configuration. Here group 2 rewinds the timeline and bumps the discontinuity counter.
	#[tokio::test]
	async fn backwards_timestamp_always_resets() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(500_000)]);
		write_group(&mut track, 2, &[ts(0)]); // rewind
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let timestamps: Vec<_> = frames.iter().map(|f| f.timestamp).collect();

		assert_eq!(timestamps, vec![ts(0), ts(500_000), ts(0)]);
		assert_eq!(consumer.discontinuity(), 1, "the backwards group triggered a reset");
	}

	// ---- Empty payloads ----

	/// Write one frame with an empty payload: a marker saying content stops at
	/// `timestamp`, carrying no media.
	fn write_marker(group: &mut moq_net::group::Producer, timestamp: Timestamp) {
		let frame = Frame {
			timestamp,
			payload: Bytes::new(),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Data)
			.write(group, &[frame])
			.unwrap();
	}

	/// An empty payload carries no media, so it's skipped rather than surfaced as a
	/// frame or raised as an error. It times the previous frame and never means the
	/// track ended.
	#[tokio::test]
	async fn empty_payload_is_skipped() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		let media = |timestamp| Frame {
			timestamp,
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Video)
			.write(&mut group, &[media(ts(0))])
			.unwrap();
		write_marker(&mut group, ts(16_000)); // closes the first frame
		Container::Legacy(crate::container::Kind::Video)
			.write(&mut group, &[media(ts(33_000))])
			.unwrap();
		write_marker(&mut group, ts(50_000)); // the group's end
		group.finish().unwrap();
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2, "markers are not surfaced as media");
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));
	}

	#[tokio::test]
	async fn leading_marker_preserves_the_first_media_keyframe() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_marker(&mut group, ts(20_000));
		Container::Legacy(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(20_000),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		let frame = consumer.read().await.unwrap().unwrap();
		assert!(frame.keyframe, "the marker does not consume the first-media slot");
		assert!(frame.duration.is_none(), "a leading marker has no previous frame");
	}

	/// Reading a marker consumes its frame, so a run of them makes progress and the
	/// consumer reaches the next group instead of spinning. `read_all` times out per
	/// read, so a stall or an infinite loop fails this rather than hanging forever.
	#[tokio::test]
	async fn consecutive_markers_do_not_stall() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		for i in 1..5u64 {
			write_marker(&mut group, ts(i * 1_000));
		}
		group.finish().unwrap();
		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![0, 100_000], "markers skipped, next group reached");
	}

	/// LOC consumers skip an empty payload so later producers can write the duration marker.
	#[tokio::test]
	async fn loc_empty_payload_is_skipped() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Loc(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		let media = Frame {
			timestamp: ts(0),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: true,
			duration: None,
		};
		Container::Loc(crate::container::Kind::Video)
			.write(&mut group, &[media])
			.unwrap();
		Container::Loc(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(33_000),
					payload: Bytes::new(),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1, "the empty LOC payload is not submitted");
		assert_eq!(frames[0].timestamp, ts(0));
	}

	// ---- Group Ordering ----

	#[tokio::test]
	async fn groups_delivered_in_sequence_order() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 2, &[ts(60_000)]);
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(10)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(30_000));
		assert_eq!(frames[2].timestamp, ts(60_000));
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn adjacent_group_flushed_immediately() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(30_000));
	}

	// ---- B-frames ----

	#[tokio::test]
	async fn bframes_within_group() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0), ts(66_000), ts(33_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(66_000));
		assert_eq!(frames[2].timestamp, ts(33_000));
	}

	// ---- Track Lifecycle ----

	#[tokio::test]
	async fn empty_track_returns_none() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		track.finish().unwrap();

		let result = tokio::time::timeout(Duration::from_millis(200), consumer.read()).await;
		match result {
			Ok(Ok(None)) => {} // expected: track ended
			Ok(Ok(Some(_))) => panic!("expected None for empty track, got Some"),
			Ok(Err(e)) => panic!("expected None for empty track, got error: {e}"),
			Err(_) => panic!("should not hang on empty track"),
		}
	}

	#[tokio::test]
	async fn track_closed_with_error() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		track.abort(moq_net::Error::Cancel).unwrap();

		let result = tokio::time::timeout(Duration::from_millis(500), async {
			let mut frames = Vec::new();
			while let Ok(Some(frame)) = consumer.read().await {
				frames.push(frame);
			}
			frames
		})
		.await;

		assert!(result.is_ok(), "Consumer should not hang after track error");
	}

	// ---- Gap Recovery ----

	#[tokio::test]
	async fn gap_in_group_sequence_recovery() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		write_group(&mut track, 1, &[ts(40_000), ts(60_000)]);
		write_group(&mut track, 3, &[ts(120_000), ts(140_000)]);
		write_group(&mut track, 4, &[ts(160_000), ts(180_000)]);
		write_group(&mut track, 5, &[ts(200_000), ts(220_000)]);
		write_group(&mut track, 6, &[ts(240_000), ts(260_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(frames.len() >= 4, "Expected >= 4 frames, got {}", frames.len());
	}

	#[tokio::test]
	async fn gap_at_start_of_sequence() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(80)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 5, &[ts(0), ts(20_000)]);
		write_group(&mut track, 7, &[ts(80_000), ts(100_000)]);
		write_group(&mut track, 8, &[ts(120_000), ts(140_000)]);
		write_group(&mut track, 9, &[ts(160_000), ts(180_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(frames.len() >= 4, "Expected >= 4 frames, got {}", frames.len());
	}

	// ---- Eviction recovery (pause/resume) ----

	/// A group that aged out of the relay cache (aborted with `Error::Old`) while the
	/// consumer was parked on it must not hang the consumer: reading it errors, and
	/// the consumer skips the gap to the next live group even though the track is NOT
	/// finished. This is the resume-from-pause path (the recorder stops reading, the
	/// group + the sequences after it evict, then it resumes).
	#[tokio::test]
	async fn evicted_group_with_gap_skips_to_live() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0: a frame the consumer reads, positioning it there.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		let first = consumer.read().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		// A live group arrives far ahead -- sequences 1..4 never come (evicted). The
		// track stays OPEN (not finished), the failure mode that used to hang.
		write_group(&mut track, 5, &[ts(150_000)]);

		// Group 0 ages out of the cache (the relay aborts it on eviction).
		group0.abort(moq_net::Error::Old).unwrap();

		// Must skip the evicted group + the gap and reach the live group, without
		// hanging on a track that never finishes.
		let next = tokio::time::timeout(Duration::from_secs(1), consumer.read())
			.await
			.expect("consumer hung on an evicted group / gap")
			.unwrap()
			.unwrap();
		assert_eq!(next.timestamp, ts(150_000), "skipped the evicted gap to the live group");
	}

	/// A missing (evicted) sequence with a newer group buffered must be skipped once the
	/// age budget runs out, even while the track is still LIVE -- not only once it's
	/// finished. This is the recorder resume stall: `current` points at a sequence the
	/// cache dropped, higher groups are buffered, and the track never finishes. The gap
	/// is indistinguishable from a stream that lost the delivery race, so the skip fires
	/// only once the newest content is a full budget past what the gap could still hold.
	#[tokio::test]
	async fn missing_sequence_skips_on_live_track() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0, then group 2 -- sequence 1 is missing (evicted) and never arrives.
		// The track is NOT finished (live), the case that used to hang. Group 3 pushes
		// the live edge a full budget past group 2's start, expiring the gap at 1.
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);
		write_group(&mut track, 3, &[ts(320_000)]);

		// Reading must reach group 2 across the gap instead of waiting forever for 1.
		let reached = tokio::time::timeout(Duration::from_secs(1), async {
			loop {
				let frame = consumer.read().await.unwrap().unwrap();
				if frame.timestamp == ts(200_000) {
					return;
				}
			}
		})
		.await;
		assert!(reached.is_ok(), "consumer hung on a missing sequence on a live track");
	}

	/// The other half of the budget-gated gap: while the newest content is still within
	/// the age budget of what the missing sequence could present, the consumer waits for
	/// it instead of writing it off, and delivers it when its stream loses the race but
	/// still arrives (#3258).
	#[tokio::test]
	async fn gap_keeps_late_arriving_group_within_max_age() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 2's stream beats group 1's, which hasn't arrived at all yet.
		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 2, &[ts(200_000)]);

		let first = consumer.read().await.unwrap().expect("group 0 frame");
		assert_eq!(first.timestamp, ts(0));

		// Group 0 is done; group 1's stream is still racing. Within the budget the
		// consumer waits at the gap rather than skipping to group 2.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"the gap at sequence 1 is within budget and must be waited for"
		);

		// Group 1's stream opens moments later, well within the 500ms budget.
		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![100_000, 200_000], "the late group is delivered in order");
	}

	/// An explicit start floor pins where delivery begins: when a later group's stream
	/// wins the arrival race, the consumer waits for the requested head under the age
	/// budget instead of dropping it on arrival (#3258).
	#[tokio::test]
	async fn startup_keeps_late_arriving_head_group_within_max_age() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(
			moq_net::track::Subscription::default()
				.with_start(moq_net::track::Position::group(0))
				.with_max_age(Duration::from_millis(500)),
		);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 1's stream wins the race, and the consumer polls before group 0 lands.
		write_group(&mut track, 1, &[ts(100_000)]);
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"the requested head is within budget and must be waited for"
		);

		// Group 0 arrives moments later, well within the 500ms budget.
		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![0, 100_000], "the head group is delivered first");
	}

	/// A group whose frames lost the race to a newer group's stream is still read once
	/// they land: the cursor starts at group 0 and waits under the budget (#3258).
	#[tokio::test]
	async fn startup_keeps_slow_earlier_stream_within_max_age() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0's stream opens first but carries no frames yet; group 1's frame wins.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_group(&mut track, 1, &[ts(100_000)]);

		// Startup latches onto the lowest arrived sequence and waits under the budget.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"group 0's frames are within budget and must be waited for"
		);

		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group0.finish().unwrap();
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![0, 100_000], "the slow stream's frames are delivered");
	}

	// ---- Decode errors ----

	/// A container that decodes each frame's payload as an 8-byte LE microsecond
	/// timestamp, but treats a `FAIL` payload as a malformed frame. Lets a test put a
	/// decodable frame first (so the consumer reads the group) and a decode failure after.
	struct FailingDecode;

	impl ContainerTrait for FailingDecode {
		type Error = crate::Error;

		fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			for frame in frames {
				group.write_frame(moq_net::Timestamp::ZERO, frame.payload.clone())?;
			}
			Ok(())
		}

		fn poll_read(
			&self,
			group: &mut moq_net::group::Consumer,
			waiter: &kio::Waiter,
		) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
			use bytes::Buf;

			let Some(mut data) = ready!(group.poll_read_frame(waiter)?).map(|f| f.payload) else {
				return Poll::Ready(Ok(None));
			};
			if data.as_ref() == b"FAIL" {
				return Poll::Ready(Err(crate::Error::UnknownFormat("malformed payload".into())));
			}
			Poll::Ready(Ok(Some(vec![Frame {
				timestamp: ts(data.get_u64_le()),
				payload: Bytes::new(),
				keyframe: false,
				duration: None,
			}])))
		}
	}

	/// A decode error on a cleanly-finished group must propagate to the caller, not be
	/// mistaken for a relay eviction and silently skipped. Eviction-skip only fires when
	/// the group's stream was actually aborted.
	#[tokio::test]
	async fn decode_error_propagates() {
		tokio::time::pause();
		let mut track = track_producer("test", None);
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, FailingDecode);

		// A decodable frame first (so the consumer reads the group), then a malformed one.
		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, Bytes::from(0u64.to_le_bytes().to_vec()))
			.unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, Bytes::from_static(b"FAIL"))
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		// The first frame decodes; the malformed second frame must surface as an error.
		let first = consumer.read().await;
		assert!(matches!(first, Ok(Some(_))), "first frame should decode, got {first:?}");

		let second = tokio::time::timeout(Duration::from_millis(200), consumer.read())
			.await
			.expect("consumer hung on a decode error");
		assert!(
			matches!(second, Err(crate::Error::UnknownFormat(_))),
			"decode error must propagate, got {second:?}"
		);
	}

	// ---- Frame Decoding ----

	#[tokio::test]
	async fn frame_timestamp_and_index_decoding() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0), ts(33_333), ts(66_666)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);

		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);

		assert_eq!(frames[1].timestamp, ts(33_333));

		assert_eq!(frames[2].timestamp, ts(66_666));
	}

	#[tokio::test]
	async fn frame_payload_preserved() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let payload_bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05];
		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from(payload_bytes.clone()),

					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);

		use bytes::Buf;
		let mut received = Vec::new();
		let mut payload = frames[0].payload.clone();
		while payload.has_remaining() {
			received.push(payload.get_u8());
		}
		assert_eq!(received, payload_bytes);
	}

	// ---- Regression ----

	#[tokio::test]
	async fn no_infinite_loop_with_buffered_frames() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 1, &[ts(100_000)]);

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			// Write group 2: recv_group fires, drops current buffer_until for group 1
			write_group(&mut track, 2, &[ts(200_000)]);
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
			track.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung — possible infinite loop regression");

		assert_eq!(frames.len(), 3);
		finisher.await.expect("finisher task panicked");
	}

	// ---- Edge Cases ----

	#[tokio::test]
	async fn large_timestamps() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(3700)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let one_hour = 3_600_000_000u64;
		write_group(&mut track, 0, &[ts(one_hour)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(one_hour));
		assert_eq!(frames[0].timestamp.as_micros(), one_hour as u128);
	}

	#[tokio::test]
	async fn set_max_age_changes_behavior() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		let frame = consumer.read().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(0));

		consumer.set_max_age(Duration::from_millis(100));

		assert!(consumer.read().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn max_timestamp_tracks_through_bframes() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(110)));
		// max age must exceed (group1_max - group0_min) = 100ms - 0ms = 100ms
		// to avoid the age skip and test B-frame timestamp tracking.
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for &timestamp in &[ts(0), ts(66_000), ts(33_000)] {
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group0,
					&[Frame {
						timestamp,
						payload: Bytes::from_static(&[0xDE, 0xAD]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
		}

		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung — max_timestamp regression");

		assert_eq!(frames.len(), 4, "Expected all 4 frames, got {}", frames.len());
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(66_000));
		assert_eq!(frames[2].timestamp, ts(33_000));
		assert_eq!(frames[3].timestamp, ts(100_000));
		finisher.await.expect("finisher task panicked");
	}

	// ---- Startup Behavior ----

	#[tokio::test]
	async fn startup_selects_earliest_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 3, &[ts(0)]);
		write_group(&mut track, 5, &[ts(150_000)]);

		let mut group7 = track.create_group(moq_net::group::Info { sequence: 7 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group7,
				&[Frame {
					timestamp: ts(300_000),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group7,
					&[Frame {
						timestamp: ts(400_000),
						payload: Bytes::from_static(&[0xBE, 0xEF]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
			group7.finish().unwrap();
			track.finish().unwrap();
		});

		let _frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("should not hang");

		finisher.await.unwrap();
	}

	/// An arrived group with no frames yet is settled only by its own FIN, abort, or the
	/// age budget, never by the track finishing: the track boundary ends new groups, not
	/// the frames still flowing on open ones. Here the budget expires it.
	#[tokio::test]
	async fn startup_skips_groups_without_data() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let _group5 = track.create_group(moq_net::group::Info { sequence: 5 }).unwrap();
		write_group(&mut track, 7, &[ts(210_000)]);

		// Group 5 is open and within budget: its frames may still arrive, so wait.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"an open frameless group within budget must be waited for"
		);

		// Group 9 pushes the newest content a full budget past what group 5 could still
		// present (bounded by group 7's start), expiring it.
		write_group(&mut track, 9, &[ts(800_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![210_000, 800_000], "the expired frameless group is skipped");
	}

	/// An aborted frameless group beyond a sequence gap is reaped rather than parked on
	/// forever: nothing else settles a group the cursor hasn't reached, so a finished
	/// track would otherwise never report its end.
	#[tokio::test]
	async fn aborted_frameless_group_after_a_gap_ends_the_track() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Sequence 0 never arrives; sequence 1's stream opens but never carries a frame.
		let group1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		track.finish().unwrap();

		// While group 1 is open its frames may still come, so the consumer waits.
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"an open frameless group must be waited for"
		);

		// The abort (an eviction) settles it, and the finished track ends cleanly.
		group1.abort(moq_net::Error::Old).unwrap();
		let end = tokio::time::timeout(Duration::from_millis(200), consumer.read())
			.await
			.expect("an aborted frameless group must not park the consumer");
		assert!(end.unwrap().is_none(), "the track ends cleanly");
	}

	/// Track completion is not group completion: a group already open when the track
	/// finishes can still receive frames, so it must not be skipped as if it were a
	/// missing sequence. Its late frames are delivered when they land within budget.
	#[tokio::test]
	async fn finished_track_waits_for_an_open_head_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0's stream is open but its frames lose the race; the track boundary
		// arrives before them.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"the open head group is within budget and must be waited for"
		);

		// Its frames land moments later, well within the 500ms budget.
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group0.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert_eq!(micros, vec![0, 100_000], "the open group's late frames are delivered");
	}

	#[tokio::test]
	async fn startup_single_group_mid_stream() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 100, &[ts(3_000_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	/// An unfloored mid-stream join adopts the publisher's served start instead of
	/// waiting for a gap below it to expire: on a quiet track that gap never would,
	/// since nothing newer arrives to age it out.
	#[tokio::test]
	async fn startup_mid_stream_live_track_starts_at_the_served_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// The track is far along and stays live (never finished); only the current
		// group is served, and nothing else arrives.
		write_group(&mut track, 100, &[ts(3_000_000)]);

		let frame = tokio::time::timeout(Duration::from_millis(200), consumer.read())
			.await
			.expect("an unfloored join must not wait out a gap below the served start")
			.unwrap()
			.expect("track still live");
		assert_eq!(frame.timestamp, ts(3_000_000));
	}

	#[tokio::test]
	async fn multiple_sequential_latency_skips() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(50)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xAA]),

					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 1, &[ts(100_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);
		write_group(&mut track, 3, &[ts(300_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty());
		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn latency_skip_boundary_exact() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xAA]),

					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty());
		finisher.await.unwrap();
	}

	/// Regression: a single stalled group with one newer group should trigger
	/// an age skip when the timestamp difference exceeds the max age.
	/// Previously, the span was computed across newer groups only (zero for one
	/// group), so the skip never fired.
	#[tokio::test]
	async fn single_newer_group_triggers_skip() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_age_only(consumer_track, Duration::from_millis(100));

		// Group 0: stalled at ts=0, NOT finished
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		// Group 1: finished, 200ms ahead (well beyond the 100ms max age)
		write_group(&mut track, 1, &[ts(200_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2, "Expected group 0 frame + group 1 frame");
		finisher.await.unwrap();
	}

	/// Regression: when the current group is fully consumed and the next sequence
	/// is missing (gap), the consumer should skip to the next available group
	/// once the track is fully received, rather than hanging forever.
	#[tokio::test]
	async fn single_missing_sequence_near_eof_skips() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_age_only(consumer_track, Duration::from_millis(100));

		// Group 0: finished normally
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		// Group 2: finished (group 1 is missing — sequence gap)
		write_group(&mut track, 2, &[ts(200_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3, "Expected group 0 (2 frames) + group 2 (1 frame)");
	}

	#[tokio::test]
	async fn group_error_skips_to_next() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		group0.abort(moq_net::Error::Cancel).unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	/// A finished group aborted afterwards (aged out of the track's max age window)
	/// keeps serving the frames it still holds: an abort only matters where a frame is
	/// missing, so a reader that stopped short of the end drains the rest and moves on
	/// to the next buffered group instead of ending the track.
	#[tokio::test]
	async fn finished_group_aborted_mid_read_drains_then_continues() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for timestamp in [ts(0), ts(10_000)] {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(&mut group0, &[frame])
				.unwrap();
		}
		group0.finish().unwrap();
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		// Take the first frame only, leaving the second unread.
		let first = consumer.read().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		// Expiry aborts the finished group while the reader is still inside it.
		group0.abort(moq_net::Error::Old).unwrap();

		let rest = read_all(&mut consumer).await.unwrap();
		assert_eq!(rest.len(), 2, "expected the held frame then group 1, got {rest:?}");
		assert_eq!(rest[0].timestamp, ts(10_000));
		assert_eq!(rest[1].timestamp, ts(30_000));
	}

	/// A live group that outgrows its cache budget is aborted. A reader whose current
	/// group died that way skips forward; the gap is not a decode error.
	#[tokio::test]
	async fn oversized_group_skips_to_next() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		let first = Frame {
			timestamp: ts(0),
			payload: Bytes::from(vec![0xDEu8; 1024]),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Data)
			.write(&mut group0, &[first])
			.unwrap();
		// Stay under the per-frame cap (hang prefixes a timestamp) so this is a group overflow,
		// not FrameTooLarge.
		let overflow = Frame {
			timestamp: ts(0),
			payload: Bytes::from(vec![0u8; (moq_net::group::MAX_CACHE_BYTES - 1024) as usize]),
			keyframe: false,
			duration: None,
		};
		let overflowed = Container::Legacy(crate::container::Kind::Data).write(&mut group0, &[overflow]);
		assert!(
			matches!(
				overflowed,
				Err(crate::Error::Hang(hang::Error::Moq(moq_net::Error::GroupTooLarge)))
			),
			"oversized group must abort as GroupTooLarge, got {overflowed:?}"
		);

		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1, "expected group 1 only, got {} frames", frames.len());
		assert_eq!(frames[0].timestamp, ts(30_000));
	}

	#[tokio::test]
	async fn track_finishes_while_reading() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			write_group(&mut track, 1, &[ts(30_000)]);
			tokio::time::sleep(Duration::from_millis(20)).await;
			track.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer should not hang");

		assert_eq!(frames.len(), 2);
		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn empty_group_advances() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		group0.finish().unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	// ---- VideoConfig Container ----

	#[tokio::test]
	async fn video_container_legacy() {
		tokio::time::pause();

		let mut track = track_producer("video", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_millis(500)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Write frames using Container::Legacy(crate::container::Kind::Data) encoding
		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		for i in 0..3u64 {
			let frame = Frame {
				timestamp: ts(i * 33_333),
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(&mut group, &[frame])
				.unwrap();
		}
		group.finish().unwrap();
		track.finish().unwrap();

		let mut frames = Vec::new();
		while let Some(frame) = consumer.read().await.unwrap() {
			frames.push(frame);
		}

		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);
		assert_eq!(frames[1].timestamp, ts(33_333));
		assert!(!frames[1].keyframe);
		assert_eq!(frames[2].timestamp, ts(66_666));
		assert!(!frames[2].keyframe);
	}

	// ---- Duration Skipping ----

	/// A stalled group whose frame covers up to the next group's start is skipped
	/// immediately, even with a max age budget far larger than the gap. Without
	/// duration support the consumer would block on the unfinished group forever.
	#[tokio::test]
	async fn duration_skip_advances_to_next_group() {
		tokio::time::pause();
		// DurationWire is a test-only container that doesn't stamp moq_net frame
		// timestamps; leave the track untimed so model-layer validation matches.
		let mut track = track_producer("test", None);
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10)));
		// The max age dwarfs the gap, so only duration coverage can trigger the skip.
		let mut consumer = Consumer::new(consumer_track, DurationWire);

		// Group 0: one frame at ts=0 lasting 33ms, never finished.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_duration_frame(&mut group0, ts(0), ts(33_000));

		// Group 1: finished, starts exactly where group 0's frame ends.
		let mut group1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_duration_frame(&mut group1, ts(33_000), ts(33_000));
		group1.finish().unwrap();

		track.finish().unwrap();

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung — duration skip regression");

		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));

		// group0 is intentionally never finished.
		drop(group0);
	}

	/// When the current group's frame ends before the next group begins, there is
	/// still a gap to cover, so we don't skip early: a late-arriving frame on the
	/// slow group is delivered rather than dropped.
	#[tokio::test]
	async fn duration_below_gap_does_not_skip() {
		tokio::time::pause();
		// DurationWire is untimed at the moq_net frame layer.
		let mut track = track_producer("test", None);
		let consumer_track =
			track.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10)));
		let mut consumer = Consumer::new(consumer_track, DurationWire);

		// Group 0: frame at ts=0 lasting only 10ms, far short of group 1 at 33ms.
		let mut group0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_duration_frame(&mut group0, ts(0), ts(10_000));

		// Group 1: finished at 33ms.
		let mut group1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_duration_frame(&mut group1, ts(33_000), ts(33_000));
		group1.finish().unwrap();
		track.finish().unwrap();

		// A second frame lands on group 0 and finishes it after the consumer has
		// had a chance to (incorrectly) skip.
		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			write_duration_frame(&mut group0, ts(20_000), ts(10_000));
			group0.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung");

		// The slow group's late frame survives because nothing covered the gap.
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(20_000));
		assert_eq!(frames[2].timestamp, ts(33_000));
		finisher.await.unwrap();
	}
	#[tokio::test]
	async fn live_duration_marker_follows_an_immediately_delivered_frame() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Video));
		let mut group = track.append_group().unwrap();
		Container::Legacy(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(b"video"),
					keyframe: true,
					duration: None,
				}],
			)
			.unwrap();
		let frame = tokio::time::timeout(Duration::from_secs(1), consumer.read())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(0));
		write_marker(&mut group, ts(15_000));
		let event = kio::wait(|waiter| consumer.poll_event(waiter)).await.unwrap();
		assert!(matches!(event, Some(Event::FrameEnd(end)) if end == ts(15_000)));
	}
}
