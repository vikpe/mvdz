//! The caller's cap on top of the format limits, which already make a decoder's memory and
//! work linear in its input. Checked before each allocation; running out is an error.

use crate::error::{Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) memory: u64,
    /// Work units (model decisions and output bytes) allowed per input
    /// byte, plus a fixed allowance of 2^20.
    pub(crate) work_per_byte: u64,
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
    /// For the encoder checking its own output.
    pub(crate) fn unlimited() -> Self {
        Self {
            memory: u64::MAX,
            work_per_byte: u64::MAX,
        }
    }
}

#[derive(Debug)]
struct Inner {
    memory: u64,
    used: AtomicU64,
    work_per_byte: u64,
    input: AtomicU64,
    work: AtomicU64,
}

/// All clones draw on the same counters.
#[derive(Debug, Clone)]
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

    pub(crate) fn unlimited() -> Self {
        let budget = Self::new(&Limits::unlimited());
        // the work limit is per input byte: without input it is the fixed allowance alone
        budget.input(1);
        budget
    }

    /// Raises the work limit.
    pub(crate) fn input(&self, len: u64) {
        self.0.input.fetch_add(len, Ordering::Relaxed);
    }

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

/// Released on drop.
#[derive(Debug)]
pub(crate) struct Charge {
    budget: Budget,
    len: u64,
}

impl Charge {
    pub(crate) fn none(budget: &Budget) -> Self {
        Self {
            budget: budget.clone(),
            len: 0,
        }
    }
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

/// Allocation failure is an error.
pub(crate) fn zeroed(len: usize) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(len)
        .map_err(|_| Error::Limit("out of memory"))?;
    buffer.resize(len, 0);
    Ok(buffer)
}

/// Allocation failure is an error.
pub(crate) fn with_capacity<T>(capacity: usize) -> Result<Vec<T>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(capacity)
        .map_err(|_| Error::Limit("out of memory"))?;
    Ok(buffer)
}
