//! mvdz: lossless, format-aware compression of `QuakeWorld` MVD demos.
//!
//! Format (FORMAT.md): a fixed header, the shared models, then segments of about 256 KiB
//! of demo coded by a structural transform (static rANS; raw DEFLATE for the rest).
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let demo = std::fs::read("game.mvd")?;
//! let file = mvdz::encode(&demo, 0)?;
//! assert_eq!(mvdz::decode(&file, 0)?, demo);
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

pub use api::{Encoder, decode, encode};
pub use error::{Error, Result};
pub use format::Header;
pub use stream::Decoder;

#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub mod fuzz;
