use montre_core::{Position, Span};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub trait SpanIndex {
	fn spans(&self, layer: &str) -> Option<&[Span]>;
	fn containing(&self, layer: &str, position: Position) -> Option<&Span>;
	fn layers(&self) -> Vec<&str>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InMemorySpans {
	data: HashMap<String, Vec<Span>>,
}

impl InMemorySpans {
	pub fn new() -> Self {
		Self {
			data: HashMap::new(),
		}
	}

	pub fn add_span(&mut self, layer: &str, span: Span) {
		self.data
			.entry(layer.to_string())
			.or_default()
			.push(span);
	}

	pub fn finalize(&mut self) {
		for spans in self.data.values_mut() {
			spans.sort_by_key(|s| s.start);
		}
	}
}

impl Default for InMemorySpans {
	fn default() -> Self {
		Self::new()
	}
}

impl SpanIndex for InMemorySpans {
	fn spans(&self, layer: &str) -> Option<&[Span]> {
		self.data.get(layer).map(|v| v.as_slice())
	}

	fn containing(&self, layer: &str, position: Position) -> Option<&Span> {
		let spans = self.data.get(layer)?;
		spans
			.binary_search_by(|span| {
				if span.end <= position {
					std::cmp::Ordering::Less
				} else if span.start > position {
					std::cmp::Ordering::Greater
				} else {
					std::cmp::Ordering::Equal
				}
			})
			.ok()
			.map(|idx| &spans[idx])
	}

	fn layers(&self) -> Vec<&str> {
		self.data.keys().map(|s| s.as_str()).collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn find_containing_span() {
		let mut index = InMemorySpans::new();
		index.add_span("sentence", Span::new(0, 5));
		index.add_span("sentence", Span::new(5, 12));
		index.add_span("sentence", Span::new(12, 20));
		index.finalize();

		assert_eq!(
			index.containing("sentence", 0),
			Some(&Span::new(0, 5))
		);
		assert_eq!(
			index.containing("sentence", 4),
			Some(&Span::new(0, 5))
		);
		assert_eq!(
			index.containing("sentence", 5),
			Some(&Span::new(5, 12))
		);
		assert_eq!(
			index.containing("sentence", 15),
			Some(&Span::new(12, 20))
		);
	}
}
