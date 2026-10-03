use std::collections::VecDeque;
use std::task::{Poll, ready};

use super::{Container, Frame};
use crate::container::reader::FrameReader;
use crate::container::{Decrypter, ProtectedReadFrame, ReadFrame};

pub struct GroupReader<'a> {
    group: &'a mut moq_net::group::Consumer,
}

impl<'a> GroupReader<'a> {
    pub fn new(group: &'a mut moq_net::group::Consumer) -> Self {
        Self { group }
    }
}

impl FrameReader for GroupReader<'_> {
    type Error = crate::Error;

    fn poll_read_frame(
        &mut self,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<ReadFrame>, Self::Error>> {
        let frame = ready!(self.group.poll_read_frame(waiter)?);

        let Some(frame) = frame else {
            return Poll::Ready(Ok(None));
        };

        Poll::Ready(Ok(Some(ReadFrame {
            timestamp: frame.timestamp,
            payload: frame.payload,
        })))
    }
}

/// Decode a single [`moq_net::group::Consumer`] into a finite stream of media
/// [`Frame`]s.
pub struct GroupConsumer<F: Container> {
    group: moq_net::group::Consumer,
    format: F,

    /// Persistent across all reads and group transitions.
    decrypter: Option<Decrypter>,

    // Frames decoded from the last wire frame but not yet returned.
    pending: VecDeque<Frame>,

    // Number of media frames returned. The first one is marked as a keyframe.
    index: u64,
}

impl<F: Container<Error = crate::error::Error>> GroupConsumer<F> {
    /// Decode `group` with the given container format.
    pub fn new(
        group: moq_net::group::Consumer,
        format: F,
        decrypter: Option<Decrypter>,
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

                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),

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

                None => return Poll::Ready(Ok(self.pop_media())),
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
                let mut reader = ProtectedReadFrame::new(raw_reader, decrypter);

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
