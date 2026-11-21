#![no_main]
//! Review target: a structure-aware demo must compress and round-trip at
//! any thread count.
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
    let (c, _) = mvdz::encode::compress(&demo, &mvdz::Config { threads }).expect("compress");
    rt(&c, &demo, "one-shot");
    assert!(mvdz::decompress(&c, 3).unwrap() == demo);
});
