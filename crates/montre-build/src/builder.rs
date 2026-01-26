use std::path::Path;

use montre_core::{layers, Value};
use montre_index::corpus::CorpusMeta;
use montre_index::forward::InMemoryForward;
use montre_index::inverted::InMemoryInverted;
use montre_index::lexicon::InMemoryLexicon;
use montre_index::spans::InMemorySpans;
use montre_index::{ForwardIndex, SpanIndex};

use crate::format::ParsedSentence;
use crate::Result;

pub struct CorpusBuilder {
	name: String,
	inverted: InMemoryInverted,
	forward: InMemoryForward,
	spans: InMemorySpans,
	lexicon: InMemoryLexicon,
	layer_indices: Vec<(String, usize)>,
}

impl CorpusBuilder {
	pub fn new(name: impl Into<String>) -> Self {
		let mut forward = InMemoryForward::new();
		let mut layer_indices = Vec::new();

		for &layer_name in &[layers::WORD, layers::LEMMA, layers::POS, layers::XPOS, layers::DEPREL] {
			let idx = forward.add_layer(layer_name);
			layer_indices.push((layer_name.to_string(), idx));
		}

		Self {
			name: name.into(),
			inverted: InMemoryInverted::new(),
			forward,
			spans: InMemorySpans::new(),
			lexicon: InMemoryLexicon::new(),
			layer_indices,
		}
	}

	pub fn add_sentences(&mut self, sentences: Vec<ParsedSentence>) {
		for sentence in sentences {
			self.spans.add_span("sentence", sentence.span);

			for (offset, token) in sentence.tokens.iter().enumerate() {
				let position = sentence.span.start + offset as u64;

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

				if let Some(ref deprel) = token.deprel {
					self.add_token_annotation(position, layers::DEPREL, deprel);
				}
			}
		}
	}

	fn add_token_annotation(&mut self, position: u64, layer: &str, value: &str) {
		self.inverted.insert(layer, value, [position]);

		if let Some((_, layer_idx)) = self.layer_indices.iter().find(|(name, _)| name == layer) {
			self.forward.set(*layer_idx, position, Value::from(value));
		}

		self.lexicon.add_term(layer, value);
	}

	pub fn build(mut self, output_path: impl AsRef<Path>) -> Result<()> {
		self.spans.finalize();

		let path = output_path.as_ref();
		if path.exists() {
			std::fs::remove_dir_all(path)?;
		}
		std::fs::create_dir_all(path)?;

		let meta = CorpusMeta {
			name: self.name,
			version: montre_index::index_version,
			token_count: self.forward.token_count(),
			layers: self.layer_indices.iter().map(|(n, _)| n.clone()).collect(),
			span_layers: self.spans.layers().into_iter().map(String::from).collect(),
		};

		let meta_json = serde_json::to_string_pretty(&serde_json::json!({
			"name": meta.name,
			"version": meta.version,
			"token_count": meta.token_count,
			"layers": meta.layers,
			"span_layers": meta.span_layers,
		}))?;
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
			"Wrote corpus: {} tokens, {} bytes inverted, {} bytes forward",
			meta.token_count,
			inverted_bytes.len(),
			forward_bytes.len()
		);

		Ok(())
	}
}

#[cfg(test)]
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
		builder.add_sentences(sentences);

		let positions = builder.inverted.get("word", "cat").unwrap();
		assert!(positions.contains(1));

		let positions = builder.inverted.get("pos", "NOUN").unwrap();
		assert!(positions.contains(1));
	}
}
