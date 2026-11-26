//! Differential test: the executor against a brute-force reference matcher.
//!
//! The reference works on the parsed query and the forward index only. It
//! shares no code with the planner, the inverted index, or the executor, and
//! states the intended semantics directly:
//!
//! - a match is a non-empty span inside one document
//! - a label binds to the span its element matched; an element that matched
//!   nothing leaves its label unbound
//! - a label on a quantified element spans the whole repetition; labels
//!   inside a repeated group take the values of the last repetition that
//!   matched something
//! - a constraint holds when all its labels are bound and the comparison is
//!   true; a constrained span matches when some binding satisfies every
//!   constraint
//! - of the bindings that produce a span, the reported one is the smallest:
//!   label by label in query order, bound before unbound, then earliest
//!   start, then shortest

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::sync::atomic::{AtomicU32, Ordering};

use montre_build::builder::CorpusBuilder;
use montre_build::format::conllu::ConllUReader;
use montre_build::format::CorpusReader;
use montre_core::Span;
use montre_index::{Corpus, ForwardIndex, SpanIndex};
use montre_query::ast::{
	CmpOp, Constraint, ConstraintOp, ConstraintValue, GlobalConstraint, Query,
};
use proptest::prelude::*;
use proptest::test_runner::{contextualize_config, TestRunner};

type Binding = BTreeMap<String, (u64, u64)>;
type Outcome = (u64, Binding);

struct Reference<'a> {
	corpus: &'a Corpus,
}

