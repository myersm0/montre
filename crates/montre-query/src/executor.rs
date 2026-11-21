use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use montre_core::{Span, UnitId, span_containing};
use montre_index::{ComponentMeta, Corpus, ForwardIndex, InvertedIndex, SpanIndex};
use rayon::prelude::*;
use roaring::RoaringBitmap;

use crate::ast::{CmpOp, GlobalConstraint, LabelAttr};
use crate::budget::{Budget, Ledger, Limits, Output};
use crate::sequence::CHARGE_STRIDE;
use crate::planner::{PlanNode, QueryPlan, SequenceStep};
use crate::sequence::{
	Bindings, Keep, LabelId, Match, Matcher, Run, Sequence, Slot, Slots, Step,
	MAX_QUANTIFIER, UNBOUND, runs_from_bitmap,
};
use crate::validate::validate;
use crate::{QueryError, Result};

#[derive(Debug, Clone)]
pub struct Hit {
	pub span: Span,
	pub document_index: u32,
	pub sentence_index: u32,
	pub captures: Vec<(String, Span)>,
}

impl Hit {
	pub const UNPOPULATED: u32 = u32::MAX;

	fn new(span: Span) -> Self {
		Self {
			span,
			document_index: Self::UNPOPULATED,
			sentence_index: Self::UNPOPULATED,
			captures: Vec::new(),
		}
	}
}

pub struct Results {
	hits: Vec<Hit>,
}

impl Results {
	pub fn new(hits: Vec<Hit>) -> Self {
		Self { hits }
	}

	pub fn empty() -> Self {
		Self::new(Vec::new())
	}

	pub fn len(&self) -> usize {
		self.hits.len()
	}

	pub fn is_empty(&self) -> bool {
		self.hits.is_empty()
	}

	pub fn hits(&self) -> &[Hit] {
		&self.hits
	}

	pub fn into_hits(self) -> Vec<Hit> {
		self.hits
	}

	/// Populate document_index and sentence_index for all hits.
	/// Call this only when you need structural context (e.g., alignment projection).
	/// Most display operations don't need this.
	pub fn populate_context(&mut self, corpus: &Corpus) {
		let doc_spans = corpus.spans().spans("document");
		let sent_spans = corpus.spans().spans("sentence");

		for hit in &mut self.hits {
			if let Some(spans) = doc_spans {
				hit.document_index = span_containing(spans, hit.span.start)
					.map(|i| i as u32)
					.unwrap_or(Hit::UNPOPULATED);
			}
			if let Some(spans) = sent_spans {
				hit.sentence_index = span_containing(spans, hit.span.start)
					.map(|i| i as u32)
					.unwrap_or(Hit::UNPOPULATED);
			}
		}
	}
}

impl IntoIterator for Results {
	type Item = Hit;
	type IntoIter = std::vec::IntoIter<Hit>;

	fn into_iter(self) -> Self::IntoIter {
		self.hits.into_iter()
	}
}

impl<'a> IntoIterator for &'a Results {
	type Item = &'a Hit;
	type IntoIter = std::slice::Iter<'a, Hit>;

	fn into_iter(self) -> Self::IntoIter {
		self.hits.iter()
	}
}

/// Hits come back sorted by (start, end), one per distinct span. A match never
/// extends past the document it starts in.
pub fn execute(plan: &QueryPlan, corpus: &Corpus) -> Result<Results> {
	execute_with(plan, corpus, &Limits::default())
}

pub fn execute_with(plan: &QueryPlan, corpus: &Corpus, limits: &Limits) -> Result<Results> {
	validate(&plan.root, corpus)?;
	let context = Context::new(&plan.root, corpus, limits);
	if plan.root.is_token_level() {
		let bitmap = context.token_bitmap(&plan.root)?;
		context.budget.report(bitmap.len() as usize)?;
		let hits = bitmap
			.iter()
			.map(u64::from)
			.map(|position| Hit::new(Span::new(position, position + 1)))
			.collect();
		return Ok(Results::new(hits));
	}
	let hits = context
		.results(&plan.root)?
		.into_iter()
		.map(|found| context.hit(found))
		.collect();
	Ok(Results::new(hits))
}

pub fn execute_count(plan: &QueryPlan, corpus: &Corpus) -> Result<usize> {
	execute_count_with(plan, corpus, &Limits::default())
}

