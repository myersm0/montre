use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use montre_core::{layers, Span, Value};
use montre_index::corpus::CorpusMeta;
use montre_index::forward::InMemoryForward;
use montre_index::inverted::InMemoryInverted;
use montre_index::lexicon::InMemoryLexicon;
use montre_index::spans::InMemorySpans;
use montre_index::{ForwardIndex, SpanIndex};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::format::conllu::ConllUReader;
use crate::format::{CorpusReader, ParsedSentence};
use crate::Result;

pub struct IndexSink {
	pub(crate) inverted: InMemoryInverted,
	pub(crate) forward: InMemoryForward,
	pub(crate) spans: InMemorySpans,
	pub(crate) lexicon: InMemoryLexicon,
	pub(crate) layer_indices: Vec<(String, usize)>,
	pub(crate) current_position: u64,
	pub(crate) document_names: Vec<String>,
	pub(crate) decompose_feats: bool,
}

impl IndexSink {
	pub fn new() -> Self {
		let mut forward = InMemoryForward::new();
		let mut layer_indices = Vec::new();

		for &layer_name in &[layers::WORD, layers::LEMMA, layers::POS, layers::XPOS, layers::FEATS, layers::DEPREL] {
			let idx = forward.add_layer(layer_name);
			layer_indices.push((layer_name.to_string(), idx));
		}

		Self {
			inverted: InMemoryInverted::new(),
			forward,
			spans: InMemorySpans::new(),
			lexicon: InMemoryLexicon::new(),
			layer_indices,
			current_position: 0,
			document_names: Vec::new(),
			decompose_feats: false,
		}
	}

	pub fn with_decompose_feats(mut self, enabled: bool) -> Self {
		self.decompose_feats = enabled;
		self
	}

	pub fn merge_from(&mut self, other: Self) {
		let offset = self.current_position;
		self.inverted.merge_from(other.inverted, offset as u32);
		self.forward.merge_from(other.forward, offset);
		self.spans.merge_from(other.spans, offset);
		self.lexicon.merge_from(other.lexicon);
		self.document_names.extend(other.document_names);
		for (name, _) in &other.layer_indices {
			self.ensure_layer(name);
		}
		self.current_position += other.current_position;
	}

	pub fn add_document(&mut self, doc_name: &str, sentences: Vec<ParsedSentence>) {
		let doc_start = self.current_position;

		for sentence in sentences {
			let sent_start = self.current_position;

			for token in &sentence.tokens {
				let position = self.current_position;

				self.add_token_annotation(position, layers::WORD, &token.word);

				if let Some(ref lemma) = token.lemma {
					self.add_token_annotation(position, layers::LEMMA, lemma);
				}

				if let Some(ref pos) = token.pos {
					self.add_token_annotation(position, layers::POS, pos);
				}

				if let Some(ref xpos) = token.xpos {
					self.add_token_annotation(position, layers::XPOS, xpos);
				}

				if let Some(ref feats) = token.feats {
					self.add_token_annotation(position, layers::FEATS, feats);
					if self.decompose_feats {
						self.decompose_feats_annotation(position, feats);
					}
				}

				if let Some(ref deprel) = token.deprel {
					self.add_token_annotation(position, layers::DEPREL, deprel);
				}

				self.current_position += 1;
			}

			let sent_end = self.current_position;
			if sent_end > sent_start {
				self.spans.add_span("sentence", Span::new(sent_start, sent_end));
			}
		}

		let doc_end = self.current_position;
		if doc_end > doc_start {
			self.spans.add_span("document", Span::new(doc_start, doc_end));
			self.document_names.push(doc_name.to_string());
		}
	}

	fn decompose_feats_annotation(&mut self, position: u64, feats: &str) {
		for pair in feats.split('|') {
			if let Some((key, value)) = pair.split_once('=') {
				let layer_name = format!("feats.{}", key);
				self.ensure_layer(&layer_name);
				self.add_token_annotation(position, &layer_name, value);
			}
		}
	}

	fn ensure_layer(&mut self, name: &str) {
		if self.layer_indices.iter().any(|(n, _)| n == name) {
			return;
		}
		let idx = self.forward.add_layer(name);
		self.layer_indices.push((name.to_string(), idx));
	}

	fn add_token_annotation(&mut self, position: u64, layer: &str, value: &str) {
		self.inverted.insert(layer, value, [position]);

		if let Some((_, layer_idx)) = self.layer_indices.iter().find(|(name, _)| name == layer) {
			self.forward.set(*layer_idx, position, Value::from(value));
		}

		self.lexicon.add_term(layer, value);
	}

