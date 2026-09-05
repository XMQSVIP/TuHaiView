//! Weighted admission. Leases travel with buffers and release on every exit path.
use parking_lot::{Condvar, Mutex};
use std::{sync::Arc, time::Duration};

#[derive(Default)]
struct Usage {
    total: usize,
    ordinary: usize,
}
pub struct ByteBudget {
    limit: usize,
    reserve: usize,
    usage: Mutex<Usage>,
    available: Condvar,
}
pub struct ByteLease {
    budget: Arc<ByteBudget>,
    bytes: usize,
    priority: bool,
}

impl ByteBudget {
    pub fn new(limit: usize, reserve: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            reserve: reserve.min(limit),
            usage: Mutex::new(Usage::default()),
            available: Condvar::new(),
        })
    }
    pub fn limit(&self, priority: bool) -> usize {
        if priority {
            self.limit
        } else {
            self.limit - self.reserve
        }
    }
    pub fn used(&self) -> usize {
        self.usage.lock().total
    }
    pub fn acquire(
        self: &Arc<Self>,
        bytes: usize,
        priority: bool,
        cancelled: impl Fn() -> bool,
    ) -> Option<ByteLease> {
        if bytes > self.limit(priority) {
            return None;
        }
        let mut usage = self.usage.lock();
        loop {
            if cancelled() {
                return None;
            }
            if usage.total <= self.limit - bytes
                && (priority || usage.ordinary <= self.limit - self.reserve - bytes)
            {
                usage.total += bytes;
                if !priority {
                    usage.ordinary += bytes;
                }
                return Some(ByteLease {
                    budget: self.clone(),
                    bytes,
                    priority,
                });
            }
            self.available
                .wait_for(&mut usage, Duration::from_millis(10));
        }
    }
    pub fn try_acquire(self: &Arc<Self>, bytes: usize) -> Option<ByteLease> {
        let mut usage = self.usage.lock();
        if bytes > self.limit || usage.total > self.limit - bytes {
            return None;
        }
        usage.total += bytes;
        Some(ByteLease {
            budget: self.clone(),
            bytes,
            priority: true,
        })
    }
}
impl Drop for ByteLease {
    fn drop(&mut self) {
        let mut usage = self.budget.usage.lock();
        usage.total -= self.bytes;
        if !self.priority {
            usage.ordinary -= self.bytes;
        }
        self.budget.available.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserve_and_cancel_do_not_leak_bytes() {
        let budget = ByteBudget::new(100, 25);
        let ordinary = budget.acquire(75, false, || false).unwrap();
        assert!(budget.acquire(76, false, || false).is_none());
        let preview = budget.acquire(25, true, || false).unwrap();
        assert_eq!(budget.used(), 100);
        assert!(budget.acquire(1, true, || true).is_none());
        drop(ordinary);
        drop(preview);
        assert_eq!(budget.used(), 0);
    }
}
