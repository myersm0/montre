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
}
