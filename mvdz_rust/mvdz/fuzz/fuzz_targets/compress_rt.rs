#![no_main]
//! Any bytes must round-trip through the encoder without its input check.
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
