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

mod decode;
mod encode;
mod error;
mod fields;
mod format;
mod lz;
mod math;
mod memory;
mod numeric;
mod parallel;
mod protocol;
mod rans;
mod segment;
mod streams;
mod walker;

#[cfg(test)]
mod tests;

pub use decode::{Decoder, decode};
pub use encode::encode;
pub use error::{Error, Result};
pub use format::Header;

#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub mod fuzz;

#[cfg(feature = "stats")]
#[doc(hidden)]
pub use segment::stats;
