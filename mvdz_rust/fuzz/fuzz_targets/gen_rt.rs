#![no_main]
//! Review target: a structure-aware demo must compress and round-trip at
//! any thread count. No segment may fail the encoder's self-check (a
//! transform bug the encoder hides by coding the segment as all tail) when
//! `REQUIRE_TRANSFORM_LOSSLESS` is set; otherwise it is reported.
use libfuzzer_sys::fuzz_target;
mod mgen;

fn rt(file: &[u8], demo: &[u8], what: &str) {
    match mvdz::decompress(file, 1) {
        Ok(d) => assert!(d == demo, "{what}: decodes to other bytes"),
        Err(e) => panic!("{what}: does not decode: {e}"),
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let threads = 1 + (data[0] as usize >> 2) % 3;
    let demo = mgen::demo(&data[1..]);
    let (file, info) = mvdz::encode::compress(&demo, &mvdz::Config { threads }).expect("compress");
    if info.self_check_fallbacks > 0 {
        if std::env::var("REQUIRE_TRANSFORM_LOSSLESS").is_ok() {
            panic!("the transform is lossy");
        }
        eprintln!("NOTE the transform is lossy here");
    }
    rt(&file, &demo, "one-shot");
    assert!(mvdz::decompress(&file, 3).unwrap() == demo);
});