pub fn execute_count_with(plan: &QueryPlan, corpus: &Corpus, limits: &Limits) -> Result<usize> {
	validate(&plan.root, corpus)?;
	Context::new(&plan.root, corpus, limits).count(&plan.root)
}

struct Context<'a> {
	corpus: &'a Corpus,
	labels: Vec<String>,
	budget: Budget,
}

impl<'a> Context<'a> {
	fn new(root: &PlanNode, corpus: &'a Corpus, limits: &Limits) -> Self {
		let mut labels = Vec::new();
		root.collect_labels(&mut labels);
		Self {
			corpus,
			labels,
			budget: Budget::new(*limits),
		}
	}

	fn hit(&self, found: Match) -> Hit {
		let mut hit = Hit::new(Span::new(found.start, found.end));
		hit.captures = found
			.captures
			.into_iter()
			.enumerate()
			.filter(|(_, slot)| *slot != UNBOUND)
			.map(|(label, (start, end))| (self.labels[label].clone(), Span::new(start, end)))
			.collect();
		hit
	}

	fn label_id(&self, name: &str) -> Option<LabelId> {
		self.labels.iter().position(|label| label == name)
	}

	// The hits of the query. A sequence, under any span, component and
	// document filters and a constraint it can apply as it runs, produces them
	// batch by batch; so does a token-level pattern. Any other shape is built as
	// an intermediate buffer and then reported whole.
	fn results(&self, root: &PlanNode) -> Result<Vec<Match>> {
		let (core, checks, filters) = self.peel(root);
		let keep = |start: u64, end: u64| self.accepts(&filters, start, end);

		if core.is_token_level() {
			let slot_count = self.labels.len();
			let bitmap = self.token_bitmap(core)?;
			let mut found = Vec::with_capacity(bitmap.len() as usize);
			for position in bitmap.iter().map(u64::from) {
				if keep(position, position + 1) {
					found.push(Match::unlabeled(position, position + 1, slot_count));
					if found.len() % CHARGE_STRIDE == 0 {
						self.budget.report(CHARGE_STRIDE)?;
					}
				}
			}
			self.budget.report(found.len() % CHARGE_STRIDE)?;
			return Ok(found);
		}

		if let PlanNode::SequenceScan { steps } = core {
			return self.sequence_matches(steps, Bindings::Canonical, &checks, Output::Final, &keep);
		}

		let built = Ledger::new(&self.budget);
		let mut found = self.matches(core, Bindings::Canonical, &[], &built)?;
		found.retain(|found| keep(found.start, found.end));
		self.budget.report(found.len())?;
		Ok(found)
	}

	fn count(&self, root: &PlanNode) -> Result<usize> {
		let inverted = self.corpus.inverted();
		let token_count = self.corpus.token_count() as usize;
		match root {
			PlanNode::ScanLiteral { layer, value } => {
				return Ok(inverted.get(layer, value).map_or(0, |bitmap| bitmap.len() as usize));
			}
			PlanNode::ScanAll => return Ok(token_count),
			PlanNode::Difference { base, subtract } => {
				if let (PlanNode::ScanAll, PlanNode::ScanLiteral { layer, value }) =
					(base.as_ref(), subtract.as_ref())
				{
					let subtracted = inverted.get(layer, value).map_or(0, |bitmap| bitmap.len());
					return Ok(token_count - subtracted as usize);
				}
			}
			_ => {}
		}

		let (core, checks, filters) = self.peel(root);
		let keep = |start: u64, end: u64| self.accepts(&filters, start, end);

		if core.is_token_level() {
			let bitmap = self.token_bitmap(core)?;
			if filters.is_empty() {
				return Ok(bitmap.len() as usize);
			}
			return Ok(bitmap
				.iter()
				.map(u64::from)
				.filter(|&position| keep(position, position + 1))
				.count());
		}

		if let PlanNode::SequenceScan { steps } = core {
			let prepared = self.prepare(steps, Bindings::Canonical, &checks)?;
			let sequence = &prepared.sequence;
			let holds = |check: usize, slots: &[Slot]| self.holds(&checks[check], slots);
			// without a constraint to decide, which captures a span has is beside the point
			let count_in = |range: &Span| match checks.is_empty() {
				true => sequence.count_in::<()>(*range, &holds, &keep, &self.budget),
				false => sequence.count_in::<Slots>(*range, &holds, &keep, &self.budget),
			};
			let ranges = self.ranges();
			return match runs_in_parallel(steps, &ranges) {
				true => ranges.par_iter().map(count_in).sum(),
				false => ranges.iter().map(count_in).sum(),
			};
		}

		let built = Ledger::new(&self.budget);
		let found = self.matches(core, Bindings::Canonical, &[], &built)?;
		Ok(found.iter().filter(|found| keep(found.start, found.end)).count())
	}

