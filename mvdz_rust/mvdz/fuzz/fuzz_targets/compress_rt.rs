#![no_main]
//! Arbitrary bytes through the encoder without the input check; the file must exist and
//! round-trip at 1 and 2 threads. With `REQUIRE_TRANSFORM_LOSSLESS` set, no segment may
//! fail the encoder's self-check (a transform bug). `encode` refuses exactly what
//! `check_demo` refuses.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let (file, self_check_fallbacks) = mvdz::fuzz::encode_any(bytes, 1).expect("compress");
    if self_check_fallbacks > 0 && std::env::var("REQUIRE_TRANSFORM_LOSSLESS").is_ok() {
        panic!("the transform is lossy");
    }
    assert!(mvdz::decode(&file, 1).unwrap() == bytes);
    assert!(mvdz::decode(&file, 2).unwrap() == bytes);
    match mvdz::encode(bytes, 1) {
        Err(mvdz::Error::NotMvd(_)) => assert!(mvdz::fuzz::check_demo(bytes).is_err()),
        result => assert!(result.expect("compress") == file),
    }
});