impl Reference<'_> {
	fn matches(&self, query: &Query) -> BTreeMap<(u64, u64), BTreeSet<Binding>> {
		let mut found: BTreeMap<(u64, u64), BTreeSet<Binding>> = BTreeMap::new();
		for document in self.corpus.spans().spans("document").unwrap_or(&[]) {
			for start in document.start..document.end {
				for (end, binding) in self.outcomes(query, start, document.end) {
					if end > start {
						found.entry((start, end)).or_default().insert(binding);
					}
				}
			}
		}
		found
	}

	// every way `query` can match beginning at `start`, the empty match included
	fn outcomes(&self, query: &Query, start: u64, limit: u64) -> BTreeSet<Outcome> {
		match query {
			Query::Token(pattern) => {
				let matched = start < limit
					&& pattern
						.constraints
						.iter()
						.all(|constraint| self.token_satisfies(start, constraint));
				matched
					.then(|| (start + 1, Binding::new()))
					.into_iter()
					.collect()
			}

			Query::Sequence(parts) => {
				let mut reached: BTreeSet<Outcome> = BTreeSet::from([(start, Binding::new())]);
				for part in parts {
					reached = reached
						.iter()
						.flat_map(|(position, binding)| {
							self.outcomes(part, *position, limit)
								.into_iter()
								.map(|(end, added)| (end, merged(binding, &added)))
						})
						.collect();
				}
				reached
			}

			Query::Repetition { inner, min, max } => {
				let (label, repeated) = match inner.as_ref() {
					Query::Capture { name, inner } => (Some(name), inner.as_ref()),
					other => (None, other),
				};
				let most = max
					.map_or(limit - start + 1, u64::from)
					.min(limit - start + 1);
				let mut results = BTreeSet::new();
				let mut frontier: BTreeSet<Outcome> = BTreeSet::from([(start, Binding::new())]);
				if *min == 0 {
					results.extend(frontier.iter().cloned());
				}
				// an empty repetition can stand in for any number of required
				// ones, so once the frontier stops changing nothing new appears
				for repetition in 1..=most.max(u64::from(*min)) {
					let next: BTreeSet<Outcome> = frontier
						.iter()
						.flat_map(|(position, carried)| {
							self.outcomes(repeated, *position, limit).into_iter().map(
								move |(end, binding)| match end == *position {
									true => (end, carried.clone()),
									false => (end, binding),
								},
							)
						})
						.collect();
					if next.is_empty() {
						break;
					}
					if repetition >= u64::from(*min) {
						results.extend(next.iter().cloned());
					}
					if next == frontier && repetition >= u64::from(*min) {
						break;
					}
					frontier = next;
				}
				results
					.into_iter()
					.map(|(end, binding)| (end, bound(label, start, end, binding)))
					.collect()
			}

			Query::Or(alternatives) => alternatives
				.iter()
				.flat_map(|alternative| self.outcomes(alternative, start, limit))
				.collect(),

			Query::Capture { name, inner } => self
				.outcomes(inner, start, limit)
				.into_iter()
				.map(|(end, binding)| (end, bound(Some(name), start, end, binding)))
				.collect(),

			Query::Within { inner, span_layer } => {
				let spans = self.corpus.spans().spans(span_layer).unwrap_or(&[]);
				self.outcomes(inner, start, limit)
					.into_iter()
					.filter(|(end, _)| *end == start || inside_one(spans, start, *end))
					.collect()
			}

			Query::Constrained { inner, constraints } => self
				.outcomes(inner, start, limit)
				.into_iter()
				.filter(|(_, binding)| {
					constraints
						.iter()
						.all(|constraint| self.constraint_holds(constraint, binding))
				})
				.collect(),

			other => panic!("reference matcher does not cover {other:?}"),
		}
	}

	fn token_satisfies(&self, position: u64, constraint: &Constraint) -> bool {
		let value = self.corpus.forward().get_str(position, &constraint.layer);
		let positive = match &constraint.value {
			ConstraintValue::Literal(literal) => value == Some(literal.as_str()),
			ConstraintValue::Regex(pattern) => {
				let regex = regex::Regex::new(pattern).unwrap();
				value.is_some_and(|value| regex.is_match(value))
			}
		};
		match constraint.op {
			ConstraintOp::Eq => positive,
			ConstraintOp::Ne => !positive,
		}
	}

	fn constraint_holds(&self, constraint: &GlobalConstraint, binding: &Binding) -> bool {
		let attribute = |label: &str, layer: &str| {
			binding
				.get(label)
				.and_then(|span| self.corpus.forward().get_str(span.0, layer))
		};
		match constraint {
			GlobalConstraint::Eq { left, right } => {
				match (
					attribute(&left.label, &left.attr),
					attribute(&right.label, &right.attr),
				) {
					(Some(left), Some(right)) => left == right,
					_ => false,
				}
			}
			GlobalConstraint::Ne { left, right } => {
				match (
					attribute(&left.label, &left.attr),
					attribute(&right.label, &right.attr),
				) {
					(Some(left), Some(right)) => left != right,
					_ => false,
				}
			}
			GlobalConstraint::Distance {
				left,
				right,
				op,
				value,
			} => match (binding.get(left), binding.get(right)) {
				(Some(left), Some(right)) => {
					let distance = right.0.saturating_sub(left.1);
					let target = u64::from(*value);
					match op {
						CmpOp::Ge => distance >= target,
						CmpOp::Gt => distance > target,
						CmpOp::Le => distance <= target,
						CmpOp::Lt => distance < target,
						CmpOp::Eq => distance == target,
						CmpOp::Ne => distance != target,
					}
				}
				_ => false,
			},
		}
	}
}

fn merged(earlier: &Binding, later: &Binding) -> Binding {
	let mut combined = earlier.clone();
	combined.extend(later.iter().map(|(label, span)| (label.clone(), *span)));
	combined
}

fn bound(label: Option<&String>, start: u64, end: u64, mut binding: Binding) -> Binding {
	if let Some(label) = label {
		if end > start {
			binding.insert(label.clone(), (start, end));
		}
	}
	binding
}

fn labels_in_query_order(query: &Query, labels: &mut Vec<String>) {
	match query {
		Query::Token(_) => {}
		Query::Capture { name, inner } => {
			labels.push(name.clone());
			labels_in_query_order(inner, labels);
		}
		Query::Sequence(parts) | Query::Or(parts) => {
			parts
				.iter()
				.for_each(|part| labels_in_query_order(part, labels));
		}
		Query::Repetition { inner, .. }
		| Query::Within { inner, .. }
		| Query::WithinComponent { inner, .. }
		| Query::WithinDocument { inner, .. }
		| Query::Project { inner, .. }
		| Query::Constrained { inner, .. } => labels_in_query_order(inner, labels),
	}
}

