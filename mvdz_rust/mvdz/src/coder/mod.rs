//! Structural and numeric streams: static rANS with the model from the file's models block.
//! The stream's kind decides its coder; no stream carries a model or coder id of its own.

pub(crate) mod rans;
pub(crate) mod rc;

use crate::error::{Error, Result};
use std::sync::Arc;

pub(crate) fn file_model(
    models: Option<&rans::Models>,
    stream: usize,
) -> Result<&Arc<rans::DecoderModel>> {
    models
        .and_then(|models| models.get(stream)?.as_ref())
        .ok_or(Error::Corrupt("stream without a model"))
}
