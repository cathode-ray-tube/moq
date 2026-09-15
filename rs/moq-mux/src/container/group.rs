use std::collections::VecDeque;
use std::task::{Poll, ready};

use super::{Container, Frame};
use crate::encryption::FrameDecrypter;
use crate::reader::{FrameReader, GroupReader, ProtectedFrame};

/// Decode a single [`moq_net::group::Consumer`] into a finite stream of media
/// [`Frame`]s.
///
/// The optional decrypter persists for the lifetime of this group consumer and
/// is reused across all calls to [`Self::poll_read`].
pub struct GroupConsumer<F: Container> {
    group: moq_net::group::Consumer,
    format: F,

    /// Optional decrypter.
    ///
    /// Persistent across reads and group transitions.
    decrypter: Option<Box<dyn FrameDecrypter>>,

    // Frames decoded from the last wire frame but not yet returned.
    pending: VecDeque<Frame>,

    // How many media frames we have returned, so the first one can be marked
    // as a keyframe.
    index: u64,
}

impl<F: Container> GroupConsumer<F> {
    /// Decode `group` with the given container format.
    pub fn new(
        group: moq_net::group::Consumer,
        format: F,
        decrypter: Option<Box<dyn FrameDecrypter>>,
    ) -> Self {
        Self {
            group,
            format,
            decrypter,
            pending: VecDeque::new(),
            index: 0,
        }
    }

    /// The sequence number of this group within its track.
    pub fn sequence(&self) -> u64 {
        self.group.sequence
    }

    /// Read the next frame, or `None` once the group ends.
    pub async fn read(&mut self) -> Result<Option<Frame>, F::Error> {
        kio::wait(|waiter| self.poll_read(waiter)).await
    }