fn smallest<'a>(bindings: &'a BTreeSet<Binding>, labels: &[String]) -> &'a Binding {
	let unbound = (u64::MAX, u64::MAX);
	bindings
		.iter()
		.min_by_key(|binding| -> Vec<(u64, u64)> {
			labels
				.iter()
				.map(|label| binding.get(label).copied().unwrap_or(unbound))
				.collect()
		})
		.unwrap()
}

fn inside_one(spans: &[Span], start: u64, end: u64) -> bool {
	spans
		.iter()
		.any(|span| span.start <= start && end <= span.end)
}

// ---- corpus generation ----

const TAGS: [&str; 3] = ["N", "V", "D"];
const LEMMAS: [&str; 3] = ["a", "b", "ab"];

static CORPUS_COUNTER: AtomicU32 = AtomicU32::new(0);

// documents of sentences of (tag index, lemma index)
type CorpusShape = Vec<Vec<Vec<(usize, usize)>>>;

fn corpus_shape() -> impl Strategy<Value = CorpusShape> {
	let token = (0..TAGS.len(), 0..LEMMAS.len());
	let sentence = prop::collection::vec(token, 1..5);
	let document = prop::collection::vec(sentence, 1..4);
	prop::collection::vec(document, 1..4)
}

fn build(shape: &CorpusShape) -> Corpus {
	let path = std::env::temp_dir().join(format!(
		"montre_differential_{}_{}",
		std::process::id(),
		CORPUS_COUNTER.fetch_add(1, Ordering::SeqCst)
	));
	let mut builder = CorpusBuilder::new("differential");
	for (index, document) in shape.iter().enumerate() {
		let conllu: String = document
			.iter()
			.map(|sentence| {
				let rows: String = sentence
					.iter()
					.enumerate()
					.map(|(position, &(tag, lemma))| {
						format!(
							"{}\t{}{}\t{}\t{}\t_\t_\t0\tdep\t_\t_\n",
							position + 1,
							LEMMAS[lemma],
							TAGS[tag],
							LEMMAS[lemma],
							TAGS[tag],
						)
					})
					.collect();
				rows + "\n"
			})
			.collect();
		let sentences = ConllUReader::new(Cursor::new(conllu))
			.read_sentences()
			.unwrap();
		builder.add_document(format!("d{index}"), sentences);
	}
	builder.build(&path).unwrap();
	let corpus = montre_index::open(&path).unwrap();
	let _ = std::fs::remove_dir_all(&path);
	corpus
}

// ---- query generation ----

#[derive(Debug, Clone)]
enum Shape {
	Token(String),
	Group(Vec<Vec<Shape>>),
	Quantified(Box<Shape>, String),
	Labeled(Box<Shape>),
	// a group with `within s`, a constraint over its own labels, or both; with
	// both, the constraint either applies inside the filter or around it
	Scoped {
		alternatives: Vec<Vec<Shape>>,
		within_sentence: bool,
		constraint: Option<ConstraintShape>,
		constraint_inside: bool,
	},
}

// (left label, right label, kind, distance), labels taken modulo those available
type ConstraintShape = (usize, usize, u8, u32);

