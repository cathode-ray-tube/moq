use bytes::Bytes;
use std::future::Future;
use std::pin::Pin;

use crate::Timestamp;

#[derive(Debug)]
pub enum PayloadError {
    Processor(String),
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Processor(message) => write!(f, "payload processor error: {message}"),
        }
    }
}

impl std::error::Error for PayloadError {}

pub struct PayloadContext {
    pub track: String,
    pub timestamp: Timestamp,
}

pub type PayloadFuture<'a> = Pin<
    Box<dyn Future<Output = Result<Bytes, PayloadError>> + Send + 'a>
>;

pub trait AsyncPayloadProcessor: Send + Sync {
    fn process<'a>(
        &'a self,
        payload: Bytes,
        context: &'a PayloadContext,
    ) -> PayloadFuture<'a>;
}
