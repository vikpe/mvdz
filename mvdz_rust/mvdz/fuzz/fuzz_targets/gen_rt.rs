#![no_main]
//! Structure-aware demos from `mgen` must round-trip at any thread count.
use libfuzzer_sys::fuzz_target;
mod mgen;

fn assert_round_trip(file: &[u8], demo: &[u8], what: &str) {
    match mvdz::fuzz::decode(file, 1) {
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
    let file = mvdz::fuzz::encode_any(&demo, threads).expect("compress");
    assert_round_trip(&file, &demo, "one-shot");
    assert!(mvdz::fuzz::decode(&file, 3).unwrap() == demo);
});
