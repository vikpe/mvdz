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
#[doc(hidden)]
pub mod coder;
mod decode;
#[doc(hidden)]
pub mod deflate;
#[doc(hidden)]
pub mod encode;
mod error;
#[doc(hidden)]
pub mod format;
mod numeric;
#[doc(hidden)]
pub mod proto;
#[doc(hidden)]
pub mod segment;
mod stream;
#[doc(hidden)]
pub mod streams;
#[doc(hidden)]
pub mod util;
mod walk;

pub use api::{
    Config, FileInfo, Info, check_demo, compress, compress_info, decompress, decompress_stream,
    decompress_with, is_mvdz, read_info, read_info_with,
};
pub use budget::Limits;
pub use error::{Error, Result};
pub use format::{Header, Index, IndexSeg};
pub use stream::Reader;
