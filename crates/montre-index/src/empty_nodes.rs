use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{IndexError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmptyNode {
	pub sentence_index: u32,
	pub node_id: String,
	pub form: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub lemma: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub upos: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub xpos: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub feats: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub deps: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub misc: Option<String>,
}

pub struct EmptyNodeStore {
	nodes: Vec<EmptyNode>,
}

impl EmptyNodeStore {
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		let data = std::fs::read_to_string(path.as_ref())?;
		let nodes: Vec<EmptyNode> = serde_json::from_str(&data)
			.map_err(|e| IndexError::Format(format!("empty_nodes.json: {}", e)))?;
		Ok(Self { nodes })
	}

	pub fn nodes(&self) -> &[EmptyNode] {
		&self.nodes
	}

	pub fn len(&self) -> usize {
		self.nodes.len()
	}

	pub fn is_empty(&self) -> bool {
		self.nodes.is_empty()
	}

	pub fn in_sentence(&self, sentence_index: u32) -> &[EmptyNode] {
		let start = self.nodes.partition_point(|n| n.sentence_index < sentence_index);
		let end = self.nodes[start..].partition_point(|n| n.sentence_index == sentence_index);
		&self.nodes[start..start + end]
	}
}

pub fn write_empty_nodes(nodes: &[EmptyNode], path: impl AsRef<Path>) -> std::io::Result<()> {
	let json = serde_json::to_string_pretty(nodes)
		.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
	std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn roundtrip_empty_nodes() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("empty_nodes.json");

		let nodes = vec![
			EmptyNode {
				sentence_index: 2,
				node_id: "6.1".into(),
				form: "apple".into(),
				lemma: Some("apple".into()),
				upos: Some("NOUN".into()),
				xpos: None,
				feats: Some("Number=Sing".into()),
				deps: Some("2:obj".into()),
				misc: None,
			},
			EmptyNode {
				sentence_index: 5,
				node_id: "3.1".into(),
				form: "be".into(),
				lemma: Some("be".into()),
				upos: Some("AUX".into()),
				xpos: None,
				feats: None,
				deps: Some("0:root".into()),
				misc: None,
			},
		];

		write_empty_nodes(&nodes, &path).unwrap();
		let store = EmptyNodeStore::open(&path).unwrap();

		assert_eq!(store.len(), 2);
		assert_eq!(store.nodes()[0].form, "apple");
		assert_eq!(store.nodes()[1].sentence_index, 5);
	}

	#[test]
	fn per_sentence_lookup() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("empty_nodes.json");

		let nodes = vec![
			EmptyNode {
				sentence_index: 2,
				node_id: "4.1".into(),
				form: "a".into(),
				lemma: None, upos: None, xpos: None,
				feats: None, deps: None, misc: None,
			},
			EmptyNode {
				sentence_index: 2,
				node_id: "4.2".into(),
				form: "b".into(),
				lemma: None, upos: None, xpos: None,
				feats: None, deps: None, misc: None,
			},
			EmptyNode {
				sentence_index: 7,
				node_id: "1.1".into(),
				form: "c".into(),
				lemma: None, upos: None, xpos: None,
				feats: None, deps: None, misc: None,
			},
		];

		write_empty_nodes(&nodes, &path).unwrap();
		let store = EmptyNodeStore::open(&path).unwrap();

		let s2 = store.in_sentence(2);
		assert_eq!(s2.len(), 2);
		assert_eq!(s2[0].form, "a");
		assert_eq!(s2[1].form, "b");

		let s7 = store.in_sentence(7);
		assert_eq!(s7.len(), 1);

		let s0 = store.in_sentence(0);
		assert_eq!(s0.len(), 0);
	}

	#[test]
	fn none_fields_omitted_in_json() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("empty_nodes.json");

		let nodes = vec![EmptyNode {
			sentence_index: 0,
			node_id: "1.1".into(),
			form: "x".into(),
			lemma: None, upos: None, xpos: None,
			feats: None, deps: None, misc: None,
		}];

		write_empty_nodes(&nodes, &path).unwrap();
		let json = std::fs::read_to_string(&path).unwrap();
		assert!(!json.contains("lemma"));
		assert!(!json.contains("deps"));
	}
}