    /// Poll for the next frame, without blocking.
    pub fn poll_read(
        &mut self,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<Frame>, F::Error>> {
        // Hold the latest media frame until a marker or FIN times it.
        // FETCH groups are already finished, so this look-ahead does not add
        // latency there.
        while self.pending.front().is_none_or(|frame| {
            self.format.kind() == super::Kind::Video
                && frame.duration.is_none()
                && self.pending.len() < 2
        }) {
            let decoded = match self.poll_read_frames(waiter) {
                Poll::Pending => return Poll::Pending,

                Poll::Ready(Err(error)) => {
                    return Poll::Ready(Err(error));
                }

                Poll::Ready(Ok(frames)) => frames,
            };

            match decoded {
                Some(frames) => {
                    for frame in frames {
                        if let Some(bound) = self.format.end(&frame) {
                            if self.format.kind() == super::Kind::Video
                                && let Some(last) = self.pending.back_mut()
                            {
                                super::close_duration(last, bound);
                            }
                        } else {
                            self.pending.push_back(frame);
                        }
                    }
                }

                None => {
                    return Poll::Ready(Ok(self.pop_media()));
                }
            }
        }

        Poll::Ready(Ok(self.pop_media()))
    }

    fn poll_read_frames(
        &mut self,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<Vec<Frame>>, F::Error>> {
        let raw_reader = GroupReader::new(&mut self.group);

        match self.decrypter.as_mut() {
            Some(decrypter) => {
                let mut reader = ProtectedFrame::new(
                    raw_reader,
                    decrypter.as_mut(),
                );

                self.format.poll_read_frames(&mut reader, waiter)
            }

            None => {
                let mut reader = raw_reader;

                self.format.poll_read_frames(&mut reader, waiter)
            }
        }
    }

    fn pop_media(&mut self) -> Option<Frame> {
        let mut frame = self.pending.pop_front()?;

        // The first frame of a group is always a keyframe by protocol
        // invariant; trust the container's flag otherwise so CMAF mid-group
        // keyframes survive.
        frame.keyframe = frame.keyframe || self.index == 0;
        self.index += 1;

        Some(frame)
    }
}


#[cfg(test)]
mod tests {
	use super::*;
	use crate::catalog::hang::Container as Hang;

	fn frame(timestamp_us: u64, payload: &'static [u8], keyframe: bool) -> Frame {
		Frame {
			timestamp: moq_net::Timestamp::from_micros(timestamp_us).unwrap(),
			payload: bytes::Bytes::from_static(payload),
			keyframe,
			duration: None,
		}
	}

	/// Read one retained group end to end, without a subscription.
	#[tokio::test]
	async fn reads_a_group_to_completion() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("media", None).unwrap();
		let consumer = broadcast.consume();

		let mut media = crate::container::Producer::new(track, Hang::Legacy(crate::container::Kind::Data));
		media.write(frame(1_000_000, b"keyframe", true)).unwrap();
		media.write(frame(1_020_000, b"delta", false)).unwrap();
		media.finish().unwrap();

		let group = consumer.track("media").unwrap().fetch_group(0, None).await.unwrap();
		let mut group = GroupConsumer::new(group, Hang::Legacy(crate::container::Kind::Data));
		assert_eq!(group.sequence(), 0);

		let first = group.read().await.unwrap().unwrap();
		assert_eq!(first.payload, b"keyframe".as_slice());
		assert!(first.keyframe);

		let second = group.read().await.unwrap().unwrap();
		assert_eq!(second.payload, b"delta".as_slice());
		assert!(!second.keyframe);

		assert!(group.read().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn empty_data_frames_survive_subscription_and_fetch() {
		for config in [hang::catalog::Container::Legacy, hang::catalog::Container::Loc] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let track = broadcast.create_track("data", hang::container::track_info(0)).unwrap();
			let subscription = track.subscribe(moq_net::track::Subscription::default());
			let consumer = broadcast.consume();
			let mut producer =
				crate::container::Producer::new(track, Hang::new(&config, crate::container::Kind::Data).unwrap());
			producer.write(frame(0, b"", true)).unwrap();
			producer.write(frame(10_000, b"data", false)).unwrap();
			producer.finish().unwrap();
			let mut live = crate::container::Consumer::new(
				subscription,
				Hang::new(&config, crate::container::Kind::Data).unwrap(),
			);
			let first = live.read().await.unwrap().unwrap();
			assert!(first.payload.is_empty());
			assert!(first.keyframe);
			assert_eq!(live.read().await.unwrap().unwrap().payload, b"data".as_slice());
			assert!(live.read().await.unwrap().is_none());
			let group = consumer.track("data").unwrap().fetch_group(0, None).await.unwrap();
			let mut fetched = GroupConsumer::new(group, Hang::new(&config, crate::container::Kind::Data).unwrap());
			assert!(fetched.read().await.unwrap().unwrap().payload.is_empty());
			assert_eq!(fetched.read().await.unwrap().unwrap().payload, b"data".as_slice());
			assert!(fetched.read().await.unwrap().is_none());
		}
	}

	/// An empty payload times the previous frame and is not returned as media.
	#[tokio::test]
	async fn a_duration_marker_times_the_last_frame() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("media", None).unwrap();
		let consumer = broadcast.consume();

		let mut media = crate::container::Producer::new(track, Hang::Legacy(crate::container::Kind::Video));
		media.write(frame(1_000_000, b"keyframe", true)).unwrap();
		media.write(frame(1_020_000, b"delta", false)).unwrap();
		media
			.cut(Some(moq_net::Timestamp::from_micros(1_053_000).unwrap()))
			.unwrap();
		media.finish().unwrap();

		let group = consumer.track("media").unwrap().fetch_group(0, None).await.unwrap();
		let mut group = GroupConsumer::new(group, Hang::Legacy(crate::container::Kind::Video));

		let first = group.read().await.unwrap().unwrap();
		assert_eq!(first.payload, b"keyframe".as_slice());
		assert_eq!(first.duration, None);

		let second = group.read().await.unwrap().unwrap();
		assert_eq!(second.payload, b"delta".as_slice());
		assert_eq!(second.duration, Some(moq_net::Timestamp::from_micros(33_000).unwrap()));

		assert!(group.read().await.unwrap().is_none());
	}

	/// One CMAF fragment decodes to several samples, which are handed back one at a time.
	#[tokio::test]
	async fn hands_back_a_cmaf_batch_one_frame_at_a_time() {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		let muxer = crate::container::fmp4::Muxer::video(&config).unwrap();
		let init = muxer.init().unwrap().expect("VP8 init should be available");
		let cmaf = hang::catalog::Container::Cmaf { init };
		// The format is not Clone, so decode with a second instance built from the same init.
		let format = Hang::new(&cmaf, crate::container::Kind::Video).unwrap();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("video", None).unwrap();
		let consumer = broadcast.consume();

		// Buffer both samples into one moof+mdat, which is what this decodes.
		let mut media = crate::container::Producer::new(track, format).with_buffer(std::time::Duration::from_secs(1));
		for (timestamp_us, payload, keyframe) in [
			(2_000_000, b"keyframe".as_slice(), true),
			(2_020_000, b"delta".as_slice(), false),
		] {
			media
				.write(Frame {
					timestamp: moq_net::Timestamp::from_micros(timestamp_us).unwrap(),
					payload: bytes::Bytes::from_static(payload),
					keyframe,
					duration: Some(moq_net::Timestamp::from_micros(20_000).unwrap()),
				})
				.unwrap();
		}
		media.finish().unwrap();

		let group = consumer.track("video").unwrap().fetch_group(0, None).await.unwrap();
		let mut group = GroupConsumer::new(group, Hang::new(&cmaf, crate::container::Kind::Video).unwrap());

		let first = group.read().await.unwrap().unwrap();
		assert_eq!(first.payload, b"keyframe".as_slice());
		assert!(first.keyframe);

		let second = group.read().await.unwrap().unwrap();
		assert_eq!(second.payload, b"delta".as_slice());
		assert!(!second.keyframe);

		assert!(group.read().await.unwrap().is_none());
	}
}
