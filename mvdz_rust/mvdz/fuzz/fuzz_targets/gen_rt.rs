#![no_main]
//! Review target: a structure-aware demo must compress and round-trip at
//! any thread count. No segment may fail the encoder's self-check (a
//! transform bug the encoder hides by coding the segment as all tail) when
//! `REQUIRE_TRANSFORM_LOSSLESS` is set; otherwise it is reported.
use libfuzzer_sys::fuzz_target;
mod mgen;

fn assert_round_trip(file: &[u8], demo: &[u8], what: &str) {
    match mvdz::decode(file, 1) {
        Ok(decoded) => assert!(decoded == demo, "{what}: decodes to other bytes"),
        Err(error) => panic!("{what}: does not decode: {error}"),
    }
}

fuzz_target!(|bytes: &[u8]| {
    if bytes.len() < 2 {
        return;
    }
    let threads = 1 + (bytes[0] as usize >> 2) % 3;
    let demo = mgen::demo(&bytes[1..]);
    let (file, self_check_fallbacks) = mvdz::fuzz::encode_any(&demo, threads).expect("compress");
    if self_check_fallbacks > 0 {
        if std::env::var("REQUIRE_TRANSFORM_LOSSLESS").is_ok() {
            panic!("the transform is lossy");
        }
        eprintln!("NOTE the transform is lossy here");
    }
    assert_round_trip(&file, &demo, "one-shot");
    assert!(mvdz::decode(&file, 3).unwrap() == demo);
});