	// Split off the span, component and document filters above `root`, and a
	// constraint that its sequence can apply as it runs. What remains is the
	// node that produces the matches those filters and checks then act on.
	fn peel<'n>(&self, root: &'n PlanNode) -> (&'n PlanNode, Vec<Check>, Vec<Filter<'a>>) {
		let mut core = root;
		let mut checks = Vec::new();
		let mut filters = Vec::new();
		loop {
			match core {
				PlanNode::FilterBySpan { inner, span_layer } => {
					let spans = self.corpus.spans().spans(span_layer).unwrap_or(&[]);
					filters.push(Filter::Within(spans));
					core = inner;
				}
				PlanNode::FilterByComponent { inner, components } => {
					let document_count = self.documents().len();
					let mut allowed = vec![false; document_count];
					for component in components.iter().filter_map(|name| self.corpus.component(name)) {
						let (first, end) = component.document_range;
						allowed[first.min(document_count)..end.min(document_count)].fill(true);
					}
					filters.push(Filter::Documents(allowed));
					core = inner;
				}
				PlanNode::FilterByDocument { inner, documents } => {
					let mut allowed = vec![false; self.documents().len()];
					for index in self.corpus.document_indices_by_name(documents) {
						if let Some(slot) = allowed.get_mut(index) {
							*slot = true;
						}
					}
					filters.push(Filter::Documents(allowed));
					core = inner;
				}
				PlanNode::GlobalConstraintFilter { inner, constraints } if applies_checks(inner) => {
					checks = self.compile(constraints);
					core = inner;
				}
				_ => return (core, checks, filters),
			}
		}
	}

	fn accepts(&self, filters: &[Filter], start: u64, end: u64) -> bool {
		filters.iter().all(|filter| match filter {
			Filter::Within(spans) => fits_within_one(spans, start, end).is_some(),
			Filter::Documents(allowed) => fits_within_one(self.documents(), start, end)
				.is_some_and(|document| allowed[document]),
		})
	}

	fn documents(&self) -> &'a [Span] {
		self.corpus.spans().spans("document").unwrap_or(&[])
	}

	/// Matches sorted by (start, end, captures), built as an intermediate
	/// buffer and charged to `ledger` as they are produced. `checks` are
	/// constraints the node applies while it runs; only a node for which
	/// `applies_checks` holds is handed any.
	fn matches(
		&self,
		node: &PlanNode,
		bindings: Bindings,
		checks: &[Check],
		ledger: &Ledger,
	) -> Result<Vec<Match>> {
		if node.is_token_level() {
			let slot_count = self.labels.len();
			let bitmap = self.token_bitmap(node)?;
			ledger.hold(self.cost(bitmap.len() as usize))?;
			return Ok(bitmap
				.iter()
				.map(u64::from)
				.map(|position| Match::unlabeled(position, position + 1, slot_count))
				.collect());
		}

		match node {
			PlanNode::Union(nodes) => {
				let mut matches = Vec::new();
				for node in nodes {
					matches.extend(self.matches(node, bindings, &[], ledger)?);
				}
				matches.sort_unstable();
				Ok(deduplicated(matches, bindings))
			}

			PlanNode::SequenceScan { steps } => {
				self.sequence_matches(steps, bindings, checks, Output::Buffered(ledger), &|_, _| true)
			}

			PlanNode::FilterBySpan { .. }
			| PlanNode::FilterByComponent { .. }
			| PlanNode::FilterByDocument { .. } => {
				let (core, _, filters) = self.peel(node);
				let keep = |start: u64, end: u64| self.accepts(&filters, start, end);
				if let PlanNode::SequenceScan { steps } = core {
					return self.sequence_matches(steps, bindings, checks, Output::Buffered(ledger), &keep);
				}
				let mut matches = self.matches(core, bindings, checks, ledger)?;
				matches.retain(|found| keep(found.start, found.end));
				Ok(matches)
			}

			PlanNode::ProjectAlignment { inner, alignment } => self.projected(inner, alignment, ledger),

			PlanNode::GlobalConstraintFilter { inner, constraints } => {
				let own = self.compile(constraints);
				if applies_checks(inner) {
					return self.matches(inner, bindings, &own, ledger);
				}
				// every assignment is enumerated, charged to a ledger of its own,
				// and only the survivors are passed on
				let enumerated = Ledger::new(&self.budget);
				let mut matches = self.matches(inner, Bindings::All, &[], &enumerated)?;
				matches.retain(|found| own.iter().all(|check| self.holds(check, &found.captures)));
				let matches = deduplicated(matches, bindings);
				ledger.hold(self.cost(matches.len()))?;
				Ok(matches)
			}

			_ => Err(QueryError::Execution(
				"token-level operator applied to a multi-token pattern".into(),
			)),
		}
	}

	fn cost(&self, matches: usize) -> u64 {
		matches as u64 * (1 + self.labels.len() as u64 / 4)
	}

	fn token_bitmap(&self, node: &PlanNode) -> Result<Cow<'a, RoaringBitmap>> {
		let inverted = self.corpus.inverted();
		match node {
			PlanNode::ScanLiteral { layer, value } => Ok(inverted
				.get(layer, value)
				.map_or_else(|| Cow::Owned(RoaringBitmap::new()), Cow::Borrowed)),

			PlanNode::ScanRegex { layer, pattern } => {
				let regex = regex::Regex::new(pattern)?;
				let mut combined = RoaringBitmap::new();
				for value in inverted.values(layer).unwrap_or_default() {
					if regex.is_match(value) {
						if let Some(bitmap) = inverted.get(layer, value) {
							combined |= bitmap;
						}
					}
				}
				Ok(Cow::Owned(combined))
			}

			PlanNode::ScanAll => {
				let mut all = RoaringBitmap::new();
				all.insert_range(0..self.corpus.token_count() as u32);
				Ok(Cow::Owned(all))
			}

			PlanNode::Intersect(nodes) => {
				let mut nodes = nodes.iter();
				let Some(first) = nodes.next() else {
					return Ok(Cow::Owned(RoaringBitmap::new()));
				};
				let mut combined = self.token_bitmap(first)?.into_owned();
				for node in nodes {
					combined &= self.token_bitmap(node)?.as_ref();
				}
				Ok(Cow::Owned(combined))
			}

			PlanNode::Union(nodes) => {
				let mut combined = RoaringBitmap::new();
				for node in nodes {
					combined |= self.token_bitmap(node)?.as_ref();
				}
				Ok(Cow::Owned(combined))
			}

			PlanNode::Difference { base, subtract } => {
				let mut remaining = self.token_bitmap(base)?.into_owned();
				remaining -= self.token_bitmap(subtract)?.as_ref();
				Ok(Cow::Owned(remaining))
			}

			_ => Err(QueryError::Execution(
				"token-level operator applied to a multi-token pattern".into(),
			)),
		}
	}

	// ranges a match may not cross: documents, or the whole corpus when there are none
	fn ranges(&self) -> Cow<'a, [Span]> {
		match self.corpus.spans().spans("document") {
			Some(documents) if !documents.is_empty() => Cow::Borrowed(documents),
			_ => Cow::Owned(vec![Span::new(0, self.corpus.token_count())]),
		}
	}

	fn prepare(
		&self,
		steps: &[SequenceStep],
		bindings: Bindings,
		checks: &[Check],
	) -> Result<Prepared<'_>> {
		let mut ledgers = Vec::new();
		let mut prepared = Vec::with_capacity(steps.len());
		for step in steps {
			let matcher = if matches!(step.node, PlanNode::ScanAll) {
				Matcher::Runs(vec![Run {
					start: 0,
					end: self.corpus.token_count(),
				}])
			} else if step.node.is_token_level() {
				Matcher::Runs(runs_from_bitmap(self.token_bitmap(&step.node)?.as_ref()))
			} else {
				let ledger = Ledger::new(&self.budget);
				let table = self.group_matches(step, bindings, checks, &ledger)?;
				ledgers.push(ledger);
				Matcher::Spans(table)
			};
			prepared.push(Step {
				matcher,
				min: if step.node.is_nullable() { 0 } else { step.min },
				max: step.max.unwrap_or(MAX_QUANTIFIER).min(MAX_QUANTIFIER),
				label: step.label.as_deref().and_then(|name| self.label_id(name)),
				declares: self.declared_by(step),
			});
		}
		let constraints: Vec<Vec<LabelId>> = checks
			.iter()
			.map(|check| check.labels().collect())
			.collect();
		Ok(Prepared {
			sequence: Sequence::new(prepared, self.labels.len(), &constraints, bindings),
			_ledgers: ledgers,
		})
	}

	fn declared_by(&self, step: &SequenceStep) -> Vec<LabelId> {
		let mut names: Vec<String> = step.label.iter().cloned().collect();
		step.node.collect_labels(&mut names);
		names.iter().filter_map(|name| self.label_id(name)).collect()
	}

	// The sub-matches of a group step. A constraint whose labels all lie inside
	// an unrepeated group is applied while the group is matched; one that also
	// reads a label outside it needs every assignment the group can offer.
	fn group_matches(
		&self,
		step: &SequenceStep,
		bindings: Bindings,
		checks: &[Check],
		ledger: &Ledger,
	) -> Result<Vec<Match>> {
		// the step's own label spans the whole group and is bound outside it
		let mut names = Vec::new();
		step.node.collect_labels(&mut names);
		let inside: Vec<LabelId> = names.iter().filter_map(|name| self.label_id(name)).collect();
		let unrepeated = step.min == 1 && step.max == Some(1);
		let settled_inside = |check: &Check| {
			unrepeated
				&& applies_checks(&step.node)
				&& check.labels().all(|label| inside.contains(&label))
		};
		let reads_inside = |check: &Check| check.labels().any(|label| inside.contains(&label));
		let inner_checks: Vec<Check> = checks
			.iter()
			.filter(|check| settled_inside(check))
			.cloned()
			.collect();
		let contested = checks
			.iter()
			.any(|check| reads_inside(check) && !settled_inside(check));
		let group_bindings = match contested {
			true => Bindings::All,
			false => bindings,
		};
		self.matches(&step.node, group_bindings, &inner_checks, ledger)
	}

	fn sequence_matches(
		&self,
		steps: &[SequenceStep],
		bindings: Bindings,
		checks: &[Check],
		output: Output,
		keep: Keep,
	) -> Result<Vec<Match>> {
		let prepared = self.prepare(steps, bindings, checks)?;
		let sequence = &prepared.sequence;
		let ranges = self.ranges();
		let holds = |check: usize, slots: &[Slot]| self.holds(&checks[check], slots);
		let matches_in = |range: &Span| {
			if self.labels.is_empty() {
				sequence.matches_in::<()>(*range, &holds, keep, &self.budget, output)
			} else {
				sequence.matches_in::<Slots>(*range, &holds, keep, &self.budget, output)
			}
		};
		let per_range: Vec<Vec<Match>> = if runs_in_parallel(steps, &ranges) {
			ranges.par_iter().map(matches_in).collect::<Result<_>>()?
		} else {
			ranges.iter().map(matches_in).collect::<Result<_>>()?
		};
		Ok(per_range.into_iter().flatten().collect())
	}

	// targets are charged to `ledger` as they are found
	fn projected(&self, inner: &PlanNode, alignment: &str, ledger: &Ledger) -> Result<Vec<Match>> {
		let missing = |what: &str, name: &str| QueryError::Execution(format!("{what} not found: {name}"));
		let corpus = self.corpus;
		let edges = corpus
			.alignment_edges(alignment)
			.ok_or_else(|| missing("Alignment", alignment))?;
		let meta = corpus
			.alignment_meta(alignment)
			.ok_or_else(|| missing("Alignment metadata", alignment))?;
		let source_component = corpus
			.component(&meta.source_component)
			.ok_or_else(|| missing("Source component", &meta.source_component))?;
		let target_component = corpus
			.component(&meta.target_component)
			.ok_or_else(|| missing("Target component", &meta.target_component))?;

		let (Some(documents), Some(source_units), Some(target_units)) = (
			corpus.spans().spans("document"),
			corpus.spans().spans(&meta.source_layer),
			corpus.spans().spans(&meta.target_layer),
		) else {
			return Ok(Vec::new());
		};

		let edge_map = build_edge_map(edges);
		let mut seen_targets = HashSet::new();
		let mut projected = Vec::new();

		let source = Ledger::new(&self.budget);
		let mut charged = 0;
		for found in self.matches(inner, Bindings::Canonical, &[], &source)? {
			let span = Span::new(found.start, found.end);
			let Some(source) = find_doc_and_sent(span, documents, source_units, source_component)
			else {
				continue;
			};
			for &(target_document, target_unit) in edge_map.get(&source).into_iter().flatten() {
				if !seen_targets.insert((target_document, target_unit)) {
					continue;
				}
				if let Some(target) = resolve_target_span(
					target_document,
					target_unit,
					documents,
					target_units,
					target_component,
				) {
					projected.push(Match::unlabeled(target.start, target.end, self.labels.len()));
				}
				if projected.len() - charged >= CHARGE_STRIDE {
					ledger.hold(self.cost(projected.len() - charged))?;
					charged = projected.len();
				}
			}
		}
		ledger.hold(self.cost(projected.len() - charged))?;

		projected.sort_unstable();
		Ok(projected)
	}

	fn compile(&self, constraints: &[GlobalConstraint]) -> Vec<Check> {
		let attribute = |side: &LabelAttr| -> (Option<LabelId>, String) {
			(self.label_id(&side.label), side.attr.clone())
		};
		constraints
			.iter()
			.map(|constraint| match constraint {
				GlobalConstraint::Eq { left, right } => Check::Compare {
					left: attribute(left),
					right: attribute(right),
					equal: true,
				},
				GlobalConstraint::Ne { left, right } => Check::Compare {
					left: attribute(left),
					right: attribute(right),
					equal: false,
				},
				GlobalConstraint::Distance { left, right, op, value } => Check::Distance {
					left: self.label_id(left),
					right: self.label_id(right),
					op: *op,
					value: u64::from(*value),
				},
			})
			.collect()
	}

	// a constraint that mentions an unbound label does not hold
	fn holds(&self, check: &Check, slots: &[Slot]) -> bool {
		let bound = |label: &Option<LabelId>| {
			label
				.and_then(|label| slots.get(label))
				.filter(|slot| **slot != UNBOUND)
		};
		let value = |(label, attribute): &(Option<LabelId>, String)| {
			bound(label).and_then(|slot| self.corpus.forward().get_str(slot.0, attribute))
		};
		match check {
			Check::Compare { left, right, equal } => match (value(left), value(right)) {
				(Some(left), Some(right)) => (left == right) == *equal,
				_ => false,
			},
			Check::Distance { left, right, op, value } => match (bound(left), bound(right)) {
				(Some(left), Some(right)) => compare(right.0.saturating_sub(left.1), *op, *value),
				_ => false,
			},
		}
	}
}

