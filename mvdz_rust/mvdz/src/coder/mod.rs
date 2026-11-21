//! Coders.
//!
//! A structural or numeric stream is static rANS with the stream's
//! model in the file's models block: the stream's kind decides its coder,
//! and no stream carries a model or a coder id of its own. The blob streams
//! of a segment are one DEFLATE stream (`segment.rs`).

pub mod rans;
pub mod rc;

use crate::error::{Error, Result};
use std::sync::Arc;

/// The file's model of stream `sid` (an error without one).
pub(crate) fn file_model(fm: Option<&rans::Models>, sid: usize) -> Result<&Arc<rans::DModel>> {
    fm.and_then(|m| m.get(sid)?.as_ref())
        .ok_or(Error::Corrupt("stream without a model"))
}
