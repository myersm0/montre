use montre_core::{Position, Value};
use serde::{Deserialize, Serialize};

pub trait ForwardIndex {
	fn token_count(&self) -> u64;
	fn get_str(&self, position: Position, layer: &str) -> Option<&str>;
	fn get_int(&self, position: Position, layer: &str) -> Option<i64>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InMemoryForward {
	layers: Vec<String>,
	data: Vec<Vec<Value>>,
	token_count: u64,
}

impl InMemoryForward {
	pub fn new() -> Self {
		Self {
			layers: Vec::new(),
			data: Vec::new(),
			token_count: 0,
		}
	}

	pub fn add_layer(&mut self, name: &str) -> usize {
		self.layers.push(name.to_string());
		self.data.push(Vec::new());
		self.layers.len() - 1
	}

	pub fn set(&mut self, layer_index: usize, position: Position, value: Value) {
		let layer_data = &mut self.data[layer_index];
		let pos = position as usize;
		if pos >= layer_data.len() {
			layer_data.resize(pos + 1, Value::Str(Default::default()));
		}
		layer_data[pos] = value;
		self.token_count = self.token_count.max(position + 1);
	}

	pub fn merge_from(&mut self, other: Self, position_offset: u64) {
		for (layer_name, source_data) in other.layers.into_iter().zip(other.data.into_iter()) {
			let target_idx = match self.layer_index(&layer_name) {
				Some(idx) => idx,
				None => self.add_layer(&layer_name),
			};
			let target_data = &mut self.data[target_idx];
			let start = position_offset as usize;
			let needed = start + source_data.len();
			if needed > target_data.len() {
				target_data.resize(needed, Value::Str(Default::default()));
			}
			for (j, val) in source_data.into_iter().enumerate() {
				target_data[start + j] = val;
			}
		}
		self.token_count = self.token_count.max(position_offset + other.token_count);
	}

	fn layer_index(&self, name: &str) -> Option<usize> {
		self.layers.iter().position(|l| l == name)
	}

	pub fn layer_names(&self) -> &[String] {
		&self.layers
	}

	pub fn layer_data(&self, name: &str) -> Option<&[Value]> {
		let idx = self.layer_index(name)?;
		Some(&self.data[idx])
	}
}

impl Default for InMemoryForward {
	fn default() -> Self {
		Self::new()
	}
}

impl ForwardIndex for InMemoryForward {
	fn token_count(&self) -> u64 {
		self.token_count
	}

	fn get_str(&self, position: Position, layer: &str) -> Option<&str> {
		let layer_idx = self.layer_index(layer)?;
		match self.data[layer_idx].get(position as usize)? {
			Value::Str(s) => Some(s.as_str()),
			Value::Int(_) => None,
		}
	}

	fn get_int(&self, position: Position, layer: &str) -> Option<i64> {
		let layer_idx = self.layer_index(layer)?;
		match self.data[layer_idx].get(position as usize)? {
			Value::Int(n) => Some(*n),
			Value::Str(_) => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn basic_forward_index() {
		let mut index = InMemoryForward::new();
		let word_layer = index.add_layer("word");

		index.set(word_layer, 0, "the".into());
		index.set(word_layer, 1, "cat".into());
		index.set(word_layer, 2, "sat".into());

		assert_eq!(index.get_str(0, "word"), Some("the"));
		assert_eq!(index.get_str(1, "word"), Some("cat"));
		assert_eq!(index.token_count(), 3);
	}

	#[test]
	fn get_str_returns_string_values() {
		let mut index = InMemoryForward::new();
		let word_layer = index.add_layer("word");

		index.set(word_layer, 0, "the".into());
		index.set(word_layer, 1, "cat".into());

		assert_eq!(index.get_str(0, "word"), Some("the"));
		assert_eq!(index.get_str(1, "word"), Some("cat"));
		assert_eq!(index.get_str(5, "word"), None);
		assert_eq!(index.get_str(0, "nonexistent"), None);
	}

	#[test]
	fn get_int_returns_integer_values() {
		let mut index = InMemoryForward::new();
		let head_layer = index.add_layer("head");

		index.set(head_layer, 0, Value::Int(3));
		index.set(head_layer, 1, Value::Int(0));

		assert_eq!(index.get_int(0, "head"), Some(3));
		assert_eq!(index.get_int(1, "head"), Some(0));
		assert_eq!(index.get_int(5, "head"), None);
	}

	#[test]
	fn get_str_returns_none_for_int() {
		let mut index = InMemoryForward::new();
		let head_layer = index.add_layer("head");

		index.set(head_layer, 0, Value::Int(3));

		assert_eq!(index.get_str(0, "head"), None);
	}

	#[test]
	fn get_int_returns_none_for_str() {
		let mut index = InMemoryForward::new();
		let word_layer = index.add_layer("word");

		index.set(word_layer, 0, "the".into());

		assert_eq!(index.get_int(0, "word"), None);
	}
}
