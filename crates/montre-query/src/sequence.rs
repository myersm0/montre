use std::cmp::Ordering;

use std::borrow::Cow;

use montre_core::Span;
use roaring::RoaringBitmap;
use smallvec::SmallVec;

use crate::budget::{Budget, Output};
use crate::Result;

pub(crate) const MAX_QUANTIFIER: u32 = 100;

/// Token steps repeated at most this often are matched against their bitmap;
/// longer repetitions are matched by runs of consecutive positions.
pub(crate) const SHORT_REPETITION: u32 = 8;

// the most fixed-length variants a sequence is split into
const MAX_VARIANTS: usize = 16;

// start positions extended together; keeps the working set of an ambiguous
// pattern proportional to the batch instead of the document
const SEED_BATCH: usize = 256;

// candidate starts each source contributes to one window, all gathered
// before any is extended; bounds that buffer by the window, not the document
const WINDOW_STARTS: usize = 1 << 14;

// matches created between two looks at the budget
pub(crate) const CHARGE_STRIDE: usize = 4096;

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

pub(crate) enum Matcher<'c> {
	/// sorted, disjoint, non-adjacent runs of single-token positions
	Runs(Vec<Run>),
	/// single-token positions
	Bitmap(Cow<'c, RoaringBitmap>),
	/// non-empty sub-matches sorted by (start, end, captures)
	Spans(Vec<Match>),
}

pub(crate) struct Step<'c> {
	pub matcher: Matcher<'c>,
	pub min: u32,
	pub max: u32,
	pub label: Option<LabelId>,
	/// every label bound by this step: its own and those inside its group
	pub declares: Vec<LabelId>,
}

