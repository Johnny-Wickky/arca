//! Shared decompression budget.
//!
//! A single [`InflateBudget`] is threaded through the scan of a document and
//! of every container nested inside it (an `.xlsx` embedded into a `.docx`,
//! a `vbaProject.bin` inside a package, ...). This defeats recursive
//! ("42.zip"-style) bombs: nesting does not reset the allowance.

use core::fmt;

/// Error returned when an operation would exceed the shared budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded {
    /// Bytes requested by the failing operation.
    pub requested: u64,
    /// Bytes that were still available.
    pub remaining: u64,
}

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "decompression budget exceeded: requested {} bytes, {} remaining",
            self.requested, self.remaining
        )
    }
}

impl core::error::Error for BudgetExceeded {}

/// Global accounting of bytes produced by decompression for one document.
#[derive(Debug, Clone)]
pub struct InflateBudget {
    limit: u64,
    remaining: u64,
    compressed_in: u64,
    max_aggregate_ratio: u64,
    aggregate_grace: u64,
}

impl InflateBudget {
    /// Creates a budget allowing at most `limit` decompressed bytes in total.
    ///
    /// `max_aggregate_ratio` bounds `total_out / total_in` across all
    /// decompressions once more than `aggregate_grace` bytes were produced.
    #[must_use]
    pub const fn new(limit: u64, max_aggregate_ratio: u64, aggregate_grace: u64) -> Self {
        Self { limit, remaining: limit, compressed_in: 0, max_aggregate_ratio, aggregate_grace }
    }

    /// Bytes still available.
    #[must_use]
    pub const fn remaining(&self) -> u64 {
        self.remaining
    }

    /// Bytes produced so far.
    #[must_use]
    pub const fn produced(&self) -> u64 {
        self.limit.saturating_sub(self.remaining)
    }

    /// Reserves `produced` output bytes obtained from `consumed` input bytes.
    ///
    /// # Errors
    /// Fails if the absolute budget or the aggregate compression ratio would be
    /// exceeded. On failure the budget is left unchanged.
    pub fn charge(&mut self, produced: u64, consumed: u64) -> Result<(), BudgetExceeded> {
        if produced > self.remaining {
            return Err(BudgetExceeded { requested: produced, remaining: self.remaining });
        }
        let total_out = self.produced().saturating_add(produced);
        let total_in = self.compressed_in.saturating_add(consumed).max(1);
        if total_out > self.aggregate_grace
            && total_out > total_in.saturating_mul(self.max_aggregate_ratio)
        {
            return Err(BudgetExceeded { requested: produced, remaining: self.remaining });
        }
        self.remaining -= produced;
        self.compressed_in = self.compressed_in.saturating_add(consumed);
        Ok(())
    }
}

impl Default for InflateBudget {
    /// 1 GiB total, aggregate ratio 100:1 after the first 16 MiB.
    fn default() -> Self {
        Self::new(1 << 30, 100, 16 << 20)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_limit() {
        let mut b = InflateBudget::new(100, 1_000, 1_000);
        assert!(b.charge(60, 60).is_ok());
        assert!(b.charge(50, 50).is_err());
        assert_eq!(b.remaining(), 40);
        assert!(b.charge(40, 40).is_ok());
        assert_eq!(b.remaining(), 0);
    }

    #[test]
    fn aggregate_ratio_after_grace() {
        let mut b = InflateBudget::new(u64::MAX, 100, 1_000);
        // Under grace: any ratio is fine.
        assert!(b.charge(900, 1).is_ok());
        // Past grace with ratio > 100 is refused.
        assert!(b.charge(10_000, 1).is_err());
        // Supplying enough input keeps the aggregate ratio in bounds.
        assert!(b.charge(10_000, 200).is_ok());
    }
}
