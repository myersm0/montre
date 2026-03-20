use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Result;

/// Corpus build manifest - describes a multi-component corpus
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
	pub corpus: CorpusConfig,

	#[serde(default)]
	pub components: HashMap<String, ComponentConfig>,

	#[serde(default)]
	pub span_layers: HashMap<String, SpanLayerConfig>,

	#[serde(default)]
	pub alignments: HashMap<String, AlignmentConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CorpusConfig {
	pub name: String,

	#[serde(default)]
	pub decompose_feats: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComponentConfig {
	pub path: PathBuf,

	#[serde(default)]
	pub language: Option<String>,

	#[serde(default)]
	pub format: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SpanLayerConfig {
	Simple(String),
	Detailed {
		source: String,
		#[serde(default)]
		pattern: Option<String>,
	},
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlignmentConfig {
	pub source: String,
	pub target: String,

	#[serde(default = "default_layer")]
	pub source_layer: String,

	#[serde(default = "default_layer")]
	pub target_layer: String,

	pub edges: PathBuf,

	#[serde(default = "default_true")]
	pub directed: bool,
}

fn default_layer() -> String {
	"sentence".to_string()
}

fn default_true() -> bool {
	true
}

impl Manifest {
	pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
		let content = std::fs::read_to_string(path.as_ref())?;
		Self::from_str(&content)
	}

	pub fn from_str(content: &str) -> Result<Self> {
		toml::from_str(content).map_err(|e| crate::BuildError::Manifest(e.to_string()))
	}

	pub fn is_single_component(&self) -> bool {
		self.components.len() <= 1
	}

	pub fn component_names(&self) -> Vec<&str> {
		self.components.keys().map(|s| s.as_str()).collect()
	}

	pub fn alignment_names(&self) -> Vec<&str> {
		self.alignments.keys().map(|s| s.as_str()).collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_simple_manifest() {
		let toml = r#"
[corpus]
name = "maupassant"

[components.fr]
path = "data/fr/conllu"
language = "fr"

[components.en]
path = "data/en/conllu"
language = "en"

[span_layers]
sentence = "auto"
paragraph = "blank_line"

[alignments.labse]
source = "fr"
target = "en"
source_layer = "sentence"
target_layer = "sentence"
edges = "alignments/labse.tsv"
"#;

		let manifest = Manifest::from_str(toml).unwrap();

		assert_eq!(manifest.corpus.name, "maupassant");
		assert_eq!(manifest.components.len(), 2);
		assert!(manifest.components.contains_key("fr"));
		assert!(manifest.components.contains_key("en"));

		assert_eq!(manifest.components["fr"].language, Some("fr".to_string()));
		assert_eq!(manifest.components["en"].path, PathBuf::from("data/en/conllu"));

		assert_eq!(manifest.alignments.len(), 1);
		assert!(manifest.alignments.contains_key("labse"));

		let labse = &manifest.alignments["labse"];
		assert_eq!(labse.source, "fr");
		assert_eq!(labse.target, "en");
		assert_eq!(labse.source_layer, "sentence");
		assert!(labse.directed);
	}

	#[test]
	fn parse_isosceles_manifest() {
		let toml = r#"
[corpus]
name = "isosceles"

[components.poe-1845]
path = "data/poe/1845/conllu"
language = "en"

[components.poe-1850]
path = "data/poe/1850/conllu"
language = "en"

[components.baudelaire]
path = "data/poe/baudelaire/conllu"
language = "fr"

[span_layers]
sentence = "auto"

[alignments.poe1845_baud]
source = "poe-1845"
target = "baudelaire"
edges = "alignments/poe1845_baud.tsv"

[alignments.poe1850_baud]
source = "poe-1850"
target = "baudelaire"
edges = "alignments/poe1850_baud.tsv"

[alignments.poe_editions]
source = "poe-1845"
target = "poe-1850"
edges = "alignments/poe_editions.tsv"
"#;

		let manifest = Manifest::from_str(toml).unwrap();

		assert_eq!(manifest.corpus.name, "isosceles");
		assert_eq!(manifest.components.len(), 3);
		assert_eq!(manifest.alignments.len(), 3);

		assert!(manifest.alignments.contains_key("poe1845_baud"));
		assert!(manifest.alignments.contains_key("poe1850_baud"));
		assert!(manifest.alignments.contains_key("poe_editions"));
	}

	#[test]
	fn defaults_applied() {
		let toml = r#"
[corpus]
name = "test"

[alignments.simple]
source = "a"
target = "b"
edges = "align.tsv"
"#;

		let manifest = Manifest::from_str(toml).unwrap();
		let align = &manifest.alignments["simple"];

		assert_eq!(align.source_layer, "sentence");
		assert_eq!(align.target_layer, "sentence");
		assert!(align.directed);
	}
}
