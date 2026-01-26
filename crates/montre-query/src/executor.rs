use montre_core::Span;
use montre_index::{Corpus, InvertedIndex};

use crate::planner::{PlanNode, QueryPlan};
use crate::Result;

#[derive(Debug, Clone)]
pub struct Hit {
	pub span: Span,
	pub captures: Vec<(String, Span)>,
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
				.map(|pos| Hit {
					span: Span::new(pos as u64, pos as u64 + 1),
					captures: Vec::new(),
				})
				.collect();

			Ok(hits)
		}

		PlanNode::ScanRegex { layer, pattern } => {
			let re = regex::Regex::new(pattern)?;
			let mut hits = Vec::new();

			if let Some(values) = corpus.inverted.values(layer) {
				for value in values {
					if re.is_match(value) {
						if let Some(bitmap) = corpus.inverted.get(layer, value) {
							for pos in bitmap.iter() {
								hits.push(Hit {
									span: Span::new(pos as u64, pos as u64 + 1),
									captures: Vec::new(),
								});
							}
						}
					}
				}
			}

			hits.sort_by_key(|h| h.span.start);
			Ok(hits)
		}

		PlanNode::SequenceScan { steps } => {
			if steps.is_empty() {
				return Ok(Vec::new());
			}

			let first_hits = execute_node(&steps[0], corpus)?;
			if steps.len() == 1 {
				return Ok(first_hits);
			}

			let mut current_starts: Vec<u64> = first_hits.iter().map(|h| h.span.start).collect();

			for step in &steps[1..] {
				let step_hits = execute_node(step, corpus)?;
				let step_positions: std::collections::HashSet<u64> =
					step_hits.iter().map(|h| h.span.start).collect();

				current_starts = current_starts
					.into_iter()
					.filter(|&start| step_positions.contains(&(start + 1)))
					.map(|start| start + 1)
					.collect();

				if current_starts.is_empty() {
					break;
				}
			}

			let sequence_len = steps.len() as u64;
			let hits: Vec<Hit> = current_starts
				.into_iter()
				.map(|end_pos| Hit {
					span: Span::new(end_pos + 1 - sequence_len, end_pos + 1),
					captures: Vec::new(),
				})
				.collect();

			Ok(hits)
		}

		PlanNode::Intersect(nodes) => {
			if nodes.is_empty() {
				return Ok(Vec::new());
			}

			let mut result_positions: Option<std::collections::HashSet<u64>> = None;

			for node in nodes {
				let hits = execute_node(node, corpus)?;
				let positions: std::collections::HashSet<u64> =
					hits.iter().map(|h| h.span.start).collect();

				result_positions = Some(match result_positions {
					None => positions,
					Some(existing) => existing.intersection(&positions).copied().collect(),
				});
			}

			let hits: Vec<Hit> = result_positions
				.unwrap_or_default()
				.into_iter()
				.map(|pos| Hit {
					span: Span::new(pos, pos + 1),
					captures: Vec::new(),
				})
				.collect();

			Ok(hits)
		}

		_ => todo!("Executor not implemented for this plan node"),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn results_iterator() {
		let hits = vec![
			Hit {
				span: Span::new(0, 1),
				captures: vec![],
			},
			Hit {
				span: Span::new(5, 6),
				captures: vec![],
			},
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
