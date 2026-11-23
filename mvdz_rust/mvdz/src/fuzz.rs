pub fn check_demo(demo: &[u8]) -> crate::Result<()> {
    crate::encode::check_demo(demo)
}

/// Skips `check_demo`.
pub fn encode_any(demo: &[u8], threads: usize) -> crate::Result<Vec<u8>> {
    crate::encode::encode_any(demo, threads)
}
