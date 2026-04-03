use std::collections::HashMap;
use std::path::{Path, PathBuf};

use montre_core::{Span, UnitId, span_containing};
use rayon;
use serde::{Deserialize, Serialize};

use crate::empty_nodes::EmptyNodeStore;
use crate::forward_flat::{MappedForward, ForwardStore};
use crate::inverted::InMemoryInverted;
use crate::lexicon::InMemoryLexicon;
use crate::mwt::MappedMWTs;
use crate::sentence_ids::MappedSentenceIds;
use crate::spacing::SpacingIndex;
use crate::spans_flat::{MappedSpans, SpanStore};
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
	meta: CorpusMeta,
	inverted: InMemoryInverted,
	forward: ForwardStore,
	spans: SpanStore,
	lexicon: InMemoryLexicon,
	alignments: AlignmentIndex,
	sentence_ids: Option<MappedSentenceIds>,
	mwts: Option<MappedMWTs>,
	spacing: Option<SpacingIndex>,
	empty_nodes: Option<EmptyNodeStore>,
}

fn load_indexes(
	path: &Path,
) -> Result<(InMemoryInverted, InMemoryLexicon)> {
	let (inverted, lexicon) = rayon::join(
		|| -> Result<InMemoryInverted> {
			let bytes = std::fs::read(path.join("inverted.bin"))?;
			bincode::deserialize(&bytes)
				.map_err(|e| IndexError::Format(format!("Failed to deserialize inverted index: {}", e)))
		},
		|| -> Result<InMemoryLexicon> {
			let bytes = std::fs::read(path.join("lexicon.bin"))?;
			bincode::deserialize(&bytes)
				.map_err(|e| IndexError::Format(format!("Failed to deserialize lexicon: {}", e)))
		},
	);

	Ok((inverted?, lexicon?))
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

		let (inverted, lexicon) = load_indexes(path)?;
		let spans = SpanStore::Mapped(MappedSpans::open(path.join("spans.bin"))?);
		let forward = ForwardStore::Mapped(MappedForward::open(path.join("forward.bin"))?);

		let alignments = if path.join("alignments.bin").exists() {
			let align_bytes = std::fs::read(path.join("alignments.bin"))?;
			bincode::deserialize(&align_bytes)
				.map_err(|e| IndexError::Format(format!("Failed to deserialize alignments: {}", e)))?
		} else {
			AlignmentIndex::new()
		};

		let sentence_ids = if path.join("sentence_ids.bin").exists() {
			Some(MappedSentenceIds::open(path.join("sentence_ids.bin"))?)
		} else {
			None
		};

		let mwts = if path.join("mwt.bin").exists() {
			Some(MappedMWTs::open(path.join("mwt.bin"))?)
		} else {
			None
		};

		let spacing = if path.join("spacing.bin").exists() {
			Some(SpacingIndex::open(path.join("spacing.bin"))?)
		} else {
			None
		};

		let empty_nodes = if path.join("empty_nodes.json").exists() {
			Some(EmptyNodeStore::open(path.join("empty_nodes.json"))?)
		} else {
			None
		};

		Ok(Self {
			path: path.to_path_buf(),
			meta,
			inverted,
			forward,
			spans,
			lexicon,
			alignments,
			sentence_ids,
			mwts,
			spacing,
			empty_nodes,
		})
	}

	pub fn path(&self) -> &Path {
		&self.path
	}

	pub fn meta(&self) -> &CorpusMeta {
		&self.meta
	}

	pub fn inverted(&self) -> &InMemoryInverted {
		&self.inverted
	}

	pub fn forward(&self) -> &ForwardStore {
		&self.forward
	}

	pub fn spans(&self) -> &SpanStore {
		&self.spans
	}

	pub fn alignment_metas(&self) -> &[AlignmentMeta] {
		&self.meta.alignments
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
		let idx = span_containing(doc_spans, position)?;
		self.meta.document_names.get(idx).map(|s| s.as_str())
	}

	pub fn document_index_by_name(&self, name: &str) -> Option<usize> {
		self.meta.document_names.iter().position(|n| n == name)
	}

	pub fn document_indices_by_name(&self, names: &[String]) -> Vec<usize> {
		self.meta
			.document_names
			.iter()
			.enumerate()
			.filter(|(_, n)| names.iter().any(|name| name == n.as_str()))
			.map(|(idx, _)| idx)
			.collect()
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

		let first = sent_spans.partition_point(|s| s.start < doc_span.start);

		let mut result = Vec::new();
		for i in first..sent_spans.len() {
			let span = sent_spans[i];
			if span.start >= doc_span.end {
				break;
			}
			if span.end <= doc_span.end {
				result.push((i, span));
			}
		}
		Some(result)
	}

	pub fn sentence_id(&self, sentence_index: usize) -> Option<&str> {
		self.sentence_ids.as_ref()?.get(sentence_index)
	}

	pub fn sentence_id_count(&self) -> usize {
		self.sentence_ids.as_ref().map_or(0, |s| s.len())
	}

	pub fn mwt_covering(&self, position: u64) -> Option<crate::mwt::MWTEntry> {
		self.mwts.as_ref()?.covering(position)
	}

	pub fn mwts_in_range(&self, start: u64, end: u64) -> Vec<crate::mwt::MWTEntry> {
		match self.mwts.as_ref() {
			Some(m) => m.in_range(start, end),
			None => Vec::new(),
		}
	}

	pub fn has_no_space_after(&self, position: u64) -> bool {
		self.spacing.as_ref().map_or(false, |s| s.has_no_space_after(position))
	}

	pub fn empty_nodes(&self) -> Option<&EmptyNodeStore> {
		self.empty_nodes.as_ref()
	}

	pub fn surface_text(&self, start: u64, end: u64) -> String {
		use crate::ForwardIndex;
		let mwts = self.mwts_in_range(start, end);
		let mut result = String::new();
		let mut suppress_space = true;
		let mut pos = start;
		while pos < end {
			if let Some(mwt) = mwts.iter().find(|m| m.start == pos) {
				if !suppress_space {
					result.push(' ');
				}
				result.push_str(&mwt.form);
				suppress_space = mwt.no_space_after;
				pos = mwt.end;
			} else if mwts.iter().any(|m| pos > m.start && pos < m.end) {
				pos += 1;
			} else {
				if !suppress_space {
					result.push(' ');
				}
				if let Some(word) = self.forward.get_str(pos, "word") {
					result.push_str(word);
				}
				suppress_space = self.has_no_space_after(pos);
				pos += 1;
			}
		}
		result
	}
}
