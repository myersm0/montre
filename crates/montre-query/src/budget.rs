use std::sync::atomic::{AtomicU64, Ordering};

use crate::{QueryError, Result};

/// Ceilings on what one query may consume. Exceeding either is an error; a
/// query never returns a partial result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
	/// Intermediate matches the query may hold at once, summed over all
	/// threads: partial matches being extended, the prepared matches of
	/// groups, and capture assignments enumerated for a constraint. Each costs
	/// roughly 100 bytes and counts once for up to three labels, more beyond
	/// that. The hits of the result itself are not counted.
	pub max_intermediate_matches: u64,
	/// Partial matches the query may create in total. A ceiling on work, where
	/// the other is a ceiling on memory.
	pub max_expansions: u64,
}

impl Limits {
	pub const fn unlimited() -> Self {
		Self {
			max_intermediate_matches: u64::MAX,
			max_expansions: u64::MAX,
		}
	}
}

impl Default for Limits {
	fn default() -> Self {
		Self {
			max_intermediate_matches: 10_000_000,
			max_expansions: u64::MAX,
		}
	}
}

/// Shared by every thread and nested group of one query.
pub(crate) struct Budget {
	limits: Limits,
	held: AtomicU64,
	expanded: AtomicU64,
}

impl Budget {
	pub(crate) fn new(limits: Limits) -> Self {
		Self {
			limits,
			held: AtomicU64::new(0),
			expanded: AtomicU64::new(0),
		}
	}

	pub(crate) fn hold(&self, amount: u64) -> Result<()> {
		let held = self.held.fetch_add(amount, Ordering::Relaxed).saturating_add(amount);
		if held > self.limits.max_intermediate_matches {
			return Err(QueryError::LimitExceeded {
				resource: "intermediate matches",
				limit: self.limits.max_intermediate_matches,
			});
		}
		Ok(())
	}

	pub(crate) fn release(&self, amount: u64) {
		self.held.fetch_sub(amount, Ordering::Relaxed);
	}

	pub(crate) fn expand(&self, amount: u64) -> Result<()> {
		let expanded = self
			.expanded
			.fetch_add(amount, Ordering::Relaxed)
			.saturating_add(amount);
		if expanded > self.limits.max_expansions {
			return Err(QueryError::LimitExceeded {
				resource: "expansions",
				limit: self.limits.max_expansions,
			});
		}
		Ok(())
	}

	#[cfg(test)]
	pub(crate) fn held(&self) -> u64 {
		self.held.load(Ordering::Relaxed)
	}
}

/// The charges for one intermediate buffer. Dropping the ledger, which its
/// owner does together with the buffer, returns them to the budget.
pub(crate) struct Ledger<'b> {
	budget: &'b Budget,
	held: AtomicU64,
}

impl<'b> Ledger<'b> {
	pub(crate) fn new(budget: &'b Budget) -> Self {
		Self {
			budget,
			held: AtomicU64::new(0),
		}
	}

	pub(crate) fn hold(&self, amount: u64) -> Result<()> {
		self.budget.hold(amount)?;
		self.adopt(amount);
		Ok(())
	}

	/// Take over an amount the budget already holds.
	pub(crate) fn adopt(&self, amount: u64) {
		self.held.fetch_add(amount, Ordering::Relaxed);
	}
}

impl Drop for Ledger<'_> {
	fn drop(&mut self) {
		self.budget.release(*self.held.get_mut());
	}
}

/// Where the matches a node produces go: back to the caller of the query, or
/// into a buffer the query keeps while it runs.
#[derive(Clone, Copy)]
pub(crate) enum Output<'l, 'b> {
	Final,
	Buffered(&'l Ledger<'b>),
}

impl Output<'_, '_> {
	pub(crate) fn hold(&self, amount: u64) -> Result<()> {
		match self {
			Output::Final => Ok(()),
			Output::Buffered(ledger) => ledger.hold(amount),
		}
	}
}
