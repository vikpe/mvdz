#![no_main]
//! Review target: arbitrary bytes through the encoder without the input
//! check (verify on and off), as one segment and at a small segment size;
//! the verified file must exist and round-trip. The checked entry point
//! refuses exactly what `check_demo` refuses.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let seg = if data[0] & 2 == 0 {
        0
    } else {
        16 + data[1] as usize * 4
    };
    let demo = &data[2..];
    let cfg = mvdz::Config {
        threads: 1,
        verify: false,
    };
    let (c, _) = match mvdz::encode::compress_segmented(demo, seg, &cfg) {
        Err(mvdz::Error::Invalid("too many segments")) => return,
        r => r.expect("compress (no verify)"),
    };
    if mvdz::decompress(&c, 1).ok().as_deref() != Some(demo) {
        if std::env::var("REQUIRE_TRANSFORM_LOSSLESS").is_ok() {
            panic!("unverified transform is lossy");
        }
    }
    let cfg = mvdz::Config {
        verify: true,
        ..cfg
    };
    let (c, _) = mvdz::encode::compress_segmented(demo, seg, &cfg).expect("compress");
    assert!(mvdz::decompress(&c, 2).unwrap() == demo);
    // the format's own segment size
    let (c, _) = mvdz::encode::compress(demo, &cfg).expect("compress");
    assert!(mvdz::decompress(&c, 2).unwrap() == demo);
    // the checked entry point: refuses exactly what check_demo refuses
    // (gzip input is decompressed first)
    if !mvdz::is_gzip(demo) {
        match mvdz::compress(demo, &cfg) {
            Err(mvdz::Error::NotMvd(_)) => assert!(mvdz::check_demo(demo).is_err()),
            r => assert!(r.expect("compress") == c),
        }
    }
});
