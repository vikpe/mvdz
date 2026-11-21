#![no_main]
//! Review target: arbitrary bytes through the encoder without the input
//! check; the file must exist and round-trip at 1 and 2 threads. The
//! checked entry point refuses exactly what `check_demo` refuses.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let cfg = mvdz::Config { threads: 1 };
    let (c, _) = mvdz::encode::compress(data, &cfg).expect("compress");
    assert!(mvdz::decompress(&c, 1).unwrap() == data);
    assert!(mvdz::decompress(&c, 2).unwrap() == data);
    // the checked entry point: refuses exactly what check_demo refuses
    match mvdz::compress(data, &cfg) {
        Err(mvdz::Error::NotMvd(_)) => assert!(mvdz::check_demo(data).is_err()),
        r => assert!(r.expect("compress") == c),
    }
});
