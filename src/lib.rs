//! mvdz: lossless, format-aware compression of QuakeWorld MVD demos.
//!
//! One file format (FORMAT.md): a fixed header, the shared models and
//! segments of 256 KiB that hold the demo coded by a structural transform
//! (static rANS; raw DEFLATE for what the transform does not model).
//!
//! ```no_run
//! let demo = std::fs::read("game.mvd").unwrap();
//! let packed = mvdz::compress(&demo, &mvdz::Config::default()).unwrap();
//! assert!(mvdz::is_mvdz(&packed));
//! assert_eq!(mvdz::decompress(&packed, 1).unwrap(), demo);
//! ```

pub mod api;
pub mod budget;
pub mod capi;
pub mod coder;
pub mod decode;
pub mod deflate;
pub mod encode;
pub mod error;
pub mod format;
pub mod numeric;
pub mod proto;
pub mod segment;
pub mod stream;
pub mod streams;
pub mod util;
pub mod walk;


pub use api::{
    Config, FileInfo, Info, check_demo, compress, compress_info, decompress,
    decompress_stream, decompress_with, is_mvdz, read_info, read_info_with, write_file_atomic,
    write_file_atomic_with,
};
pub use budget::Limits;
pub use error::{Error, Result};
pub use format::{Header, Index, IndexSeg};
pub use stream::{Reader, Seekable, Seq};
