use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub trait Lexicon {
	fn term_to_id(&self, layer: &str, term: &str) -> Option<u32>;
	fn id_to_term(&self, layer: &str, id: u32) -> Option<&str>;
	fn term_count(&self, layer: &str) -> usize;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InMemoryLexicon {
	to_id: HashMap<String, HashMap<String, u32>>,
	to_term: HashMap<String, Vec<String>>,
}

impl InMemoryLexicon {
	pub fn new() -> Self {
		Self {
			to_id: HashMap::new(),
			to_term: HashMap::new(),
		}
	}

	pub fn add_term(&mut self, layer: &str, term: &str) -> u32 {
		let layer_to_id = self.to_id.entry(layer.to_string()).or_default();
		let layer_to_term = self.to_term.entry(layer.to_string()).or_default();

		if let Some(&id) = layer_to_id.get(term) {
			return id;
		}

		let id = layer_to_term.len() as u32;
		layer_to_id.insert(term.to_string(), id);
		layer_to_term.push(term.to_string());
		id
	}
}

impl Default for InMemoryLexicon {
	fn default() -> Self {
		Self::new()
	}
}

impl Lexicon for InMemoryLexicon {
	fn term_to_id(&self, layer: &str, term: &str) -> Option<u32> {
		self.to_id.get(layer)?.get(term).copied()
	}

	fn id_to_term(&self, layer: &str, id: u32) -> Option<&str> {
		self.to_term
			.get(layer)?
			.get(id as usize)
			.map(|s| s.as_str())
	}

	fn term_count(&self, layer: &str) -> usize {
		self.to_term.get(layer).map(|v| v.len()).unwrap_or(0)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn lexicon_roundtrip() {
		let mut lex = InMemoryLexicon::new();

		let id_the = lex.add_term("word", "the");
		let id_cat = lex.add_term("word", "cat");
		let id_the2 = lex.add_term("word", "the");

		assert_eq!(id_the, id_the2);
		assert_ne!(id_the, id_cat);

		assert_eq!(lex.id_to_term("word", id_the), Some("the"));
		assert_eq!(lex.id_to_term("word", id_cat), Some("cat"));
		assert_eq!(lex.term_to_id("word", "the"), Some(id_the));
	}
}
