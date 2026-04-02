use std::io::{BufRead, BufReader, Read};

use crate::format::{CorpusReader, ParsedSentence, ParsedToken};
use crate::Result;
use montre_core::Span;

pub struct ParseStats {
	pub sentences_parsed: usize,
	pub sentences_skipped: usize,
	pub tokens_parsed: usize,
}

impl ParseStats {
	fn new() -> Self {
		Self {
			sentences_parsed: 0,
			sentences_skipped: 0,
			tokens_parsed: 0,
		}
	}
}

pub struct ConllUReader<R> {
	reader: BufReader<R>,
	current_position: u64,
	line_number: usize,
	source_name: String,
	stats: ParseStats,
}

impl<R: Read> ConllUReader<R> {
	pub fn new(reader: R) -> Self {
		Self {
			reader: BufReader::new(reader),
			current_position: 0,
			line_number: 0,
			source_name: "<unknown>".into(),
			stats: ParseStats::new(),
		}
	}

	pub fn with_source_name(mut self, name: impl Into<String>) -> Self {
		self.source_name = name.into();
		self
	}

	pub fn stats(&self) -> &ParseStats {
		&self.stats
	}

	fn try_parse_sentence(
		&mut self,
		lines: &[String],
		start_line: usize,
	) -> Option<ParsedSentence> {
		let start_position = self.current_position;
		let mut tokens = Vec::new();
		let mut sent_id = None;

		for (offset, line) in lines.iter().enumerate() {
			if line.starts_with('#') {
				if let Some(rest) = line.strip_prefix("# sent_id = ")
					.or_else(|| line.strip_prefix("# sent_id="))
				{
					sent_id = Some(rest.trim().to_string());
				}
				continue;
			}

			let fields: Vec<&str> = line.split('\t').collect();
			if fields.len() < 10 {
				tracing::warn!(
					"Skipping malformed sentence at {}:{} (expected 10 fields, found {})",
					self.source_name,
					start_line + offset,
					fields.len()
				);
				return None;
			}

			let id = fields[0];
			if id.contains('-') || id.contains('.') {
				continue;
			}

			let token = ParsedToken {
				word: fields[1].to_string(),
				lemma: non_empty(fields[2]),
				upos: non_empty(fields[3]),
				xpos: non_empty(fields[4]),
				feats: non_empty(fields[5]),
				head: fields[6].parse().ok(),
				deprel: non_empty(fields[7]),
			};

			tokens.push(token);
			self.current_position += 1;
		}

		if tokens.is_empty() {
			return None;
		}

		Some(ParsedSentence {
			span: Span::new(start_position, self.current_position),
			tokens,
			sent_id,
		})
	}
}

fn non_empty(s: &str) -> Option<String> {
	if s == "_" || s.is_empty() {
		None
	} else {
		Some(s.to_string())
	}
}

impl<R: Read> ConllUReader<R> {
	pub fn read_sentences_strict(&mut self) -> Result<Vec<ParsedSentence>> {
		let mut sentences = Vec::new();
		let mut current_lines = Vec::new();
		let mut sentence_start_line = 1;
		let mut line = String::new();

		loop {
			line.clear();
			let bytes_read = self.reader.read_line(&mut line)?;
			self.line_number += 1;

			if bytes_read == 0 {
				if !current_lines.is_empty() {
					let sentence = self.parse_sentence_strict(&current_lines, sentence_start_line)?;
					if !sentence.tokens.is_empty() {
						self.stats.tokens_parsed += sentence.tokens.len();
						self.stats.sentences_parsed += 1;
						sentences.push(sentence);
					}
				}
				break;
			}

			let trimmed = line.trim();
			if trimmed.is_empty() {
				if !current_lines.is_empty() {
					let sentence = self.parse_sentence_strict(&current_lines, sentence_start_line)?;
					if !sentence.tokens.is_empty() {
						self.stats.tokens_parsed += sentence.tokens.len();
						self.stats.sentences_parsed += 1;
						sentences.push(sentence);
					}
					current_lines.clear();
				}
				sentence_start_line = self.line_number + 1;
			} else {
				current_lines.push(trimmed.to_string());
			}
		}

		Ok(sentences)
	}

