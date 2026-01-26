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
	data: HashMap<(String, String), RoaringBitmap>,
	layer_names: Vec<String>,
}

impl InMemoryInverted {
	pub fn new() -> Self {
		Self {
			data: HashMap::new(),
			layer_names: Vec::new(),
		}
	}

	pub fn insert(&mut self, layer: &str, value: &str, positions: impl IntoIterator<Item = Position>) {
		let key = (layer.to_string(), value.to_string());
		let bitmap = self.data.entry(key).or_insert_with(RoaringBitmap::new);
		for pos in positions {
			bitmap.insert(pos as u32);
		}
		if !self.layer_names.contains(&layer.to_string()) {
			self.layer_names.push(layer.to_string());
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
		let key = (layer.to_string(), value.to_string());
		self.data.get(&key)
	}

	fn layers(&self) -> Vec<&str> {
		self.layer_names.iter().map(|s| s.as_str()).collect()
	}

	fn values(&self, layer: &str) -> Option<Vec<&str>> {
		let values: Vec<&str> = self
			.data
			.keys()
			.filter(|(l, _)| l == layer)
			.map(|(_, v)| v.as_str())
			.collect();
		if values.is_empty() {
			None
		} else {
			Some(values)
		}
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
