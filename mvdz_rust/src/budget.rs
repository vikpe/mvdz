//! Decoder resource budget.
//!
//! The format rules (FORMAT.md, "Limits") already bound every declared
//! size by the bytes actually present: a decoder's memory and work are
//! linear in its input. The budget is the caller's cap on top of that: one
//! memory limit for everything a decode holds at once (output, segment
//! scratch, models, metadata) and one work limit (model
//! decisions plus output bytes, per input byte). It is checked before an
//! allocation is made, and running out is an error, never an abort.

use crate::error::{Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Caller limits on the resources one read may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes a read may hold at once.
    pub memory: u64,
    /// Work units (model decisions and output bytes) allowed per input
    /// byte, plus a fixed allowance of 2^20.
    pub work_per_byte: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory: if usize::BITS >= 64 {
                1 << 30
            } else {
                256 << 20
            },
            work_per_byte: 4096,
        }
    }
}

impl Limits {
    /// No limits (the encoder checking its own output).
    pub(crate) fn unlimited() -> Self {
        Self {
            memory: u64::MAX,
            work_per_byte: u64::MAX,
        }
    }
}

struct Inner {
    mem: u64,
    used: AtomicU64,
    work_per_byte: u64,
    input: AtomicU64,
    work: AtomicU64,
}

/// A shared budget; cheap to clone (all clones draw on the same counters).
#[derive(Clone)]
pub(crate) struct Budget(Arc<Inner>);

impl Budget {
    pub(crate) fn new(l: &Limits) -> Self {
        Self(Arc::new(Inner {
            mem: l.memory,
            used: AtomicU64::new(0),
            work_per_byte: l.work_per_byte,
            input: AtomicU64::new(0),
            work: AtomicU64::new(0),
        }))
    }

    /// No limits, whatever input is recorded.
    pub(crate) fn unlimited() -> Self {
        let b = Self::new(&Limits::unlimited());
        // the work limit is per input byte: without any it would be the
        // fixed allowance alone
        b.input(1);
        b
    }

    /// Records `n` more bytes of input (raises the work limit).
    pub(crate) fn input(&self, n: u64) {
        self.0.input.fetch_add(n, Ordering::Relaxed);
    }

    /// Reserves `n` bytes until the returned charge is dropped.
    pub(crate) fn alloc(&self, n: u64) -> Result<Charge> {
        self.reserve(n)?;
        Ok(Charge { b: self.clone(), n })
    }

    fn reserve(&self, n: u64) -> Result<()> {
        let i = &self.0;
        let mut cur = i.used.load(Ordering::Relaxed);
        loop {
            let new = cur
                .checked_add(n)
                .filter(|&v| v <= i.mem)
                .ok_or(Error::Limit("memory"))?;
            match i
                .used
                .compare_exchange_weak(cur, new, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return Ok(()),
                Err(c) => cur = c,
            }
        }
    }

    /// Accounts `n` units of work.
    pub(crate) fn work(&self, n: u64) -> Result<()> {
        let i = &self.0;
        let lim = i
            .input
            .load(Ordering::Relaxed)
            .saturating_mul(i.work_per_byte)
            .saturating_add(1 << 20);
        let w = i.work.fetch_add(n, Ordering::Relaxed).saturating_add(n);
        if w > lim {
            return Err(Error::Limit("work"));
        }
        Ok(())
    }
}

/// A reservation of budget memory, released on drop.
pub(crate) struct Charge {
    b: Budget,
    n: u64,
}

impl Charge {
    /// A charge of nothing.
    pub(crate) fn none(b: &Budget) -> Self {
        Self { b: b.clone(), n: 0 }
    }
    /// Reserves `n` more bytes under this charge.
    pub(crate) fn grow(&mut self, n: u64) -> Result<()> {
        self.b.reserve(n)?;
        self.n += n;
        Ok(())
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.b.0.used.fetch_sub(self.n, Ordering::Relaxed);
    }
}

/// A zeroed buffer of `n` bytes; allocation failure is an error.
pub(crate) fn zeroed(n: usize) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| Error::Limit("out of memory"))?;
    v.resize(n, 0);
    Ok(v)
}

/// An empty buffer with room for `n` bytes; allocation failure is an error.
pub(crate) fn with_capacity<T>(n: usize) -> Result<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| Error::Limit("out of memory"))?;
    Ok(v)
}
