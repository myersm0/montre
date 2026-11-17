use std::cmp::Ordering;

use montre_core::Span;
use roaring::RoaringBitmap;
use smallvec::SmallVec;

use crate::budget::Budget;
use crate::Result;

pub(crate) const MAX_QUANTIFIER: u32 = 100;

// start positions extended together; keeps the working set of an ambiguous
// pattern proportional to the batch instead of the document
const SEED_BATCH: usize = 256;

// partial matches created between two looks at the budget
const CHARGE_STRIDE: usize = 4096;

/// Index of a label among the labels of the query, in query order.
pub(crate) type LabelId = usize;

/// The span bound to one label. Bound slots order before `UNBOUND`, then by
/// earliest start, then by shortest span.
pub(crate) type Slot = (u64, u64);
pub(crate) const UNBOUND: Slot = (u64::MAX, u64::MAX);

/// Capture assignment of a partial match; stays off the heap for up to four labels.
pub(crate) type Slots = SmallVec<[Slot; 4]>;

/// `captures` holds one slot per label of the query, so that comparing two
/// matches of the same span compares their capture assignments label by label.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Match {
	pub start: u64,
	pub end: u64,
	pub captures: Vec<Slot>,
}

impl Match {
	pub(crate) fn unlabeled(start: u64, end: u64, slot_count: usize) -> Self {
		Self {
			start,
			end,
			captures: vec![UNBOUND; slot_count],
		}
	}

	pub(crate) fn same_span(&self, other: &Self) -> bool {
		self.start == other.start && self.end == other.end
	}
}

/// Whether a span reachable through several capture assignments is reported
/// once (with its smallest assignment) or once per assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bindings {
	Canonical,
	All,
}

pub(crate) trait Track: Clone + Ord + Send {
	fn unbound(slot_count: usize) -> Self;
	fn bind(&mut self, label: Option<LabelId>, start: u64, end: u64, inner: &[Slot]);
	fn slots(&self) -> &[Slot];
	fn into_captures(self) -> Vec<Slot>;
}

impl Track for () {
	fn unbound(_slot_count: usize) -> Self {}

	fn bind(&mut self, _label: Option<LabelId>, _start: u64, _end: u64, _inner: &[Slot]) {}

	fn slots(&self) -> &[Slot] {
		&[]
	}

	fn into_captures(self) -> Vec<Slot> {
		Vec::new()
	}
}

impl Track for Slots {
	fn unbound(slot_count: usize) -> Self {
		SmallVec::from_elem(UNBOUND, slot_count)
	}

	fn bind(&mut self, label: Option<LabelId>, start: u64, end: u64, inner: &[Slot]) {
		if let Some(label) = label {
			self[label] = (start, end);
		}
		for (slot, bound) in self.iter_mut().zip(inner) {
			if *bound != UNBOUND {
				*slot = *bound;
			}
		}
	}

	fn slots(&self) -> &[Slot] {
		self
	}

	fn into_captures(self) -> Vec<Slot> {
		self.into_vec()
	}
}

// field order gives the active set its (end, start, track) ordering
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Partial<T> {
	end: u64,
	start: u64,
	track: T,
}

