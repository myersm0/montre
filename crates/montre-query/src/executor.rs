use std::collections::HashSet;

use montre_core::Span;
use montre_index::{Corpus, InvertedIndex, SpanIndex};

use crate::planner::{PlanNode, QueryPlan, SequenceStep};
use crate::Result;

#[derive(Debug, Clone)]
pub struct Hit {
	pub span: Span,
	pub document_index: u32,
	pub sentence_index: u32,
	pub captures: Vec<(String, Span)>,
}

impl Hit {
	fn new(span: Span) -> Self {
		Self {
			span,
			document_index: 0,
			sentence_index: 0,
			captures: Vec::new(),
		}
	}
}

fn binary_search_span(spans: &[Span], position: u64) -> Option<usize> {
	if spans.is_empty() {
		return None;
	}

	let mut lo = 0;
	let mut hi = spans.len();

	while lo < hi {
		let mid = lo + (hi - lo) / 2;
		let span = &spans[mid];

		if position < span.start {
			hi = mid;
		} else if position >= span.end {
			lo = mid + 1;
		} else {
			return Some(mid);
		}
	}

	None
}

pub struct Results {
	hits: Vec<Hit>,
	position: usize,
}

impl Results {
	pub fn new(hits: Vec<Hit>) -> Self {
		Self { hits, position: 0 }
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

	/// Populate document_index and sentence_index for all hits.
	/// Call this only when you need structural context (e.g., alignment projection).
	/// Most display operations don't need this.
	pub fn populate_context(&mut self, corpus: &Corpus) {
		let doc_spans = corpus.spans.spans("document");
		let sent_spans = corpus.spans.spans("sentence");

		for hit in &mut self.hits {
			if let Some(spans) = doc_spans {
				hit.document_index = binary_search_span(spans, hit.span.start).unwrap_or(0) as u32;
			}
			if let Some(spans) = sent_spans {
				hit.sentence_index = binary_search_span(spans, hit.span.start).unwrap_or(0) as u32;
			}
		}
	}
}

impl Iterator for Results {
	type Item = Hit;

