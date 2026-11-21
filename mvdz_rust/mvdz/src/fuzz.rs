//! For the fuzz targets only (feature `fuzz`).

/// What `encode` refuses with `Error::NotMvd`.
pub fn check_demo(demo: &[u8]) -> crate::Result<()> {
    crate::api::check_demo(demo)
}

/// Any bytes, also those `check_demo` refuses; returns the file and the number of segments
/// that failed the encoder's self-check (nonzero only for a transform bug).
pub fn encode_any(demo: &[u8], threads: usize) -> crate::Result<(Vec<u8>, usize)> {
    crate::encode::compress(demo, threads).map(|(file, stats)| (file, stats.self_check_fallbacks))
}
