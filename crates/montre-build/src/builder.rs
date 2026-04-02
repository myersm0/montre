use std::collections::HashSet;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use montre_core::{layers, Span, Value};
use montre_index::corpus::CorpusMeta;
use montre_index::forward::InMemoryForward;
use montre_index::inverted::InMemoryInverted;
use montre_index::lexicon::InMemoryLexicon;
use montre_index::spans::InMemorySpans;
use montre_index::SpanIndex;
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::format::conllu::ConllUReader;
use crate::format::{CorpusReader, ParsedSentence};
use crate::Result;

fn default_forward_only_layers() -> HashSet<String> {
	let mut set = HashSet::new();
	set.insert(layers::HEAD.to_string());
	set
}

pub struct IndexSink {
	pub(crate) inverted: InMemoryInverted,
	pub(crate) forward: Option<InMemoryForward>,
	pub(crate) spans: InMemorySpans,
	pub(crate) lexicon: InMemoryLexicon,
	pub(crate) layer_indices: Vec<(String, usize)>,
	pub(crate) current_position: u64,
	pub(crate) document_names: Vec<String>,
	pub(crate) sentence_ids: Vec<String>,
	pub(crate) forward_only_layers: HashSet<String>,
	pub(crate) decompose_feats: bool,
}

impl IndexSink {
	pub fn new() -> Self {
		let mut forward = InMemoryForward::new();
		let mut layer_indices = Vec::new();

		for &layer_name in &[layers::WORD, layers::LEMMA, layers::UPOS, layers::XPOS, layers::FEATS, layers::HEAD, layers::DEPREL] {
			let idx = forward.add_layer(layer_name);
			layer_indices.push((layer_name.to_string(), idx));
		}

		Self {
			inverted: InMemoryInverted::new(),
			forward: Some(forward),
			spans: InMemorySpans::new(),
			lexicon: InMemoryLexicon::new(),
			layer_indices,
			current_position: 0,
			document_names: Vec::new(),
			sentence_ids: Vec::new(),
			forward_only_layers: default_forward_only_layers(),
			decompose_feats: false,
		}
	}

