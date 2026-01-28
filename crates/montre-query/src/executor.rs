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

fn find_span_context(position: u64, corpus: &Corpus) -> (u32, u32) {
	let document_index = if let Some(doc_spans) = corpus.spans.spans("document") {
		binary_search_span(doc_spans, position).unwrap_or(0) as u32
	} else {
		0
	};

	let sentence_index = if let Some(sent_spans) = corpus.spans.spans("sentence") {
		binary_search_span(sent_spans, position).unwrap_or(0) as u32
	} else {
		0
	};

	(document_index, sentence_index)
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
	let mut hits = execute_node(&plan.root, corpus)?;

	for hit in &mut hits {
		let (doc_idx, sent_idx) = find_span_context(hit.span.start, corpus);
		hit.document_index = doc_idx;
		hit.sentence_index = sent_idx;
	}

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

		PlanNode::SequenceScan { steps } => execute_sequence(steps, corpus),
	}
}

fn execute_sequence(steps: &[SequenceStep], corpus: &Corpus) -> Result<Vec<Hit>> {
	if steps.is_empty() {
		return Ok(Vec::new());
	}

	let token_count = corpus.token_count();

	let first_step = &steps[0];
	let first_positions = get_matching_positions(&first_step.node, corpus)?;

	if first_positions.is_empty() {
		return Ok(Vec::new());
	}

	let mut active: Vec<(u64, u64)> = if first_step.min == 1 && first_step.max == Some(1) {
		first_positions.iter().map(|&p| (p, p + 1)).collect()
	} else {
		let position_set: HashSet<u64> = first_positions.iter().copied().collect();
		let mut result = Vec::new();
		for &start in &first_positions {
			let ends = expand_repetition_with_set(
				start,
				first_step.min,
				first_step.max,
				&position_set,
				token_count,
			);
			for end in ends {
				result.push((start, end));
			}
		}
		result
	};

	for step in &steps[1..] {
		if active.is_empty() {
			break;
		}

		let step_positions = get_matching_positions(&step.node, corpus)?;
		let step_set: HashSet<u64> = step_positions.into_iter().collect();
		let is_scan_all = matches!(step.node, PlanNode::ScanAll);

		if step.min == 1 && step.max == Some(1) {
			active = active
				.into_iter()
				.filter(|&(_, end)| is_scan_all || step_set.contains(&end))
				.map(|(start, end)| (start, end + 1))
				.collect();
		} else {
			let mut next_active = Vec::new();

			for (start, current_end) in active {
				if step.min == 0 {
					next_active.push((start, current_end));
				}

				let max_rep = step.max.unwrap_or(100).min(100) as u64;
				let mut pos = current_end;
				let mut count = 0u64;

				while pos < token_count && count < max_rep {
					if is_scan_all || step_set.contains(&pos) {
						count += 1;
						if count >= step.min as u64 {
							next_active.push((start, pos + 1));
						}
						pos += 1;
					} else {
						break;
					}
				}
			}

			active = next_active;
		}
	}

	active.sort_unstable();
	active.dedup();

	let hits: Vec<Hit> = active
		.into_iter()
		.map(|(start, end)| Hit::new(Span::new(start, end)))
		.collect();

	Ok(hits)
}

fn expand_repetition_with_set(
	start: u64,
	min: u32,
	max: Option<u32>,
	position_set: &HashSet<u64>,
	token_count: u64,
) -> Vec<u64> {
	let max_rep = max.unwrap_or(100).min(100) as u64;
	let mut results = Vec::new();

	if min == 0 {
		results.push(start);
	}

	let mut pos = start;
	let mut count = 0u64;

	while pos < token_count && count < max_rep {
		if position_set.contains(&pos) {
			count += 1;
			if count >= min as u64 {
				results.push(pos + 1);
			}
			pos += 1;
		} else {
			break;
		}
	}

	results
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
