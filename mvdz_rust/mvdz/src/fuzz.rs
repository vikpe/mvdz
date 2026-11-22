pub fn check_demo(demo: &[u8]) -> crate::Result<()> {
    crate::encode::check_demo(demo)
}

/// Skips `check_demo`. The `usize` counts segments that failed the self-check: a transform bug.
pub fn encode_any(demo: &[u8], threads: usize) -> crate::Result<(Vec<u8>, usize)> {
    crate::encode::compress(demo, threads)
}
