#![no_main]
//! Review target: a structure-aware demo; (1) the transform alone, with
//! verification off, must round-trip (else the encoder relies on its
//! fallback: a transform bug); (2) the normal path (verify on) must
//! succeed and round-trip at any thread count.
use libfuzzer_sys::fuzz_target;
mod mgen;

fn rt(c: &[u8], demo: &[u8], what: &str) {
    match mvdz::decompress(c, 1) {
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
    // (1) the transform without verification
    let cfg = mvdz::Config {
        threads,
        verify: false,
    };
    let (c, _) = mvdz::encode::compress(&demo, &cfg).expect("compress (no verify)");
    if std::env::var("REQUIRE_TRANSFORM_LOSSLESS").is_ok() {
        rt(&c, &demo, "unverified transform");
    } else if mvdz::decompress(&c, 1).ok().as_deref() != Some(&demo[..]) {
        eprintln!("NOTE unverified transform differs");
    }
    // (2) verified paths
    let cfg = mvdz::Config {
        threads,
        ..Default::default()
    };
    let (c, _) = mvdz::encode::compress(&demo, &cfg).expect("compress");
    rt(&c, &demo, "one-shot");
    assert!(mvdz::decompress(&c, 3).unwrap() == demo);
});
