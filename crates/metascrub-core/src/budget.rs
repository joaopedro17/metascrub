//! Images, PDFs and Office files are read whole and take several times
//! their size in memory while they're cleaned. Each reserves an estimate
//! from a shared budget first, so a few large ones run one after another
//! instead of together. A file larger than the whole budget runs alone.

use std::sync::{Condvar, Mutex, PoisonError};

use crate::limits::LARGE_FILE_MEMORY_BYTES;
use crate::memory;
use crate::model::FormatId;

/// How many times its size a file of this format takes in memory, measured
/// on large files; `0` for formats that stream from disk.
fn factor(format: FormatId) -> u64 {
    match format {
        // The file, its parsed objects and the rewritten copy.
        FormatId::Pdf => 6,
        // Compressed parts unpacked, plus the new file.
        FormatId::Docx
        | FormatId::Xlsx
        | FormatId::Pptx
        | FormatId::Odt
        | FormatId::Ods
        | FormatId::Odp
        | FormatId::Odg => 4,
        FormatId::Jpeg | FormatId::Png | FormatId::Webp => 2,
        _ => 0,
    }
}

pub(crate) fn estimate(format: FormatId, len: u64) -> u64 {
    len.saturating_mul(factor(format))
}

#[derive(Debug)]
pub(crate) struct Budget {
    total: u64,
    available: Mutex<u64>,
    freed: Condvar,
}

impl Budget {
    pub(crate) fn new(total: u64) -> Self {
        Self {
            total,
            available: Mutex::new(total),
            freed: Condvar::new(),
        }
    }

    /// Waits until `amount` (at most the whole budget) is free.
    pub(crate) fn reserve(&self, amount: u64) -> Reservation<'_> {
        let amount = amount.min(self.total);
        let mut available = self
            .available
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while *available < amount {
            available = self
                .freed
                .wait(available)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *available -= amount;
        Reservation {
            budget: self,
            amount,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Reservation<'a> {
    budget: &'a Budget,
    amount: u64,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.amount == 0 {
            return;
        }
        *self
            .budget
            .available
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += self.amount;
        // The file's data is freed by now: it was held inside the handler.
        if self.amount >= LARGE_FILE_MEMORY_BYTES {
            memory::release_freed();
        }
        self.budget.freed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn large_files_wait_for_each_other() {
        let budget = Budget::new(100);
        let in_use = AtomicU64::new(0);
        let most = AtomicU64::new(0);
        std::thread::scope(|scope| {
            for amount in [60, 60, 60, 10] {
                let (budget, in_use, most) = (&budget, &in_use, &most);
                scope.spawn(move || {
                    let _reservation = budget.reserve(amount);
                    let now = in_use.fetch_add(amount, Ordering::SeqCst) + amount;
                    most.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    in_use.fetch_sub(amount, Ordering::SeqCst);
                });
            }
        });
        assert!(most.load(Ordering::SeqCst) <= 100);
        assert_eq!(*budget.available.lock().unwrap(), 100);
    }

    #[test]
    fn a_file_larger_than_the_budget_runs_alone() {
        let budget = Budget::new(100);
        let reservation = budget.reserve(1_000);
        assert_eq!(reservation.amount, 100);
        drop(reservation);
        let _streamed = budget.reserve(estimate(FormatId::Mp4, u64::MAX));
        let _photo = budget.reserve(estimate(FormatId::Jpeg, 40));
        assert_eq!(*budget.available.lock().unwrap(), 20);
    }
}
