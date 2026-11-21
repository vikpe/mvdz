#![no_main]
//! Reading the header and the segment headers of arbitrary input.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = mvdz::Limits {
        memory: 256 << 20,
        ..Default::default()
    };
    let _ = mvdz::read_info_with(std::io::Cursor::new(data), &limits);
});
