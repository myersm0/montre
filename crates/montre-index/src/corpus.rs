use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::forward::InMemoryForward;
use crate::inverted::InMemoryInverted;
use crate::lexicon::InMemoryLexicon;
use crate::spans::InMemorySpans;
use crate::{IndexError, Result, SpanIndex};

#[derive(Debug, Clone, Deserialize)]
pub struct CorpusMeta {
	pub name: String,
	pub version: u32,
	pub token_count: u64,
	pub layers: Vec<String>,
	pub span_layers: Vec<String>,
	#[serde(default)]
	pub document_names: Vec<String>,
}

pub struct Corpus {
	path: PathBuf,
	pub meta: CorpusMeta,
	pub inverted: InMemoryInverted,
	pub forward: InMemoryForward,
	pub spans: InMemorySpans,
	pub lexicon: InMemoryLexicon,
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

		Ok(Self {
			path: path.to_path_buf(),
			meta,
			inverted,
			forward,
			spans,
			lexicon,
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
}
