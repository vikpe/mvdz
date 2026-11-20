//! C API reader handles: out-of-range seeks, one handle used from several
//! threads, closed handles, sticky read errors, empty results and the
//! metadata JSON.

mod common;
use common::*;
use mvdz::capi::*;

#[test]
fn c_handles() {
    let src = synthetic(&[], false);
    let c = compress(&src, &cfg(300)).unwrap();
    unsafe {
        let mut err = 0;
        let r = mvdz_open_memory(c.as_ptr() as *const _, c.len(), 1, 0, 0, &mut err);
        assert!(!r.is_null() && err == 0);
        let n = mvdz_segment_count(r);
        assert!(n > 1);
        let mut off = 0u64;
        assert_eq!(
            mvdz_seek_segment(r, n as usize, &mut off),
            MVDZ_E_INVALID,
            "out of range"
        );
        assert_eq!(mvdz_seek_segment(r, 1, &mut off), MVDZ_OK);
        // one handle used from several threads: calls are serialised
        let rr = r as usize;
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(move || {
                    let mut buf = [0u8; 100];
                    for _ in 0..50 {
                        let k = mvdz_read(rr as *mut _, buf.as_mut_ptr() as *mut _, buf.len());
                        assert!(k >= 0);
                    }
                });
            }
        });
        mvdz_close(r);
        mvdz_close(r); // harmless
        let mut buf = [0u8; 10];
        assert_eq!(
            mvdz_read(r, buf.as_mut_ptr() as *mut _, 10),
            MVDZ_E_INVALID as i64
        );
        assert_eq!(
            mvdz_read(12345 as *mut _, buf.as_mut_ptr() as *mut _, 10),
            MVDZ_E_INVALID as i64
        );
        // a sticky read error
        let mut bad = c.clone();
        let k = bad.len() / 2;
        bad[k] ^= 0x40;
        let r = mvdz_open_memory(bad.as_ptr() as *const _, bad.len(), 1, 0, 0, &mut err);
        if !r.is_null() {
            let mut big = vec![0u8; 1 << 20];
            let mut first = 0;
            for _ in 0..100 {
                let k = mvdz_read(r, big.as_mut_ptr() as *mut _, big.len());
                if k < 0 {
                    first = k;
                    break;
                }
            }
            assert!(first < 0);
            assert_eq!(
                mvdz_read(r, big.as_mut_ptr() as *mut _, big.len()),
                first,
                "sticky"
            );
            mvdz_close(r);
        }
        // empty results are NULL
        let (mut out, mut on) = (std::ptr::null_mut(), 1usize);
        let empty = unchecked(b"", &cfg(0))
            .unwrap()
            .0;
        assert_eq!(
            mvdz_decompress(
                empty.as_ptr() as *const _,
                empty.len(),
                1,
                0,
                0,
                &mut out,
                &mut on
            ),
            MVDZ_OK
        );
        assert!(out.is_null() && on == 0);
        mvdz_free(out as *mut _, on);
        // the JSON is valid
        let (mut j, mut jn) = (std::ptr::null_mut(), 0usize);
        assert_eq!(
            mvdz_info_json_memory(c.as_ptr() as *const _, c.len(), 0, &mut j, &mut jn),
            MVDZ_OK
        );
        let s = std::slice::from_raw_parts(j as *const u8, jn);
        assert_eq!(*j.add(jn), 0, "NUL-terminated");
        serde_json::from_slice::<serde_json::Value>(s).unwrap();
        mvdz_free(j as *mut _, 12345); // the length is not trusted
    }
}
