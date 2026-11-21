//! Coders.
//!
//! A structural or numeric stream is static rANS with the stream's
//! model in the file's models block: the stream's kind decides its coder,
//! and no stream carries a model or a coder id of its own. The blob streams
//! of a segment are one DEFLATE stream (`segment.rs`).

pub(crate) mod rans;
pub(crate) mod rc;

use crate::error::{Error, Result};
use std::sync::Arc;

/// The file's model of stream `stream` (an error without one).
pub(crate) fn file_model(
    models: Option<&rans::Models>,
    stream: usize,
) -> Result<&Arc<rans::DecoderModel>> {
    models
        .and_then(|models| models.get(stream)?.as_ref())
        .ok_or(Error::Corrupt("stream without a model"))
}
