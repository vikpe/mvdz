//! mvdz: lossless, format-aware compression of `QuakeWorld` MVD demos.
//!
//! One file format (FORMAT.md): a fixed header, the shared models and
//! segments of 256 KiB that hold the demo coded by a structural transform
//! (static rANS; raw DEFLATE for what the transform does not model).
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let demo = std::fs::read("game.mvd")?;
//! let packed = mvdz::compress(&demo, &mvdz::Config::default())?;
//! assert!(mvdz::is_mvdz(&packed));
//! assert_eq!(mvdz::decompress(&packed, 1)?, demo);
//! # Ok(())
//! # }
//! ```

mod api;
mod budget;
mod coder;
mod decode;
mod deflate;
mod encode;
mod error;
mod format;
mod numeric;
mod proto;
mod segment;
mod stream;
mod streams;
mod util;
mod walk;

#[cfg(test)]
mod tests;

pub use api::{
    Config, FileInfo, Info, check_demo, compress, compress_any, compress_info, decompress,
    decompress_stream, decompress_with, is_mvdz, read_info, read_info_with,
};
pub use budget::Limits;
pub use error::{Error, Result};
pub use format::{Header, Index, IndexSegment, VERSION};
/// For the fuzz targets' demo generator only.
#[doc(hidden)]
pub use proto::Proto;
pub use stream::Reader;
