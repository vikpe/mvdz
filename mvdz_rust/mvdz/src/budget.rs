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
    memory: u64,
    used: AtomicU64,
    work_per_byte: u64,
    input: AtomicU64,
    work: AtomicU64,
}

/// A shared budget; cheap to clone (all clones draw on the same counters).
#[derive(Clone)]
pub(crate) struct Budget(Arc<Inner>);

impl Budget {
    pub(crate) fn new(limits: &Limits) -> Self {
        Self(Arc::new(Inner {
            memory: limits.memory,
            used: AtomicU64::new(0),
            work_per_byte: limits.work_per_byte,
            input: AtomicU64::new(0),
            work: AtomicU64::new(0),
        }))
    }

    /// No limits, whatever input is recorded.
    pub(crate) fn unlimited() -> Self {
        let budget = Self::new(&Limits::unlimited());
        // the work limit is per input byte: without any it would be the
        // fixed allowance alone
        budget.input(1);
        budget
    }

    /// Records `len` more bytes of input (raises the work limit).
    pub(crate) fn input(&self, len: u64) {
        self.0.input.fetch_add(len, Ordering::Relaxed);
    }

    /// Reserves `len` bytes until the returned charge is dropped.
    pub(crate) fn alloc(&self, len: u64) -> Result<Charge> {
        self.reserve(len)?;
        Ok(Charge {
            budget: self.clone(),
            len,
        })
    }

    fn reserve(&self, len: u64) -> Result<()> {
        let inner = &self.0;
        let mut current = inner.used.load(Ordering::Relaxed);
        loop {
            let new = current
                .checked_add(len)
                .filter(|&total| total <= inner.memory)
                .ok_or(Error::Limit("memory"))?;
            match inner.used.compare_exchange_weak(
                current,
                new,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => current = actual,
            }
        }
    }

    /// Accounts `units` of work.
    pub(crate) fn work(&self, units: u64) -> Result<()> {
        let inner = &self.0;
        let limit = inner
            .input
            .load(Ordering::Relaxed)
            .saturating_mul(inner.work_per_byte)
            .saturating_add(1 << 20);
        let total = inner
            .work
            .fetch_add(units, Ordering::Relaxed)
            .saturating_add(units);
        if total > limit {
            return Err(Error::Limit("work"));
        }
        Ok(())
    }
}

/// A reservation of budget memory, released on drop.
pub(crate) struct Charge {
    budget: Budget,
    len: u64,
}

impl Charge {
    /// A charge of nothing.
    pub(crate) fn none(budget: &Budget) -> Self {
        Self {
            budget: budget.clone(),
            len: 0,
        }
    }
    /// Reserves `len` more bytes under this charge.
    pub(crate) fn grow(&mut self, len: u64) -> Result<()> {
        self.budget.reserve(len)?;
        self.len += len;
        Ok(())
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.budget.0.used.fetch_sub(self.len, Ordering::Relaxed);
    }
}

/// A zeroed buffer of `len` bytes; allocation failure is an error.
pub(crate) fn zeroed(len: usize) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(len)
        .map_err(|_| Error::Limit("out of memory"))?;
    buffer.resize(len, 0);
    Ok(buffer)
}

/// An empty buffer with room for `capacity` bytes; allocation failure is an
/// error.
pub(crate) fn with_capacity<T>(capacity: usize) -> Result<Vec<T>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(capacity)
        .map_err(|_| Error::Limit("out of memory"))?;
    Ok(buffer)
}