fn token() -> impl Strategy<Value = Shape> {
	let tag = prop::sample::select(TAGS.to_vec());
	let lemma = prop::sample::select(LEMMAS.to_vec());
	let regex = prop::sample::select(vec!["a", "^b", "b$", "^a$"]);
	prop_oneof![
		1 => Just("[]".to_string()),
		4 => tag.clone().prop_map(|tag| format!(r#"[pos="{tag}"]"#)),
		2 => tag.clone().prop_map(|tag| format!(r#"[pos!="{tag}"]"#)),
		2 => lemma.clone().prop_map(|lemma| format!(r#"[lemma="{lemma}"]"#)),
		1 => regex.clone().prop_map(|regex| format!("[lemma=/{regex}/]")),
		1 => regex.prop_map(|regex| format!("[lemma!=/{regex}/]")),
		1 => (tag.clone(), lemma.clone())
			.prop_map(|(tag, lemma)| format!(r#"[pos="{tag}" & lemma!="{lemma}"]"#)),
		1 => (tag, lemma).prop_map(|(tag, lemma)| format!(r#"[pos!="{tag}" & lemma="{lemma}"]"#)),
	]
	.prop_map(Shape::Token)
}

fn quantifier() -> impl Strategy<Value = String> {
	prop_oneof![
		Just("?".to_string()),
		Just("*".to_string()),
		Just("+".to_string()),
		(1u32..3).prop_map(|count| format!("{{{count}}}")),
		(0u32..2, 1u32..3).prop_map(|(min, extra)| format!("{{{min},{}}}", min + extra)),
		(0u32..3).prop_map(|min| format!("{{{min},}}")),
	]
}

fn element() -> impl Strategy<Value = Shape> {
	token().prop_recursive(3, 24, 3, |inner| {
		let sequence = prop::collection::vec(inner.clone(), 1..4);
		prop_oneof![
			3 => inner.clone(),
			2 => (inner.clone(), quantifier())
				.prop_map(|(shape, quantifier)| Shape::Quantified(Box::new(shape), quantifier)),
			2 => inner.prop_map(|shape| Shape::Labeled(Box::new(shape))),
			2 => prop::collection::vec(sequence.clone(), 1..3).prop_map(Shape::Group),
			1 => (
				prop::collection::vec(sequence, 1..3),
				prop::bool::ANY,
				prop::option::of((0usize..8, 0usize..8, 0u8..5, 0u32..3)),
				prop::bool::ANY,
			)
				.prop_map(|(alternatives, within_sentence, constraint, constraint_inside)| {
					Shape::Scoped {
						alternatives,
						within_sentence,
						constraint,
						constraint_inside,
					}
				}),
		]
	})
}

#[derive(Debug, Clone)]
struct QueryShape {
	alternatives: Vec<Vec<Shape>>,
	within_sentence: bool,
	constraints: Vec<ConstraintShape>,
}

fn query_shape() -> impl Strategy<Value = QueryShape> {
	(
		prop::collection::vec(prop::collection::vec(element(), 1..4), 1..3),
		prop::bool::weighted(0.25),
		prop::collection::vec((0usize..8, 0usize..8, 0u8..5, 0u32..3), 0..3),
	)
		.prop_map(|(alternatives, within_sentence, constraints)| QueryShape {
			alternatives,
			within_sentence,
			constraints,
		})
}

const LABELS: [&str; 12] = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "m"];

struct Renderer {
	labels_used: usize,
}

impl Renderer {
	fn alternatives(&mut self, alternatives: &[Vec<Shape>]) -> String {
		alternatives
			.iter()
			.map(|sequence| {
				sequence
					.iter()
					.map(|shape| self.shape(shape))
					.collect::<Vec<_>>()
					.join(" ")
			})
			.collect::<Vec<_>>()
			.join(" | ")
	}

	fn shape(&mut self, shape: &Shape) -> String {
		match shape {
			Shape::Token(text) => text.clone(),
			Shape::Group(alternatives) => format!("({})", self.alternatives(alternatives)),
			Shape::Quantified(inner, quantifier) => match inner.as_ref() {
				Shape::Quantified(..) => format!("({}){quantifier}", self.shape(inner)),
				_ => format!("{}{quantifier}", self.shape(inner)),
			},
			Shape::Labeled(inner) => {
				if self.labels_used == LABELS.len() {
					return self.shape(inner);
				}
				let label = LABELS[self.labels_used];
				self.labels_used += 1;
				match inner.as_ref() {
					Shape::Token(_) | Shape::Group(_) | Shape::Scoped { .. } => {
						format!("{label}:{}", self.shape(inner))
					}
					_ => format!("{label}:({})", self.shape(inner)),
				}
			}
			Shape::Scoped {
				alternatives,
				within_sentence,
				constraint,
				constraint_inside,
			} => {
				let first_label = self.labels_used;
				let body = self.alternatives(alternatives);
				let declared = &LABELS[first_label..self.labels_used];
				let constraint = constraint
					.filter(|_| !declared.is_empty())
					.map(|constraint| format!(" :: {}", render_constraint(constraint, declared)))
					.unwrap_or_default();
				let within = if *within_sentence { " within s" } else { "" };
				if *constraint_inside {
					format!("(({body}{constraint}){within})")
				} else {
					format!("({body}{within}{constraint})")
				}
			}
		}
	}
}

fn render_constraint((left, right, kind, value): ConstraintShape, labels: &[&str]) -> String {
	let label = |index: usize| labels[index % labels.len()];
	let (left, right) = (label(left), label(right));
	match kind {
		0 => format!("{left}.lemma = {right}.lemma"),
		1 => format!("{left}.pos != {right}.pos"),
		2 => format!("{left}.pos = {right}.pos"),
		3 => format!("distance({left},{right}) >= {value}"),
		_ => format!("distance({left},{right}) < {value}"),
	}
}

fn render(shape: &QueryShape) -> String {
	let mut renderer = Renderer { labels_used: 0 };
	let mut query = renderer.alternatives(&shape.alternatives);
	if shape.within_sentence {
		query.push_str(" within s");
	}
	if renderer.labels_used > 0 && !shape.constraints.is_empty() {
		let declared = &LABELS[..renderer.labels_used];
		let rendered: Vec<String> = shape
			.constraints
			.iter()
			.map(|&constraint| render_constraint(constraint, declared))
			.collect();
		query.push_str(" :: ");
		query.push_str(&rendered.join(" & "));
	}
	query
}

fn unconstrained(alternatives: &[Vec<Shape>]) -> Vec<Vec<Shape>> {
	alternatives
		.iter()
		.map(|sequence| sequence.iter().map(Shape::unconstrained).collect())
		.collect()
}

impl Shape {
	fn unconstrained(&self) -> Shape {
		match self {
			Shape::Token(_) => self.clone(),
			Shape::Group(alternatives) => Shape::Group(unconstrained(alternatives)),
			Shape::Quantified(inner, quantifier) => {
				Shape::Quantified(Box::new(inner.unconstrained()), quantifier.clone())
			}
			Shape::Labeled(inner) => Shape::Labeled(Box::new(inner.unconstrained())),
			Shape::Scoped {
				alternatives,
				within_sentence,
				constraint_inside,
				..
			} => Shape::Scoped {
				alternatives: unconstrained(alternatives),
				within_sentence: *within_sentence,
				constraint: None,
				constraint_inside: *constraint_inside,
			},
		}
	}
}

impl QueryShape {
	fn within_sentence(&self) -> QueryShape {
		QueryShape {
			within_sentence: true,
			..self.clone()
		}
	}

	// every constraint removed, the top-level clause and those of scoped groups
	fn unconstrained(&self) -> QueryShape {
		QueryShape {
			alternatives: unconstrained(&self.alternatives),
			within_sentence: self.within_sentence,
			constraints: Vec::new(),
		}
	}
}

// ---- the property ----

type Reported = Vec<((u64, u64), Binding)>;

fn executed(corpus: &Corpus, query: &Query) -> Reported {
	let plan = montre_query::planner::plan(query).unwrap();
	montre_query::executor::execute(&plan, corpus)
		.unwrap()
		.hits()
		.iter()
		.map(|hit| {
			let binding = hit
				.captures
				.iter()
				.map(|(label, span)| (label.clone(), (span.start, span.end)))
				.collect();
			((hit.span.start, hit.span.end), binding)
		})
		.collect()
}

fn check(corpus: &Corpus, text: &str) -> Result<(), TestCaseError> {
	let query = match montre_query::parse(text) {
		Ok(query) => query,
		Err(error) => return Err(TestCaseError::fail(format!("generated {text}: {error}"))),
	};
	let expected = Reference { corpus }.matches(&query);
	let reported = executed(corpus, &query);

	let expected_spans: Vec<(u64, u64)> = expected.keys().copied().collect();
	let reported_spans: Vec<(u64, u64)> = reported.iter().map(|(span, _)| *span).collect();
	prop_assert_eq!(&reported_spans, &expected_spans, "spans for {}", text);

	let mut labels = Vec::new();
	labels_in_query_order(&query, &mut labels);
	for (span, binding) in &reported {
		prop_assert_eq!(
			binding,
			smallest(&expected[span], &labels),
			"captures for {:?} in {}; valid bindings: {:?}",
			span,
			text,
			&expected[span]
		);
	}

	prop_assert_eq!(
		&executed(corpus, &query),
		&reported,
		"repeat run of {}",
		text
	);

	let plan = montre_query::planner::plan(&query).unwrap();
	let counted = montre_query::executor::execute_count(&plan, corpus).unwrap();
	prop_assert_eq!(counted, expected_spans.len(), "count for {}", text);
	Ok(())
}

proptest! {
	#![proptest_config(ProptestConfig {
		cases: 64,
		max_shrink_iters: 4000,
		..ProptestConfig::default()
	})]

	#[test]
	fn executor_agrees_with_reference(
		shape in corpus_shape(),
		queries in prop::collection::vec(query_shape(), 12),
	) {
		let corpus = build(&shape);
		for query in &queries {
			check(&corpus, &render(query))?;
		}
	}
}

// ---- restrictions only remove matches ----
//
// These hold of the executor whatever the reference says. `within s` is a
// filter on whole hits, captures included; a constraint is existential over
// capture assignments, so it is a filter on spans only.

fn parsed(text: &str) -> Result<Query, TestCaseError> {
	montre_query::parse(text)
		.map_err(|error| TestCaseError::fail(format!("generated {text}: {error}")))
}

fn counted(corpus: &Corpus, query: &Query) -> usize {
	let plan = montre_query::planner::plan(query).unwrap();
	montre_query::executor::execute_count(&plan, corpus).unwrap()
}

fn check_within_filters(corpus: &Corpus, shape: &QueryShape) -> Result<(), TestCaseError> {
	let (text, restricted_text) = (render(shape), render(&shape.within_sentence()));
	let restricted = parsed(&restricted_text)?;
	let sentences = corpus.spans().spans("sentence").unwrap_or(&[]);
	let expected: Reported = executed(corpus, &parsed(&text)?)
		.into_iter()
		.filter(|((start, end), _)| inside_one(sentences, *start, *end))
		.collect();
	prop_assert_eq!(
		&executed(corpus, &restricted),
		&expected,
		"{} against {} restricted to one sentence",
		restricted_text,
		text
	);
	prop_assert_eq!(
		counted(corpus, &restricted),
		expected.len(),
		"count for {}",
		restricted_text
	);
	Ok(())
}

fn check_constraints_filter_spans(
	corpus: &Corpus,
	shape: &QueryShape,
) -> Result<bool, TestCaseError> {
	let (text, unconstrained_text) = (render(shape), render(&shape.unconstrained()));
	if text == unconstrained_text {
		return Ok(false);
	}
	let spans = |reported: Reported| -> BTreeSet<(u64, u64)> {
		reported.into_iter().map(|(span, _)| span).collect()
	};
	let constrained = spans(executed(corpus, &parsed(&text)?));
	let unconstrained = spans(executed(corpus, &parsed(&unconstrained_text)?));
	let added: Vec<_> = constrained.difference(&unconstrained).collect();
	prop_assert!(
		added.is_empty(),
		"{} reports spans {:?} that {} does not",
		text,
		added,
		unconstrained_text
	);
	Ok(true)
}

#[derive(Default)]
struct Tally {
	corpora: Cell<usize>,
	within: Cell<usize>,
	constrained: Cell<usize>,
}

impl Tally {
	fn add(counter: &Cell<usize>, amount: usize) {
		counter.set(counter.get() + amount);
	}
}

#[test]
fn restrictions_only_remove_matches() {
	let mut runner = TestRunner::new(contextualize_config(ProptestConfig {
		cases: 64,
		max_shrink_iters: 4000,
		source_file: Some(file!()),
		..ProptestConfig::default()
	}));
	let tally = Tally::default();
	let strategy = (corpus_shape(), prop::collection::vec(query_shape(), 12));
	let outcome = runner.run(&strategy, |(shape, queries)| {
		let corpus = build(&shape);
		Tally::add(&tally.corpora, 1);
		for query in queries.iter().filter(|query| !query.within_sentence) {
			check_within_filters(&corpus, query)?;
			Tally::add(&tally.within, 1);
		}
		for query in &queries {
			let checked = check_constraints_filter_spans(&corpus, query)?;
			Tally::add(&tally.constrained, usize::from(checked));
		}
		Ok(())
	});
	if let Err(failure) = outcome {
		panic!("{failure}");
	}
	eprintln!(
		"restrictions_only_remove_matches: {} corpora, {} within pairs, {} constraint pairs",
		tally.corpora.get(),
		tally.within.get(),
		tally.constrained.get()
	);
	assert!(tally.within.get() > 0 && tally.constrained.get() > 0);
}
