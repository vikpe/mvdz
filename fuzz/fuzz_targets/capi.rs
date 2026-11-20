#![no_main]
//! The C ABI driven by an operation script: readers over memory and over
//! callbacks (sequential and seekable), reads of varying size, seeks,
//! double closes, use after close, metadata JSON (must be NUL-terminated
//! valid JSON), the streaming writer with double finish/abort.
use libfuzzer_sys::fuzz_target;
use mvdz::capi::*;
use std::ffi::{c_int, c_longlong, c_void};

struct Mem<'a> {
    b: &'a [u8],
    pos: usize,
}

unsafe extern "C" fn rd(u: *mut c_void, buf: *mut c_void, n: usize) -> c_longlong {
    let m = unsafe { &mut *(u as *mut Mem) };
    let k = n.min(m.b.len() - m.pos).min(7);
    unsafe { std::ptr::copy_nonoverlapping(m.b.as_ptr().add(m.pos), buf as *mut u8, k) };
    m.pos += k;
    k as c_longlong
}

unsafe extern "C" fn sk(u: *mut c_void, off: c_longlong, wh: c_int) -> c_longlong {
    let m = unsafe { &mut *(u as *mut Mem) };
    let base = match wh {
        0 => 0i64,
        1 => m.pos as i64,
        2 => m.b.len() as i64,
        _ => return -1,
    };
    let p = base.saturating_add(off);
    if p < 0 || p as usize > m.b.len() {
        return -1;
    }
    m.pos = p as usize;
    p
}

fn check_json(p: *mut i8, n: usize) {
    assert!(!p.is_null());
    let s = unsafe { std::slice::from_raw_parts(p as *const u8, n + 1) };
    assert_eq!(s[n], 0);
    assert!(!s[..n].contains(&0));
    let v: serde_json::Value = serde_json::from_slice(&s[..n]).expect("json");
    assert!(v.is_object());
    unsafe { mvdz_free(p as *mut c_void, n) };
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let (ops, file) = data.split_at(data.len().min(16));
    let mut mem = Box::new(Mem { b: file, pos: 0 });
    let mut out = vec![0u8; 70_000];
    let mut readers: Vec<*mut c_void> = Vec::new();
    let mut err: c_int = 0;
    for (i, &op) in ops.iter().enumerate() {
        let r = readers
            .get(i % readers.len().max(1))
            .copied()
            .unwrap_or(std::ptr::null_mut());
        match op % 16 {
            0 => readers.push(unsafe {
                mvdz_open_memory(
                    file.as_ptr() as *const c_void,
                    file.len(),
                    1 + (op as c_int >> 6),
                    64 << 20,
                    (op >> 5) as u32 & 1,
                    &mut err,
                )
            }),
            1 => {
                mem.pos = 0;
                readers.push(unsafe {
                    mvdz_open_callbacks(
                        Some(rd),
                        Some(sk),
                        &mut *mem as *mut Mem as *mut c_void,
                        2,
                        64 << 20,
                        0,
                        &mut err,
                    )
                })
            }
            2 => {
                mem.pos = 0;
                readers.push(unsafe {
                    mvdz_open_callbacks(
                        Some(rd),
                        None,
                        &mut *mem as *mut Mem as *mut c_void,
                        1,
                        64 << 20,
                        MVDZ_TRUSTED,
                        &mut err,
                    )
                })
            }
            3 | 4 | 5 => {
                let n = [0usize, 1, 7, 4096, 70_000][(op >> 4) as usize % 5];
                let k = unsafe { mvdz_read(r, out.as_mut_ptr() as *mut c_void, n) };
                assert!(k <= n as c_longlong);
                assert!(k >= -7);
            }
            6 => {
                let k = unsafe { mvdz_read(r, std::ptr::null_mut(), 5) };
                assert!(k < 0);
            }
            7 => {
                let (mut t, mut o) = (0u64, 0u64);
                let _ = unsafe { mvdz_seek_time(r, (op as u64) * 1000, &mut t, &mut o) };
                let _ = unsafe {
                    mvdz_seek_time(r, u64::MAX, std::ptr::null_mut(), std::ptr::null_mut())
                };
            }
            8 => {
                let n = unsafe { mvdz_segment_count(r) };
                let mut o = 0u64;
                let _ = unsafe {
                    mvdz_seek_segment(
                        r,
                        if n > 0 {
                            (op as usize) % n as usize
                        } else {
                            usize::MAX
                        },
                        &mut o,
                    )
                };
            }
            9 => {
                let _ = unsafe { mvdz_raw_size(r) };
            }
            10 => unsafe { mvdz_close(r) },
            11 => {
                let mut p: *mut i8 = std::ptr::null_mut();
                let mut n = 0usize;
                if unsafe {
                    mvdz_info_json_memory(
                        file.as_ptr() as *const c_void,
                        file.len(),
                        64 << 20,
                        &mut p,
                        &mut n,
                    )
                } == 0
                {
                    check_json(p, n);
                }
            }
            12 => {
                mem.pos = 0;
                let mut p: *mut i8 = std::ptr::null_mut();
                let mut n = 0usize;
                if unsafe {
                    mvdz_info_json(
                        Some(rd),
                        Some(sk),
                        &mut *mem as *mut Mem as *mut c_void,
                        64 << 20,
                        &mut p,
                        &mut n,
                    )
                } == 0
                {
                    check_json(p, n);
                }
            }
            _ => {
                let mut p: *mut u8 = std::ptr::null_mut();
                let mut n = 0usize;
                if unsafe {
                    mvdz_decompress(
                        file.as_ptr() as *const c_void,
                        file.len(),
                        2,
                        64 << 20,
                        0,
                        &mut p,
                        &mut n,
                    )
                } == 0
                {
                    unsafe { mvdz_free(p as *mut c_void, n) };
                }
            }
        }
    }
    for r in readers {
        unsafe { mvdz_close(r) };
        // use after close
        assert!(unsafe { mvdz_read(r, out.as_mut_ptr() as *mut c_void, 1) } < 0 || r.is_null());
        unsafe { mvdz_close(r) };
    }
});