	pub fn write(mut self, path: &Path, meta: CorpusMeta) -> Result<()> {
		self.spans.finalize();

		if path.exists() {
			std::fs::remove_dir_all(path)?;
		}
		std::fs::create_dir_all(path)?;

		let meta_json = serde_json::to_string_pretty(&meta)?;
		std::fs::write(path.join("corpus.json"), meta_json)?;

		let inverted_bytes = bincode::serialize(&self.inverted)
			.map_err(|e| crate::BuildError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
		std::fs::write(path.join("inverted.bin"), &inverted_bytes)?;

		let forward_bytes = bincode::serialize(&self.forward)
			.map_err(|e| crate::BuildError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
		std::fs::write(path.join("forward.bin"), &forward_bytes)?;

		let spans_bytes = bincode::serialize(&self.spans)
			.map_err(|e| crate::BuildError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
		std::fs::write(path.join("spans.bin"), spans_bytes)?;

		let lexicon_bytes = bincode::serialize(&self.lexicon)
			.map_err(|e| crate::BuildError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
		std::fs::write(path.join("lexicon.bin"), lexicon_bytes)?;

		tracing::info!(
			"Wrote corpus: {} tokens, {} documents, {} bytes inverted, {} bytes forward",
			meta.token_count,
			meta.document_names.len(),
			inverted_bytes.len(),
			forward_bytes.len()
		);

		Ok(())
	}
}

pub fn build_from_directory(
	dir: &Path,
	decompose_feats: bool,
	strict: bool,
) -> Result<IndexSink> {
	let mut files: Vec<_> = WalkDir::new(dir)
		.into_iter()
		.filter_map(|e| e.ok())
		.filter(|e| {
			e.path()
				.extension()
				.map(|ext| ext == "conllu")
				.unwrap_or(false)
		})
		.map(|e| e.path().to_path_buf())
		.collect();

	files.sort();

	let file_sinks: Vec<IndexSink> = files
		.par_iter()
		.map(|path| {
			let file = File::open(path)?;
			let reader = BufReader::new(file);
			let mut conllu = ConllUReader::new(reader).with_source_name(
				path.file_name()
					.map(|s| s.to_string_lossy().to_string())
					.unwrap_or_else(|| "unknown".into()),
			);

			let result = if strict {
				conllu.read_sentences_strict()
			} else {
				conllu.read_sentences()
			};

			match result {
				Ok(sentences) => {
					let doc_name = path
						.file_name()
						.map(|s| s.to_string_lossy().to_string())
						.unwrap_or_else(|| "unknown".into());
					let mut sink = IndexSink::new().with_decompose_feats(decompose_feats);
					sink.add_document(&doc_name, sentences);
					Ok(sink)
				}
				Err(e) => {
					if strict {
						Err(e)
					} else {
						tracing::warn!("Skipping {:?}: {}", path, e);
						Ok(IndexSink::new().with_decompose_feats(decompose_feats))
					}
				}
			}
		})
		.collect::<Result<Vec<_>>>()?;

	let mut combined = IndexSink::new().with_decompose_feats(decompose_feats);
	for sink in file_sinks {
		if sink.current_position > 0 {
			combined.merge_from(sink);
		}
	}

	Ok(combined)
}

pub struct CorpusBuilder {
	name: String,
	pub(crate) sink: IndexSink,
}

impl CorpusBuilder {
	pub fn new(name: impl Into<String>) -> Self {
		Self {
			name: name.into(),
			sink: IndexSink::new(),
		}
	}

	pub fn from_directory(
		name: impl Into<String>,
		dir: &Path,
		decompose_feats: bool,
		strict: bool,
	) -> Result<Self> {
		let sink = build_from_directory(dir, decompose_feats, strict)?;
		Ok(Self {
			name: name.into(),
			sink,
		})
	}

	pub fn decompose_feats(mut self, enabled: bool) -> Self {
		self.sink.decompose_feats = enabled;
		self
	}

	pub fn add_document(&mut self, doc_name: impl Into<String>, sentences: Vec<ParsedSentence>) {
		self.sink.add_document(&doc_name.into(), sentences);
	}

	pub fn add_sentences(&mut self, sentences: Vec<ParsedSentence>) {
		self.sink.add_document("unknown", sentences);
	}

	pub fn current_position(&self) -> u64 {
		self.sink.current_position
	}

	pub fn document_count(&self) -> usize {
		self.sink.document_names.len()
	}

	pub fn build(self, output_path: impl AsRef<Path>) -> Result<()> {
		let meta = CorpusMeta {
			name: self.name,
			version: montre_index::index_version,
			token_count: self.sink.forward.token_count(),
			layers: self.sink.layer_indices.iter().map(|(n, _)| n.clone()).collect(),
			span_layers: self.sink.spans.layers().into_iter().map(String::from).collect(),
			document_names: self.sink.document_names.clone(),
			components: Vec::new(),
			alignments: Vec::new(),
		};

		self.sink.write(output_path.as_ref(), meta)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::format::conllu::ConllUReader;
	use crate::format::CorpusReader;
	use montre_index::InvertedIndex;
	use std::io::Cursor;

	#[test]
	fn build_from_conllu() {
		let conllu = r#"1	The	the	DET	DT	_	2	det	_	_
2	cat	cat	NOUN	NN	_	3	nsubj	_	_
3	sat	sit	VERB	VBD	_	0	root	_	_
"#;

		let mut reader = ConllUReader::new(Cursor::new(conllu));
		let sentences = reader.read_sentences().unwrap();

		let mut builder = CorpusBuilder::new("test");
		builder.add_document("test_doc.conllu", sentences);

		let positions = builder.sink.inverted.get("word", "cat").unwrap();
		assert!(positions.contains(1));

		let positions = builder.sink.inverted.get("pos", "NOUN").unwrap();
		assert!(positions.contains(1));

		assert_eq!(builder.document_count(), 1);
	}

	#[test]
	fn multi_document() {
		let doc1 = r#"1	Hello	hello	INTJ	UH	_	0	root	_	_
"#;
		let doc2 = r#"1	World	world	NOUN	NN	_	0	root	_	_
"#;

		let mut builder = CorpusBuilder::new("test");

		let mut reader1 = ConllUReader::new(Cursor::new(doc1));
		builder.add_document("doc1.conllu", reader1.read_sentences().unwrap());

		let mut reader2 = ConllUReader::new(Cursor::new(doc2));
		builder.add_document("doc2.conllu", reader2.read_sentences().unwrap());

		assert_eq!(builder.current_position(), 2);
		assert_eq!(builder.document_count(), 2);

		let hello_pos = builder.sink.inverted.get("word", "Hello").unwrap();
		assert!(hello_pos.contains(0));

		let world_pos = builder.sink.inverted.get("word", "World").unwrap();
		assert!(world_pos.contains(1));
	}
}
