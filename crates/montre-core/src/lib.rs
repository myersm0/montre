use compact_str::CompactString;
use serde::{Deserialize, Serialize};

pub type Position = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(C)]
pub struct Span {
	pub start: Position,
	pub end: Position,
}

impl Span {
	pub fn new(start: Position, end: Position) -> Self {
		assert!(start <= end);
		Self { start, end }
	}

	pub fn len(&self) -> u64 {
		self.end - self.start
	}

	pub fn is_empty(&self) -> bool {
		self.start == self.end
	}

	pub fn contains(&self, position: Position) -> bool {
		position >= self.start && position < self.end
	}

	pub fn contains_span(&self, other: &Span) -> bool {
		self.start <= other.start && other.end <= self.end
	}

	pub fn overlaps(&self, other: &Span) -> bool {
		self.start < other.end && other.start < self.end
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Value {
	Str(CompactString),
	Int(i64),
}

impl From<&str> for Value {
	fn from(s: &str) -> Self {
		Value::Str(CompactString::from(s))
	}
}

impl From<String> for Value {
	fn from(s: String) -> Self {
		Value::Str(CompactString::from(s))
	}
}

impl From<i64> for Value {
	fn from(n: i64) -> Self {
		Value::Int(n)
	}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relation {
	pub source: Span,
	pub target: Span,
	pub kind: CompactString,
	pub score: Option<f32>,
}

/// A component is a labeled subcorpus within a multi-component corpus.
/// Each document belongs to exactly one component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
	pub id: u32,
	pub name: CompactString,
	pub language: CompactString,
}

/// Unit identifier for alignment: (document_index, unit_index_within_doc)
/// Stable across rebuilds as long as document order and segmentation are preserved.
pub type UnitId = (u32, u32);

/// A named relation mapping units in one component/layer to units in another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alignment {
	pub name: CompactString,
	pub source_component: CompactString,
	pub target_component: CompactString,
	pub source_layer: CompactString,
	pub target_layer: CompactString,
	pub directed: bool,
	pub edges: Vec<(UnitId, UnitId)>,
}

/// Future: weighted alignment edge with confidence scores
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentEdge {
	pub source: UnitId,
	pub target: UnitId,
	pub weight: Option<f32>,
	pub flags: u8,
}

/// Alignment edge flags
pub mod alignment_flags {
	pub const MANUAL: u8 = 0b0001;
	pub const AUTO: u8 = 0b0010;
	pub const REVIEWED: u8 = 0b0100;
	pub const FILTERED: u8 = 0b1000;
}

pub mod layers {
	pub const WORD: &str = "word";
	pub const LEMMA: &str = "lemma";
	pub const UPOS: &str = "upos";
	pub const XPOS: &str = "xpos";
	pub const FEATS: &str = "feats";
	pub const HEAD: &str = "head";
	pub const DEPREL: &str = "deprel";
	pub const DEPS: &str = "deps";
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn span_contains() {
		let span = Span::new(10, 20);
		assert!(span.contains(10));
		assert!(span.contains(15));
		assert!(!span.contains(20));
		assert!(!span.contains(5));
	}

	#[test]
	fn span_overlaps() {
		let a = Span::new(10, 20);
		let b = Span::new(15, 25);
		let c = Span::new(20, 30);
		let d = Span::new(5, 15);

		assert!(a.overlaps(&b));
		assert!(!a.overlaps(&c));
		assert!(a.overlaps(&d));
	}
}

#[cfg(test)]
mod proptests {
	use super::*;
	use proptest::prelude::*;

	fn span_strategy() -> impl Strategy<Value = Span> {
		(0u64..10_000, 0u64..10_000).prop_map(|(a, b)| {
			let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
			Span::new(lo, hi)
		})
	}

	proptest! {
		#[test]
		fn len_is_end_minus_start(span in span_strategy()) {
			prop_assert_eq!(span.len(), span.end - span.start);
		}

		#[test]
		fn contains_span_is_reflexive(span in span_strategy()) {
			prop_assert!(span.contains_span(&span));
		}

		#[test]
		fn nonempty_span_contains_start_not_end(span in span_strategy()) {
			if span.start < span.end {
				prop_assert!(span.contains(span.start));
				prop_assert!(!span.contains(span.end));
			}
		}

		#[test]
		fn contains_span_implies_overlaps(
			a in span_strategy(),
			b in span_strategy(),
		) {
			if a.contains_span(&b) && !b.is_empty() {
				prop_assert!(a.overlaps(&b));
			}
		}

		#[test]
		fn overlaps_is_symmetric(
			a in span_strategy(),
			b in span_strategy(),
		) {
			prop_assert_eq!(a.overlaps(&b), b.overlaps(&a));
		}
	}
}
