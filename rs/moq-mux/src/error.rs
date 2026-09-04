use crate::encryption::EncryptionError;

/// Renders an error and its `source()` chain into a single message.
///
/// Dependency errors are stored as messages so their crates stay out of this
/// crate's public API. Several of them keep the actionable half in
/// `source()` and nothing but a category in `Display`, so a plain
/// `to_string()` would drop the only detail worth reporting.
pub(crate) fn message(err: impl std::error::Error) -> String {
    use std::fmt::Write;

    let mut out = err.to_string();
    let mut source = err.source();

    while let Some(err) = source {
        let _ = write!(out, ": {err}");
        source = err.source();
    }

    out
}

/// Errors from moq-mux operations.
///
/// Most variants are delegations to underlying layers: [`moq_net::Error`]
/// for transport / pub-sub failures, [`hang::Error`] for catalog/codec
/// parsing, the per-format errors for container shape problems, and the
/// per-codec errors for bitstream parsing problems.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Error from the underlying moq-net transport.
    #[error("moq: {0}")]
    Moq(#[from] moq_net::Error),

    /// Error from the hang catalog/codec layer.
    #[error("hang: {0}")]
    Hang(#[from] hang::Error),

    /// Error publishing or consuming JSON over a track.
    #[error("json: {0}")]
    Json(#[from] moq_json::Error),

    /// Error publishing or consuming binary payloads over a track.
    #[error("binary: {0}")]
    Binary(#[from] moq_binary::Error),

    /// A catalog entry declares a track mode this build does not implement.
    #[error("unsupported track mode: {0}")]
    UnsupportedMode(String),

    /// A catalog entry declares a compression this build does not implement.
    #[error("unsupported track compression: {0}")]
    UnsupportedCompression(String),

    /// Error parsing or building CMAF moof+mdat fragments.
    #[error("cmaf: {0}")]
    Cmaf(#[from] crate::container::fmp4::Error),

    /// Error parsing or building MKV / WebM streams.
    #[error("mkv: {0}")]
    Mkv(#[from] crate::container::mkv::Error),

    /// Error decoding the MSF catalog.
    #[error("msf: {0}")]
    Msf(#[from] crate::catalog::msf::Error),

    /// Error parsing or building LOC frames.
    #[error("loc: {0}")]
    Loc(#[from] moq_loc::Error),

    /// Error parsing an Annex B NAL stream.
    #[error("annexb: {0}")]
    Annexb(#[from] crate::codec::annexb::Error),

    /// Error parsing AAC.
    #[error("aac: {0}")]
    Aac(#[from] crate::codec::aac::Error),

    /// Error parsing Opus.
    #[error("opus: {0}")]
    Opus(#[from] crate::codec::opus::Error),

    /// Error parsing FLAC.
    #[error("flac: {0}")]
    Flac(#[from] crate::codec::flac::Error),

    /// Error parsing MP3.
    #[error("mp3: {0}")]
    Mp3(#[from] crate::codec::mp3::Error),

    /// Error parsing H.264.
    #[error("h264: {0}")]
    H264(#[from] crate::codec::h264::Error),

    /// Error parsing H.265.
    #[error("h265: {0}")]
    H265(#[from] crate::codec::h265::Error),

    /// Error parsing AV1.
    #[error("av1: {0}")]
    Av1(#[from] crate::codec::av1::Error),

    /// Error parsing VP8.
    #[error("vp8: {0}")]
    Vp8(#[from] crate::codec::vp8::Error),

    /// Error parsing VP9.
    #[error("vp9: {0}")]
    Vp9(#[from] crate::codec::vp9::Error),

    /// Error parsing legacy audio (MP2 / AC-3 / E-AC-3).
    #[error("legacy: {0}")]
    Legacy(#[from] crate::codec::legacy::Error),

    /// Timestamp overflow when converting between timescales.
    #[error("timestamp overflow")]
    TimestampOverflow(#[from] moq_net::TimeOverflow),

    /// Error decoding or encoding an mp4 atom.
    #[error("mp4: {0}")]
    Mp4(std::sync::Arc<mp4_atom::Error>),

    /// I/O error.
    #[error("io: {0}")]
    Io(std::sync::Arc<std::io::Error>),

    /// URL parse error.
    #[error("url: {0}")]
    Url(String),

    /// Unknown media format.
    #[error("unknown format: {0}")]
    UnknownFormat(String),

    /// A video format that a raw byte stream cannot be split into.
    #[error("{0} is not self-describing, so its frame boundaries can't be inferred from a stream")]
    NotSelfDescribing(String),

    /// A format was handed to a constructor for a different kind of import.
    #[error("{format} is a {actual} format, not {wanted}")]
    WrongKind {
        format: String,
        actual: &'static str,
        wanted: &'static str,
    },

    /// Error from the encryption layer.
    #[error("encryption: {0}")]
    Encryption(#[from] EncryptionError),

    /// A non-keyframe frame was received before any keyframe opened a group.
    #[error("{0}")]
    MissingKeyframe(#[from] crate::container::MissingKeyframe),

    /// A FLV video frame resolved to a negative presentation timestamp.
    #[error(
        "negative FLV video presentation timestamp: \
         dts={dts_ms}ms composition_time={composition_time_ms}ms"
    )]
    NegativeFlvPts {
        dts_ms: u64,
        composition_time_ms: i32,
    },

    /// A segment ran past the advertised maximum duration.
    #[error(
        "timeline segment {segment} lasted {duration:?}, \
         over the declared maximum {duration_max:?}"
    )]
    TimelineOverrun {
        segment: u64,
        duration: std::time::Duration,
        duration_max: std::time::Duration,
    },

    /// `Producer::finish` was called while deferred records remained pending.
    #[error("finish and commit every deferred timeline record before closing the Producer")]
    TimelineDeferredPending,

    /// A pending record was not yielded by its deferred publisher.
    #[error("timeline segment {0} was not yielded for deferred publication")]
    TimelineDeferredRecord(u64),

    /// Error from a muxer/demuxer that reports via `anyhow`.
    #[error("{0}")]
    Other(std::sync::Arc<anyhow::Error>),

    /// A timeline catalog section declared an invalid timescale.
    #[error("invalid timeline timescale: {0}")]
    InvalidTimescale(u32),

    /// An application catalog section used a reserved name.
    #[error("reserved catalog section: {0}")]
    ReservedSection(String),

    /// A rendition declared an unsupported container.
    #[error("unsupported container: {0}")]
    UnsupportedContainer(String),

    /// A broadcast reference escapes the consumer's authorized root.
    #[error("broadcast reference escapes the root: {0}")]
    EscapingBroadcast(String),
}

impl Error {
    pub(crate) fn unsupported_container(
        container: &hang::catalog::UnknownContainer,
    ) -> Self {
        Self::UnsupportedContainer(
            container.kind().unwrap_or("<missing>").to_string(),
        )
    }
}

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        Self::Other(std::sync::Arc::new(err))
    }
}

impl From<mp4_atom::Error> for Error {
    fn from(err: mp4_atom::Error) -> Self {
        Self::Mp4(std::sync::Arc::new(err))
    }
}

// Flattened to its message so `url` stays out of this crate's public API.
impl From<url::ParseError> for Error {
    fn from(err: url::ParseError) -> Self {
        Self::Url(message(err))
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(std::sync::Arc::new(err))
    }
}

/// A result type alias for moq-mux operations.
pub type Result<T> = std::result::Result<T, Error>;
