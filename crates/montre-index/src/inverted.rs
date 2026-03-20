use montre_core::Position;
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub trait InvertedIndex {
	fn get(&self, layer: &str, value: &str) -> Option<&RoaringBitmap>;
	fn layers(&self) -> Vec<&str>;
	fn values(&self, layer: &str) -> Option<Vec<&str>>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InMemoryInverted {
	data: HashMap<String, HashMap<String, RoaringBitmap>>,
}

impl InMemoryInverted {
	pub fn new() -> Self {
		Self {
			data: HashMap::new(),
		}
	}

	pub fn insert(&mut self, layer: &str, value: &str, positions: impl IntoIterator<Item = Position>) {
		let bitmap = self
			.data
			.entry(layer.to_string())
			.or_default()
			.entry(value.to_string())
			.or_default();
		for pos in positions {
			bitmap.insert(pos as u32);
		}
	}
}

impl InMemoryInverted {
	pub fn merge_from(&mut self, other: Self, position_offset: u32) {
		for (layer, values) in other.data {
			let target_layer = self.data.entry(layer).or_default();
			for (value, bitmap) in values {
				let shifted: RoaringBitmap =
					bitmap.iter().map(|p| p + position_offset).collect();
				target_layer
					.entry(value)
					.and_modify(|existing| *existing |= &shifted)
					.or_insert(shifted);
			}
		}
	}
}

impl Default for InMemoryInverted {
	fn default() -> Self {
		Self::new()
	}
}

impl InvertedIndex for InMemoryInverted {
	fn get(&self, layer: &str, value: &str) -> Option<&RoaringBitmap> {
		self.data.get(layer)?.get(value)
	}

	fn layers(&self) -> Vec<&str> {
		self.data.keys().map(|s| s.as_str()).collect()
	}

	fn values(&self, layer: &str) -> Option<Vec<&str>> {
		self.data.get(layer).map(|values| {
			values.keys().map(|s| s.as_str()).collect()
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn basic_insert_and_get() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0, 5, 10]);
		index.insert("word", "cat", [3, 8]);

		let the_positions = index.get("word", "the").unwrap();
		assert!(the_positions.contains(0));
		assert!(the_positions.contains(5));
		assert!(the_positions.contains(10));
		assert!(!the_positions.contains(3));

		let cat_positions = index.get("word", "cat").unwrap();
		assert!(cat_positions.contains(3));
		assert!(cat_positions.contains(8));
	}

	#[test]
	fn get_missing_layer() {
		let index = InMemoryInverted::new();
		assert!(index.get("word", "the").is_none());
	}

	#[test]
	fn get_missing_value() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0]);
		assert!(index.get("word", "elephant").is_none());
	}

	#[test]
	fn layers_returns_all() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0]);
		index.insert("pos", "DET", [0]);
		index.insert("lemma", "the", [0]);

		let mut layers = index.layers();
		layers.sort();
		assert_eq!(layers, vec!["lemma", "pos", "word"]);
	}

	#[test]
	fn values_for_layer() {
		let mut index = InMemoryInverted::new();
		index.insert("pos", "DET", [0, 5]);
		index.insert("pos", "NOUN", [3, 8]);
		index.insert("pos", "VERB", [4]);

		let mut values = index.values("pos").unwrap();
		values.sort();
		assert_eq!(values, vec!["DET", "NOUN", "VERB"]);
	}

	#[test]
	fn values_missing_layer() {
		let index = InMemoryInverted::new();
		assert!(index.values("nonexistent").is_none());
	}

	#[test]
	fn incremental_insert() {
		let mut index = InMemoryInverted::new();
		index.insert("word", "the", [0]);
		index.insert("word", "the", [5, 10]);

		let positions = index.get("word", "the").unwrap();
		assert_eq!(positions.len(), 3);
	}
}
