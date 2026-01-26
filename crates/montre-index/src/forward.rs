use montre_core::{Position, Value};
use serde::{Deserialize, Serialize};

pub trait ForwardIndex {
	fn get(&self, position: Position, layer: &str) -> Option<&Value>;
	fn get_range(&self, start: Position, end: Position, layer: &str) -> Vec<Option<&Value>>;
	fn token_count(&self) -> u64;
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

	fn layer_index(&self, name: &str) -> Option<usize> {
		self.layers.iter().position(|l| l == name)
	}
}

impl Default for InMemoryForward {
	fn default() -> Self {
		Self::new()
	}
}

impl ForwardIndex for InMemoryForward {
	fn get(&self, position: Position, layer: &str) -> Option<&Value> {
		let layer_idx = self.layer_index(layer)?;
		self.data[layer_idx].get(position as usize)
	}

	fn get_range(&self, start: Position, end: Position, layer: &str) -> Vec<Option<&Value>> {
		let layer_idx = match self.layer_index(layer) {
			Some(idx) => idx,
			None => return vec![None; (end - start) as usize],
		};
		(start..end)
			.map(|pos| self.data[layer_idx].get(pos as usize))
			.collect()
	}

	fn token_count(&self) -> u64 {
		self.token_count
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

		assert_eq!(index.get(0, "word"), Some(&Value::from("the")));
		assert_eq!(index.get(1, "word"), Some(&Value::from("cat")));
		assert_eq!(index.token_count(), 3);
	}
}
