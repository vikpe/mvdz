#![no_main]
//! Whole-file decoding of arbitrary input at 1 and 2 threads.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let one_thread = mvdz::decode(bytes, 1);
    let two_threads = mvdz::decode(bytes, 2);
    assert_eq!(one_thread.is_ok(), two_threads.is_ok());
    if let (Ok(one_thread), Ok(two_threads)) = (one_thread, two_threads) {
        assert!(one_thread == two_threads);
    }
});
