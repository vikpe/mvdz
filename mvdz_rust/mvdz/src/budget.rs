//! A memory limit on top of the format's, which already make a decoder's memory and work
//! linear in its input. Checked before each allocation; running out is an error.

use crate::error::{Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// 1 GiB on 64-bit targets, 256 MiB on 32-bit ones.
pub(crate) const DEFAULT_MAX_MEMORY: u64 = if usize::BITS >= 64 {
    1 << 30
} else {
    256 << 20
};

#[derive(Debug)]
struct Inner {
    max_memory: u64,
    used: AtomicU64,
}

/// All clones draw on the same counter.
#[derive(Debug, Clone)]
pub(crate) struct Budget(Arc<Inner>);

impl Budget {
    pub(crate) fn new(max_memory: u64) -> Self {
        Self(Arc::new(Inner {
            max_memory,
            used: AtomicU64::new(0),
        }))
    }

    pub(crate) fn unlimited() -> Self {
        Self::new(u64::MAX)
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
                .filter(|&total| total <= inner.max_memory)
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
