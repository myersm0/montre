use montre_core::{Position, Span, span_containing};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub trait SpanIndex {
	fn spans(&self, layer: &str) -> Option<&[Span]>;
	fn containing_with_index(&self, layer: &str, position: Position) -> Option<(usize, &Span)>;
	fn layers(&self) -> Vec<&str>;

	fn containing(&self, layer: &str, position: Position) -> Option<&Span> {
		self.containing_with_index(layer, position).map(|(_, span)| span)
	}
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

	pub fn merge_from(&mut self, other: Self, position_offset: u64) {
		for (layer, spans) in other.data {
			let target = self.data.entry(layer).or_default();
			target.extend(spans.into_iter().map(|s| {
				Span::new(s.start + position_offset, s.end + position_offset)
			}));
		}
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

	fn containing_with_index(&self, layer: &str, position: Position) -> Option<(usize, &Span)> {
		let spans = self.data.get(layer)?;
		span_containing(spans, position).map(|idx| (idx, &spans[idx]))
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

	#[test]
	fn containing_position_beyond_all_spans() {
		let mut index = InMemorySpans::new();
		index.add_span("sentence", Span::new(0, 5));
		index.finalize();
		assert_eq!(index.containing("sentence", 100), None);
	}

	#[test]
	fn containing_missing_layer() {
		let index = InMemorySpans::new();
		assert_eq!(index.containing("nonexistent", 0), None);
	}

	#[test]
	fn spans_returns_sorted() {
		let mut index = InMemorySpans::new();
		index.add_span("sentence", Span::new(10, 20));
		index.add_span("sentence", Span::new(0, 10));
		index.add_span("sentence", Span::new(20, 30));
		index.finalize();

		let spans = index.spans("sentence").unwrap();
		assert_eq!(spans[0], Span::new(0, 10));
		assert_eq!(spans[1], Span::new(10, 20));
		assert_eq!(spans[2], Span::new(20, 30));
	}

	#[test]
	fn multiple_layers() {
		let mut index = InMemorySpans::new();
		index.add_span("sentence", Span::new(0, 5));
		index.add_span("document", Span::new(0, 20));
		index.finalize();

		assert!(index.spans("sentence").is_some());
		assert!(index.spans("document").is_some());
		assert!(index.spans("paragraph").is_none());

		let mut layers = index.layers();
		layers.sort();
		assert_eq!(layers, vec!["document", "sentence"]);
	}

	#[test]
	fn containing_at_boundary() {
		let mut index = InMemorySpans::new();
		index.add_span("s", Span::new(0, 10));
		index.add_span("s", Span::new(10, 20));
		index.finalize();

		// Position 9 is in first span, 10 is in second (half-open intervals)
		assert_eq!(index.containing("s", 9), Some(&Span::new(0, 10)));
		assert_eq!(index.containing("s", 10), Some(&Span::new(10, 20)));
	}

	#[test]
	fn single_span() {
		let mut index = InMemorySpans::new();
		index.add_span("document", Span::new(0, 100));
		index.finalize();

		assert_eq!(index.containing("document", 0), Some(&Span::new(0, 100)));
		assert_eq!(index.containing("document", 50), Some(&Span::new(0, 100)));
		assert_eq!(index.containing("document", 99), Some(&Span::new(0, 100)));
		assert_eq!(index.containing("document", 100), None);
	}

	#[test]
	fn containing_with_index_returns_correct_position() {
		let mut index = InMemorySpans::new();
		index.add_span("sentence", Span::new(0, 5));
		index.add_span("sentence", Span::new(5, 12));
		index.add_span("sentence", Span::new(12, 20));
		index.finalize();

		let spans = index.spans("sentence").unwrap();

		let (idx, span) = index.containing_with_index("sentence", 0).unwrap();
		assert_eq!(idx, 0);
		assert_eq!(span, &spans[0]);

		let (idx, span) = index.containing_with_index("sentence", 7).unwrap();
		assert_eq!(idx, 1);
		assert_eq!(span, &spans[1]);

		let (idx, span) = index.containing_with_index("sentence", 15).unwrap();
		assert_eq!(idx, 2);
		assert_eq!(span, &spans[2]);

		assert_eq!(index.containing_with_index("sentence", 100), None);
		assert_eq!(index.containing_with_index("nonexistent", 0), None);
	}
}
