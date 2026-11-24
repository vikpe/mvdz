//! mvdz: lossless compression of `QuakeWorld` MVD demos (FORMAT.md).
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use std::io::Read;
//! let mut file = Vec::new();
//! mvdz::Encoder::new().encode(std::fs::File::open("game.mvd")?, &mut file)?;
//! let mut demo = Vec::new();
//! mvdz::Decoder::new(&file[..])?.read_to_end(&mut demo)?;
//! assert_eq!(demo, std::fs::read("game.mvd")?);
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

pub use decode::Decoder;
pub use encode::Encoder;

#[cfg(test)]
pub(crate) use decode::decode;
#[cfg(test)]
pub(crate) use encode::encode;
pub use error::{Error, Result};
pub use format::Header;

#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub mod fuzz;
