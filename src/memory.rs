use std::sync::{Condvar, Mutex};

pub(crate) struct Memory {
    total: u64,
    available: Mutex<u64>,
    released: Condvar,
}

impl Memory {
    pub(crate) fn new(total: u64) -> Self {
        Memory {
            total,
            available: Mutex::new(total),
            released: Condvar::new(),
        }
    }

    pub(crate) fn reserve(&self, wanted: u64) -> Reservation<'_> {
        let amount = wanted.min(self.total);
        let mut available = self
            .available
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while *available < amount {
            available = self
                .released
                .wait(available)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        *available -= amount;
        Reservation {
            memory: self,
            amount,
        }
    }

    pub(crate) fn reserve_within(
        &self,
        wanted: u64,
        patience: std::time::Duration,
    ) -> Option<Reservation<'_>> {
        let amount = wanted.min(self.total);
        let available = self
            .available
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (mut available, waited) = self
            .released
            .wait_timeout_while(available, patience, |available| *available < amount)
            .unwrap_or_else(|poison| poison.into_inner());
        if waited.timed_out() && *available < amount {
            return None;
        }
        *available -= amount;
        Some(Reservation {
            memory: self,
            amount,
        })
    }
}

pub(crate) struct Reservation<'a> {
    memory: &'a Memory,
    amount: u64,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        *self
            .memory
            .available
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) += self.amount;
        self.memory.released.notify_all();
    }
}