enum Filter<'a> {
	// a match must lie inside one of these spans
	Within(&'a [Span]),
	// a match must lie inside a document marked true, by document index
	Documents(Vec<bool>),
}

// a sequence ready to run, with the charges for its group tables
struct Prepared<'b> {
	sequence: Sequence,
	_ledgers: Vec<Ledger<'b>>,
}

#[derive(Clone)]
enum Check {
	Compare {
		left: (Option<LabelId>, String),
		right: (Option<LabelId>, String),
		equal: bool,
	},
	Distance {
		left: Option<LabelId>,
		right: Option<LabelId>,
		op: CmpOp,
		value: u64,
	},
}

impl Check {
	fn labels(&self) -> impl Iterator<Item = LabelId> {
		let (left, right) = match self {
			Check::Compare { left, right, .. } => (left.0, right.0),
			Check::Distance { left, right, .. } => (*left, *right),
		};
		[left, right].into_iter().flatten()
	}
}

// a sequence, possibly beneath span, component or document filters, applies
// constraints while it runs
fn applies_checks(node: &PlanNode) -> bool {
	match node {
		PlanNode::SequenceScan { .. } => true,
		PlanNode::FilterBySpan { inner, .. }
		| PlanNode::FilterByComponent { inner, .. }
		| PlanNode::FilterByDocument { inner, .. } => applies_checks(inner),
		_ => false,
	}
}