	pub fn new_without_forward() -> Self {
		let layer_indices: Vec<(String, usize)> =
			[layers::WORD, layers::LEMMA, layers::UPOS, layers::XPOS, layers::FEATS, layers::HEAD, layers::DEPREL]
				.iter()
				.enumerate()
				.map(|(i, &name)| (name.to_string(), i))
				.collect();

		Self {
			inverted: InMemoryInverted::new(),
			forward: None,
			spans: InMemorySpans::new(),
			lexicon: InMemoryLexicon::new(),
			layer_indices,
			current_position: 0,
			document_names: Vec::new(),
			sentence_ids: Vec::new(),
			forward_only_layers: default_forward_only_layers(),
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
		if let (Some(ref mut self_fwd), Some(other_fwd)) = (&mut self.forward, other.forward) {
			self_fwd.merge_from(other_fwd, offset);
		}
		self.spans.merge_from(other.spans, offset);
		self.lexicon.merge_from(other.lexicon);
		self.document_names.extend(other.document_names);
		self.sentence_ids.extend(other.sentence_ids);
		for (name, _) in &other.layer_indices {
			self.ensure_layer(name);
		}
		self.current_position += other.current_position;
	}

	pub fn take_forward(&mut self) -> Option<InMemoryForward> {
		self.forward.take()
	}

	pub fn add_document(&mut self, doc_name: &str, sentences: Vec<ParsedSentence>) {
		let doc_start = self.current_position;
		let mut sent_index_within_doc: u32 = 0;

		for sentence in sentences {
			let sent_start = self.current_position;

			for token in &sentence.tokens {
				let position = self.current_position;

				self.add_token_annotation(position, layers::WORD, &token.word);

				if let Some(ref lemma) = token.lemma {
					self.add_token_annotation(position, layers::LEMMA, lemma);
				}

				if let Some(ref upos) = token.upos {
					self.add_token_annotation(position, layers::UPOS, upos);
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

				if let Some(head) = token.head {
					self.add_token_int_annotation(position, layers::HEAD, head);
				}

				if let Some(ref deprel) = token.deprel {
					self.add_token_annotation(position, layers::DEPREL, deprel);
				}

				self.current_position += 1;
			}

			let sent_end = self.current_position;
			if sent_end > sent_start {
				self.spans.add_span("sentence", Span::new(sent_start, sent_end));
				let id = sentence.sent_id.unwrap_or_else(|| {
					format!("{}:{}", doc_name, sent_index_within_doc)
				});
				self.sentence_ids.push(id);
				sent_index_within_doc += 1;
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
		let idx = match self.forward {
			Some(ref mut fwd) => fwd.add_layer(name),
			None => 0,
		};
		self.layer_indices.push((name.to_string(), idx));
	}

	fn add_token_annotation(&mut self, position: u64, layer: &str, value: &str) {
		if !self.forward_only_layers.contains(layer) {
			self.inverted.insert(layer, value, [position]);
			self.lexicon.add_term(layer, value);
		}

		if let Some(ref mut fwd) = self.forward {
			if let Some((_, layer_idx)) = self.layer_indices.iter().find(|(name, _)| name == layer) {
				fwd.set(*layer_idx, position, Value::from(value));
			}
		}
	}

	fn add_token_int_annotation(&mut self, position: u64, layer: &str, value: i64) {
		if let Some(ref mut fwd) = self.forward {
			if let Some((_, layer_idx)) = self.layer_indices.iter().find(|(name, _)| name == layer) {
				fwd.set(*layer_idx, position, Value::Int(value));
			}
		}
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

		if let Some(ref forward) = self.forward {
			montre_index::write_flat_forward(forward, &path.join("forward.bin"))?;
		}

		montre_index::write_flat_spans(&self.spans, &path.join("spans.bin"))?;

		let lexicon_bytes = bincode::serialize(&self.lexicon)
			.map_err(|e| crate::BuildError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
		std::fs::write(path.join("lexicon.bin"), lexicon_bytes)?;

		if !self.sentence_ids.is_empty() {
			let sentence_ids_bytes = bincode::serialize(&self.sentence_ids)
				.map_err(|e| crate::BuildError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
			std::fs::write(path.join("sentence_ids.bin"), sentence_ids_bytes)?;
		}

		tracing::info!(
			"Wrote corpus: {} tokens, {} documents, {} bytes inverted",
			meta.token_count,
			meta.document_names.len(),
			inverted_bytes.len()
		);

		Ok(())
	}
}

pub fn build_from_directory_streaming(
	dir: &Path,
	decompose_feats: bool,
	strict: bool,
	streaming_forward: &mut crate::streaming_forward::StreamingForwardWriter,
	position_offset: u64,
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

	let mut combined = IndexSink::new_without_forward().with_decompose_feats(decompose_feats);
	for mut sink in file_sinks {		if sink.current_position > 0 {
			if let Some(forward) = sink.take_forward() {
				let offset = position_offset + combined.current_position;
				streaming_forward.append_from(forward, offset)?;
			}
			combined.merge_from(sink);
		}
	}

	Ok(combined)
}

pub struct CorpusBuilder {
	name: String,
	pub(crate) sink: IndexSink,
	streaming_forward: Option<crate::streaming_forward::StreamingForwardWriter>,
}

impl CorpusBuilder {
	pub fn new(name: impl Into<String>) -> Self {
		Self {
			name: name.into(),
			sink: IndexSink::new(),
			streaming_forward: None,
		}
	}

	pub fn from_directory(
		name: impl Into<String>,
		dir: &Path,
		decompose_feats: bool,
		strict: bool,
	) -> Result<Self> {
		use std::sync::atomic::{AtomicU64, Ordering};
		static COUNTER: AtomicU64 = AtomicU64::new(0);

		let build_id = COUNTER.fetch_add(1, Ordering::SeqCst);
		let temp_dir = std::env::temp_dir().join(format!(
			"montre_cb_{}_{}", std::process::id(), build_id
		));
		let mut streaming_forward = crate::streaming_forward::StreamingForwardWriter::new(&temp_dir)?;
		let sink = build_from_directory_streaming(dir, decompose_feats, strict, &mut streaming_forward, 0)?;
		Ok(Self {
			name: name.into(),
			sink,
			streaming_forward: Some(streaming_forward),
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

	pub fn build(mut self, output_path: impl AsRef<Path>) -> Result<()> {
		let meta = CorpusMeta {
			name: self.name,
			version: montre_index::index_version,
			token_count: self.sink.current_position,
			layers: self.sink.layer_indices.iter().map(|(n, _)| n.clone()).collect(),
			span_layers: self.sink.spans.layers().into_iter().map(String::from).collect(),
			document_names: self.sink.document_names.clone(),
			components: Vec::new(),
			alignments: Vec::new(),
		};

		let path = output_path.as_ref();
		self.sink.write(path, meta)?;

		if let Some(ref mut streaming) = self.streaming_forward {
			streaming.finalize(&path.join("forward.bin"))?;
		}

		Ok(())
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

		let positions = builder.sink.inverted.get("upos", "NOUN").unwrap();
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
