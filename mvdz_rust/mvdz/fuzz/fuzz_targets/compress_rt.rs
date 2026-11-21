#![no_main]
//! Review target: arbitrary bytes through the encoder without the input
//! check; the file must exist and round-trip at 1 and 2 threads. With
//! `REQUIRE_TRANSFORM_LOSSLESS` set, no segment may fail the encoder's
//! self-check (a transform bug). The checked entry point refuses exactly
//! what `check_demo` refuses.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let cfg = mvdz::Config { threads: 1 };
    let (file, info) = mvdz::encode::compress(data, &cfg).expect("compress");
    if info.self_check_fallbacks > 0 && std::env::var("REQUIRE_TRANSFORM_LOSSLESS").is_ok() {
        panic!("the transform is lossy");
    }
    assert!(mvdz::decompress(&file, 1).unwrap() == data);
    assert!(mvdz::decompress(&file, 2).unwrap() == data);
    // the checked entry point: refuses exactly what check_demo refuses
    match mvdz::compress(data, &cfg) {
        Err(mvdz::Error::NotMvd(_)) => assert!(mvdz::check_demo(data).is_err()),
        result => assert!(result.expect("compress") == file),
    }
});