impl Step<'_> {
	fn continuations<'a>(
		&'a self,
		position: u64,
		limit: u64,
		budget: &Budget,
		out: &mut Vec<(u64, &'a [Slot])>,
	) -> Result<()> {
		match &self.matcher {
			Matcher::Runs(runs) => {
				let Some(run) = run_containing(runs, position) else {
					return Ok(());
				};
				let available = run.end.min(limit).saturating_sub(position);
				let shortest = u64::from(self.min.max(1));
				let longest = available.min(u64::from(self.max));
				out.extend((shortest..=longest).map(|length| (position + length, &[][..])));
				Ok(())
			}
			Matcher::Bitmap(bitmap) => {
				let shortest = u64::from(self.min.max(1));
				let longest = u64::from(self.max).min(limit.saturating_sub(position));
				let available = (0..longest)
					.take_while(|&length| bitmap.contains((position + length) as u32))
					.count() as u64;
				out.extend((shortest..=available).map(|length| (position + length, &[][..])));
				Ok(())
			}
			Matcher::Spans(table) => self.repeated_spans(table, position, limit, budget, out),
		}
	}

	// captures inside a repeated group are those of its last repetition
	fn repeated_spans<'a>(
		&self,
		table: &'a [Match],
		position: u64,
		limit: u64,
		budget: &Budget,
		out: &mut Vec<(u64, &'a [Slot])>,
	) -> Result<()> {
		let mut frontier: Vec<(u64, &'a [Slot])> = vec![(position, &[][..])];
		// entries of the frontier and of `out` charged while this call runs
		let mut held = 0;
		let charge = |entries: usize, held: &mut u64| -> Result<()> {
			budget.hold(entries as u64)?;
			budget.expand(entries as u64)?;
			*held += entries as u64;
			Ok(())
		};
		for repetition in 1..=self.max {
			let mut reached: Vec<(u64, &'a [Slot])> = Vec::new();
			let mut charged = 0;
			for &(from, _) in &frontier {
				reached.extend(
					matches_starting_at(table, from)
						.filter(|candidate| candidate.end <= limit)
						.map(|candidate| (candidate.end, candidate.captures.as_slice())),
				);
				if reached.len() - charged >= CHARGE_STRIDE {
					charge(reached.len() - charged, &mut held)?;
					charged = reached.len();
				}
			}
			charge(reached.len() - charged, &mut held)?;
			let candidates = reached.len();
			reached.sort_unstable();
			reached.dedup();
			// the duplicates just removed, and the frontier this one replaces
			let dropped = (candidates - reached.len() + frontier.len()) as u64;
			budget.release(dropped.min(held));
			held -= dropped.min(held);
			if reached.is_empty() {
				break;
			}
			if repetition >= self.min {
				budget.hold(reached.len() as u64)?;
				held += reached.len() as u64;
				out.extend_from_slice(&reached);
			}
			frontier = reached;
		}
		out.sort_unstable();
		out.dedup();
		budget.release(held);
		Ok(())
	}

	fn is_token_step(&self) -> bool {
		!matches!(self.matcher, Matcher::Spans(_))
	}

	fn fixed_width(&self) -> Option<u64> {
		let fixed = self.is_token_step() && self.min == self.max && self.min > 0;
		fixed.then_some(u64::from(self.min))
	}

	fn matches_nowhere(&self) -> bool {
		match &self.matcher {
			Matcher::Runs(runs) => runs.is_empty(),
			Matcher::Bitmap(bitmap) => bitmap.is_empty(),
			Matcher::Spans(table) => table.is_empty(),
		}
	}

	fn start_count(&self) -> u64 {
		match &self.matcher {
			Matcher::Runs(runs) => runs.iter().map(|run| run.end - run.start).sum(),
			Matcher::Bitmap(bitmap) => bitmap.len(),
			Matcher::Spans(table) => table.len() as u64,
		}
	}

	// whether a token step matches every position of [position, position +
	// length) and that span ends by `limit`; `cursor` carries the search over
	// from the previous, smaller position
	fn covers(&self, position: u64, length: u64, limit: u64, cursor: &mut usize) -> bool {
		let end = position + length;
		match &self.matcher {
			Matcher::Runs(runs) => {
				*cursor = gallop(runs, *cursor, |run| run.end <= position);
				runs.get(*cursor)
					.is_some_and(|run| run.start <= position && run.end.min(limit) >= end)
			}
			Matcher::Bitmap(bitmap) => {
				end <= limit && bitmap.contains_range(position as u32..end as u32)
			}
			Matcher::Spans(_) => unreachable!("a group step is not matched token by token"),
		}
	}

	// whether one repetition of the step, or `min` of them for a token step,
	// can begin at `position` and end by `limit`; `cursor` carries the search
	// over from the previous, smaller position
	fn can_start_at(&self, position: u64, limit: u64, cursor: &mut usize) -> bool {
		match &self.matcher {
			Matcher::Runs(_) | Matcher::Bitmap(_) => {
				self.covers(position, u64::from(self.min.max(1)), limit, cursor)
			}
			Matcher::Spans(table) => {
				*cursor = gallop(table, *cursor, |candidate| candidate.start < position);
				table[*cursor..]
					.iter()
					.take_while(|candidate| candidate.start == position)
					.any(|candidate| candidate.end <= limit)
			}
		}
	}

	// the first `count` positions inside `range` at which the step begins
	fn start_positions(&self, range: Span, count: usize, out: &mut Vec<u64>) {
		let Some(last) = count.checked_sub(1) else {
			return;
		};
		let full = out.len() + count;
		match &self.matcher {
			Matcher::Runs(runs) => {
				let first = runs.partition_point(|run| run.end <= range.start);
				for run in runs[first..].iter().take_while(|run| run.start < range.end) {
					let start = run.start.max(range.start);
					let room = (full - out.len()) as u64;
					out.extend(start..run.end.min(range.end).min(start + room));
					if out.len() == full {
						break;
					}
				}
			}
			Matcher::Bitmap(bitmap) => {
				// bounded by position rather than by `take`, which would bypass
				// the bulk path roaring gives `extend`
				let end = bitmap
					.range(range.start as u32..range.end as u32)
					.nth(last)
					.map_or(range.end, |position| u64::from(position) + 1);
				out.extend(bitmap.range(range.start as u32..end as u32).map(u64::from));
			}
			Matcher::Spans(table) => {
				let first = table.partition_point(|candidate| candidate.start < range.start);
				out.extend(
					table[first..]
						.iter()
						.take_while(|candidate| candidate.start < range.end)
						.map(|candidate| candidate.start)
						.take(count),
				);
			}
		}
	}
}

// Every choice of repetition count for the steps, when all are token steps
// and the choices are few. A variant that matches nothing at all is dropped.
fn variants(steps: &[Step]) -> Vec<Variant> {
	let mut choices: Vec<Vec<u32>> = vec![Vec::new()];
	for step in steps {
		let counts = step.max - step.min + 1;
		if !step.is_token_step() || choices.len() * counts as usize > MAX_VARIANTS {
			return Vec::new();
		}
		choices = choices
			.into_iter()
			.flat_map(|chosen| {
				(step.min..=step.max).map(move |count| [chosen.as_slice(), &[count]].concat())
			})
			.collect();
	}
	let start_counts: Vec<u64> = steps.iter().map(Step::start_count).collect();
	choices
		.into_iter()
		.filter_map(|counts| {
			let mut segments = Vec::new();
			let mut offset = 0;
			for (step, &count) in counts.iter().enumerate() {
				if count > 0 {
					let length = u64::from(count);
					segments.push(Segment {
						step,
						offset,
						length,
					});
					offset += length;
				}
			}
			segments.sort_by_key(|segment| start_counts[segment.step]);
			(offset > 0).then_some(Variant {
				segments,
				width: offset,
			})
		})
		.collect()
}

// the first index from `from` at which `before` fails, for a predicate that
// holds on a prefix of `items`; O(log distance) from `from`
fn gallop<T>(items: &[T], from: usize, before: impl Fn(&T) -> bool) -> usize {
	let rest = &items[from..];
	let mut bound = 1;
	while bound < rest.len() && before(&rest[bound]) {
		bound *= 2;
	}
	let low = bound / 2;
	from + low + rest[low..bound.min(rest.len())].partition_point(before)
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

/// Decides whether a match spanning (start, end) passes the span, component
/// and document filters above the sequence.
pub(crate) type Keep<'a> = &'a (dyn Fn(u64, u64) -> bool + Sync);

// tokens of a step at a fixed offset from the start of a match
struct Segment {
	step: usize,
	offset: u64,
	length: u64,
}

// one choice of repetition count for every step: a pattern of fixed width
// whose segments are listed rarest step first
struct Variant {
	segments: Vec<Segment>,
	width: u64,
}

pub(crate) struct Sequence<'c> {
	steps: Vec<Step<'c>>,
	seed_steps: usize,
	// mandatory steps at a fixed offset from the start of every match, rarest
	// first; empty when the first step is optional
	anchors: Vec<(usize, u64)>,
	// set when nothing is bound and every step is a token step with few
	// possible repetition counts; each match is then the match of a variant
	variants: Vec<Variant>,
	// steps, with their offsets, whose start positions less the offset are
	// the candidate starts: one per variant, the rarest anchor, or else every
	// step that may begin a match
	sources: Vec<(usize, u64)>,
	// a mandatory step matches nowhere
	unmatchable: bool,
	slot_count: usize,
	match_cost: u64,
	// per step: slots it declares that a constraint reads; left unbound by the
	// step, they stay unbound and the constraint cannot hold
	required: Vec<Vec<LabelId>>,
	// per step: the constraints decidable once it has run
	decidable: Vec<Vec<usize>>,
	// per step: the slots something later still reads
	live: Vec<Vec<LabelId>>,
}

impl<'c> Sequence<'c> {
	/// `constraints` lists, for each constraint the caller will be asked to
	/// decide, the slots it reads.
	pub(crate) fn new(
		steps: Vec<Step<'c>>,
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
		let mut anchors = Vec::new();
		let mut offset = 0;
		for (index, step) in steps.iter().enumerate() {
			if step.min == 0 {
				break;
			}
			anchors.push((index, offset));
			match step.fixed_width() {
				Some(width) => offset += width,
				None => break,
			}
		}
		let binds_nothing = constraints.is_empty()
			&& steps
				.iter()
				.all(|step| step.label.is_none() && step.declares.is_empty());
		let variants = match binds_nothing {
			true => variants(&steps),
			false => Vec::new(),
		};
		if anchors.len() > 1 {
			anchors.sort_by_cached_key(|&(index, _)| steps[index].start_count());
		}
		let sources = match (variants.is_empty(), anchors.len()) {
			(false, _) => variants
				.iter()
				.map(|variant| (variant.segments[0].step, variant.segments[0].offset))
				.collect(),
			(true, count) if count > 1 => vec![anchors[0]],
			(true, _) => (0..seed_steps).map(|step| (step, 0)).collect(),
		};
		let unmatchable = steps
			.iter()
			.any(|step| step.min > 0 && step.matches_nowhere());
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
			anchors,
			variants,
			sources,
			unmatchable,
			slot_count,
			match_cost: 1 + slot_count as u64 / 4,
			required,
			decidable,
			live,
		}
	}

	/// Non-empty matches lying inside `range` that `keep` accepts, sorted by
	/// (start, end, captures).
	pub(crate) fn matches_in<T: Track>(
		&self,
		range: Span,
		holds: Holds,
		keep: Keep,
		budget: &Budget,
		output: Output,
	) -> Result<Vec<Match>> {
		let mut matches = Vec::new();
		for starts in self.start_windows(range) {
			for batch in starts.chunks(SEED_BATCH) {
				let active = self.extend_seeds::<T>(batch, range.end, holds, budget)?;
				let surviving = active.len();
				let first_new = matches.len();
				matches.extend(
					active
						.into_iter()
						.filter(|partial| {
							partial.end > partial.start && keep(partial.start, partial.end)
						})
						.map(|partial| Match {
							start: partial.start,
							end: partial.end,
							captures: partial.track.into_captures(),
						}),
				);
				matches[first_new..].sort_unstable();
				let emitted = matches.len() - first_new;
				// matches bound for a buffer stay charged, to that buffer's ledger
				let buffered = match output {
					Output::Final => 0,
					Output::Buffered(ledger) => {
						ledger.adopt(self.cost(emitted));
						emitted
					}
				};
				budget.release(self.cost(surviving - buffered));
				if let Output::Final = output {
					budget.report(emitted)?;
				}
			}
		}
		Ok(matches)
	}

	/// The number of matches `matches_in` would return, without building them.
	pub(crate) fn count_in<T: Track>(
		&self,
		range: Span,
		holds: Holds,
		keep: Keep,
		budget: &Budget,
	) -> Result<usize> {
		let mut count = 0;
		for starts in self.start_windows(range) {
			for batch in starts.chunks(SEED_BATCH) {
				let active = self.extend_seeds::<T>(batch, range.end, holds, budget)?;
				count += active
					.iter()
					.filter(|partial| {
						partial.end > partial.start && keep(partial.start, partial.end)
					})
					.count();
				budget.release(self.cost(active.len()));
			}
		}
		Ok(count)
	}

	fn cost(&self, matches: usize) -> u64 {
		matches as u64 * self.match_cost
	}

	// The partial matches left once every step has run over `starts`, the
	// empty ones included. They remain charged to the budget; the caller
	// releases them.
	fn extend_seeds<T: Track>(
		&self,
		starts: &[(u64, u64)],
		limit: u64,
		holds: Holds,
		budget: &Budget,
	) -> Result<Vec<Partial<T>>> {
		let mut active: Vec<Partial<T>> = starts
			.iter()
			.map(|&(start, end)| Partial {
				end,
				start,
				track: T::unbound(self.slot_count),
			})
			.collect();
		if !self.variants.is_empty() {
			budget.hold(self.cost(active.len()))?;
			budget.expand(active.len() as u64)?;
			return Ok(active);
		}
		budget.hold(self.cost(active.len()))?;

		for (index, step) in self.steps.iter().enumerate() {
			if active.is_empty() {
				break;
			}
			let mut next: Vec<Partial<T>> = Vec::with_capacity(active.len());
			let mut charged = 0;
			let mut continuations: Vec<(u64, &[Slot])> = Vec::new();
			for partial in &active {
				if step.min == 0 {
					next.push(partial.clone());
				}
				continuations.clear();
				step.continuations(partial.end, limit, budget, &mut continuations)?;
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
					budget.hold(self.cost(next.len() - charged))?;
					budget.expand((next.len() - charged) as u64)?;
					charged = next.len();
				}
			}
			budget.hold(self.cost(next.len() - charged))?;
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
				next.dedup_by(|later, earlier| later.compare_distinguishing(earlier, live).is_eq());
			}
			budget.release(self.cost(created - next.len() + active.len()));
			active = next;
		}
		Ok(active)
	}

	// The starts inside `range`, sorted, a window at a time: see `starts`.
	fn start_windows(&self, range: Span) -> impl Iterator<Item = Vec<(u64, u64)>> + '_ {
		let mut from = range.start;
		std::iter::from_fn(move || {
			if from >= range.end {
				return None;
			}
			let (starts, end) = self.starts(from, range.end);
			from = end;
			Some(starts)
		})
	}

	// Matches of the variants as (start, end), or where matches may start as
	// (start, start), sorted, for matches ending by `limit` that start in the
	// window from `from`; with where that window ends.
	fn starts(&self, from: u64, limit: u64) -> (Vec<(u64, u64)>, u64) {
		if self.unmatchable {
			return (Vec::new(), limit);
		}
		let (candidates, end) = self.candidates(from, limit);
		let starts = match self.variants.is_empty() {
			true => self
				.seeds(candidates, limit)
				.into_iter()
				.map(|start| (start, start))
				.collect(),
			false => self.variant_matches(candidates, limit),
		};
		(starts, end)
	}

	// The candidate starts of each source from `from` on, cut back to the
	// window in which none has more than `WINDOW_STARTS`; with where that
	// window ends.
	fn candidates(&self, from: u64, limit: u64) -> (Vec<Vec<u64>>, u64) {
		let mut candidates: Vec<Vec<u64>> = self
			.sources
			.iter()
			.map(|&(step, offset)| {
				let mut positions = Vec::new();
				if from + offset < limit {
					let range = Span::new(from + offset, limit);
					self.steps[step].start_positions(range, WINDOW_STARTS, &mut positions);
				}
				if offset > 0 {
					positions
						.iter_mut()
						.for_each(|position| *position -= offset);
				}
				positions
			})
			.collect();
		let end = candidates
			.iter()
			.filter(|positions| positions.len() == WINDOW_STARTS)
			.map(|positions| positions[WINDOW_STARTS - 1] + 1)
			.min()
			.unwrap_or(limit);
		for positions in &mut candidates {
			positions.truncate(positions.partition_point(|&start| start < end));
		}
		(candidates, end)
	}

	// The matches, ending by `limit`, of each variant from its candidates.
	fn variant_matches(&self, candidates: Vec<Vec<u64>>, limit: u64) -> Vec<(u64, u64)> {
		let mut spans = Vec::new();
		for (variant, starts) in self.variants.iter().zip(candidates) {
			let mut cursors = vec![0; variant.segments.len()];
			spans.extend(
				starts
					.into_iter()
					.filter(|&start| {
						variant
							.segments
							.iter()
							.zip(&mut cursors)
							.all(|(segment, cursor)| {
								let step = &self.steps[segment.step];
								step.covers(start + segment.offset, segment.length, limit, cursor)
							})
					})
					.map(|start| (start, start + variant.width)),
			);
		}
		// variants of equal width may match the same span
		if self.variants.len() > 1 {
			spans.sort_unstable();
			spans.dedup();
		}
		spans
	}

	// Where matches ending by `limit` may start, from the candidates. With
	// anchors, the candidates come from the rarest of them and must suit
	// every one.
	fn seeds(&self, candidates: Vec<Vec<u64>>, limit: u64) -> Vec<u64> {
		// a single source's candidates are in order already
		let mut positions = match <[Vec<u64>; 1]>::try_from(candidates) {
			Ok([positions]) => positions,
			Err(candidates) => {
				let mut positions = candidates.concat();
				positions.sort_unstable();
				positions
			}
		};
		positions.dedup();
		// a lone anchor is the first step, whose starts are the seeds already
		if self.anchors.len() < 2 {
			return positions;
		}
		let mut cursors = vec![0; self.anchors.len()];
		positions.retain(|&start| {
			self.anchors
				.iter()
				.zip(&mut cursors)
				.all(|(&(step, offset), cursor)| {
					self.steps[step].can_start_at(start + offset, limit, cursor)
				})
		});
		positions
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::budget::{Ledger, Limits};
	use crate::QueryError;

	fn runs(positions: &[u32]) -> Vec<Run> {
		runs_from_bitmap(&positions.iter().copied().collect())
	}

	fn token_step(positions: &[u32], min: u32, max: u32) -> Step<'static> {
		Step {
			matcher: Matcher::Runs(runs(positions)),
			min,
			max,
			label: None,
			declares: Vec::new(),
		}
	}

	fn bitmap_step(positions: &[u32], min: u32, max: u32) -> Step<'static> {
		Step {
			matcher: Matcher::Bitmap(Cow::Owned(positions.iter().copied().collect())),
			..token_step(&[], min, max)
		}
	}

	fn labeled(positions: &[u32], min: u32, max: u32, label: LabelId) -> Step<'static> {
		Step {
			label: Some(label),
			declares: vec![label],
			..token_step(positions, min, max)
		}
	}

	fn unlabeled(steps: Vec<Step<'static>>) -> Sequence<'static> {
		Sequence::new(steps, 0, &[], Bindings::Canonical)
	}

	fn unlimited() -> Budget {
		Budget::new(Limits::unlimited())
	}

	fn spans(sequence: &Sequence, end: u64) -> Vec<(u64, u64)> {
		sequence
			.matches_in::<()>(
				Span::new(0, end),
				&|_, _| true,
				&|_, _| true,
				&unlimited(),
				Output::Final,
			)
			.unwrap()
			.into_iter()
			.map(|found| (found.start, found.end))
			.collect()
	}

	fn assignments(sequence: &Sequence, end: u64, holds: Holds) -> Vec<Match> {
		sequence
			.matches_in::<Slots>(
				Span::new(0, end),
				holds,
				&|_, _| true,
				&unlimited(),
				Output::Final,
			)
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
	fn gallop_finds_the_end_of_the_prefix_from_any_start() {
		let items: Vec<u32> = (0..37).collect();
		for from in 0..=items.len() {
			for threshold in 0..40 {
				let expected = (threshold as usize).clamp(from, items.len());
				assert_eq!(gallop(&items, from, |&item| item < threshold), expected);
			}
		}
		assert_eq!(gallop(&[] as &[u32], 0, |_| true), 0);
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
		let sequence = unlabeled(vec![
			token_step(&[1], 0, 1),
			token_step(&[2, 4, 5, 6], 1, 1),
		]);
		assert_eq!(
			spans(&sequence, 10),
			vec![(1, 3), (2, 3), (4, 5), (5, 6), (6, 7)]
		);
	}

	fn seed_starts(sequence: &Sequence, end: u64) -> Vec<u64> {
		sequence
			.start_windows(Span::new(0, end))
			.flatten()
			.map(|(start, _)| start)
			.collect()
	}

	#[test]
	fn a_mandatory_step_that_matches_nowhere_leaves_no_seeds() {
		let sequence = unlabeled(vec![token_step(&[0, 1, 2], 1, 1), token_step(&[], 1, 1)]);
		assert!(seed_starts(&sequence, 10).is_empty());
		let optional = unlabeled(vec![token_step(&[0, 1, 2], 1, 1), token_step(&[], 0, 1)]);
		assert_eq!(spans(&optional, 10), vec![(0, 1), (1, 2), (2, 3)]);
	}

	#[test]
	fn seeds_come_from_the_rarest_anchored_step() {
		let every = (0..10).collect::<Vec<u32>>();
		let steps = vec![
			labeled(&every, 1, 1, 0),
			token_step(&every, 2, 2),
			token_step(&[4, 8], 1, 1),
		];
		let sequence = Sequence::new(steps, 1, &[], Bindings::Canonical);
		assert!(sequence.variants.is_empty());
		assert_eq!(seed_starts(&sequence, 10), vec![1, 5]);
		assert_eq!(spans(&sequence, 10), vec![(1, 5), (5, 9)]);
	}

	#[test]
	fn an_anchor_after_a_variable_step_is_not_used() {
		let steps = vec![labeled(&[0, 1, 2, 3], 1, 2, 0), token_step(&[3], 1, 1)];
		let sequence = Sequence::new(steps, 1, &[], Bindings::Canonical);
		assert!(sequence.variants.is_empty());
		assert_eq!(seed_starts(&sequence, 10), vec![0, 1, 2, 3]);
		assert_eq!(spans(&sequence, 10), vec![(1, 4), (2, 4)]);
	}

	#[test]
	fn fixed_width_matches_agree_with_labeled_extension() {
		let positions = [0, 1, 2, 4, 5, 7, 8, 9];
		let fixed = unlabeled(vec![
			token_step(&positions, 1, 1),
			token_step(&positions, 2, 2),
		]);
		let extended = Sequence::new(
			vec![labeled(&positions, 1, 1, 0), token_step(&positions, 2, 2)],
			1,
			&[],
			Bindings::Canonical,
		);
		assert!(!fixed.variants.is_empty());
		assert!(extended.variants.is_empty());
		let expected: Vec<(u64, u64)> = assignments(&extended, 10, &|_, _| true)
			.into_iter()
			.map(|found| (found.start, found.end))
			.collect();
		assert_eq!(spans(&fixed, 10), expected);
		assert_eq!(expected, vec![(0, 3), (7, 10)]);
	}

	fn distinct_spans(matches: Vec<Match>) -> Vec<(u64, u64)> {
		let mut spans: Vec<(u64, u64)> = matches
			.iter()
			.map(|found| (found.start, found.end))
			.collect();
		spans.dedup();
		spans
	}

	#[test]
	fn bitmaps_runs_and_variants_find_the_same_spans() {
		let first = [0, 1, 2, 4, 5, 7, 8, 9, 12];
		let second = [1, 2, 3, 5, 6, 9, 10, 11, 12, 13];
		let shapes: [(u32, u32, u32, u32); 6] = [
			(1, 1, 1, 1),
			(0, 1, 1, 1),
			(1, 3, 1, 1),
			(0, 2, 0, 2),
			(2, 2, 1, 2),
			(1, 1, 0, 3),
		];
		for (first_min, first_max, second_min, second_max) in shapes {
			let by_runs = Sequence::new(
				vec![
					labeled(&first, first_min, first_max, 0),
					token_step(&second, second_min, second_max),
				],
				1,
				&[],
				Bindings::Canonical,
			);
			let by_bitmaps = Sequence::new(
				vec![
					Step {
						label: Some(0),
						declares: vec![0],
						..bitmap_step(&first, first_min, first_max)
					},
					bitmap_step(&second, second_min, second_max),
				],
				1,
				&[],
				Bindings::Canonical,
			);
			let by_variants = unlabeled(vec![
				bitmap_step(&first, first_min, first_max),
				bitmap_step(&second, second_min, second_max),
			]);
			assert!(!by_variants.variants.is_empty());
			let expected = distinct_spans(assignments(&by_runs, 14, &|_, _| true));
			assert_eq!(
				distinct_spans(assignments(&by_bitmaps, 14, &|_, _| true)),
				expected
			);
			assert_eq!(spans(&by_variants, 14), expected);
			assert_eq!(
				spans(&by_variants, 11),
				distinct_spans(assignments(&by_runs, 11, &|_, _| true))
			);
		}
	}

	#[test]
	fn variants_of_equal_width_report_a_span_once() {
		let sequence = unlabeled(vec![bitmap_step(&[3], 0, 1), bitmap_step(&[3], 0, 1)]);
		assert_eq!(sequence.variants.len(), 3);
		assert_eq!(spans(&sequence, 10), vec![(3, 4)]);
	}

	#[test]
	fn many_repetition_counts_are_not_split() {
		let sequence = unlabeled(vec![bitmap_step(&[1], 0, 5), bitmap_step(&[2], 0, 5)]);
		assert!(sequence.variants.is_empty());
		assert_eq!(spans(&sequence, 10), vec![(1, 2), (1, 3), (2, 3)]);
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

	fn every_position_to(end: u64) -> Vec<u32> {
		(0..end as u32).collect()
	}

	#[test]
	fn seeded_matches_cross_start_windows() {
		let window = WINDOW_STARTS as u64;
		let length = 2 * window + 10;
		let steps = vec![labeled(&every_position_to(length), 1, 3, 0)];
		let sequence = Sequence::new(steps, 1, &[], Bindings::Canonical);
		assert!(sequence.variants.is_empty());
		let found = distinct_spans(assignments(&sequence, length, &|_, _| true));
		assert_eq!(found.len() as u64, 3 * length - 3);
		assert!(found.windows(2).all(|pair| pair[0] < pair[1]));
		assert!(found.contains(&(window - 1, window + 2)));
	}

	#[test]
	fn anchored_seeds_cross_start_windows() {
		let length = 3 * WINDOW_STARTS as u64;
		let every = every_position_to(length);
		let even: Vec<u32> = every.iter().copied().step_by(2).collect();
		let steps = vec![
			labeled(&every, 1, 1, 0),
			token_step(&every, 2, 2),
			token_step(&even, 1, 1),
		];
		let sequence = Sequence::new(steps, 1, &[], Bindings::Canonical);
		assert_eq!(sequence.sources, vec![(2, 3)]);
		let expected: Vec<(u64, u64)> = (0..=length - 4)
			.filter(|start| (start + 3) % 2 == 0)
			.map(|start| (start, start + 4))
			.collect();
		assert_eq!(
			distinct_spans(assignments(&sequence, length, &|_, _| true)),
			expected
		);
	}

	#[test]
	fn variant_matches_cross_start_windows() {
		let length = 3 * WINDOW_STARTS as u64 + 10;
		let even: Vec<u32> = every_position_to(length).into_iter().step_by(2).collect();
		let sequence = unlabeled(vec![
			bitmap_step(&every_position_to(length), 0, 1),
			bitmap_step(&even, 1, 1),
		]);
		assert!(sequence.sources.contains(&(1, 1)));
		let ends = even.iter().map(|&position| u64::from(position) + 1);
		let alone = ends.clone().map(|end| (end - 1, end));
		let preceded = ends.filter(|&end| end >= 2).map(|end| (end - 2, end));
		let mut expected: Vec<(u64, u64)> = alone.chain(preceded).collect();
		expected.sort_unstable();
		assert_eq!(spans(&sequence, length), expected);
	}

	#[test]
	fn all_bindings_keeps_every_capture_assignment() {
		let gap = || token_step(&[0, 1, 2, 3], 0, 2);
		let steps = || vec![gap(), labeled(&[1, 2], 1, 1, 0), gap()];
		let over_whole_range = |found: &&Match| found.start == 0 && found.end == 4;

		let all = assignments(
			&Sequence::new(steps(), 1, &[], Bindings::All),
			4,
			&|_, _| true,
		);
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
		let found = assignments(
			&Sequence::new(steps, 2, &[], Bindings::Canonical),
			2,
			&|_, _| true,
		);
		let whole: Vec<&Match> = found
			.iter()
			.filter(|found| found.end - found.start == 2)
			.collect();
		assert_eq!(whole.len(), 1);
		assert_eq!(whole[0].captures, vec![(0, 1), (1, 2)]);
	}

	// gap a:[] gap b:[] over five tokens, with one constraint reading both labels
	fn floating_pair(bindings: Bindings) -> Sequence<'static> {
		let everywhere = [0, 1, 2, 3, 4];
		let gap = || token_step(&everywhere, 0, 3);
		let steps = vec![
			gap(),
			labeled(&everywhere, 1, 1, 0),
			gap(),
			labeled(&everywhere, 1, 1, 1),
		];
		Sequence::new(steps, 2, &[vec![0, 1]], bindings)
	}

	#[test]
	fn a_pending_constraint_keeps_the_assignments_it_must_choose_between() {
		// accept only a at position 2 together with b at position 4
		let holds = |_: usize, slots: &[Slot]| slots[0] == (2, 3) && slots[1] == (4, 5);
		let found = assignments(&floating_pair(Bindings::Canonical), 5, &holds);
		let reported: Vec<(u64, u64)> =
			found.iter().map(|found| (found.start, found.end)).collect();
		assert_eq!(reported, vec![(0, 5), (1, 5), (2, 5)]);
		assert!(found
			.iter()
			.all(|found| found.captures == vec![(2, 3), (4, 5)]));
	}

	#[test]
	fn a_skipped_label_that_a_constraint_reads_ends_the_partial_match() {
		// a:[]? b:[] with a constraint over both: only the path that binds a survives
		let everywhere = [0, 1, 2];
		let steps = vec![labeled(&everywhere, 0, 1, 0), labeled(&everywhere, 1, 1, 1)];
		let sequence = Sequence::new(steps, 2, &[vec![0, 1]], Bindings::Canonical);
		let found = assignments(&sequence, 3, &|_, _| true);
		let reported: Vec<(u64, u64)> =
			found.iter().map(|found| (found.start, found.end)).collect();
		assert_eq!(reported, vec![(0, 2), (1, 3)]);
	}

	#[test]
	fn budget_is_returned_once_results_are_final() {
		let budget = unlimited();
		floating_pair(Bindings::Canonical)
			.matches_in::<Slots>(
				Span::new(0, 5),
				&|_, _| true,
				&|_, _| true,
				&budget,
				Output::Final,
			)
			.unwrap();
		assert_eq!(budget.held(), 0);
	}

	#[test]
	fn buffered_matches_stay_charged_until_their_ledger_is_dropped() {
		let budget = unlimited();
		let ledger = Ledger::new(&budget);
		let found = floating_pair(Bindings::All)
			.matches_in::<Slots>(
				Span::new(0, 5),
				&|_, _| true,
				&|_, _| true,
				&budget,
				Output::Buffered(&ledger),
			)
			.unwrap();
		assert_eq!(budget.held(), found.len() as u64);
		drop(ledger);
		assert_eq!(budget.held(), 0);
	}

	// every span of 1 to 100 tokens over 5,000 positions: about 495,000 matches
	fn wide_spans() -> Sequence<'static> {
		let positions: Vec<u32> = (0..5000).collect();
		unlabeled(vec![token_step(&positions, 1, 100)])
	}

	#[test]
	fn a_buffer_is_refused_while_it_fills_not_after() {
		let limit = 60_000;
		let budget = Budget::new(Limits {
			max_intermediate_matches: limit,
			..Limits::unlimited()
		});
		let ledger = Ledger::new(&budget);
		let outcome = wide_spans().matches_in::<()>(
			Span::new(0, 5000),
			&|_, _| true,
			&|_, _| true,
			&budget,
			Output::Buffered(&ledger),
		);
		assert!(matches!(outcome, Err(QueryError::LimitExceeded { .. })));
		// stopped within one batch of the limit, far short of the whole buffer
		assert!(budget.held() < limit + (SEED_BATCH as u64 + 1) * 100);
	}

	#[test]
	fn counting_does_not_build_the_matches() {
		let budget = Budget::new(Limits {
			max_intermediate_matches: 60_000,
			..Limits::unlimited()
		});
		let expected: usize = (1..=100).map(|length| 5000 - length + 1).sum();
		let counted = wide_spans()
			.count_in::<()>(Span::new(0, 5000), &|_, _| true, &|_, _| true, &budget)
			.unwrap();
		assert_eq!(counted, expected);
		assert_eq!(budget.held(), 0);
	}

	#[test]
	fn results_are_counted_after_filtering() {
		// 495,050 spans, of which only those inside [0, 50) are kept: 3,725
		let budget = Budget::new(Limits {
			max_results: 4_000,
			..Limits::unlimited()
		});
		let inside = |start: u64, end: u64| start < 50 && end <= 50;
		let found = wide_spans()
			.matches_in::<()>(
				Span::new(0, 5000),
				&|_, _| true,
				&inside,
				&budget,
				Output::Final,
			)
			.unwrap();
		assert_eq!(
			found.len(),
			(1..=50).map(|length| 50 - length + 1).sum::<usize>()
		);

		let tight = Budget::new(Limits {
			max_results: 1_000,
			..Limits::unlimited()
		});
		let outcome = wide_spans().matches_in::<()>(
			Span::new(0, 5000),
			&|_, _| true,
			&inside,
			&tight,
			Output::Final,
		);
		assert!(matches!(
			outcome,
			Err(QueryError::LimitExceeded {
				resource: "results",
				limit: 1_000
			})
		));
	}

	#[test]
	fn repeated_group_enumeration_is_charged_and_returned() {
		// a group matching one or two tokens anywhere, repeated up to 100 times
		let table: Vec<Match> = (0..200)
			.flat_map(|start| {
				[
					Match::unlabeled(start, start + 1, 0),
					Match::unlabeled(start, start + 2, 0),
				]
			})
			.filter(|found| found.end <= 200)
			.collect();
		let group = || Step {
			matcher: Matcher::Spans(table.clone()),
			min: 1,
			max: MAX_QUANTIFIER,
			label: None,
			declares: Vec::new(),
		};
		let budget = unlimited();
		let found = unlabeled(vec![group()])
			.matches_in::<()>(
				Span::new(0, 200),
				&|_, _| true,
				&|_, _| true,
				&budget,
				Output::Final,
			)
			.unwrap();
		assert!(!found.is_empty());
		assert_eq!(budget.held(), 0);

		let tight = Budget::new(Limits {
			max_intermediate_matches: 300,
			..Limits::unlimited()
		});
		let outcome = unlabeled(vec![group()]).matches_in::<()>(
			Span::new(0, 200),
			&|_, _| true,
			&|_, _| true,
			&tight,
			Output::Final,
		);
		assert!(matches!(outcome, Err(QueryError::LimitExceeded { .. })));
	}

	#[test]
	fn exceeding_the_budget_is_an_error() {
		let run = |limits: Limits| {
			floating_pair(Bindings::Canonical).matches_in::<Slots>(
				Span::new(0, 5),
				&|_, _| true,
				&|_, _| true,
				&Budget::new(limits),
				Output::Final,
			)
		};
		assert!(matches!(
			run(Limits {
				max_intermediate_matches: 8,
				..Limits::unlimited()
			}),
			Err(QueryError::LimitExceeded {
				resource: "intermediate matches",
				limit: 8
			})
		));
		assert!(matches!(
			run(Limits {
				max_expansions: 10,
				..Limits::unlimited()
			}),
			Err(QueryError::LimitExceeded {
				resource: "expansions",
				limit: 10
			})
		));
	}
}