	fn next(&mut self) -> Option<Self::Item> {
		if self.position < self.hits.len() {
			let hit = self.hits[self.position].clone();
			self.position += 1;
			Some(hit)
		} else {
			None
		}
	}
}

pub fn execute(plan: &QueryPlan, corpus: &Corpus) -> Result<Results> {
	let hits = execute_node(&plan.root, corpus)?;
	Ok(Results::new(hits))
}

fn execute_node(node: &PlanNode, corpus: &Corpus) -> Result<Vec<Hit>> {
	match node {
		PlanNode::ScanLiteral { layer, value } => {
			let Some(bitmap) = corpus.inverted.get(layer, value) else {
				return Ok(Vec::new());
			};

			let hits: Vec<Hit> = bitmap
				.iter()
				.map(|pos| Hit::new(Span::new(pos as u64, pos as u64 + 1)))
				.collect();

			Ok(hits)
		}

		PlanNode::ScanRegex { layer, pattern } => {
			let re = regex::Regex::new(pattern)?;
			let mut positions = Vec::new();

			if let Some(values) = corpus.inverted.values(layer) {
				for value in values {
					if re.is_match(value) {
						if let Some(bitmap) = corpus.inverted.get(layer, value) {
							positions.extend(bitmap.iter().map(|p| p as u64));
						}
					}
				}
			}

			positions.sort_unstable();
			positions.dedup();

			let hits = positions
				.into_iter()
				.map(|pos| Hit::new(Span::new(pos, pos + 1)))
				.collect();

			Ok(hits)
		}

		PlanNode::ScanAll => {
			let token_count = corpus.token_count();
			let hits = (0..token_count)
				.map(|pos| Hit::new(Span::new(pos, pos + 1)))
				.collect();
			Ok(hits)
		}

		PlanNode::Intersect(nodes) => {
			if nodes.is_empty() {
				return Ok(Vec::new());
			}

			let mut result_positions: Option<HashSet<u64>> = None;

			for node in nodes {
				let hits = execute_node(node, corpus)?;
				let positions: HashSet<u64> = hits.iter().map(|h| h.span.start).collect();

				result_positions = Some(match result_positions {
					None => positions,
					Some(existing) => existing.intersection(&positions).copied().collect(),
				});
			}

			let mut positions: Vec<u64> = result_positions.unwrap_or_default().into_iter().collect();
			positions.sort_unstable();

			let hits = positions
				.into_iter()
				.map(|pos| Hit::new(Span::new(pos, pos + 1)))
				.collect();

			Ok(hits)
		}

		PlanNode::Union(nodes) => {
			let mut all_positions = HashSet::new();

			for node in nodes {
				let hits = execute_node(node, corpus)?;
				for hit in hits {
					all_positions.insert(hit.span.start);
				}
			}

			let mut positions: Vec<u64> = all_positions.into_iter().collect();
			positions.sort_unstable();

			let hits = positions
				.into_iter()
				.map(|pos| Hit::new(Span::new(pos, pos + 1)))
				.collect();

			Ok(hits)
		}

		PlanNode::Difference { base, subtract } => {
			let base_hits = execute_node(base, corpus)?;
			let subtract_hits = execute_node(subtract, corpus)?;

			let subtract_positions: HashSet<u64> =
				subtract_hits.iter().map(|h| h.span.start).collect();

			let hits = base_hits
				.into_iter()
				.filter(|h| !subtract_positions.contains(&h.span.start))
				.collect();

			Ok(hits)
		}

		PlanNode::FilterBySpan { inner, span_layer } => {
			let hits = execute_node(inner, corpus)?;

			let Some(spans) = corpus.spans.spans(span_layer) else {
				return Ok(hits);
			};

			let hits = hits
				.into_iter()
				.filter(|hit| {
					spans
						.iter()
						.any(|span| span.start <= hit.span.start && hit.span.end <= span.end)
				})
				.collect();

			Ok(hits)
		}

		PlanNode::FilterByComponent { inner, component } => {
			let hits = execute_node(inner, corpus)?;

			let Some(comp_meta) = corpus.component(component) else {
				return Ok(Vec::new());
			};

			let doc_spans = corpus.spans.spans("document");

			let hits = hits
				.into_iter()
				.filter(|hit| {
					if let Some(spans) = doc_spans {
						for (doc_idx, span) in spans.iter().enumerate() {
							if hit.span.start >= span.start && hit.span.end <= span.end {
								return doc_idx >= comp_meta.document_range.0
									&& doc_idx < comp_meta.document_range.1;
							}
						}
					}
					false
				})
				.collect();

			Ok(hits)
		}

		PlanNode::ProjectAlignment { inner, alignment } => {
			let source_hits = execute_node(inner, corpus)?;

			let Some(edges) = corpus.alignment_edges(alignment) else {
				return Err(crate::QueryError::Execution(format!(
					"Alignment not found: {}",
					alignment
				)));
			};

			let Some(align_meta) = corpus.alignment_meta(alignment) else {
				return Err(crate::QueryError::Execution(format!(
					"Alignment metadata not found: {}",
					alignment
				)));
			};

			let Some(source_comp) = corpus.component(&align_meta.source_component) else {
				return Err(crate::QueryError::Execution(format!(
					"Source component not found: {}",
					align_meta.source_component
				)));
			};

			let Some(target_comp) = corpus.component(&align_meta.target_component) else {
				return Err(crate::QueryError::Execution(format!(
					"Target component not found: {}",
					align_meta.target_component
				)));
			};

			let Some(doc_spans) = corpus.spans.spans("document") else {
				return Ok(Vec::new());
			};

			let Some(sent_spans) = corpus.spans.spans(&align_meta.source_layer) else {
				return Ok(Vec::new());
			};

			let Some(target_sent_spans) = corpus.spans.spans(&align_meta.target_layer) else {
				return Ok(Vec::new());
			};

			let find_doc_and_sent_for_hit = |hit: &Hit| -> Option<(u32, u32)> {
				for doc_idx in source_comp.document_range.0..source_comp.document_range.1 {
					let doc_span = doc_spans.get(doc_idx)?;
					if hit.span.start >= doc_span.start && hit.span.end <= doc_span.end {
						let doc_within_comp = (doc_idx - source_comp.document_range.0) as u32;
						let mut sent_within_doc = 0u32;
						for sent_span in sent_spans.iter() {
							if sent_span.start >= doc_span.start && sent_span.end <= doc_span.end {
								if hit.span.start >= sent_span.start && hit.span.end <= sent_span.end {
									return Some((doc_within_comp, sent_within_doc));
								}
								sent_within_doc += 1;
							} else if sent_span.start >= doc_span.end {
								break;
							}
						}
					}
				}
				None
			};

			let get_target_span = |tgt_doc: u32, tgt_sent: u32| -> Option<Span> {
				let abs_doc_idx = target_comp.document_range.0 + tgt_doc as usize;
				let doc_span = doc_spans.get(abs_doc_idx)?;
				let mut sent_count = 0u32;
				for sent_span in target_sent_spans.iter() {
					if sent_span.start >= doc_span.start && sent_span.end <= doc_span.end {
						if sent_count == tgt_sent {
							return Some(*sent_span);
						}
						sent_count += 1;
					} else if sent_span.start >= doc_span.end {
						break;
					}
				}
				None
			};

			let mut result_hits = Vec::new();
			let mut seen_targets = HashSet::new();

			for hit in &source_hits {
				if let Some((src_doc, src_sent)) = find_doc_and_sent_for_hit(hit) {
					for &((edge_src_doc, edge_src_sent), (tgt_doc, tgt_sent)) in edges {
						if edge_src_doc == src_doc && edge_src_sent == src_sent {
							let target_key = (tgt_doc, tgt_sent);
							if seen_targets.insert(target_key) {
								if let Some(target_span) = get_target_span(tgt_doc, tgt_sent) {
									result_hits.push(Hit::new(target_span));
								}
							}
						}
					}
				}
			}

			result_hits.sort_by_key(|h| h.span.start);
			Ok(result_hits)
		}

		PlanNode::SequenceScan { steps } => execute_sequence(steps, corpus),
	}
}

fn execute_sequence(steps: &[SequenceStep], corpus: &Corpus) -> Result<Vec<Hit>> {
	use std::collections::HashMap;

	if steps.is_empty() {
		return Ok(Vec::new());
	}

	let token_count = corpus.token_count();

	let run_indices: Vec<RunIndex> = steps
		.iter()
		.map(|step| {
			let positions = get_matching_positions(&step.node, corpus).unwrap_or_default();
			RunIndex::from_positions(&positions)
		})
		.collect();

	let first_step = &steps[0];
	let first_runs = &run_indices[0];

	let mut active: HashMap<u64, Vec<u64>> = HashMap::new();

	for (start, end) in first_runs.spans_for_quantifier(first_step.min, first_step.max) {
		active.entry(end).or_default().push(start);
	}

	if first_step.min == 0 && steps.len() > 1 {
		for run in &run_indices[1].runs {
			active.entry(run.start).or_default().push(run.start);
		}
	}

	if active.is_empty() {
		return Ok(Vec::new());
	}

	for (step_idx, step) in steps[1..].iter().enumerate() {
		if active.is_empty() {
			break;
		}

		let runs = &run_indices[step_idx + 1];
		let is_scan_all = matches!(step.node, PlanNode::ScanAll);

		let mut next_active: HashMap<u64, Vec<u64>> = HashMap::new();

		for (end_pos, starts) in &active {
			if step.min == 0 {
				for &s in starts {
					next_active.entry(*end_pos).or_default().push(s);
				}
			}

			let continuations = if is_scan_all {
				spans_for_scan_all(*end_pos, step.min, step.max, token_count)
			} else {
				runs.continuations_at(*end_pos, step.min, step.max)
			};

			for new_end in continuations {
				for &s in starts {
					next_active.entry(new_end).or_default().push(s);
				}
			}
		}

		for starts in next_active.values_mut() {
			starts.sort_unstable();
			starts.dedup();
		}

		active = next_active;
	}

	let mut hits: Vec<Hit> = active
		.into_iter()
		.flat_map(|(end, starts)| {
			starts
				.into_iter()
				.filter(move |&start| end > start)
				.map(move |start| Hit::new(Span::new(start, end)))
		})
		.collect();

	hits.sort_by_key(|h| (h.span.start, h.span.end));
	Ok(hits)
}

fn spans_for_scan_all(start: u64, min: u32, max: Option<u32>, token_count: u64) -> Vec<u64> {
	let max_len = max.unwrap_or(MAX_QUANTIFIER).min(MAX_QUANTIFIER) as u64;
	let mut results = Vec::new();

	for len in (min as u64)..=max_len {
		let end = start + len;
		if end <= token_count {
			results.push(end);
		} else {
			break;
		}
	}

	results
}

const MAX_QUANTIFIER: u32 = 100;

#[derive(Debug, Clone, Copy)]
struct Run {
	start: u64,
	end: u64,
}

impl Run {
	fn len(&self) -> u64 {
		self.end - self.start
	}
}

struct RunIndex {
	runs: Vec<Run>,
}

impl RunIndex {
	fn from_positions(positions: &[u64]) -> Self {
		if positions.is_empty() {
			return Self { runs: Vec::new() };
		}

		let mut sorted = positions.to_vec();
		sorted.sort_unstable();
		sorted.dedup();

		let mut runs = Vec::new();
		let mut run_start = sorted[0];
		let mut prev = sorted[0];

		for &pos in &sorted[1..] {
			if pos == prev + 1 {
				prev = pos;
			} else {
				runs.push(Run {
					start: run_start,
					end: prev + 1,
				});
				run_start = pos;
				prev = pos;
			}
		}

		runs.push(Run {
			start: run_start,
			end: prev + 1,
		});

		Self { runs }
	}