fn compare(value: u64, op: CmpOp, target: u64) -> bool {
	match op {
		CmpOp::Ge => value >= target,
		CmpOp::Gt => value > target,
		CmpOp::Le => value <= target,
		CmpOp::Lt => value < target,
		CmpOp::Eq => value == target,
		CmpOp::Ne => value != target,
	}
}

// expects matches sorted by (start, end, captures)
fn deduplicated(mut matches: Vec<Match>, bindings: Bindings) -> Vec<Match> {
	match bindings {
		Bindings::Canonical => matches.dedup_by(|later, earlier| later.same_span(earlier)),
		Bindings::All => matches.dedup(),
	}
	matches
}

fn fits_within_one(spans: &[Span], start: u64, end: u64) -> Option<usize> {
	span_containing(spans, start).filter(|&index| end <= spans[index].end)
}

fn runs_in_parallel(steps: &[SequenceStep], ranges: &[Span]) -> bool {
	ranges.len() > 1
		&& steps.iter().any(|step| {
			step.min != 1
				|| step.max != Some(1)
				|| !step.node.is_token_level()
				|| matches!(step.node, PlanNode::ScanAll)
				|| matches!(&step.node, PlanNode::Difference { base, .. } if matches!(**base, PlanNode::ScanAll))
		})
}

