pub fn check_demo(demo: &[u8]) -> crate::Result<()> {
    crate::encode::check_demo(demo)
}

pub fn encode(demo: &[u8], max_threads: u8) -> crate::Result<Vec<u8>> {
    crate::encode::encode(demo, max_threads)
}

pub fn decode(file: &[u8], max_threads: u8) -> crate::Result<Vec<u8>> {
    crate::decode::decode(file, max_threads)
}

/// Skips `check_demo`.
pub fn encode_any(demo: &[u8], threads: usize) -> crate::Result<Vec<u8>> {
    crate::encode::encode_any(demo, threads)
}