	fn spans_for_quantifier(&self, min: u32, max: Option<u32>) -> Vec<(u64, u64)> {
		let max_len = max.unwrap_or(MAX_QUANTIFIER).min(MAX_QUANTIFIER) as u64;
		let min_len = (min as u64).max(1);
		let mut spans = Vec::new();

		for run in &self.runs {
			if run.len() < min_len {
				continue;
			}

			let max_start = run.end - min_len;

			for span_start in run.start..=max_start {
				let available = run.end - span_start;
				let span_max = available.min(max_len);

				for span_len in min_len..=span_max {
					spans.push((span_start, span_start + span_len));
				}
			}
		}

		spans
	}

	fn continuations_at(&self, pos: u64, min: u32, max: Option<u32>) -> Vec<u64> {
		let max_len = max.unwrap_or(MAX_QUANTIFIER).min(MAX_QUANTIFIER) as u64;
		let min_len = min as u64;

		let Some(run) = self.run_containing(pos) else {
			return Vec::new();
		};

		let available = run.end - pos;
		if available < min_len {
			return Vec::new();
		}

		let mut results = Vec::new();
		let usable = available.min(max_len);

		for len in min_len..=usable {
			results.push(pos + len);
		}

		results
	}

	fn run_containing(&self, pos: u64) -> Option<&Run> {
		let idx = self.runs.partition_point(|r| r.end <= pos);
		if idx < self.runs.len() && self.runs[idx].start <= pos {
			Some(&self.runs[idx])
		} else {
			None
		}
	}
}

fn get_matching_positions(node: &PlanNode, corpus: &Corpus) -> Result<Vec<u64>> {
	match node {
		PlanNode::ScanAll => Ok((0..corpus.token_count()).collect()),

		PlanNode::ScanLiteral { layer, value } => {
			let Some(bitmap) = corpus.inverted.get(layer, value) else {
				return Ok(Vec::new());
			};
			Ok(bitmap.iter().map(|p| p as u64).collect())
		}

		PlanNode::ScanRegex { layer, pattern } => {
			let re = regex::Regex::new(pattern)?;
			let mut positions = Vec::new();

			if let Some(values) = corpus.inverted.values(layer) {
				for value in values {
					if re.is_match(value) {
						if let Some(bitmap) = corpus.inverted.get(layer, value) {
							positions.extend(bitmap.iter().map(|p| p as u64));
						}
					}
				}
			}

			positions.sort_unstable();
			positions.dedup();
			Ok(positions)
		}

		_ => {
			let hits = execute_node(node, corpus)?;
			Ok(hits.into_iter().map(|h| h.span.start).collect())
		}
	}
}

impl PartialEq for PlanNode {
	fn eq(&self, other: &Self) -> bool {
		match (self, other) {
			(PlanNode::ScanAll, PlanNode::ScanAll) => true,
			(
				PlanNode::ScanLiteral { layer: l1, value: v1 },
				PlanNode::ScanLiteral { layer: l2, value: v2 },
			) => l1 == l2 && v1 == v2,
			(
				PlanNode::ScanRegex { layer: l1, pattern: p1 },
				PlanNode::ScanRegex { layer: l2, pattern: p2 },
			) => l1 == l2 && p1 == p2,
			_ => false,
		}
	}
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

		let mut results = Results::new(hits);
		assert_eq!(results.len(), 2);

		let first = results.next().unwrap();
		assert_eq!(first.span.start, 0);

		let second = results.next().unwrap();
		assert_eq!(second.span.start, 5);

		assert!(results.next().is_none());
	}
}