pub fn build_edge_map(edges: &[(UnitId, UnitId)]) -> HashMap<(u32, u32), Vec<(u32, u32)>> {
	let mut map: HashMap<(u32, u32), Vec<(u32, u32)>> = HashMap::new();
	for &((src_doc, src_sent), (tgt_doc, tgt_sent)) in edges {
		map.entry((src_doc, src_sent))
			.or_default()
			.push((tgt_doc, tgt_sent));
	}
	map
}

pub fn find_doc_and_sent(
	span: Span,
	doc_spans: &[Span],
	sent_spans: &[Span],
	comp: &ComponentMeta,
) -> Option<(u32, u32)> {
	let doc_idx = span_containing(doc_spans, span.start)?;
	if doc_idx < comp.document_range.0 || doc_idx >= comp.document_range.1 {
		return None;
	}
	let doc_span = &doc_spans[doc_idx];
	if span.end > doc_span.end {
		return None;
	}
	let doc_within_comp = (doc_idx - comp.document_range.0) as u32;

	let first_sent = sent_spans.partition_point(|s| s.start < doc_span.start);
	let mut sent_within_doc = 0u32;
	for i in first_sent..sent_spans.len() {
		let sent_span = &sent_spans[i];
		if sent_span.start >= doc_span.end {
			break;
		}
		if span.start >= sent_span.start && span.end <= sent_span.end {
			return Some((doc_within_comp, sent_within_doc));
		}
		if sent_span.end <= doc_span.end {
			sent_within_doc += 1;
		}
	}
	None
}

pub fn resolve_target_span(
	tgt_doc: u32,
	tgt_sent: u32,
	doc_spans: &[Span],
	target_sent_spans: &[Span],
	target_comp: &ComponentMeta,
) -> Option<Span> {
	let abs_doc_idx = target_comp.document_range.0 + tgt_doc as usize;
	let doc_span = doc_spans.get(abs_doc_idx)?;

	let first_sent = target_sent_spans.partition_point(|s| s.start < doc_span.start);
	let mut sent_count = 0u32;
	for i in first_sent..target_sent_spans.len() {
		let sent_span = &target_sent_spans[i];
		if sent_span.start >= doc_span.end {
			break;
		}
		if sent_span.end <= doc_span.end {
			if sent_count == tgt_sent {
				return Some(*sent_span);
			}
			sent_count += 1;
		}
	}
	None
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn results_iterator() {
		let hits = vec![
			Hit::new(Span::new(0, 1)),
			Hit::new(Span::new(5, 6)),
		];

		let results = Results::new(hits);
		assert_eq!(results.len(), 2);

		let collected: Vec<_> = results.into_iter().collect();
		assert_eq!(collected[0].span.start, 0);
		assert_eq!(collected[1].span.start, 5);
	}
}