	fn parse_sentence_strict(
		&mut self,
		lines: &[String],
		start_line: usize,
	) -> Result<ParsedSentence> {
		let start_position = self.current_position;
		let mut tokens = Vec::new();
		let mut sent_id = None;

		for (offset, line) in lines.iter().enumerate() {
			if line.starts_with('#') {
				if let Some(rest) = line.strip_prefix("# sent_id = ")
					.or_else(|| line.strip_prefix("# sent_id="))
				{
					sent_id = Some(rest.trim().to_string());
				}
				continue;
			}

			let fields: Vec<&str> = line.split('\t').collect();
			if fields.len() < 10 {
				return Err(crate::BuildError::Parse {
					line: start_line + offset,
					message: format!(
						"{}:{}: expected 10 fields, found {}",
						self.source_name,
						start_line + offset,
						fields.len()
					),
				});
			}

			let id = fields[0];
			if id.contains('-') || id.contains('.') {
				continue;
			}

			let token = ParsedToken {
				word: fields[1].to_string(),
				lemma: non_empty(fields[2]),
				upos: non_empty(fields[3]),
				xpos: non_empty(fields[4]),
				feats: non_empty(fields[5]),
				head: fields[6].parse().ok(),
				deprel: non_empty(fields[7]),
			};

			tokens.push(token);
			self.current_position += 1;
		}

		Ok(ParsedSentence {
			span: Span::new(start_position, self.current_position),
			tokens,
			sent_id,
		})
	}
}

