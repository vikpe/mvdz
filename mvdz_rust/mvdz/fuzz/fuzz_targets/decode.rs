#![no_main]
//! Whole-file decoding of arbitrary input (1 and 2 threads), under a
//! small memory budget.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = mvdz::Limits {
        memory: 256 << 20,
        ..Default::default()
    };
    let one_thread = mvdz::decompress_with(data, 1, &limits);
    let two_threads = mvdz::decompress_with(data, 2, &limits);
    assert_eq!(one_thread.is_ok(), two_threads.is_ok());
    if let (Ok(one_thread), Ok(two_threads)) = (one_thread, two_threads) {
        assert!(one_thread == two_threads);
    }
});
