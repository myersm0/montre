use std::collections::HashMap;
use std::path::{Path, PathBuf};

use montre_core::{Span, UnitId};
use serde::{Deserialize, Serialize};

use crate::forward::InMemoryForward;
use crate::inverted::InMemoryInverted;
use crate::lexicon::InMemoryLexicon;
use crate::spans::InMemorySpans;
use crate::{IndexError, Result, SpanIndex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentMeta {
	pub id: u32,
	pub name: String,
	pub language: String,
	pub document_range: (usize, usize),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentMeta {
	pub name: String,
	pub source_component: String,
	pub target_component: String,
	pub source_layer: String,
	pub target_layer: String,
	pub directed: bool,
	pub edge_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusMeta {
	pub name: String,
	pub version: u32,
	pub token_count: u64,
	pub layers: Vec<String>,
	pub span_layers: Vec<String>,
	#[serde(default)]
	pub document_names: Vec<String>,
	#[serde(default)]
	pub components: Vec<ComponentMeta>,
	#[serde(default)]
	pub alignments: Vec<AlignmentMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AlignmentIndex {
	pub alignments: HashMap<String, Vec<(UnitId, UnitId)>>,
}

impl AlignmentIndex {
	pub fn new() -> Self {
		Self {
			alignments: HashMap::new(),
		}
	}

	pub fn add(&mut self, name: &str, edges: Vec<(UnitId, UnitId)>) {
		self.alignments.insert(name.to_string(), edges);
	}

	pub fn get(&self, name: &str) -> Option<&[(UnitId, UnitId)]> {
		self.alignments.get(name).map(|v| v.as_slice())
	}

	pub fn names(&self) -> impl Iterator<Item = &str> {
		self.alignments.keys().map(|s| s.as_str())
	}
}

pub struct Corpus {
	path: PathBuf,
	pub meta: CorpusMeta,
	pub inverted: InMemoryInverted,
	pub forward: InMemoryForward,
	pub spans: InMemorySpans,
	pub lexicon: InMemoryLexicon,
	pub alignments: AlignmentIndex,
}

impl Corpus {
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		let path = path.as_ref();
		if !path.exists() {
			return Err(IndexError::NotFound(path.display().to_string()));
		}

		let meta_path = path.join("corpus.json");
		let meta_str = std::fs::read_to_string(&meta_path)
			.map_err(|e| IndexError::Format(format!("Failed to read corpus.json: {}", e)))?;
		let meta: CorpusMeta = serde_json::from_str(&meta_str)
			.map_err(|e| IndexError::Format(format!("Failed to parse corpus.json: {}", e)))?;

		if meta.version != crate::index_version {
			return Err(IndexError::VersionMismatch {
				expected: crate::index_version,
				found: meta.version,
			});
		}

		let inverted_bytes = std::fs::read(path.join("inverted.bin"))?;
		let inverted: InMemoryInverted = bincode::deserialize(&inverted_bytes)
			.map_err(|e| IndexError::Format(format!("Failed to deserialize inverted index: {}", e)))?;

		let forward_bytes = std::fs::read(path.join("forward.bin"))?;
		let forward: InMemoryForward = bincode::deserialize(&forward_bytes)
			.map_err(|e| IndexError::Format(format!("Failed to deserialize forward index: {}", e)))?;

		let spans_bytes = std::fs::read(path.join("spans.bin"))?;
		let spans: InMemorySpans = bincode::deserialize(&spans_bytes)
			.map_err(|e| IndexError::Format(format!("Failed to deserialize spans index: {}", e)))?;

		let lexicon_bytes = std::fs::read(path.join("lexicon.bin"))?;
		let lexicon: InMemoryLexicon = bincode::deserialize(&lexicon_bytes)
			.map_err(|e| IndexError::Format(format!("Failed to deserialize lexicon: {}", e)))?;

		let alignments = if path.join("alignments.bin").exists() {
			let align_bytes = std::fs::read(path.join("alignments.bin"))?;
			bincode::deserialize(&align_bytes)
				.map_err(|e| IndexError::Format(format!("Failed to deserialize alignments: {}", e)))?
		} else {
			AlignmentIndex::new()
		};

		Ok(Self {
			path: path.to_path_buf(),
			meta,
			inverted,
			forward,
			spans,
			lexicon,
			alignments,
		})
	}

	pub fn path(&self) -> &Path {
		&self.path
	}

	pub fn name(&self) -> &str {
		&self.meta.name
	}

	pub fn token_count(&self) -> u64 {
		self.meta.token_count
	}

	pub fn layers(&self) -> &[String] {
		&self.meta.layers
	}

	pub fn span_layers(&self) -> &[String] {
		&self.meta.span_layers
	}

	pub fn document_names(&self) -> &[String] {
		&self.meta.document_names
	}

	pub fn document_at(&self, position: u64) -> Option<&str> {
		let doc_spans = self.spans.spans("document")?;
		for (i, span) in doc_spans.iter().enumerate() {
			if position >= span.start && position < span.end {
				return self.meta.document_names.get(i).map(|s| s.as_str());
			}
		}
		None
	}

	pub fn components(&self) -> &[ComponentMeta] {
		&self.meta.components
	}

	pub fn component(&self, name: &str) -> Option<&ComponentMeta> {
		self.meta.components.iter().find(|c| c.name == name)
	}

	pub fn component_for_document(&self, doc_index: usize) -> Option<&ComponentMeta> {
		self.meta.components.iter().find(|c| {
			doc_index >= c.document_range.0 && doc_index < c.document_range.1
		})
	}

	pub fn is_multi_component(&self) -> bool {
		self.meta.components.len() > 1
	}

	pub fn alignment_meta(&self, name: &str) -> Option<&AlignmentMeta> {
		self.meta.alignments.iter().find(|a| a.name == name)
	}

	pub fn alignment_edges(&self, name: &str) -> Option<&[(UnitId, UnitId)]> {
		self.alignments.get(name)
	}

	pub fn document_span(&self, doc_index: usize) -> Option<Span> {
		self.spans.spans("document").and_then(|spans| spans.get(doc_index).copied())
	}

	pub fn sentence_span(&self, sent_index: usize) -> Option<Span> {
		self.spans.spans("sentence").and_then(|spans| spans.get(sent_index).copied())
	}

	pub fn sentences_in_document(&self, doc_index: usize) -> Option<Vec<(usize, Span)>> {
		let doc_span = self.document_span(doc_index)?;
		let sent_spans = self.spans.spans("sentence")?;

		let mut result = Vec::new();
		for (i, span) in sent_spans.iter().enumerate() {
			if span.start >= doc_span.start && span.end <= doc_span.end {
				result.push((i, *span));
			} else if span.start >= doc_span.end {
				break;
			}
		}
		Some(result)
	}
}