impl<R: Read> CorpusReader for ConllUReader<R> {
	fn read_sentences(&mut self) -> Result<Vec<ParsedSentence>> {
		let mut sentences = Vec::new();
		let mut current_lines = Vec::new();
		let mut sentence_start_line = 1;
		let mut line = String::new();

		loop {
			line.clear();
			let bytes_read = self.reader.read_line(&mut line)?;
			self.line_number += 1;

			if bytes_read == 0 {
				if !current_lines.is_empty() {
					match self.try_parse_sentence(&current_lines, sentence_start_line) {
						Some(sentence) => {
							self.stats.tokens_parsed += sentence.tokens.len();
							self.stats.sentences_parsed += 1;
							sentences.push(sentence);
						}
						None => {
							self.stats.sentences_skipped += 1;
						}
					}
				}
				break;
			}

			let trimmed = line.trim();
			if trimmed.is_empty() {
				if !current_lines.is_empty() {
					match self.try_parse_sentence(&current_lines, sentence_start_line) {
						Some(sentence) => {
							self.stats.tokens_parsed += sentence.tokens.len();
							self.stats.sentences_parsed += 1;
							sentences.push(sentence);
						}
						None => {
							self.stats.sentences_skipped += 1;
						}
					}
					current_lines.clear();
				}
				sentence_start_line = self.line_number + 1;
			} else {
				current_lines.push(trimmed.to_string());
			}
		}

		Ok(sentences)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::io::Cursor;

	const SAMPLE_CONLLU: &str = r#"# sent_id = 1
# text = The cat sat.
1	The	the	DET	DT	_	2	det	_	_
2	cat	cat	NOUN	NN	_	3	nsubj	_	_
3	sat	sit	VERB	VBD	_	0	root	_	_
4	.	.	PUNCT	.	_	3	punct	_	_

# sent_id = 2
# text = Dogs bark.
1	Dogs	dog	NOUN	NNS	_	2	nsubj	_	_
2	bark	bark	VERB	VBP	_	0	root	_	_
3	.	.	PUNCT	.	_	2	punct	_	_
"#;

	const MALFORMED_CONLLU: &str = r#"# sent_id = 1
# text = Good sentence.
1	Good	good	ADJ	JJ	_	2	amod	_	_
2	sentence	sentence	NOUN	NN	_	0	root	_	_
3	.	.	PUNCT	.	_	2	punct	_	_

# sent_id = 2
# text = Bad sentence - missing fields
1	Bad
2	sentence

# sent_id = 3
# text = Another good one.
1	Another	another	DET	DT	_	3	det	_	_
2	good	good	ADJ	JJ	_	3	amod	_	_
3	one	one	NOUN	NN	_	0	root	_	_
4	.	.	PUNCT	.	_	3	punct	_	_
"#;

	#[test]
	fn parse_valid_conllu() {
		let cursor = Cursor::new(SAMPLE_CONLLU);
		let mut reader = ConllUReader::new(cursor).with_source_name("test.conllu");
		let sentences = reader.read_sentences().unwrap();

		assert_eq!(sentences.len(), 2);
		assert_eq!(reader.stats().sentences_parsed, 2);
		assert_eq!(reader.stats().sentences_skipped, 0);
		assert_eq!(reader.stats().tokens_parsed, 7);

		assert_eq!(sentences[0].tokens.len(), 4);
		assert_eq!(sentences[0].tokens[0].word, "The");
		assert_eq!(sentences[1].tokens.len(), 3);
		assert_eq!(sentences[1].tokens[0].word, "Dogs");
	}

	#[test]
	fn skip_malformed_sentences() {
		let cursor = Cursor::new(MALFORMED_CONLLU);
		let mut reader = ConllUReader::new(cursor).with_source_name("malformed.conllu");
		let sentences = reader.read_sentences().unwrap();

		assert_eq!(sentences.len(), 2);
		assert_eq!(reader.stats().sentences_parsed, 2);
		assert_eq!(reader.stats().sentences_skipped, 1);

		assert_eq!(sentences[0].tokens[0].word, "Good");
		assert_eq!(sentences[1].tokens[0].word, "Another");
	}

	#[test]
	fn span_positions_account_for_skipped() {
		let cursor = Cursor::new(MALFORMED_CONLLU);
		let mut reader = ConllUReader::new(cursor);
		let sentences = reader.read_sentences().unwrap();

		assert_eq!(sentences[0].span.start, 0);
		assert_eq!(sentences[0].span.end, 3);
		assert_eq!(sentences[1].span.start, 3);
		assert_eq!(sentences[1].span.end, 7);
	}

	#[test]
	fn empty_input() {
		let cursor = Cursor::new("");
		let mut reader = ConllUReader::new(cursor);
		let sentences = reader.read_sentences().unwrap();
		assert!(sentences.is_empty());
	}

	#[test]
	fn comments_only() {
		let cursor = Cursor::new("# just a comment\n# another comment\n\n");
		let mut reader = ConllUReader::new(cursor);
		let sentences = reader.read_sentences().unwrap();
		assert!(sentences.is_empty());
	}

	#[test]
	fn skips_multiword_tokens() {
		let input = "\
1\tDon\tdo\tAUX\t_\t_\t0\troot\t_\t_
2-3\tdon't\t_\t_\t_\t_\t_\t_\t_\t_
2\tdo\tdo\tAUX\t_\t_\t0\troot\t_\t_
3\tnot\tnot\tPART\t_\t_\t2\tadvmod\t_\t_
4\tworry\tworry\tVERB\t_\t_\t2\txcomp\t_\t_
";
		let cursor = Cursor::new(input);
		let mut reader = ConllUReader::new(cursor);
		let sentences = reader.read_sentences().unwrap();

		assert_eq!(sentences.len(), 1);
		// Multi-word token line (2-3) should be skipped; tokens 1,2,3,4 should be kept
		assert_eq!(sentences[0].tokens.len(), 4);
		assert_eq!(sentences[0].tokens[0].word, "Don");
		assert_eq!(sentences[0].tokens[1].word, "do");
		assert_eq!(sentences[0].tokens[2].word, "not");
		assert_eq!(sentences[0].tokens[3].word, "worry");
	}

	#[test]
	fn skips_empty_node_tokens() {
		let input = "\
1\tThey\tthey\tPRON\t_\t_\t2\tnsubj\t_\t_
2\tran\trun\tVERB\t_\t_\t0\troot\t_\t_
2.1\tand\tand\tCCONJ\t_\t_\t_\t_\t_\t_
3\tjumped\tjump\tVERB\t_\t_\t2\tconj\t_\t_
";
		let cursor = Cursor::new(input);
		let mut reader = ConllUReader::new(cursor);
		let sentences = reader.read_sentences().unwrap();

		assert_eq!(sentences[0].tokens.len(), 3);
		assert_eq!(sentences[0].tokens[2].word, "jumped");
	}

	#[test]
	fn strict_mode_rejects_malformed() {
		let cursor = Cursor::new(MALFORMED_CONLLU);
		let mut reader = ConllUReader::new(cursor).with_source_name("test.conllu");
		let result = reader.read_sentences_strict();
		assert!(result.is_err());
	}

	#[test]
	fn underscore_fields_become_none() {
		let input = "1\tHello\t_\t_\t_\t_\t0\troot\t_\t_\n";
		let cursor = Cursor::new(input);
		let mut reader = ConllUReader::new(cursor);
		let sentences = reader.read_sentences().unwrap();

		let token = &sentences[0].tokens[0];
		assert_eq!(token.word, "Hello");
		assert!(token.lemma.is_none());
		assert!(token.upos.is_none());
		assert!(token.xpos.is_none());
	}
}
