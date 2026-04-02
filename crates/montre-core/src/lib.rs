use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

pub type Position = u64;
pub type LayerId = u16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(C)]
pub struct Span {
	pub start: Position,
	pub end: Position,
}

impl Span {
	pub fn new(start: Position, end: Position) -> Self {
		debug_assert!(start <= end);
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

pub type Annotations = SmallVec<[(LayerId, Value); 4]>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
	pub position: Position,
	pub annotations: Annotations,
}

impl Token {
	pub fn new(position: Position) -> Self {
		Self {
			position,
			annotations: SmallVec::new(),
		}
	}

	pub fn with_annotation(mut self, layer: LayerId, value: impl Into<Value>) -> Self {
		self.annotations.push((layer, value.into()));
		self
	}

	pub fn get(&self, layer: LayerId) -> Option<&Value> {
		self.annotations
			.iter()
			.find(|(l, _)| *l == layer)
			.map(|(_, v)| v)
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LayerName(pub CompactString);

impl LayerName {
	pub fn new(name: impl Into<CompactString>) -> Self {
		Self(name.into())
	}

	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl From<&str> for LayerName {
	fn from(s: &str) -> Self {
		Self::new(s)
	}
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

	#[test]
	fn token_annotations() {
		let token = Token::new(42)
			.with_annotation(0, "house")
			.with_annotation(1, "NOUN");

		assert_eq!(token.get(0), Some(&Value::from("house")));
		assert_eq!(token.get(1), Some(&Value::from("NOUN")));
		assert_eq!(token.get(2), None);
	}
}
