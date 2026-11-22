//! mvdz: lossless compression of `QuakeWorld` MVD demos (FORMAT.md).
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let demo = std::fs::read("game.mvd")?;
//! let file = mvdz::encode(&demo, 0)?;
//! assert_eq!(mvdz::decode(&file, 0)?, demo);
//! # Ok(())
//! # }
//! ```

mod coder;
mod decode;
mod deflate;
mod encode;
mod error;
mod fields;
mod format;
mod memory;
mod numeric;
mod parallel;
mod proto;
mod segment;
mod streams;
mod walk;

#[cfg(test)]
mod tests;

pub use decode::{Decoder, decode};
pub use encode::{Encoder, encode};
pub use error::{Error, Result};
pub use format::Header;

#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub mod fuzz;