impl<T: Track> Partial<T> {
	// partial matches that agree here cannot be told apart by anything still to come
	fn compare_distinguishing(&self, other: &Self, live: &[LabelId]) -> Ordering {
		let (ours, theirs) = (self.track.slots(), other.track.slots());
		(self.end, self.start)
			.cmp(&(other.end, other.start))
			.then_with(|| {
				live.iter()
					.map(|&label| ours[label])
					.cmp(live.iter().map(|&label| theirs[label]))
			})
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Run {
	pub start: u64,
	pub end: u64,
}

pub(crate) fn runs_from_bitmap(bitmap: &RoaringBitmap) -> Vec<Run> {
	let mut runs: Vec<Run> = Vec::new();
	for position in bitmap.iter().map(u64::from) {
		match runs.last_mut() {
			Some(run) if run.end == position => run.end += 1,
			_ => runs.push(Run {
				start: position,
				end: position + 1,
			}),
		}
	}
	runs
}

pub(crate) enum Matcher {
	/// sorted, disjoint, non-adjacent runs of single-token positions
	Runs(Vec<Run>),
	/// non-empty sub-matches sorted by (start, end, captures)
	Spans(Vec<Match>),
}

pub(crate) struct Step {
	pub matcher: Matcher,
	pub min: u32,
	pub max: u32,
	pub label: Option<LabelId>,
	/// every label bound by this step: its own and those inside its group
	pub declares: Vec<LabelId>,
}

impl Step {
	fn continuations<'a>(&'a self, position: u64, limit: u64, out: &mut Vec<(u64, &'a [Slot])>) {
		match &self.matcher {
			Matcher::Runs(runs) => {
				let Some(run) = run_containing(runs, position) else {
					return;
				};
				let available = run.end.min(limit).saturating_sub(position);
				let shortest = u64::from(self.min.max(1));
				let longest = available.min(u64::from(self.max));
				out.extend((shortest..=longest).map(|length| (position + length, &[][..])));
			}
			Matcher::Spans(table) => self.repeated_spans(table, position, limit, out),
		}
	}

	// captures inside a repeated group are those of its last repetition
	fn repeated_spans<'a>(
		&self,
		table: &'a [Match],
		position: u64,
		limit: u64,
		out: &mut Vec<(u64, &'a [Slot])>,
	) {
		let mut frontier: Vec<(u64, &'a [Slot])> = vec![(position, &[][..])];
		for repetition in 1..=self.max {
			let mut reached: Vec<(u64, &'a [Slot])> = frontier
				.iter()
				.flat_map(|&(from, _)| matches_starting_at(table, from))
				.filter(|candidate| candidate.end <= limit)
				.map(|candidate| (candidate.end, candidate.captures.as_slice()))
				.collect();
			reached.sort_unstable();
			reached.dedup();
			if reached.is_empty() {
				break;
			}
			if repetition >= self.min {
				out.extend_from_slice(&reached);
			}
			frontier = reached;
		}
		out.sort_unstable();
		out.dedup();
	}

	fn start_positions(&self, range: Span, out: &mut Vec<u64>) {
		match &self.matcher {
			Matcher::Runs(runs) => {
				let first = runs.partition_point(|run| run.end <= range.start);
				for run in runs[first..].iter().take_while(|run| run.start < range.end) {
					out.extend(run.start.max(range.start)..run.end.min(range.end));
				}
			}
			Matcher::Spans(table) => {
				let first = table.partition_point(|candidate| candidate.start < range.start);
				out.extend(
					table[first..]
						.iter()
						.take_while(|candidate| candidate.start < range.end)
						.map(|candidate| candidate.start),
				);
			}
		}
	}
}

fn run_containing(runs: &[Run], position: u64) -> Option<&Run> {
	let index = runs.partition_point(|run| run.end <= position);
	runs.get(index).filter(|run| run.start <= position)
}

fn matches_starting_at(table: &[Match], position: u64) -> impl Iterator<Item = &Match> {
	let first = table.partition_point(|candidate| candidate.start < position);
	table[first..]
		.iter()
		.take_while(move |candidate| candidate.start == position)
}

/// Decides the constraint at an index for a capture assignment.
pub(crate) type Holds<'a> = &'a (dyn Fn(usize, &[Slot]) -> bool + Sync);

pub(crate) struct Sequence {
	steps: Vec<Step>,
	seed_steps: usize,
	slot_count: usize,
	match_cost: u64,
	output_is_intermediate: bool,
	// per step: slots it declares that a constraint reads; left unbound by the
	// step, they stay unbound and the constraint cannot hold
	required: Vec<Vec<LabelId>>,
	// per step: the constraints decidable once it has run
	decidable: Vec<Vec<usize>>,
	// per step: the slots something later still reads
	live: Vec<Vec<LabelId>>,
}

impl Sequence {
	/// `constraints` lists, for each constraint the caller will be asked to
	/// decide, the slots it reads.
	pub(crate) fn new(
		steps: Vec<Step>,
		slot_count: usize,
		constraints: &[Vec<LabelId>],
		bindings: Bindings,
	) -> Self {
		// a match can only begin where one of the steps up to and including the
		// first mandatory one begins
		let seed_steps = steps
			.iter()
			.position(|step| step.min > 0)
			.map_or(steps.len(), |index| index + 1);
		let last_step = steps.len().saturating_sub(1);
		let declaring_step = |slot: &LabelId| {
			steps
				.iter()
				.position(|step| step.declares.contains(slot))
				.unwrap_or(last_step)
		};
		// a constraint is decidable after the last step that declares one of its slots
		let decided_after: Vec<usize> = constraints
			.iter()
			.map(|slots| slots.iter().map(declaring_step).max().unwrap_or(last_step))
			.collect();
		let required = steps
			.iter()
			.map(|step| {
				step.declares
					.iter()
					.copied()
					.filter(|slot| constraints.iter().any(|slots| slots.contains(slot)))
					.collect()
			})
			.collect();
		let decidable = (0..steps.len())
			.map(|step| {
				(0..constraints.len())
					.filter(|&constraint| decided_after[constraint] == step)
					.collect()
			})
			.collect();
		let live = (0..steps.len())
			.map(|step| match bindings {
				Bindings::All => (0..slot_count).collect(),
				Bindings::Canonical => {
					let mut awaited: Vec<LabelId> = constraints
						.iter()
						.zip(&decided_after)
						.filter(|(_, &after)| after > step)
						.flat_map(|(slots, _)| slots.iter().copied())
						.collect();
					awaited.sort_unstable();
					awaited.dedup();
					awaited
				}
			})
			.collect();
		Self {
			steps,
			seed_steps,
			slot_count,
			match_cost: 1 + slot_count as u64 / 4,
			output_is_intermediate: bindings == Bindings::All,
			required,
			decidable,
			live,
		}
	}

	/// Non-empty matches lying inside `range`, sorted by (start, end, captures).
	pub(crate) fn matches_in<T: Track>(
		&self,
		range: Span,
		holds: Holds,
		budget: &Budget,
	) -> Result<Vec<Match>> {
		let mut matches = Vec::new();
		for seeds in self.seeds(range).chunks(SEED_BATCH) {
			self.extend_seeds::<T>(seeds, range.end, holds, budget, &mut matches)?;
		}
		Ok(matches)
	}

	pub(crate) fn count_in(&self, range: Span, budget: &Budget) -> Result<usize> {
		Ok(self.matches_in::<()>(range, &|_, _| true, budget)?.len())
	}

	fn extend_seeds<T: Track>(
		&self,
		seeds: &[u64],
		limit: u64,
		holds: Holds,
		budget: &Budget,
		out: &mut Vec<Match>,
	) -> Result<()> {
		let cost = |matches: usize| matches as u64 * self.match_cost;
		let mut active: Vec<Partial<T>> = seeds
			.iter()
			.map(|&position| Partial {
				end: position,
				start: position,
				track: T::unbound(self.slot_count),
			})
			.collect();
		budget.hold(cost(active.len()))?;

		for (index, step) in self.steps.iter().enumerate() {
			if active.is_empty() {
				return Ok(());
			}
			let mut next: Vec<Partial<T>> = Vec::with_capacity(active.len());
			let mut charged = 0;
			let mut continuations: Vec<(u64, &[Slot])> = Vec::new();
			for partial in &active {
				if step.min == 0 {
					next.push(partial.clone());
				}
				continuations.clear();
				step.continuations(partial.end, limit, &mut continuations);
				for &(end, inner) in &continuations {
					let mut track = partial.track.clone();
					track.bind(step.label, partial.end, end, inner);
					next.push(Partial {
						end,
						start: partial.start,
						track,
					});
				}
				if next.len() - charged >= CHARGE_STRIDE {
					budget.hold(cost(next.len() - charged))?;
					budget.expand((next.len() - charged) as u64)?;
					charged = next.len();
				}
			}
			budget.hold(cost(next.len() - charged))?;
			budget.expand((next.len() - charged) as u64)?;
			let created = next.len();

			let required = &self.required[index];
			if !required.is_empty() {
				next.retain(|partial| {
					required
						.iter()
						.all(|&slot| partial.track.slots()[slot] != UNBOUND)
				});
			}
			for &constraint in &self.decidable[index] {
				next.retain(|partial| holds(constraint, partial.track.slots()));
			}

			// of the partial matches nothing later can tell apart, keep the smallest
			let live = &self.live[index];
			if live.is_empty() {
				next.sort_unstable();
				next.dedup_by(|later, earlier| {
					later.end == earlier.end && later.start == earlier.start
				});
			} else {
				next.sort_unstable_by(|left, right| {
					left.compare_distinguishing(right, live)
						.then_with(|| left.track.cmp(&right.track))
				});
				next.dedup_by(|later, earlier| {
					later.compare_distinguishing(earlier, live).is_eq()
				});
			}
			budget.release(cost(created - next.len() + active.len()));
			active = next;
		}

		let surviving = active.len();
		let first_new = out.len();
		out.extend(
			active
				.into_iter()
				.filter(|partial| partial.end > partial.start)
				.map(|partial| Match {
					start: partial.start,
					end: partial.end,
					captures: partial.track.into_captures(),
				}),
		);
		// matches that feed a group or a constraint stay charged to the query
		let kept = match self.output_is_intermediate {
			true => out.len() - first_new,
			false => 0,
		};
		budget.release(cost(surviving - kept));
		out[first_new..].sort_unstable();
		Ok(())
	}

	fn seeds(&self, range: Span) -> Vec<u64> {
		let mut positions = Vec::new();
		for step in &self.steps[..self.seed_steps] {
			step.start_positions(range, &mut positions);
		}
		positions.sort_unstable();
		positions.dedup();
		positions
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::budget::Limits;
	use crate::QueryError;

	fn runs(positions: &[u32]) -> Vec<Run> {
		runs_from_bitmap(&positions.iter().copied().collect())
	}

	fn token_step(positions: &[u32], min: u32, max: u32) -> Step {
		Step {
			matcher: Matcher::Runs(runs(positions)),
			min,
			max,
			label: None,
			declares: Vec::new(),
		}
	}

	fn labeled(positions: &[u32], min: u32, max: u32, label: LabelId) -> Step {
		Step {
			label: Some(label),
			declares: vec![label],
			..token_step(positions, min, max)
		}
	}

	fn unlabeled(steps: Vec<Step>) -> Sequence {
		Sequence::new(steps, 0, &[], Bindings::Canonical)
	}

	fn unlimited() -> Budget {
		Budget::new(Limits::unlimited())
	}

	fn spans(sequence: &Sequence, end: u64) -> Vec<(u64, u64)> {
		sequence
			.matches_in::<()>(Span::new(0, end), &|_, _| true, &unlimited())
			.unwrap()
			.into_iter()
			.map(|found| (found.start, found.end))
			.collect()
	}

	fn assignments(sequence: &Sequence, end: u64, holds: Holds) -> Vec<Match> {
		sequence
			.matches_in::<Slots>(Span::new(0, end), holds, &unlimited())
			.unwrap()
	}

	#[test]
	fn runs_merge_adjacent_positions() {
		let found = runs(&[0, 1, 2, 5, 6, 10]);
		assert_eq!(
			found,
			vec![
				Run { start: 0, end: 3 },
				Run { start: 5, end: 7 },
				Run { start: 10, end: 11 },
			]
		);
	}

	#[test]
	fn run_containing_respects_gaps() {
		let found = runs(&[0, 1, 2, 5, 6]);
		assert!(run_containing(&found, 2).is_some());
		assert!(run_containing(&found, 3).is_none());
		assert!(run_containing(&found, 5).is_some());
		assert!(run_containing(&found, 7).is_none());
	}

	#[test]
	fn quantified_single_step_enumerates_every_span_of_a_run() {
		let sequence = unlabeled(vec![token_step(&[0, 1, 2], 1, 3)]);
		assert_eq!(
			spans(&sequence, 10),
			vec![(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)]
		);
	}

	#[test]
	fn optional_first_step_matches_every_position_of_the_next() {
		// positions 4, 5, 6 all satisfy the second step; none is preceded by the first
		let sequence = unlabeled(vec![token_step(&[1], 0, 1), token_step(&[2, 4, 5, 6], 1, 1)]);
		assert_eq!(spans(&sequence, 10), vec![(1, 3), (2, 3), (4, 5), (5, 6), (6, 7)]);
	}

	#[test]
	fn matches_stop_at_the_range_limit() {
		let sequence = unlabeled(vec![token_step(&[2], 1, 1), token_step(&[3], 1, 1)]);
		assert_eq!(spans(&sequence, 10), vec![(2, 4)]);
		assert!(spans(&sequence, 3).is_empty());
	}

	#[test]
	fn span_step_advances_by_whole_sub_matches() {
		let group = Step {
			matcher: Matcher::Spans(vec![Match::unlabeled(0, 2, 0), Match::unlabeled(2, 4, 0)]),
			min: 1,
			max: MAX_QUANTIFIER,
			label: None,
			declares: Vec::new(),
		};
		let sequence = unlabeled(vec![group, token_step(&[2, 4], 1, 1)]);
		assert_eq!(spans(&sequence, 10), vec![(0, 3), (0, 5), (2, 5)]);
	}

	#[test]
	fn results_do_not_depend_on_seed_batching() {
		let positions: Vec<u32> = (0..5000).collect();
		let sequence = unlabeled(vec![token_step(&positions, 1, 2)]);
		let found = spans(&sequence, 5000);
		assert_eq!(found.len(), 5000 + 4999);
		assert!(found.windows(2).all(|pair| pair[0] < pair[1]));
	}

	#[test]
	fn all_bindings_keeps_every_capture_assignment() {
		let gap = || token_step(&[0, 1, 2, 3], 0, 2);
		let steps = || vec![gap(), labeled(&[1, 2], 1, 1, 0), gap()];
		let over_whole_range = |found: &&Match| found.start == 0 && found.end == 4;

		let all = assignments(&Sequence::new(steps(), 1, &[], Bindings::All), 4, &|_, _| true);
		assert_eq!(all.iter().filter(over_whole_range).count(), 2);

		let canonical = assignments(
			&Sequence::new(steps(), 1, &[], Bindings::Canonical),
			4,
			&|_, _| true,
		);
		let kept: Vec<&Match> = canonical.iter().filter(over_whole_range).collect();
		assert_eq!(kept.len(), 1);
		assert_eq!(kept[0].captures, vec![(1, 2)]);
	}

	#[test]
	fn a_bound_label_is_preferred_to_an_unbound_one() {
		// a:[]? []? b:[] over two tokens: the whole span is reachable with or without a
		let everywhere = [0, 1];
		let steps = vec![
			labeled(&everywhere, 0, 1, 0),
			token_step(&everywhere, 0, 1),
			labeled(&everywhere, 1, 1, 1),
		];
		let found = assignments(&Sequence::new(steps, 2, &[], Bindings::Canonical), 2, &|_, _| true);
		let whole: Vec<&Match> = found.iter().filter(|found| found.end - found.start == 2).collect();
		assert_eq!(whole.len(), 1);
		assert_eq!(whole[0].captures, vec![(0, 1), (1, 2)]);
	}

	// gap a:[] gap b:[] over five tokens, with one constraint reading both labels
	fn floating_pair(bindings: Bindings) -> Sequence {
		let everywhere = [0, 1, 2, 3, 4];
		let gap = || token_step(&everywhere, 0, 3);
		let steps = vec![gap(), labeled(&everywhere, 1, 1, 0), gap(), labeled(&everywhere, 1, 1, 1)];
		Sequence::new(steps, 2, &[vec![0, 1]], bindings)
	}

	#[test]
	fn a_pending_constraint_keeps_the_assignments_it_must_choose_between() {
		// accept only a at position 2 together with b at position 4
		let holds = |_: usize, slots: &[Slot]| slots[0] == (2, 3) && slots[1] == (4, 5);
		let found = assignments(&floating_pair(Bindings::Canonical), 5, &holds);
		let reported: Vec<(u64, u64)> = found.iter().map(|found| (found.start, found.end)).collect();
		assert_eq!(reported, vec![(0, 5), (1, 5), (2, 5)]);
		assert!(found.iter().all(|found| found.captures == vec![(2, 3), (4, 5)]));
	}

	#[test]
	fn a_skipped_label_that_a_constraint_reads_ends_the_partial_match() {
		// a:[]? b:[] with a constraint over both: only the path that binds a survives
		let everywhere = [0, 1, 2];
		let steps = vec![labeled(&everywhere, 0, 1, 0), labeled(&everywhere, 1, 1, 1)];
		let sequence = Sequence::new(steps, 2, &[vec![0, 1]], Bindings::Canonical);
		let found = assignments(&sequence, 3, &|_, _| true);
		let reported: Vec<(u64, u64)> = found.iter().map(|found| (found.start, found.end)).collect();
		assert_eq!(reported, vec![(0, 2), (1, 3)]);
	}

	#[test]
	fn budget_is_returned_once_results_are_final() {
		let budget = unlimited();
		floating_pair(Bindings::Canonical)
			.matches_in::<Slots>(Span::new(0, 5), &|_, _| true, &budget)
			.unwrap();
		assert_eq!(budget.held(), 0);
	}

	#[test]
	fn enumerated_assignments_stay_charged() {
		let budget = unlimited();
		let found = floating_pair(Bindings::All)
			.matches_in::<Slots>(Span::new(0, 5), &|_, _| true, &budget)
			.unwrap();
		assert_eq!(budget.held(), found.len() as u64);
	}

	#[test]
	fn exceeding_the_budget_is_an_error() {
		let budget = Budget::new(Limits {
			max_intermediate_matches: 8,
			max_expansions: u64::MAX,
		});
		let outcome = floating_pair(Bindings::Canonical).matches_in::<Slots>(
			Span::new(0, 5),
			&|_, _| true,
			&budget,
		);
		assert!(matches!(
			outcome,
			Err(QueryError::LimitExceeded { resource: "intermediate matches", limit: 8 })
		));

		let budget = Budget::new(Limits {
			max_intermediate_matches: u64::MAX,
			max_expansions: 10,
		});
		let outcome = floating_pair(Bindings::Canonical).matches_in::<Slots>(
			Span::new(0, 5),
			&|_, _| true,
			&budget,
		);
		assert!(matches!(
			outcome,
			Err(QueryError::LimitExceeded { resource: "expansions", limit: 10 })
		));
	}
}
